"""The Cloud Run Jobs launcher (DPES-809): one jobs.run per launch, the execution name it returns,
and the stop and confirmation of that one execution.

No network, no credentials. The authorized session is a fake that records each request. Live
jobs.run behaviour, the Operation metadata shape, the cancel and get endpoints, and whether a 404
means absence are unverified until a deployed run proves them.
"""

import logging
from typing import Any

import google.auth
import pytest
import requests
from fakes import run_record, worker_bootstrap
from mao_cloud import cloud_run_jobs
from mao_cloud.broker import LaunchAmbiguous, LaunchFailed
from mao_cloud.cloud_run_jobs import (
    ATTEMPT_ID_ENV,
    CLOUD_PLATFORM_SCOPE,
    INTAKE_URL_ENV,
    NO_EXECUTION_BLOCKER,
    RUN_API,
    RUN_ID_ENV,
    TIMEOUT,
    CloudRunJobLauncher,
    LauncherUnavailable,
)
from mao_cloud.protocol import RunRecord

URL = f"{RUN_API}/projects/mao-dev/locations/us-central1/jobs/mao-worker:run"
EXECUTION = "projects/mao-dev/locations/us-central1/jobs/mao-worker/executions/mao-worker-7f2kq"
EXECUTION_URL = f"{RUN_API}/{EXECUTION}"
# A fake signed URL. It stands for a credential, so tests assert on it only by identity, never by
# quoting it in a message.
INTAKE_URL = "https://storage.invalid/runs/run_x/attempts/000001/intake.json?X-Goog-Signature=fake"


class FakeResponse:
    """An HTTP answer. json_error makes .json() raise the way a non-JSON body does."""

    def __init__(self, status_code: int, body: Any = None, *, json_error: bool = False) -> None:
        self.status_code = status_code
        self._body = body
        self._json_error = json_error

    def json(self) -> Any:
        if self._json_error:
            raise ValueError("not JSON")
        return self._body


def operation(name: str | None) -> dict[str, Any]:
    """The Operation jobs.run answers with: the execution name sits in its metadata."""
    metadata: dict[str, Any] = {"@type": "type.googleapis.com/google.cloud.run.v2.Execution"}
    if name is not None:
        metadata["name"] = name
    return {"name": "operations/op-1", "metadata": metadata}


def execution(**fields: Any) -> FakeResponse:
    """A 200 answer for the execution resource, with the given fields."""
    return FakeResponse(200, fields)


class FakeSession:
    """Records every request. Each method answers with its own response, or raises its own error."""

    def __init__(
        self,
        *,
        post: FakeResponse | Exception | None = None,
        get: FakeResponse | Exception | None = None,
    ) -> None:
        self.posts: list[tuple[str, dict[str, Any], tuple[float, float]]] = []
        self.gets: list[tuple[str, tuple[float, float]]] = []
        self._post = FakeResponse(200, operation(EXECUTION)) if post is None else post
        self._get = FakeResponse(404) if get is None else get

    def post(
        self, url: str, *, json: dict[str, Any], timeout: tuple[float, float]
    ) -> FakeResponse:
        self.posts.append((url, json, timeout))
        if isinstance(self._post, Exception):
            raise self._post
        return self._post

    def get(self, url: str, *, timeout: tuple[float, float]) -> FakeResponse:
        self.gets.append((url, timeout))
        if isinstance(self._get, Exception):
            raise self._get
        return self._get


def launcher(session: FakeSession) -> CloudRunJobLauncher:
    """The launcher with the intake gate open. Production never builds it this way."""
    return CloudRunJobLauncher(
        session=session,
        project="mao-dev",
        location="us-central1",
        job="mao-worker",
        intake_ready=True,
    )


def closed_launcher(session: FakeSession) -> CloudRunJobLauncher:
    """The launcher as production builds it: no intake_ready flag, so the gate is closed."""
    return CloudRunJobLauncher(
        session=session, project="mao-dev", location="us-central1", job="mao-worker"
    )


def stored(name: str | None = EXECUTION) -> RunRecord:
    """An attempt record that holds the given execution name, or none."""
    return run_record(execution_name=name)


# Worker intake gate, closed as production builds it -----------------------


def test_the_gate_is_closed_by_default_and_refuses_launch_before_any_request() -> None:
    session = FakeSession()

    with pytest.raises(LaunchFailed, match="the launch gate is closed in production"):
        closed_launcher(session).launch(worker_bootstrap(), INTAKE_URL)

    assert session.posts == []
    assert session.gets == []


@pytest.mark.parametrize("status", [200, 201, 400, 429, 500, 503])
def test_a_closed_gate_sends_nothing_whatever_the_answer_would_have_been(status: int) -> None:
    session = FakeSession(post=FakeResponse(status, operation(EXECUTION)))

    with pytest.raises(LaunchFailed):
        closed_launcher(session).launch(worker_bootstrap(), INTAKE_URL)

    assert session.posts == []


def test_a_closed_gate_refuses_stop_and_reports_no_stop_even_with_a_stored_name() -> None:
    session = FakeSession()

    with pytest.raises(
        LauncherUnavailable, match="execution stop and confirmation are not available"
    ):
        closed_launcher(session).request_stop(stored())
    assert closed_launcher(session).execution_stopped(stored()) is False

    assert session.posts == []
    assert session.gets == []


# Launch ------------------------------------------------------------------


def test_launch_posts_one_jobs_run_with_only_the_three_overrides() -> None:
    bootstrap = worker_bootstrap()
    session = FakeSession()

    launcher(session).launch(bootstrap, INTAKE_URL)

    assert session.posts == [
        (
            URL,
            {
                "overrides": {
                    "containerOverrides": [
                        {
                            "env": [
                                {"name": RUN_ID_ENV, "value": bootstrap.run_id},
                                {
                                    "name": ATTEMPT_ID_ENV,
                                    "value": str(bootstrap.attempt),
                                },
                                {"name": INTAKE_URL_ENV, "value": INTAKE_URL},
                            ]
                        }
                    ]
                }
            },
            TIMEOUT,
        )
    ]
    assert session.gets == []


@pytest.mark.parametrize(
    "failure",
    [
        requests.ConnectionError("connection reset"),
        FakeResponse(500),
        FakeResponse(429),
    ],
    ids=["transport", "server", "refused"],
)
def test_the_intake_url_is_never_logged_or_quoted_in_a_launch_failure(
    failure: Any, caplog: pytest.LogCaptureFixture
) -> None:
    session = FakeSession(post=failure)

    with caplog.at_level(logging.DEBUG):
        with pytest.raises((LaunchAmbiguous, LaunchFailed)) as raised:
            launcher(session).launch(worker_bootstrap(), INTAKE_URL)

    # The URL reaches the request body only, never a log record or an exception message.
    assert INTAKE_URL not in caplog.text
    assert INTAKE_URL not in str(raised.value)
    assert INTAKE_URL not in repr(raised.value)


@pytest.mark.parametrize("status", [200, 201])
def test_a_2xx_answer_returns_the_execution_name_from_the_operation(status: int) -> None:
    session = FakeSession(post=FakeResponse(status, operation(EXECUTION)))

    assert launcher(session).launch(worker_bootstrap(), INTAKE_URL) == EXECUTION


def test_a_2xx_answer_with_no_body_names_no_execution_and_says_so(
    caplog: pytest.LogCaptureFixture,
) -> None:
    session = FakeSession(post=FakeResponse(204))

    with caplog.at_level(logging.WARNING, logger="mao_cloud.cloud_run_jobs"):
        assert launcher(session).launch(worker_bootstrap(), INTAKE_URL) is None

    assert "without a usable execution name" in caplog.text


@pytest.mark.parametrize(
    "answer",
    [
        pytest.param(operation(None), id="metadata-without-name"),
        pytest.param({"name": "operations/op-1"}, id="no-metadata"),
        pytest.param({"metadata": "Execution"}, id="metadata-not-an-object"),
        pytest.param(operation(42), id="name-not-a-string"),
        pytest.param(operation("mao-worker-7f2kq"), id="bare-execution-id"),
        pytest.param(
            operation(EXECUTION.replace("us-central1", "europe-west1")),
            id="other-location",
        ),
        pytest.param(operation(EXECUTION.replace("mao-worker", "other-job")), id="other-job"),
        pytest.param(operation(EXECUTION + "/extra"), id="trailing-path"),
        pytest.param(["not", "an", "object"], id="json-list"),
    ],
)
def test_an_answer_naming_no_usable_execution_returns_none_and_logs(
    answer: Any, caplog: pytest.LogCaptureFixture
) -> None:
    session = FakeSession(post=FakeResponse(200, answer))

    with caplog.at_level(logging.WARNING, logger="mao_cloud.cloud_run_jobs"):
        assert launcher(session).launch(worker_bootstrap(), INTAKE_URL) is None

    assert "without a usable execution name" in caplog.text


def test_a_non_json_2xx_answer_returns_none_and_logs(caplog: pytest.LogCaptureFixture) -> None:
    session = FakeSession(post=FakeResponse(200, json_error=True))

    with caplog.at_level(logging.WARNING, logger="mao_cloud.cloud_run_jobs"):
        assert launcher(session).launch(worker_bootstrap(), INTAKE_URL) is None

    assert "without a usable execution name" in caplog.text


@pytest.mark.parametrize("status", [500, 502, 503])
def test_a_5xx_answer_is_ambiguous_because_the_execution_may_exist(status: int) -> None:
    session = FakeSession(post=FakeResponse(status))

    with pytest.raises(LaunchAmbiguous):
        launcher(session).launch(worker_bootstrap(), INTAKE_URL)

    assert len(session.posts) == 1


@pytest.mark.parametrize("status", [400, 401, 403, 404, 409, 429])
def test_a_4xx_answer_is_a_refused_launch(status: int) -> None:
    with pytest.raises(LaunchFailed):
        launcher(FakeSession(post=FakeResponse(status))).launch(worker_bootstrap(), INTAKE_URL)


@pytest.mark.parametrize(
    "error",
    [requests.ConnectionError("connection reset"), requests.Timeout("read timed out")],
    ids=["connection-error", "timeout"],
)
def test_a_transport_error_is_ambiguous_and_the_launch_is_never_replayed(
    error: requests.RequestException,
) -> None:
    session = FakeSession(post=error)

    with pytest.raises(LaunchAmbiguous):
        launcher(session).launch(worker_bootstrap(), INTAKE_URL)

    assert len(session.posts) == 1


# Stop --------------------------------------------------------------------


@pytest.mark.parametrize("status", [200, 202])
def test_request_stop_cancels_the_stored_execution_and_does_not_wait(status: int) -> None:
    session = FakeSession(post=FakeResponse(status))

    launcher(session).request_stop(stored())

    assert session.posts == [(f"{EXECUTION_URL}:cancel", {}, TIMEOUT)]
    assert session.gets == []


def test_a_cancel_answered_404_means_the_execution_is_already_gone() -> None:
    session = FakeSession(post=FakeResponse(404))

    launcher(session).request_stop(stored())

    assert len(session.posts) == 1


@pytest.mark.parametrize("status", [400, 403, 409, 500, 503])
def test_a_refused_cancel_is_unavailable_so_the_broker_retries_on_the_next_reconcile(
    status: int,
) -> None:
    session = FakeSession(post=FakeResponse(status))

    with pytest.raises(LauncherUnavailable, match=f"cancel refused: HTTP {status}"):
        launcher(session).request_stop(stored())


def test_a_cancel_with_no_answer_is_unavailable() -> None:
    session = FakeSession(post=requests.ConnectionError("connection reset"))

    with pytest.raises(LauncherUnavailable, match="cancel got no answer"):
        launcher(session).request_stop(stored())


def test_a_stop_with_no_stored_name_is_refused_before_any_request() -> None:
    session = FakeSession()

    with pytest.raises(LauncherUnavailable) as exc:
        launcher(session).request_stop(stored(None))

    assert str(exc.value) == NO_EXECUTION_BLOCKER
    assert session.posts == []
    assert session.gets == []


# Confirmation ------------------------------------------------------------


def test_confirmation_reads_the_stored_execution_and_nothing_else() -> None:
    session = FakeSession(get=execution(completionTime="2026-10-22T10:00:00Z"))

    assert launcher(session).execution_stopped(stored()) is True

    assert session.gets == [(EXECUTION_URL, TIMEOUT)]
    assert session.posts == []


def test_a_404_on_the_execution_is_confirmed_absence() -> None:
    session = FakeSession(get=FakeResponse(404))

    assert launcher(session).execution_stopped(stored()) is True


@pytest.mark.parametrize(
    "fields",
    [
        pytest.param(
            {"completionTime": "2026-10-22T10:00:00Z", "succeededCount": 1},
            id="completed-with-zero-running-count-omitted",
        ),
        pytest.param(
            {"completionTime": "2026-10-22T10:00:00Z", "runningCount": 0},
            id="completed-with-explicit-zero",
        ),
        pytest.param(
            {"completionTime": "2026-10-22T10:00:00Z", "runningCount": "0"},
            id="completed-with-string-zero",
        ),
        pytest.param({"completionTime": "2026-10-22T10:00:00Z", "failedCount": 1}, id="failed"),
    ],
)
def test_a_completed_execution_with_no_running_task_is_confirmed_termination(
    fields: dict[str, Any],
) -> None:
    assert launcher(FakeSession(get=execution(**fields))).execution_stopped(stored()) is True


@pytest.mark.parametrize(
    "fields",
    [
        pytest.param({}, id="no-completion-time"),
        pytest.param({"completionTime": ""}, id="empty-completion-time"),
        pytest.param({"runningCount": 0}, id="running-count-without-completion"),
        pytest.param(
            {"completionTime": "2026-10-22T10:00:00Z", "runningCount": 1},
            id="completed-with-a-running-task",
        ),
        pytest.param(
            {"completionTime": "2026-10-22T10:00:00Z", "runningCount": "1"},
            id="completed-with-a-running-task-as-string",
        ),
        pytest.param(
            {"completionTime": "2026-10-22T10:00:00Z", "reconciling": True},
            id="completed-but-reconciling",
        ),
    ],
)
def test_an_execution_that_may_still_run_is_never_reported_stopped(
    fields: dict[str, Any],
) -> None:
    assert launcher(FakeSession(get=execution(**fields))).execution_stopped(stored()) is False


def test_an_execution_body_that_is_not_an_object_is_not_stopped() -> None:
    session = FakeSession(get=FakeResponse(200, ["completionTime"]))

    assert launcher(session).execution_stopped(stored()) is False


def test_an_execution_answer_that_is_not_json_is_not_stopped() -> None:
    session = FakeSession(get=FakeResponse(200, json_error=True))

    assert launcher(session).execution_stopped(stored()) is False


@pytest.mark.parametrize("status", [403, 500, 503])
def test_a_failed_read_is_uncertainty_and_never_stopped(status: int) -> None:
    session = FakeSession(get=FakeResponse(status))

    assert launcher(session).execution_stopped(stored()) is False


def test_a_read_with_no_answer_is_not_stopped() -> None:
    session = FakeSession(get=requests.Timeout("read timed out"))

    assert launcher(session).execution_stopped(stored()) is False


def test_an_attempt_with_no_stored_name_is_never_stopped_and_nothing_is_read() -> None:
    session = FakeSession(get=FakeResponse(404))

    assert launcher(session).execution_stopped(stored(None)) is False
    assert session.gets == []


def test_a_launched_name_addresses_the_same_execution_for_stop_and_confirmation() -> None:
    session = FakeSession()
    name = launcher(session).launch(worker_bootstrap(), INTAKE_URL)

    attempt = stored(name)
    launcher(session).request_stop(attempt)
    launcher(session).execution_stopped(attempt)

    assert [url for url, _, _ in session.posts[1:]] == [f"{EXECUTION_URL}:cancel"]
    assert [url for url, _ in session.gets] == [EXECUTION_URL]


# Session -----------------------------------------------------------------


def test_open_session_is_authorized_with_the_cloud_platform_scope(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen: dict[str, Any] = {}

    def fake_default(*, scopes: list[str]) -> tuple[str, str]:
        seen["scopes"] = scopes
        return ("credentials", "mao-dev")

    class FakeAuthorizedSession:
        def __init__(self, credentials: str) -> None:
            seen["credentials"] = credentials

    monkeypatch.setattr(google.auth, "default", fake_default)
    monkeypatch.setattr(cloud_run_jobs, "AuthorizedSession", FakeAuthorizedSession)

    session = cloud_run_jobs.open_session()

    assert seen == {"scopes": [CLOUD_PLATFORM_SCOPE], "credentials": "credentials"}
    assert isinstance(session, FakeAuthorizedSession)
