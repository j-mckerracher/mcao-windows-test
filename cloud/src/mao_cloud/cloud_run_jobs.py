"""Cloud Run Jobs launcher for DPES-809: jobs.run, execution get and execution cancel.

Worker intake gate: launch refuses before jobs.run unless the launcher is built with
intake_ready=True. The intake path is built (protocol-v1 gap 72, option (a)): the broker writes the
attempt's intake object and passes its read-only signed URL here. Production never sets the flag,
because the signer that mints that URL is built but not live-verified (DPES-899), and there are no
worker ports (DPES-807, DPES-804, DPES-819). With the gate closed no attempt is launched,
request_stop refuses with EXECUTION_BLOCKER,
and execution_stopped is False, which keeps capacity reserved. Everything below the gate is
exercised under mocks with the gate open.

With the gate open, launch is the only platform write that starts work. It calls jobs.run once per
dispatched attempt, with the run ID, attempt ID, and intake URL as container overrides and nothing
else. The URL is a credential: it is never logged or quoted in an error. The Operation that jobs.run
returns names the execution in its metadata, and the broker stores that name on the attempt by
compare-and-set. A transport error or a 5xx answer means the execution may exist, so it raises
LaunchAmbiguous and the run enters reconciling. Any other refusal raises LaunchFailed. Neither path
is retried here, and the broker never relaunches an attempt.

With a stored execution name, the launcher addresses that one execution and no other:
- execution_stopped reads it. A 404 is confirmed absence. A completed execution (completionTime set,
  no running tasks, not reconciling) is confirmed termination. Every other answer, and every
  failed read, is not stopped.
- request_stop cancels it and returns without waiting. The broker repeats the request on each
  reconcile until execution_stopped is True.

An attempt with no stored name is never reported stopped and its stop is refused. Cloud Run offers
no run or attempt filter, so such an execution cannot be found, and its capacity stays reserved
under the unknown-launch alert. This covers an ambiguous launch and a successful launch whose answer
named no execution.

Only the controller imports this module. Its requests and google-auth imports are declared by the
controller extra and are absent from the runner extra.
"""

import logging
import re
from collections.abc import Mapping
from typing import Any, Protocol

import google.auth
import requests
from google.auth.transport.requests import AuthorizedSession

from mao_cloud.broker import LaunchAmbiguous, LaunchFailed
from mao_cloud.protocol import (
    ATTEMPT_ID_ENV,
    INTAKE_URL_ENV,
    RUN_ID_ENV,
    RunRecord,
    WorkerBootstrap,
)

RUN_API = "https://run.googleapis.com/v2"
CLOUD_PLATFORM_SCOPE = "https://www.googleapis.com/auth/cloud-platform"
# (connect, read) seconds. A lost answer is ambiguous, so the wait is bounded, not retried.
TIMEOUT: tuple[float, float] = (10.0, 60.0)

_LOG = logging.getLogger("mao_cloud.cloud_run_jobs")

BOOTSTRAP_INTAKE_BLOCKER = (
    "the launch gate is closed in production: the worker intake is built, but the signer that mints "
    "its read capability is not live-verified (DPES-899) and there are no worker ports to run the "
    "attempt (DPES-807, DPES-804, DPES-819); jobs.run was not called"
)

EXECUTION_BLOCKER = (
    "execution stop and confirmation are not available in production: the launch gate is closed, "
    "so no attempt is launched and no execution exists to stop (DPES-899, DPES-807, DPES-804, "
    "DPES-819)"
)

NO_EXECUTION_BLOCKER = (
    "no execution name is recorded for this attempt, because the launch was ambiguous or its "
    "answer named no execution; the execution cannot be addressed, so its stop cannot be requested"
)


class LauncherUnavailable(RuntimeError):
    """A stop cannot be requested here. The broker records it and retries on the next reconcile."""


class JobsResponse(Protocol):
    status_code: int

    def json(self) -> Any: ...


class JobsSession(Protocol):
    def post(
        self, url: str, *, json: Mapping[str, Any], timeout: tuple[float, float]
    ) -> JobsResponse: ...

    def get(self, url: str, *, timeout: tuple[float, float]) -> JobsResponse: ...


class CloudRunJobLauncher:
    """Launcher over one Cloud Run job in one project and location.

    intake_ready defaults to False: production builds the launcher that way, so every launch is
    refused. True opens the launch and stop paths. Only tests set it: production cannot yet mint
    intake capabilities or run workers (see BOOTSTRAP_INTAKE_BLOCKER).
    """

    def __init__(
        self,
        *,
        session: JobsSession,
        project: str,
        location: str,
        job: str,
        intake_ready: bool = False,
    ) -> None:
        self._session = session
        self._intake_ready = intake_ready
        self._url = f"{RUN_API}/projects/{project}/locations/{location}/jobs/{job}:run"
        # Only an execution of this job in this location is accepted as a stored name.
        self._execution_name = re.compile(
            rf"projects/[A-Za-z0-9.:-]+/locations/{re.escape(location)}/jobs/{re.escape(job)}"
            r"/executions/[A-Za-z0-9-]+"
        )

    def launch(self, bootstrap: WorkerBootstrap, intake_url: str) -> str | None:
        if not self._intake_ready:
            raise LaunchFailed(BOOTSTRAP_INTAKE_BLOCKER)
        # Exactly three overrides. The intake URL is a credential, so it is sent and never logged.
        body = {
            "overrides": {
                "containerOverrides": [
                    {
                        "env": [
                            {"name": RUN_ID_ENV, "value": bootstrap.run_id},
                            {"name": ATTEMPT_ID_ENV, "value": str(bootstrap.attempt)},
                            {"name": INTAKE_URL_ENV, "value": intake_url},
                        ]
                    }
                ]
            }
        }
        try:
            response = self._session.post(self._url, json=body, timeout=TIMEOUT)
        except requests.RequestException as exc:
            raise LaunchAmbiguous(
                f"jobs.run got no answer ({type(exc).__name__}); the execution may exist"
            ) from exc
        status = response.status_code
        if status >= 500:
            raise LaunchAmbiguous(
                f"jobs.run answered HTTP {status}; the execution may exist"
            )
        if not 200 <= status < 300:
            raise LaunchFailed(f"jobs.run refused the launch: HTTP {status}")
        name = self._launched_execution(response)
        if name is None:
            _LOG.warning(
                "jobs.run answered HTTP %s without a usable execution name; "
                "that execution cannot be addressed",
                status,
            )
        return name

    def request_stop(self, record: RunRecord) -> None:
        if not self._intake_ready:
            raise LauncherUnavailable(EXECUTION_BLOCKER)
        name = record.execution_name
        if name is None:
            raise LauncherUnavailable(NO_EXECUTION_BLOCKER)
        try:
            response = self._session.post(
                f"{RUN_API}/{name}:cancel", json={}, timeout=TIMEOUT
            )
        except requests.RequestException as exc:
            raise LauncherUnavailable(
                f"cancel got no answer ({type(exc).__name__})"
            ) from exc
        status = response.status_code
        if status == 404:
            # Already gone. execution_stopped confirms absence on the next observation.
            return
        if not 200 <= status < 300:
            raise LauncherUnavailable(f"cancel refused: HTTP {status}")

    def execution_stopped(self, record: RunRecord) -> bool:
        if not self._intake_ready:
            # No attempt can have launched through a closed gate. Unknown is never stopped.
            return False
        name = record.execution_name
        if name is None:
            # Unknown is never stopped: the reservation stays until a stop can be confirmed.
            return False
        try:
            response = self._session.get(f"{RUN_API}/{name}", timeout=TIMEOUT)
        except requests.RequestException:
            return False
        status = response.status_code
        if status == 404:
            return True
        if not 200 <= status < 300:
            return False
        return _terminated(response)

    def _launched_execution(self, response: JobsResponse) -> str | None:
        """The execution the Operation's metadata names, or None when it is absent or malformed."""
        try:
            answer = response.json()
        except ValueError:
            return None
        metadata = answer.get("metadata") if isinstance(answer, dict) else None
        name = metadata.get("name") if isinstance(metadata, dict) else None
        if isinstance(name, str) and self._execution_name.fullmatch(name):
            return name
        return None


def _terminated(response: JobsResponse) -> bool:
    """True only for a completed execution with no running task and no reconcile in progress."""
    try:
        body = response.json()
    except ValueError:
        return False
    if not isinstance(body, dict) or body.get("reconciling"):
        return False
    if not body.get("completionTime"):
        return False
    # Proto3 JSON omits a zero count, so an absent runningCount means none are running.
    return str(body.get("runningCount", 0)) == "0"


def open_session() -> AuthorizedSession:
    """An authorized session under the runtime service account. Credentials are discovered here."""
    credentials, _ = google.auth.default(scopes=[CLOUD_PLATFORM_SCOPE])
    return AuthorizedSession(credentials)
