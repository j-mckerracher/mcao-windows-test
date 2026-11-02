"""The worker entry (protocol-v1 gap 72, option (a)): the intake is fetched, verified, and handed to
the trusted bootstrap only when it is this attempt's and matches its receipt.

Two halves are tested here. The unit tests drive worker_entry.main with an injected fetch and a
recorded trusted_bootstrap.main, so every refusal is shown to stop before the claim. The end-to-end
tests run the controller's real dispatch, take the three container overrides that the real
CloudRunJobLauncher sends, and feed them to the worker entry, with the signed URL resolved against
the in-memory object store. No network, no credentials. The intake URL is a credential, so no test
prints it or asserts on its text outside its own containment check.
"""

import stat
import urllib.error
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlsplit

import pytest
from fakes import OWNER_A
from flows import LONG_BUDGET, World, admit_and_start, dispatch, make_world
from mao_cloud import trusted_bootstrap, worker_entry
from mao_cloud.cloud_run_jobs import CloudRunJobLauncher
from mao_cloud.protocol import (
    ATTEMPT_ID_ENV,
    INTAKE_URL_ENV,
    MAX_INTAKE_BYTES,
    RUN_ID_ENV,
    ObjectKind,
    WorkerBootstrap,
    WorkerIntake,
    object_name,
)
from mao_cloud.worker_entry import IntakeMalformed, IntakeRefused, IntakeUnavailable

EXIT_REPORTED = trusted_bootstrap.EXIT_REPORTED
EXIT_REFUSED = trusted_bootstrap.EXIT_REFUSED
EXIT_UNREADABLE = trusted_bootstrap.EXIT_UNREADABLE
EXIT_UNAVAILABLE = trusted_bootstrap.EXIT_UNAVAILABLE

EXECUTION = (
    "projects/mao-dev/locations/us-central1/jobs/mao-worker/executions/mao-worker-7f2kq"
)


class _Answer:
    def __init__(self, status_code: int, body: Any) -> None:
        self.status_code = status_code
        self._body = body

    def json(self) -> Any:
        return self._body


class _Session:
    """Records the jobs.run request and answers with an Operation that names the execution."""

    def __init__(self) -> None:
        self.posts: list[tuple[str, dict[str, Any]]] = []

    def post(
        self, url: str, *, json: dict[str, Any], timeout: tuple[float, float]
    ) -> _Answer:
        self.posts.append((url, json))
        return _Answer(
            200, {"name": "operations/op-1", "metadata": {"name": EXECUTION}}
        )

    def get(self, url: str, *, timeout: tuple[float, float]) -> _Answer:
        raise AssertionError("the worker entry path never reads execution state")


@dataclass
class Dispatched:
    world: World
    run_id: str
    bootstrap: WorkerBootstrap
    intake_url: str
    env: dict[str, str]
    session: _Session

    @property
    def intake_name(self) -> str:
        return object_name(
            ObjectKind.INTAKE, run_id=self.run_id, attempt=self.bootstrap.attempt
        )

    def fetch(self, url: str) -> bytes:
        """Resolve a signed URL against the object store, as the storage service would."""
        body = self.world.objects.get(urlsplit(url).path.lstrip("/"))
        if body is None:
            raise IntakeUnavailable("HTTP 404")
        return body


@pytest.fixture
def dispatched(monkeypatch: pytest.MonkeyPatch) -> Iterator[Dispatched]:
    """A run dispatched through the broker, launched through the real CloudRunJobLauncher.

    The broker calls world.launcher.launch(bootstrap, intake_url). That call goes to the real
    launcher, which posts the jobs.run body. The recording the fake launcher keeps is kept too, so
    dispatch() reads the bootstrap the same way it does for every other broker test.
    """
    world = make_world()
    world.clock.advance(seconds=1)
    run_id, _, _ = admit_and_start(
        world, OWNER_A, at=world.clock.now(), budget=LONG_BUDGET
    )
    session = _Session()
    cloud = CloudRunJobLauncher(
        session=session,
        project="mao-dev",
        location="us-central1",
        job="mao-worker",
        intake_ready=True,
    )

    def launch_through_cloud(bootstrap: WorkerBootstrap, intake_url: str) -> str | None:
        name = cloud.launch(bootstrap, intake_url)
        world.launcher.launched.append(bootstrap)
        world.launcher.intake_urls.append(intake_url)
        return name

    monkeypatch.setattr(world.launcher, "launch", launch_through_cloud)
    bootstrap = dispatch(world, run_id)

    ((_, body),) = session.posts
    env_list = body["overrides"]["containerOverrides"][0]["env"]
    env = {item["name"]: item["value"] for item in env_list}
    yield Dispatched(
        world=world,
        run_id=run_id,
        bootstrap=bootstrap,
        intake_url=world.launcher.intake_urls[-1],
        env=env,
        session=session,
    )


@pytest.fixture
def claimed(monkeypatch: pytest.MonkeyPatch) -> list[list[str]]:
    """Record every trusted_bootstrap.main call. Returns the argv of each call."""
    calls: list[list[str]] = []

    def recorder(argv: list[str], **_: Any) -> int:
        calls.append(list(argv))
        return EXIT_REPORTED

    monkeypatch.setattr(trusted_bootstrap, "main", recorder)
    return calls


# -- the fetch -------------------------------------------------------------------------


def test_a_location_that_is_not_https_is_refused_before_any_request() -> None:
    with pytest.raises(IntakeUnavailable, match="not an https URL"):
        worker_entry.fetch_intake("http://storage.invalid/intake.json?sig=fake")


def test_a_transport_failure_reports_only_the_exception_type_never_the_url(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    url = "https://storage.invalid/runs/r/attempts/000001/intake.json?X-Goog-Signature=secret"

    class Opener:
        def open(self, request: str, *, timeout: float) -> None:
            raise urllib.error.URLError(f"no route to {request}")

    monkeypatch.setattr(worker_entry, "_OPENER", Opener())

    with pytest.raises(IntakeUnavailable) as raised:
        worker_entry.fetch_intake(url)

    assert str(raised.value) == "URLError"
    assert "secret" not in str(raised.value)


def test_an_http_error_reports_its_status_code_only(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    url = "https://storage.invalid/intake.json?X-Goog-Signature=secret"

    class Opener:
        def open(self, request: str, *, timeout: float) -> None:
            headers: Any = {}
            raise urllib.error.HTTPError(request, 403, "Forbidden", headers, None)

    monkeypatch.setattr(worker_entry, "_OPENER", Opener())

    with pytest.raises(IntakeUnavailable, match="HTTP 403") as raised:
        worker_entry.fetch_intake(url)
    assert "secret" not in str(raised.value)


def test_a_redirect_is_never_followed() -> None:
    handler = worker_entry._RefuseRedirects()
    followed = handler.redirect_request(
        None,  # type: ignore[arg-type]
        None,  # type: ignore[arg-type]
        302,
        "Found",
        {},  # type: ignore[arg-type]
        "https://elsewhere.invalid/intake.json",
    )
    assert followed is None


# -- refusals before the claim ------------------------------------------------------


def test_a_missing_override_is_unreadable_and_nothing_is_fetched(
    claimed: list[list[str]],
) -> None:
    fetched: list[str] = []

    code = worker_entry.main(
        environ={RUN_ID_ENV: "run_x"},
        fetch=lambda url: fetched.append(url) or b"",
    )

    assert code == EXIT_UNREADABLE
    assert fetched == []
    assert claimed == []


def test_a_fetch_failure_exits_unavailable_and_never_reaches_the_claim(
    dispatched: Dispatched, claimed: list[list[str]], capsys: pytest.CaptureFixture[str]
) -> None:
    def unavailable(url: str) -> bytes:
        raise IntakeUnavailable("HTTP 503")

    code = worker_entry.main(environ=dict(dispatched.env), fetch=unavailable)

    assert code == EXIT_UNAVAILABLE
    assert claimed == []
    captured = capsys.readouterr()
    assert "no attempt was claimed" in captured.err
    assert dispatched.intake_url not in captured.err + captured.out


@pytest.mark.parametrize(
    "body",
    [b"{not json", b"[]", b'{"bootstrap": {}}'],
    ids=["not-json", "not-an-object", "missing-fields"],
)
def test_a_malformed_intake_exits_unreadable_and_never_reaches_the_claim(
    dispatched: Dispatched, claimed: list[list[str]], body: bytes
) -> None:
    code = worker_entry.main(environ=dict(dispatched.env), fetch=lambda url: body)

    assert code == EXIT_UNREADABLE
    assert claimed == []


def test_an_oversized_intake_is_refused_before_it_is_parsed(
    dispatched: Dispatched, claimed: list[list[str]]
) -> None:
    oversized = b" " * (MAX_INTAKE_BYTES + 1)

    code = worker_entry.main(environ=dict(dispatched.env), fetch=lambda url: oversized)

    assert code == EXIT_UNREADABLE
    assert claimed == []


def test_an_intake_for_another_attempt_is_refused_before_the_claim(
    dispatched: Dispatched, claimed: list[list[str]]
) -> None:
    env = dict(dispatched.env)
    env[ATTEMPT_ID_ENV] = str(dispatched.bootstrap.attempt + 1)

    code = worker_entry.main(environ=env, fetch=dispatched.fetch)

    assert code == EXIT_REFUSED
    assert claimed == []


def test_an_intake_for_another_run_is_refused_before_the_claim(
    dispatched: Dispatched, claimed: list[list[str]]
) -> None:
    env = dict(dispatched.env)
    env[RUN_ID_ENV] = "run_someone_else"

    code = worker_entry.main(environ=env, fetch=dispatched.fetch)

    assert code == EXIT_REFUSED
    assert claimed == []


# -- the success path ---------------------------------------------------------------


def test_a_verified_intake_is_written_private_and_handed_to_the_bootstrap(
    dispatched: Dispatched, monkeypatch: pytest.MonkeyPatch
) -> None:
    seen: dict[str, Any] = {}

    def recorder(argv: list[str], **_: Any) -> int:
        bootstrap_path, receipt_path = Path(argv[1]), Path(argv[3])
        seen["modes"] = [
            stat.S_IMODE(bootstrap_path.stat().st_mode),
            stat.S_IMODE(receipt_path.stat().st_mode),
        ]
        seen["dir_mode"] = stat.S_IMODE(bootstrap_path.parent.stat().st_mode)
        seen["bootstrap"] = WorkerBootstrap.model_validate_json(
            bootstrap_path.read_bytes()
        )
        seen["receipt_run"] = receipt_path.read_bytes()
        seen["paths"] = (bootstrap_path, receipt_path)
        return EXIT_REPORTED

    monkeypatch.setattr(trusted_bootstrap, "main", recorder)
    env = dict(dispatched.env)

    code = worker_entry.main(environ=env, fetch=dispatched.fetch)

    assert code == EXIT_REPORTED
    assert seen["modes"] == [0o600, 0o600]
    assert seen["dir_mode"] == 0o700
    assert seen["bootstrap"] == dispatched.bootstrap
    assert seen["receipt_run"]
    # The documents exist only for the attempt: the directory is removed after the call returns.
    bootstrap_path, receipt_path = seen["paths"]
    assert not bootstrap_path.exists()
    assert not receipt_path.exists()
    assert not bootstrap_path.parent.exists()


def test_the_intake_url_is_removed_from_the_environment_before_anything_else(
    dispatched: Dispatched, claimed: list[list[str]]
) -> None:
    env = dict(dispatched.env)

    worker_entry.main(environ=env, fetch=dispatched.fetch)

    assert INTAKE_URL_ENV not in env


def test_the_intake_url_is_removed_even_when_the_fetch_fails(
    dispatched: Dispatched, claimed: list[list[str]]
) -> None:
    env = dict(dispatched.env)

    def unavailable(url: str) -> bytes:
        raise IntakeUnavailable("HTTP 500")

    worker_entry.main(environ=env, fetch=unavailable)

    assert INTAKE_URL_ENV not in env
    assert claimed == []


# -- end to end: the controller's overrides drive the worker entry --------------------


def test_the_real_launcher_overrides_drive_the_worker_entry_to_the_claim(
    dispatched: Dispatched,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    # Exactly the three overrides, and nothing else, reached the container.
    assert set(dispatched.env) == {RUN_ID_ENV, ATTEMPT_ID_ENV, INTAKE_URL_ENV}
    assert dispatched.env[RUN_ID_ENV] == dispatched.run_id
    assert dispatched.env[ATTEMPT_ID_ENV] == str(dispatched.bootstrap.attempt)

    seen: dict[str, Any] = {}

    def run_attempt(
        bootstrap: WorkerBootstrap, receipt: Any, **_: Any
    ) -> SimpleNamespace:
        seen["bootstrap"] = bootstrap
        seen["receipt"] = receipt
        return SimpleNamespace(status="reported", reason="")

    monkeypatch.setattr(trusted_bootstrap, "run_attempt", run_attempt)

    # The ports are a stand-in: run_attempt is replaced, so nothing here touches them.
    code = worker_entry.main(
        environ=dict(dispatched.env),
        fetch=dispatched.fetch,
        ports_factory=lambda: SimpleNamespace(),  # type: ignore[arg-type,return-value]
    )

    assert code == EXIT_REPORTED
    assert seen["bootstrap"] == dispatched.bootstrap
    assert seen["receipt"].run_id == dispatched.run_id
    captured = capsys.readouterr()
    assert dispatched.intake_url not in captured.out + captured.err


def test_an_altered_bootstrap_document_is_refused_before_the_claim(
    dispatched: Dispatched,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    _alter_intake(
        dispatched,
        lambda intake: intake.model_copy(
            update={
                "bootstrap": intake.bootstrap.model_copy(
                    update={
                        "max_runtime_seconds": intake.bootstrap.max_runtime_seconds - 1
                    }
                )
            }
        ),
    )
    calls = _record_run_attempt(monkeypatch)

    code = worker_entry.main(environ=dict(dispatched.env), fetch=dispatched.fetch)

    assert code == EXIT_REFUSED
    assert calls == []
    captured = capsys.readouterr()
    assert "no attempt was claimed" in captured.err
    assert dispatched.intake_url not in captured.err + captured.out


def test_an_altered_receipt_digest_is_refused_before_the_claim(
    dispatched: Dispatched,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    _alter_intake(
        dispatched,
        lambda intake: intake.model_copy(
            update={
                "receipt": intake.receipt.model_copy(
                    update={"projection_digest": "0" * 64}
                )
            }
        ),
    )
    calls = _record_run_attempt(monkeypatch)

    code = worker_entry.main(environ=dict(dispatched.env), fetch=dispatched.fetch)

    assert code == EXIT_REFUSED
    assert calls == []
    assert dispatched.intake_url not in capsys.readouterr().err


def test_verify_intake_names_no_run_data_in_its_refusal(dispatched: Dispatched) -> None:
    body = dispatched.world.objects.get(dispatched.intake_name)
    assert body is not None
    intake = WorkerIntake.model_validate_json(body)
    other_run = "run_" + "0" * 32
    other = intake.model_copy(
        update={"bootstrap": intake.bootstrap.model_copy(update={"run_id": other_run})}
    )

    with pytest.raises(IntakeRefused) as refused:
        worker_entry.verify_intake(
            other.model_dump_json().encode(),
            run_id=dispatched.run_id,
            attempt=dispatched.bootstrap.attempt,
        )
    assert dispatched.run_id not in str(refused.value)


def test_verify_intake_rejects_a_body_that_is_not_a_worker_intake() -> None:
    with pytest.raises(IntakeMalformed):
        worker_entry.verify_intake(b"{}", run_id="run_x", attempt=1)


# -- helpers --------------------------------------------------------------------------


def _alter_intake(
    dispatched: Dispatched, change: Callable[[WorkerIntake], WorkerIntake]
) -> None:
    """Replace the stored intake with an altered one, as a tampered object would be written."""
    stored = dispatched.world.objects.get(dispatched.intake_name)
    assert stored is not None
    altered = change(WorkerIntake.model_validate_json(stored))
    dispatched.world.objects.write(
        dispatched.intake_name, altered.model_dump_json().encode()
    )


def _record_run_attempt(monkeypatch: pytest.MonkeyPatch) -> list[WorkerBootstrap]:
    """Record every attempt that gets past verification. An empty list means none did."""
    calls: list[WorkerBootstrap] = []

    def run_attempt(
        bootstrap: WorkerBootstrap, receipt: Any, **_: Any
    ) -> SimpleNamespace:
        calls.append(bootstrap)
        return SimpleNamespace(status="reported", reason="")

    monkeypatch.setattr(trusted_bootstrap, "run_attempt", run_attempt)
    return calls
