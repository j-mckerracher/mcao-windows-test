"""Signer server: the only process that signs object capabilities (DPES-899).

The signer image's entrypoint is this module. It reads its configuration from the environment, opens
the run store and a URL signer that signs as the signer service account, and serves
POST /v1/capabilities on Cloud Run's PORT through SignerService. Each step refuses in order, and
nothing is bound until every step passes:

1. PORT is a TCP port number (mao_cloud.serve.port_from). Exit 2 otherwise.
2. The signer configuration is complete and valid (load_config). Exit 2 otherwise, naming each
   missing or invalid setting and echoing no value.
3. Every SDK client opens (build_service). Exit 3 otherwise.

Every request must carry a Google ID token that verifies against MAO_CLOUD_SIGNER_AUDIENCE and has
a verified email. SignerService then requires that email to be MAO_CLOUD_CONTROLLER_IDENTITY, so
this layer only names the caller. A refusal body holds the refusal code and its fixed message,
never the request or any run data. The wire shapes are in mao_cloud.signer_wire.

Nothing here has been run against live Firestore, Cloud Storage, IAM or Cloud Run. Every live
behaviour is unverified until a deployed signer proves it.
"""

import logging
import os
import re
import sys
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any

import uvicorn
from fastapi import FastAPI, Request
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse
from google.auth.transport import requests as google_requests
from google.oauth2 import id_token
from pydantic import ValidationError

from mao_cloud.bearer import (
    CERT_FETCH_TIMEOUT_SECONDS,
    bearer_credential,
    bounded_fetch,
)
from mao_cloud.broker import SystemClock
from mao_cloud.firestore_store import (
    FirestoreClient,
    FirestoreRunStore,
    production_firestore_client,
)
from mao_cloud.production import (
    DEFAULT_FIRESTORE_DATABASE,
    ENV_FIRESTORE_DATABASE,
    ENV_OBJECT_BUCKET,
    ENV_PROJECT,
    PROJECT_ID,
)
from mao_cloud.serve import EXIT_CONFIG, EXIT_UNAVAILABLE, HOST, PortError, port_from
from mao_cloud.signer import (
    CapabilityRequest,
    RefusalCode,
    SignerRefused,
    SignerService,
    SigningBackend,
)
from mao_cloud.signer_wire import (
    CAPABILITY_PATH,
    SIGNER_ORIGIN,
    issued_body,
    refusal_body,
    status_for,
)

ENV_SIGNER_AUDIENCE = "MAO_CLOUD_SIGNER_AUDIENCE"
ENV_CONTROLLER_IDENTITY = "MAO_CLOUD_CONTROLLER_IDENTITY"
ENV_SIGNING_IDENTITY = "MAO_CLOUD_SIGNING_IDENTITY"
_REQUIRED = (
    ENV_PROJECT,
    ENV_OBJECT_BUCKET,
    ENV_SIGNER_AUDIENCE,
    ENV_CONTROLLER_IDENTITY,
    ENV_SIGNING_IDENTITY,
)
# A user-managed service account email. The controller identity is compared with a token's email,
# and the signing identity is put into an IAM request path, so both must match exactly.
_SERVICE_ACCOUNT = re.compile(
    r"[a-z][a-z0-9-]{4,28}[a-z0-9]@[a-z0-9.-]+\.iam\.gserviceaccount\.com"
)
# A capability request is a few hundred bytes. A larger body is refused before it is parsed.
MAX_REQUEST_BYTES = 8192

_AUDIT = logging.getLogger("mao_cloud.signer.audit")


class SignerConfigError(Exception):
    """Required signer configuration is missing or invalid. Startup refuses."""


class SignerUnavailable(Exception):
    """A signer client could not be opened. Startup refuses and binds nothing."""


@dataclass(frozen=True)
class SignerConfig:
    project: str
    firestore_database: str
    object_bucket: str
    audience: str
    controller_identity: str
    signing_identity: str


def load_config(environ: Mapping[str, str]) -> SignerConfig:
    """Read every signer setting from environ. Each required name must be present and well formed."""
    values = {
        name: environ.get(name, "").strip()
        for name in (*_REQUIRED, ENV_FIRESTORE_DATABASE)
    }
    missing = [name for name in _REQUIRED if not values[name]]
    if missing:
        raise SignerConfigError("missing required configuration: " + ", ".join(missing))
    # Every malformed setting is named together, and no value is echoed into the refusal.
    invalid: list[str] = []
    if PROJECT_ID.fullmatch(values[ENV_PROJECT]) is None:
        invalid.append(f"{ENV_PROJECT} must be a Google Cloud project ID")
    if SIGNER_ORIGIN.fullmatch(values[ENV_SIGNER_AUDIENCE]) is None:
        invalid.append(
            f"{ENV_SIGNER_AUDIENCE} must be the https origin of the signer service"
        )
    for name in (ENV_CONTROLLER_IDENTITY, ENV_SIGNING_IDENTITY):
        if _SERVICE_ACCOUNT.fullmatch(values[name]) is None:
            invalid.append(f"{name} must be a service account email")
    if invalid:
        raise SignerConfigError("; ".join(invalid))
    return SignerConfig(
        project=values[ENV_PROJECT],
        firestore_database=values[ENV_FIRESTORE_DATABASE] or DEFAULT_FIRESTORE_DATABASE,
        object_bucket=values[ENV_OBJECT_BUCKET],
        audience=values[ENV_SIGNER_AUDIENCE],
        controller_identity=values[ENV_CONTROLLER_IDENTITY],
        signing_identity=values[ENV_SIGNING_IDENTITY],
    )


class IdTokenCaller:
    """Names the verified caller of one request: the email of a verified Google ID token, or None."""

    def __init__(self, *, audience: str, transport: Callable[..., Any]) -> None:
        self._audience = audience
        self._transport = bounded_fetch(transport, CERT_FETCH_TIMEOUT_SECONDS)

    def email_of(self, request: Request) -> str | None:
        token = bearer_credential(request)
        if token is None:
            _AUDIT.info("refused reason=no_credential")
            return None
        try:
            claims = id_token.verify_oauth2_token(
                token, self._transport, audience=self._audience
            )
        except (
            Exception
        ) as exc:  # a bad token or a failed certificate fetch: refuse either way
            # Only the exception type is logged: its message can echo token content.
            _AUDIT.info("refused reason=token_invalid error=%s", type(exc).__name__)
            return None
        email = claims.get("email")
        if claims.get("email_verified") is not True or not isinstance(email, str):
            _AUDIT.info("refused reason=email_unverified")
            return None
        return email


def create_app(*, service: SignerService, caller: IdTokenCaller) -> FastAPI:
    """The signer's HTTP app: one route, which answers with an issued capability or a refusal."""
    app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)

    @app.post(CAPABILITY_PATH)
    async def issue(request: Request) -> JSONResponse:
        raw = bytearray()
        async for chunk in request.stream():
            raw.extend(chunk[: MAX_REQUEST_BYTES + 1 - len(raw)])
            if len(raw) > MAX_REQUEST_BYTES:
                break
        return await run_in_threadpool(_answer, service, caller, request, bytes(raw))

    return app


def _answer(
    service: SignerService, caller: IdTokenCaller, request: Request, raw: bytes
) -> JSONResponse:
    """Authenticate, parse, then sign or refuse. Verification and signing block, so this runs
    off the event loop."""
    email = caller.email_of(request)
    if email is None:
        return _refusal(RefusalCode.UNAUTHENTICATED)
    parsed = _parse(raw)
    if parsed is None:
        return _refusal(RefusalCode.MALFORMED_REQUEST)
    try:
        issued = service.issue(caller=email, request=parsed)
    except SignerRefused as refusal:
        return _refusal(refusal.code)
    except (
        Exception
    ) as exc:  # the run store or the URL signer failed: nothing was signed
        _AUDIT.warning("unavailable error=%s", type(exc).__name__)
        return _refusal(RefusalCode.UNAVAILABLE)
    return JSONResponse(status_code=200, content=issued_body(issued))


def _parse(raw: bytes) -> CapabilityRequest | None:
    if len(raw) > MAX_REQUEST_BYTES:
        return None
    try:
        return CapabilityRequest.model_validate_json(raw)
    except ValidationError:
        return None


def _refusal(code: RefusalCode) -> JSONResponse:
    _AUDIT.info("refused code=%s", code.value)
    return JSONResponse(status_code=status_for(code), content=refusal_body(code))


def _open_signing_backend(
    *, project: str, name: str, signing_identity: str
) -> SigningBackend:
    # Imported here so that importing this module does not load google-cloud-storage.
    from mao_cloud.gcs_sdk import open_signer

    return open_signer(project=project, name=name, signing_identity=signing_identity)


@dataclass(frozen=True)
class SignerSdk:
    """The factories that open SDK clients. The defaults build real clients; tests inject fakes."""

    firestore_client: Callable[..., FirestoreClient] = production_firestore_client
    signing_backend: Callable[..., SigningBackend] = _open_signing_backend


def build_service(config: SignerConfig, sdk: SignerSdk | None = None) -> SignerService:
    """Open the run store and the URL signer, then assemble the service. Opening calls no API."""
    factories = SignerSdk() if sdk is None else sdk
    try:
        firestore = factories.firestore_client(
            project=config.project, database=config.firestore_database
        )
        backend = factories.signing_backend(
            project=config.project,
            name=config.object_bucket,
            signing_identity=config.signing_identity,
        )
    except Exception as exc:
        # Only the exception type is reported: client construction can carry credential detail.
        raise SignerUnavailable(
            f"could not open a signer client ({type(exc).__name__})"
        ) from exc
    return SignerService(
        runs=FirestoreRunStore(firestore),
        backend=backend,
        controller_identity=config.controller_identity,
        now=SystemClock().now,
    )


def _serve(app: FastAPI, *, host: str, port: int) -> None:
    uvicorn.run(app, host=host, port=port, log_level="info")


def main(
    *,
    environ: Mapping[str, str] | None = None,
    make_service: Callable[[SignerConfig], SignerService] | None = None,
    serve: Callable[..., None] = _serve,
    transport: Callable[..., Any] | None = None,
) -> int:
    env = os.environ if environ is None else environ
    build = make_service or build_service
    try:
        port = port_from(env)
        config = load_config(env)
        service = build(config)
    except (PortError, SignerConfigError) as exc:
        print(f"mao-cloud-signer: {exc}", file=sys.stderr)
        return EXIT_CONFIG
    except SignerUnavailable as exc:
        print(f"mao-cloud-signer: refusing to start. {exc}", file=sys.stderr)
        return EXIT_UNAVAILABLE
    caller = IdTokenCaller(
        audience=config.audience,
        transport=google_requests.Request() if transport is None else transport,
    )
    serve(create_app(service=service, caller=caller), host=HOST, port=port)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
