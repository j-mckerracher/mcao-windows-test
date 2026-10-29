"""Worker entry: fetch this attempt's intake, verify it, then hand it to trusted_bootstrap.

This is the image entrypoint (protocol-v1 gap 72, option (a)). Its one network effect is a single
HTTPS GET of the attempt's intake object, through the read-only signed URL the launcher passed as
MAO_INTAKE_URL. It uses only the standard library and the protocol models already in the image.

Order: read the run and attempt identifiers and the URL from the environment, then remove the URL
so no child process inherits it. Fetch the object. Parse it, check that it names this run and
attempt, and check that the bootstrap reproduces the accepted receipt. Only then write the two
documents to private temporary files and call trusted_bootstrap.main, which takes the claim. Every
refusal before that call ends the attempt with no claim, no source read, and no worker run.

The URL is never printed or logged. A failure message names the kind of failure, and for an HTTP
error its status code, never the URL or the response body.
"""

import http.client
import os
import sys
import tempfile
import urllib.error
import urllib.request
from collections.abc import Callable, MutableMapping
from datetime import datetime
from pathlib import Path
from typing import Final
from urllib.parse import urlsplit

from pydantic import ValidationError

from mao_cloud import trusted_bootstrap
from mao_cloud.protocol import (
    ATTEMPT_ID_ENV,
    INTAKE_URL_ENV,
    MAX_INTAKE_BYTES,
    RUN_ID_ENV,
    ProtocolError,
    WorkerIntake,
    verify_bootstrap,
)

FETCH_TIMEOUT_SECONDS: Final = 60.0


class IntakeUnavailable(Exception):
    """The intake could not be read over HTTPS. The message names the failure, never the URL."""


class IntakeMalformed(Exception):
    """The intake was read but is not a worker intake of the protocol's size and shape."""


class IntakeRefused(Exception):
    """The intake is well formed but does not verify for this run and attempt."""


class _RefuseRedirects(urllib.request.HTTPRedirectHandler):
    """A signed object is served where it is named. A redirect is refused, never followed."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[override]
        return None


_OPENER = urllib.request.build_opener(_RefuseRedirects)


def fetch_intake(url: str) -> bytes:
    """One HTTPS GET of the intake object, bounded to MAX_INTAKE_BYTES. Raises IntakeUnavailable."""
    if urlsplit(url).scheme != "https":
        raise IntakeUnavailable("the intake location is not an https URL")
    try:
        with _OPENER.open(url, timeout=FETCH_TIMEOUT_SECONDS) as response:
            if response.status != 200:
                raise IntakeUnavailable(f"HTTP {response.status}")
            return response.read(MAX_INTAKE_BYTES + 1)
    except urllib.error.HTTPError as exc:
        raise IntakeUnavailable(f"HTTP {exc.code}") from None
    except (OSError, ValueError, http.client.HTTPException) as exc:
        # str() of these can carry the URL, so only the exception type is reported.
        raise IntakeUnavailable(type(exc).__name__) from None


def verify_intake(body: bytes, *, run_id: str, attempt: int) -> WorkerIntake:
    """Parse the intake and check it is this attempt's and internally consistent, or raise."""
    if len(body) > MAX_INTAKE_BYTES:
        raise IntakeMalformed("the intake is larger than the protocol allows")
    try:
        intake = WorkerIntake.model_validate_json(body)
    except ValidationError:
        raise IntakeMalformed("the intake is not a valid worker intake") from None
    if intake.bootstrap.run_id != run_id or intake.bootstrap.attempt != attempt:
        raise IntakeRefused("the intake is for a different run or attempt")
    try:
        verify_bootstrap(intake.bootstrap, intake.receipt)
    except ProtocolError as exc:
        raise IntakeRefused(exc.message) from None
    return intake


def _write_private(path: Path, data: bytes) -> None:
    """Create the file owner-only and refuse to reuse an existing one."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as handle:
        handle.write(data)


def _report(message: str) -> None:
    print(f"worker entry refused: {message}", file=sys.stderr)


def main(
    *,
    environ: MutableMapping[str, str] | None = None,
    fetch: Callable[[str], bytes] = fetch_intake,
    ports_factory: Callable[
        [], trusted_bootstrap.Ports
    ] = trusted_bootstrap.production_ports,
    clock: Callable[[], datetime] | None = None,
) -> int:
    env = os.environ if environ is None else environ
    url = env.pop(INTAKE_URL_ENV, None)
    run_id = env.get(RUN_ID_ENV, "")
    attempt_text = env.get(ATTEMPT_ID_ENV, "")
    if not url or not run_id or not attempt_text.isdigit():
        _report("the run, attempt, or intake override is missing or malformed")
        return trusted_bootstrap.EXIT_UNREADABLE

    try:
        body = fetch(url)
    except IntakeUnavailable as exc:
        _report(f"the intake could not be read ({exc}); no attempt was claimed")
        return trusted_bootstrap.EXIT_UNAVAILABLE

    try:
        intake = verify_intake(body, run_id=run_id, attempt=int(attempt_text))
    except IntakeMalformed as exc:
        _report(f"{exc}; no attempt was claimed")
        return trusted_bootstrap.EXIT_UNREADABLE
    except IntakeRefused as exc:
        _report(f"{exc}; no attempt was claimed")
        return trusted_bootstrap.EXIT_REFUSED

    # The documents live only as long as the attempt's trusted run. The directory is created 0700.
    with tempfile.TemporaryDirectory(prefix="mao-attempt-") as directory:
        bootstrap_path = Path(directory) / "bootstrap.json"
        receipt_path = Path(directory) / "receipt.json"
        _write_private(bootstrap_path, intake.bootstrap.model_dump_json().encode())
        _write_private(receipt_path, intake.receipt.model_dump_json().encode())
        return trusted_bootstrap.main(
            ["--bootstrap", str(bootstrap_path), "--receipt", str(receipt_path)],
            ports_factory=ports_factory,
            clock=clock,
        )


if __name__ == "__main__":
    raise SystemExit(main())
