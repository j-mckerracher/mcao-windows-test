"""mao-cloud: schema export and check, offline validation, caller tokens, and snapshot submit.

Exit codes: 0 success or plan, 1 the input or schema is refused, 2 the input cannot be read or used
(including --execute without a transport), 75 retry the same command later.
"""

import argparse
import re
import sys
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any

from pydantic import ValidationError

from mao_cloud import schemas
from mao_cloud.config import API_AUDIENCE_PATTERN
from mao_cloud.doctor import run_doctor
from mao_cloud.documents import InputError, read_json
from mao_cloud.protocol import (
    ProtocolError,
    WorkerBootstrap,
    parse_create_request,
    parse_document,
    parse_start_request,
)
from mao_cloud.run_record import RecordStore
from mao_cloud.source import SourceRefused, SourceUnreadable, export_snapshot
from mao_cloud.submit import SubmitRequest, Transport, new_request_uuid, plan, submit_snapshot

_VALIDATORS: dict[str, Callable[[Any], object]] = {
    "create": parse_create_request,
    "start": parse_start_request,
    "bootstrap": lambda payload: parse_document(WorkerBootstrap, payload),
}


def _schema_export(args: argparse.Namespace) -> int:
    written = schemas.export(args.out)
    print(f"mao-cloud: wrote {len(written)} schema files to {args.out}")
    return 0


def _schema_check(args: argparse.Namespace) -> int:
    problems = schemas.check(args.dir)
    for problem in problems:
        print(f"mao-cloud: {problem}", file=sys.stderr)
    if problems:
        return 1
    print(f"mao-cloud: {args.dir} matches the protocol-v1 schemas")
    return 0


def _doctor(args: argparse.Namespace) -> int:
    try:
        report = run_doctor(args.repo, args.ref)
    except SourceUnreadable as exc:
        print(f"mao-cloud: {exc}", file=sys.stderr)
        return 2
    for line in report.lines:
        print(f"mao-cloud: {line}")
    return 0 if report.ok else 1


def _submit(args: argparse.Namespace) -> int:
    """Export the branch, then plan (default) or submit (--execute). Plan mode writes nothing."""
    if args.execute and args.transport is None:
        print(
            "mao-cloud: --execute needs a service transport, and none is configured",
            file=sys.stderr,
        )
        return 2
    request = SubmitRequest(
        task_id=args.task_id,
        prompt_id=args.prompt_id,
        toolchain_profile=args.profile,
        target=args.target,
        harness=args.harness,
        model=args.model,
        thinking=args.thinking,
        max_runtime_seconds=args.max_runtime_seconds,
        workflow_correlation=args.workflow_correlation,
        request_uuid=args.request_uuid or new_request_uuid(),
    )
    try:
        snapshot = export_snapshot(args.repo, args.ref)
    except SourceUnreadable as exc:
        print(f"mao-cloud: {exc}", file=sys.stderr)
        return 2
    except SourceRefused as exc:
        print(f"mao-cloud: refused: {exc}", file=sys.stderr)
        return 1
    try:
        prepared = plan(snapshot, request)
    except ValidationError as exc:
        fields = sorted({".".join(str(part) for part in error["loc"]) for error in exc.errors()})
        print(f"mao-cloud: submit refused: invalid field(s): {', '.join(fields)}", file=sys.stderr)
        return 1
    if not args.execute:
        print("mao-cloud: plan ok; nothing was sent (add --execute to submit)")
        print(f"mao-cloud: request-uuid {prepared.request_uuid}")
        print(f"mao-cloud: canonical-digest {prepared.canonical_digest}")
        print(
            f"mao-cloud: {args.ref} at {snapshot.base_sha}, archive {snapshot.archive_size} bytes"
        )
        return 0
    result = submit_snapshot(
        snapshot,
        request,
        records=RecordStore(args.state_dir),
        transport=args.transport,
    )
    print(f"mao-cloud: request-uuid {prepared.request_uuid}")
    if result.exit_code == 0:
        print(f"mao-cloud: run {result.run_id} started")
        return 0
    print(f"mao-cloud: {result.code}: {result.message}", file=sys.stderr)
    if result.exit_code == 75:
        print(
            "mao-cloud: the outcome is unknown or paused; rerun the same command with the same "
            "--request-uuid to resume it",
            file=sys.stderr,
        )
    return result.exit_code


def _validate(args: argparse.Namespace) -> int:
    try:
        payload = read_json(args.file)
    except InputError as exc:
        print(f"mao-cloud: {exc}", file=sys.stderr)
        return 2
    try:
        _VALIDATORS[args.kind](payload)
    except ProtocolError as exc:
        print(f"mao-cloud: {args.kind} refused: {exc.code.value}: {exc.message}", file=sys.stderr)
        return 1
    except ValidationError as exc:
        print(f"mao-cloud: {args.kind} refused: {exc.error_count()} validation error(s)", file=sys.stderr)
        return 1
    print(f"mao-cloud: {args.kind} document is valid")
    return 0


class TokenError(RuntimeError):
    """A caller token could not be obtained. Messages never carry a token or a response body."""


_INVOKER_PATTERN = re.compile(
    r"^[a-z][a-z0-9-]{4,62}@[a-z][a-z0-9-]{4,62}\.iam\.gserviceaccount\.com$"
)
_COMPACT_JWS = re.compile(r"^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$")
_IAM_GENERATE_ID_TOKEN = (
    "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/{invoker}:generateIdToken"
)
_CLOUD_PLATFORM = "https://www.googleapis.com/auth/cloud-platform"
_IAM_TIMEOUT_SECONDS = 30


def mint_invoker_token(*, audience: str, invoker: str, session: Any | None = None) -> str:
    """Mint an ID token for the control API as the invoker service account, using ADC.

    The audience is the control API origin and the token carries the invoker's email. Inputs are
    checked before any network call. The token is returned to the caller and never printed here.
    """
    if not API_AUDIENCE_PATTERN.fullmatch(audience):
        raise TokenError("audience must be the https origin of the control API")
    if not _INVOKER_PATTERN.fullmatch(invoker):
        raise TokenError("invoker must be a service account email")
    http = session if session is not None else _adc_session()
    response = http.post(
        _IAM_GENERATE_ID_TOKEN.format(invoker=invoker),
        json={"audience": audience, "includeEmail": True},
        timeout=_IAM_TIMEOUT_SECONDS,
    )
    if response.status_code != 200:
        raise TokenError(f"generateIdToken refused with HTTP {response.status_code}")
    try:
        body = response.json()
    except ValueError as exc:
        raise TokenError("generateIdToken returned an unreadable body") from exc
    token = body.get("token") if isinstance(body, dict) else None
    if not isinstance(token, str) or not _COMPACT_JWS.fullmatch(token):
        raise TokenError("generateIdToken returned no ID token")
    return token


def invoker_headers(token: str) -> dict[str, str]:
    """Both headers a call to the control API needs, carrying the same token.

    Cloud Run's invoker check reads X-Serverless-Authorization. The broker verifies Authorization.
    """
    if not _COMPACT_JWS.fullmatch(token):
        raise TokenError("token is not a compact JWS")
    value = f"Bearer {token}"
    return {"Authorization": value, "X-Serverless-Authorization": value}


def _adc_session() -> Any:
    """An authorized session from Application Default Credentials, built only when needed."""
    import google.auth
    from google.auth.transport.requests import AuthorizedSession

    credentials, _project = google.auth.default(scopes=[_CLOUD_PLATFORM])
    return AuthorizedSession(credentials)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="mao-cloud", description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)

    schema = commands.add_parser("schema", help="export or check the protocol-v1 schemas")
    schema_actions = schema.add_subparsers(dest="action", required=True)
    export = schema_actions.add_parser("export", help="write the schemas to a directory")
    export.add_argument("--out", required=True, type=Path)
    export.set_defaults(handler=_schema_export)
    check = schema_actions.add_parser("check", help="verify a directory matches the schemas")
    check.add_argument("--dir", required=True, type=Path)
    check.set_defaults(handler=_schema_check)

    validate = commands.add_parser("validate", help="validate one protocol document offline")
    kinds = validate.add_subparsers(dest="kind", required=True)
    for kind in _VALIDATORS:
        leaf = kinds.add_parser(kind, help=f"validate a {kind} document")
        leaf.add_argument("file", type=Path)
        leaf.set_defaults(handler=_validate)

    doctor = commands.add_parser("doctor", help="check that a branch can be submitted, offline")
    doctor.add_argument("--repo", required=True, type=Path)
    doctor.add_argument("--ref", required=True, help="branch name, such as main")
    doctor.set_defaults(handler=_doctor)

    submit = commands.add_parser("submit", help="export a branch and submit it as one run")
    submit.add_argument("--repo", required=True, type=Path)
    submit.add_argument("--ref", required=True, help="branch name, such as main")
    submit.add_argument(
        "--state-dir", required=True, type=Path, help="durable local record directory"
    )
    submit.add_argument("--task-id", required=True)
    submit.add_argument("--prompt-id", required=True)
    submit.add_argument("--profile", required=True, help="toolchain profile id")
    submit.add_argument("--target", required=True)
    submit.add_argument("--harness", required=True)
    submit.add_argument("--model", required=True)
    submit.add_argument("--thinking", required=True)
    submit.add_argument("--max-runtime-seconds", required=True, type=int)
    submit.add_argument("--workflow-correlation", required=True)
    submit.add_argument(
        "--request-uuid",
        help="resume a request by reusing its UUID; a new one is generated when omitted",
    )
    submit.add_argument(
        "--execute", action="store_true", help="send the request; otherwise plan only"
    )
    submit.set_defaults(handler=_submit)
    return parser


def main(argv: Sequence[str] | None = None, *, transport: Transport | None = None) -> int:
    args = build_parser().parse_args(argv)
    args.transport = transport
    return int(args.handler(args))


if __name__ == "__main__":
    raise SystemExit(main())
