"""Controller startup: the entrypoint builds the app from policy, PORT, and adapters, or refuses.

Bad or missing configuration exits 2, naming the setting. A production client that cannot open
exits 3. Neither binds a port. A valid policy with injected adapters builds the broker, which
answers over HTTP on the requested port and serves owner routes only to a verified token.
"""

import json
import os
import socket
import subprocess
import sys
import threading
import time
from collections.abc import Iterator, Mapping
from contextlib import contextmanager
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import httpx
import pytest
import uvicorn
from auth_support import (
    AUDIENCE,
    GOOGLE_SIGNER,
    IDENTITIES,
    CertTransport,
    bearer,
    mint,
)
from fakes import (
    InMemoryObjectStore,
    InMemoryRunStore,
    ManualClock,
    RecordingLauncher,
    create_payload,
    make_policy,
    make_ports,
    policy_document,
)
from fastapi import FastAPI
from firestore_fake import FakeFirestore
from mao_cloud import serve
from mao_cloud.config import Policy, PolicyError
from mao_cloud.production import ProductionConfigError, ProductionSdk
from mao_cloud.serve import (
    ENV_POLICY_FILE,
    Adapters,
    build_app,
    main,
    production_adapters,
)


def _policy_text(**overrides: Any) -> str:
    values: dict[str, Any] = {
        **policy_document(),
        "api_audience": AUDIENCE,
        "identities": [
            {
                "subject": i.subject,
                "email": i.email,
                "role": i.role.value,
                "owner_id": i.owner_id,
            }
            for i in IDENTITIES
        ],
    }
    values.update(overrides)
    return json.dumps(values)


def fake_adapters() -> Adapters:
    """The six production adapters, as in-memory fakes. Production builds them in mao_cloud.production."""
    clock = ManualClock()
    ports = make_ports(
        store=InMemoryRunStore(),
        objects=InMemoryObjectStore(),
        launcher=RecordingLauncher(),
        clock=clock,
    )
    return Adapters(
        store=ports.store,
        objects=ports.objects,
        signer=ports.signer,
        launcher=ports.launcher,
        admissions=ports.admissions,
        clock=clock,
    )


@dataclass
class AdapterSpy:
    """Stands in for make_adapters and counts how often startup reached the adapter step."""

    calls: list[Policy] = field(default_factory=list)

    def __call__(self, policy: Policy) -> Adapters:
        self.calls.append(policy)
        return fake_adapters()


@dataclass
class ServeRecorder:
    """Stands in for serve. Records what would have been bound, and binds nothing."""

    calls: list[tuple[FastAPI, str, int]] = field(default_factory=list)

    def __call__(self, app: FastAPI, *, host: str, port: int) -> None:
        self.calls.append((app, host, port))


@pytest.fixture
def policy_file(tmp_path: Path) -> Path:
    path = tmp_path / "policy.json"
    path.write_text(_policy_text(), encoding="utf-8")
    return path


def test_valid_policy_and_adapters_serve_on_all_interfaces_at_the_port(
    policy_file: Path,
) -> None:
    recorder = ServeRecorder()

    status = main(
        ["--policy", str(policy_file)],
        environ={"PORT": "9090"},
        make_adapters=AdapterSpy(),
        serve=recorder,
    )

    assert status == 0
    [(app, host, port)] = recorder.calls
    assert isinstance(app, FastAPI)
    assert (host, port) == ("0.0.0.0", 9090)


def test_port_defaults_to_8080_when_cloud_run_does_not_set_it(
    policy_file: Path,
) -> None:
    recorder = ServeRecorder()

    main(
        ["--policy", str(policy_file)],
        environ={},
        make_adapters=AdapterSpy(),
        serve=recorder,
    )

    assert [port for _app, _host, port in recorder.calls] == [8080]


def test_default_serve_hands_uvicorn_the_all_interfaces_host_and_the_port(
    policy_file: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    seen: dict[str, Any] = {}

    def fake_run(app: FastAPI, **kwargs: Any) -> None:
        seen["app"] = app
        seen.update(kwargs)

    monkeypatch.setattr(serve.uvicorn, "run", fake_run)

    status = main(
        ["--policy", str(policy_file)],
        environ={"PORT": "8081"},
        make_adapters=AdapterSpy(),
    )

    assert status == 0
    assert isinstance(seen["app"], FastAPI)
    assert (seen["host"], seen["port"]) == ("0.0.0.0", 8081)


@pytest.mark.parametrize(
    "raw", ["", "0", "65536", "99999", "-1", "+80", "80a", " 80", "1e3"]
)
def test_a_bad_port_refuses_with_exit_2_before_the_policy_or_adapters_are_read(
    policy_file: Path, raw: str, capsys: pytest.CaptureFixture[str]
) -> None:
    recorder = ServeRecorder()
    adapters = AdapterSpy()

    status = main(
        ["--policy", str(policy_file)],
        environ={"PORT": raw},
        make_adapters=adapters,
        serve=recorder,
    )

    assert status == 2
    assert recorder.calls == []
    assert adapters.calls == []
    assert "PORT must be a TCP port number" in capsys.readouterr().err


def test_the_environment_names_the_policy_when_no_argument_is_given(
    policy_file: Path,
) -> None:
    recorder = ServeRecorder()
    adapters = AdapterSpy()

    status = main(
        [],
        environ={ENV_POLICY_FILE: str(policy_file), "PORT": "9090"},
        make_adapters=adapters,
        serve=recorder,
    )

    assert status == 0
    [(app, host, port)] = recorder.calls
    assert isinstance(app, FastAPI)
    assert (host, port) == ("0.0.0.0", 9090)
    [policy] = adapters.calls
    assert policy.api_audience == AUDIENCE
    assert asdict(policy.admission) == policy_document()["admission"]


def test_the_policy_argument_takes_precedence_over_the_environment(
    policy_file: Path, tmp_path: Path
) -> None:
    recorder = ServeRecorder()

    status = main(
        ["--policy", str(policy_file)],
        environ={ENV_POLICY_FILE: str(tmp_path / "absent.json"), "PORT": "9090"},
        make_adapters=AdapterSpy(),
        serve=recorder,
    )

    assert status == 0
    assert len(recorder.calls) == 1


@pytest.mark.parametrize("environ", [{}, {ENV_POLICY_FILE: ""}], ids=["unset", "empty"])
def test_no_policy_named_refuses_with_exit_2_and_names_the_setting(
    environ: Mapping[str, str], capsys: pytest.CaptureFixture[str]
) -> None:
    recorder = ServeRecorder()
    adapters = AdapterSpy()

    status = main([], environ=environ, make_adapters=adapters, serve=recorder)

    assert status == 2
    assert recorder.calls == []
    assert adapters.calls == []
    err = capsys.readouterr().err
    assert err.startswith("mao-cloud-broker: no policy file")
    assert ENV_POLICY_FILE in err


@pytest.mark.parametrize("via", ["argument", "environment"])
@pytest.mark.parametrize(
    "document",
    [
        None,
        "not json",
        _policy_text(api_audience="https://mao.example.test/"),
        _policy_text(server_id="BAD ID"),
        _policy_text(admission=None),
        _policy_text(
            admission={**policy_document()["admission"], "team_queue_depth": 0}
        ),
    ],
    ids=[
        "missing-file",
        "not-json",
        "audience-not-an-origin",
        "server-id-invalid",
        "admission-missing",
        "admission-limit-zero",
    ],
)
def test_a_policy_that_cannot_be_read_or_is_invalid_refuses_with_exit_2(
    tmp_path: Path, document: str | None, via: str, capsys: pytest.CaptureFixture[str]
) -> None:
    path = tmp_path / "policy.json"
    if document is not None:
        path.write_text(document, encoding="utf-8")
    argv = ["--policy", str(path)] if via == "argument" else []
    environ = {} if via == "argument" else {ENV_POLICY_FILE: str(path)}
    recorder = ServeRecorder()
    adapters = AdapterSpy()

    status = main(argv, environ=environ, make_adapters=adapters, serve=recorder)

    assert status == 2
    assert recorder.calls == []
    assert adapters.calls == []
    assert capsys.readouterr().err.startswith("mao-cloud-broker: ")


def test_a_policy_without_an_api_audience_refuses_before_adapters_are_built(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = tmp_path / "policy.json"
    path.write_text(_policy_text(api_audience=None, identities=[]), encoding="utf-8")
    recorder = ServeRecorder()
    adapters = AdapterSpy()

    status = main(
        ["--policy", str(path)], environ={}, make_adapters=adapters, serve=recorder
    )

    assert status == 2
    assert recorder.calls == []
    assert adapters.calls == []
    assert "api_audience is required" in capsys.readouterr().err


CONFIGURED: dict[str, str] = {
    "MAO_CLOUD_PROJECT": "mao-dev",
    "MAO_CLOUD_OBJECT_BUCKET": "mao-dev-objects",
    "MAO_CLOUD_RUN_LOCATION": "us-central1",
    "MAO_CLOUD_RUN_JOB": "mao-worker",
    "MAO_CLOUD_ADMISSIONS": "open",
    "MAO_CLOUD_SIGNER_URL": "https://mcao-cloud-signer-test.a.run.app",
}


def _unopenable_sdk() -> ProductionSdk:
    """Every client refuses to open, with a detail the refusal must not repeat."""

    def refuse(**_: Any) -> Any:
        raise RuntimeError("client construction failed")

    return ProductionSdk(firestore_client=refuse, bucket=refuse, jobs_session=refuse)


def test_missing_production_configuration_refuses_with_exit_2_and_names_the_setting(
    policy_file: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    recorder = ServeRecorder()

    status = main(["--policy", str(policy_file)], environ={}, serve=recorder)

    assert status == 2
    assert recorder.calls == []
    err = capsys.readouterr().err
    assert err.startswith(
        "mao-cloud-broker: missing required configuration: MAO_CLOUD_PROJECT"
    )
    assert "MAO_CLOUD_ADMISSIONS" in err


def test_a_client_that_cannot_open_refuses_with_exit_3_and_binds_nothing(
    policy_file: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    recorder = ServeRecorder()

    status = main(
        ["--policy", str(policy_file)],
        environ=CONFIGURED,
        make_adapters=lambda policy: production_adapters(
            policy, CONFIGURED, sdk=_unopenable_sdk()
        ),
        serve=recorder,
    )

    assert status == 3
    assert recorder.calls == []
    err = capsys.readouterr().err
    assert "refusing to start" in err
    assert "RuntimeError" in err
    assert "client construction failed" not in err


def test_configured_production_adapters_bind_the_cloud_run_port(
    policy_file: Path,
) -> None:
    recorder = ServeRecorder()
    env = {**CONFIGURED, "PORT": "9191"}
    sdk = ProductionSdk(
        firestore_client=lambda **_: FakeFirestore(),
        bucket=lambda **_: MagicMock(name="bucket"),
        jobs_session=lambda: MagicMock(name="session"),
    )

    status = main(
        ["--policy", str(policy_file)],
        environ=env,
        make_adapters=lambda policy: production_adapters(policy, env, sdk=sdk),
        serve=recorder,
    )

    assert status == 0
    [(app, host, port)] = recorder.calls
    assert isinstance(app, FastAPI)
    assert (host, port) == ("0.0.0.0", 9191)


def test_the_environment_policy_starts_the_production_controller_with_admissions_closed(
    policy_file: Path,
) -> None:
    env = {
        **CONFIGURED,
        "MAO_CLOUD_ADMISSIONS": "closed",
        ENV_POLICY_FILE: str(policy_file),
        "PORT": "9292",
    }
    sdk = ProductionSdk(
        firestore_client=lambda **_: FakeFirestore(),
        bucket=lambda **_: MagicMock(name="bucket"),
        jobs_session=lambda: MagicMock(name="session"),
    )
    built: list[Adapters] = []

    def build(policy: Policy) -> Adapters:
        adapters = production_adapters(policy, env, sdk=sdk)
        built.append(adapters)
        return adapters

    recorder = ServeRecorder()

    status = main([], environ=env, make_adapters=build, serve=recorder)

    assert status == 0
    [(app, host, port)] = recorder.calls
    assert isinstance(app, FastAPI)
    assert (host, port) == ("0.0.0.0", 9292)
    [adapters] = built
    assert adapters.admissions.is_open() is False


def test_production_adapters_refuse_missing_configuration_before_any_client_opens() -> (
    None
):
    opened: list[str] = []

    def record(name: str) -> Any:
        def open_client(**_: Any) -> Any:
            opened.append(name)
            raise AssertionError("no client may open before configuration passes")

        return open_client

    sdk = ProductionSdk(
        firestore_client=record("firestore"),
        bucket=record("bucket"),
        jobs_session=record("jobs"),
    )

    with pytest.raises(ProductionConfigError, match="MAO_CLOUD_PROJECT"):
        production_adapters(
            make_policy(api_audience=AUDIENCE, identities=IDENTITIES), {}, sdk=sdk
        )

    assert opened == []


@pytest.mark.parametrize(
    "environ",
    [{"PORT": "not-a-port"}, {"PORT": "70000"}],
    ids=["port-not-numeric", "port-out-of-range"],
)
def test_port_errors_exit_2_before_production_configuration_is_read(
    policy_file: Path, environ: Mapping[str, str]
) -> None:
    status = main(
        ["--policy", str(policy_file)], environ=environ, serve=ServeRecorder()
    )

    assert status == 2


def test_build_app_refuses_without_an_audience_and_never_builds_adapters() -> None:
    adapters = AdapterSpy()

    with pytest.raises(PolicyError, match="api_audience is required"):
        build_app(make_policy(), adapters, transport=CertTransport(GOOGLE_SIGNER))

    assert adapters.calls == []


@contextmanager
def serving(app: FastAPI) -> Iterator[int]:
    """Run the app under uvicorn on 127.0.0.1 in this process, and yield the bound port."""
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    server = uvicorn.Server(
        uvicorn.Config(
            app, host="127.0.0.1", port=port, log_level="warning", lifespan="off"
        )
    )
    thread = threading.Thread(target=server.run, daemon=True)
    thread.start()
    deadline = time.monotonic() + 10
    while not server.started:
        if time.monotonic() > deadline or not thread.is_alive():
            raise AssertionError("uvicorn did not start")
        time.sleep(0.01)
    try:
        yield port
    finally:
        server.should_exit = True
        thread.join(timeout=10)


def test_the_built_app_serves_a_verified_owner_and_refuses_an_anonymous_caller_over_http() -> (
    None
):
    app = build_app(
        make_policy(api_audience=AUDIENCE, identities=IDENTITIES),
        lambda _policy: fake_adapters(),
        transport=CertTransport(GOOGLE_SIGNER),
    )

    with serving(app) as port:
        url = f"http://127.0.0.1:{port}/v1/runs"
        anonymous = httpx.post(url, json=create_payload(), timeout=10, trust_env=False)
        owner = httpx.post(
            url,
            json=create_payload(),
            headers=bearer(mint()),
            timeout=10,
            trust_env=False,
        )

    assert anonymous.status_code == 403
    assert "not_allowed" in anonymous.text
    assert owner.status_code == 201


def _subprocess_env(**overrides: str) -> dict[str, str]:
    """The inherited environment without PORT, production settings, or credentials, plus overrides."""
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in {"PORT", "GOOGLE_APPLICATION_CREDENTIALS"}
        and not key.startswith("MAO_CLOUD_")
    }
    env.update(overrides)
    return env


def test_the_entrypoint_module_refuses_with_exit_2_when_production_configuration_is_missing(
    policy_file: Path,
) -> None:
    completed = subprocess.run(
        [sys.executable, "-m", "mao_cloud.serve", "--policy", str(policy_file)],
        env=_subprocess_env(),
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )

    assert completed.returncode == 2
    assert "missing required configuration: MAO_CLOUD_PROJECT" in completed.stderr


def test_the_entrypoint_module_refuses_with_exit_3_when_the_real_sdk_cannot_open(
    policy_file: Path, tmp_path: Path
) -> None:
    # A credentials file that does not exist makes google-auth refuse at once, with no network call.
    completed = subprocess.run(
        [sys.executable, "-m", "mao_cloud.serve", "--policy", str(policy_file)],
        env=_subprocess_env(
            **CONFIGURED,
            GOOGLE_APPLICATION_CREDENTIALS=str(tmp_path / "no-such-credentials.json"),
        ),
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )

    assert completed.returncode == 3
    assert "refusing to start" in completed.stderr
    assert "could not open a production client" in completed.stderr


def test_the_entrypoint_module_refuses_a_bad_port_with_exit_2(
    policy_file: Path,
) -> None:
    completed = subprocess.run(
        [sys.executable, "-m", "mao_cloud.serve", "--policy", str(policy_file)],
        env=_subprocess_env(PORT="0"),
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )

    assert completed.returncode == 2
    assert "PORT must be a TCP port number" in completed.stderr


def test_the_entrypoint_module_with_no_policy_argument_exits_2() -> None:
    completed = subprocess.run(
        [sys.executable, "-m", "mao_cloud.serve"],
        env=_subprocess_env(),
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )

    assert completed.returncode == 2
    assert "--policy" in completed.stderr
