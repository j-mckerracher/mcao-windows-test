"""Production adapter composition (DPES-809): configuration, the admissions switch, the signer
refusal, and the wiring of each port from injected fake clients.

Every SDK client is an injected fake, so there is no network, no credentials, and no SDK
construction. Live Firestore, Cloud Storage and Cloud Run behaviour is unverified until a deployed
run proves it.
"""

from dataclasses import replace
from typing import Any
from unittest.mock import MagicMock

import pytest
from fakes import worker_bootstrap
from firestore_fake import FakeFirestore
from mao_cloud.broker import LaunchFailed, SystemClock
from mao_cloud.cloud_run_jobs import CloudRunJobLauncher
from mao_cloud.firestore_store import FirestoreRunStore
from mao_cloud.gcs_objects import GcsObjectStore
from mao_cloud.production import (
    DEFAULT_FIRESTORE_DATABASE,
    ENV_ADMISSIONS,
    ENV_FIRESTORE_DATABASE,
    ENV_OBJECT_BUCKET,
    ENV_PROJECT,
    ENV_RUN_JOB,
    ENV_RUN_LOCATION,
    ENV_SIGNER_URL,
    Adapters,
    EnvironmentAdmissionSwitch,
    ProductionAdaptersUnavailable,
    ProductionConfig,
    ProductionConfigError,
    ProductionSdk,
    build_adapters,
    load_config,
)
from mao_cloud.serve import production_adapters
from mao_cloud.signer_client import HttpSignerClient

SIGNER_URL = "https://mcao-cloud-signer-test.a.run.app"
COMPLETE: dict[str, str] = {
    ENV_PROJECT: "mao-dev",
    ENV_OBJECT_BUCKET: "mao-dev-objects",
    ENV_RUN_LOCATION: "us-central1",
    ENV_RUN_JOB: "mao-worker",
    ENV_ADMISSIONS: "open",
    ENV_SIGNER_URL: SIGNER_URL,
}
REQUIRED = (
    ENV_PROJECT,
    ENV_OBJECT_BUCKET,
    ENV_RUN_LOCATION,
    ENV_RUN_JOB,
    ENV_ADMISSIONS,
    ENV_SIGNER_URL,
)


def recording_sdk(calls: dict[str, Any]) -> ProductionSdk:
    """Fake factories that record how they were called and return clients that record any use."""

    def firestore_client(*, project: str, database: str) -> FakeFirestore:
        calls["firestore"] = (project, database)
        return FakeFirestore()

    def bucket(*, project: str, name: str) -> MagicMock:
        calls["bucket"] = (project, name)
        calls["bucket_client"] = MagicMock(name="bucket")
        return calls["bucket_client"]

    def jobs_session() -> MagicMock:
        calls["jobs"] = True
        calls["session"] = MagicMock(name="session")
        return calls["session"]

    return ProductionSdk(
        firestore_client=firestore_client, bucket=bucket, jobs_session=jobs_session
    )


# Configuration


def test_complete_configuration_loads_with_the_default_database() -> None:
    assert load_config(COMPLETE) == ProductionConfig(
        project="mao-dev",
        firestore_database="mcao-cloud",
        object_bucket="mao-dev-objects",
        run_location="us-central1",
        run_job="mao-worker",
        admissions_open=True,
        signer_url=SIGNER_URL,
    )
    assert DEFAULT_FIRESTORE_DATABASE == "mcao-cloud"


def test_an_explicit_database_name_overrides_the_default() -> None:
    config = load_config({**COMPLETE, ENV_FIRESTORE_DATABASE: "mcao-test"})

    assert config.firestore_database == "mcao-test"


@pytest.mark.parametrize("blank", ["", "   "])
def test_a_blank_database_name_falls_back_to_the_default(blank: str) -> None:
    config = load_config({**COMPLETE, ENV_FIRESTORE_DATABASE: blank})

    assert config.firestore_database == "mcao-cloud"


@pytest.mark.parametrize("missing", REQUIRED)
def test_each_missing_required_setting_is_named_in_the_refusal(missing: str) -> None:
    environ = {key: value for key, value in COMPLETE.items() if key != missing}

    with pytest.raises(ProductionConfigError) as refused:
        load_config(environ)

    assert str(refused.value) == f"missing required configuration: {missing}"


def test_a_blank_required_setting_counts_as_missing() -> None:
    with pytest.raises(ProductionConfigError, match=ENV_PROJECT):
        load_config({**COMPLETE, ENV_PROJECT: "   "})


def test_every_missing_required_setting_is_named_together() -> None:
    with pytest.raises(ProductionConfigError) as refused:
        load_config({})

    assert str(refused.value) == "missing required configuration: " + ", ".join(
        REQUIRED
    )


FIELD_FOR_SETTING = {
    ENV_PROJECT: "project",
    ENV_RUN_LOCATION: "run_location",
    ENV_RUN_JOB: "run_job",
    ENV_SIGNER_URL: "signer_url",
}

WELL_FORMED = [
    pytest.param(ENV_PROJECT, "mao-dev", id="project"),
    pytest.param(ENV_PROJECT, "example.com:mao-dev", id="project-domain-scoped"),
    pytest.param(ENV_PROJECT, "abcdef", id="project-minimum-length"),
    pytest.param(ENV_PROJECT, "a" + "b" * 28 + "c", id="project-maximum-length"),
    pytest.param(ENV_RUN_LOCATION, "us-central1", id="region"),
    pytest.param(ENV_RUN_LOCATION, "northamerica-northeast1", id="region-several-words"),
    pytest.param(ENV_RUN_JOB, "mao-worker", id="job"),
    pytest.param(ENV_RUN_JOB, "w", id="job-single-character"),
    pytest.param(ENV_RUN_JOB, "w" + "x" * 48 + "9", id="job-fifty-characters"),
    pytest.param(ENV_RUN_JOB, "w" + "x" * 60 + "9", id="job-maximum-length-62"),
    pytest.param(
        ENV_SIGNER_URL,
        "https://mcao-cloud-signer-abc123-uc.a.run.app",
        id="signer-origin",
    ),
]

MALFORMED = [
    pytest.param(ENV_RUN_JOB, "workers/extra", id="job-with-slash"),
    pytest.param(ENV_RUN_JOB, "../mao-worker", id="job-traversal"),
    pytest.param(ENV_RUN_JOB, "Mao-Worker", id="job-uppercase"),
    pytest.param(ENV_RUN_JOB, "-mao", id="job-leading-hyphen"),
    pytest.param(ENV_RUN_JOB, "mao-", id="job-trailing-hyphen"),
    pytest.param(ENV_RUN_JOB, "mao_worker", id="job-underscore"),
    pytest.param(ENV_RUN_JOB, "w" + "x" * 61 + "9", id="job-63-characters"),
    pytest.param(ENV_RUN_LOCATION, "us-central1/extra", id="region-with-slash"),
    pytest.param(ENV_RUN_LOCATION, "US-CENTRAL1", id="region-uppercase"),
    pytest.param(ENV_RUN_LOCATION, "us central1", id="region-with-space"),
    pytest.param(ENV_PROJECT, "mao/dev", id="project-with-slash"),
    pytest.param(ENV_PROJECT, "Mao-Dev", id="project-uppercase"),
    pytest.param(ENV_PROJECT, "abc", id="project-too-short"),
    pytest.param(ENV_PROJECT, "a" + "b" * 29 + "c", id="project-too-long"),
    pytest.param(ENV_PROJECT, "mao-dev-", id="project-trailing-hyphen"),
    pytest.param(ENV_PROJECT, "1mao-dev", id="project-leading-digit"),
    pytest.param(
        ENV_SIGNER_URL, "http://signer.example.run.app", id="signer-plain-http"
    ),
    pytest.param(ENV_SIGNER_URL, "signer.example.run.app", id="signer-without-scheme"),
    pytest.param(
        ENV_SIGNER_URL, "https://signer.example.run.app/", id="signer-with-slash"
    ),
    pytest.param(
        ENV_SIGNER_URL,
        "https://signer.example.run.app/v1/capabilities",
        id="signer-with-path",
    ),
    pytest.param(
        ENV_SIGNER_URL, "https://signer.example.run.app:8443", id="signer-with-port"
    ),
    pytest.param(
        ENV_SIGNER_URL, "https://Signer.example.run.app", id="signer-uppercase"
    ),
]


@pytest.mark.parametrize(("name", "value"), WELL_FORMED)
def test_a_well_formed_identifier_loads_unchanged(name: str, value: str) -> None:
    config = load_config({**COMPLETE, name: value})

    assert getattr(config, FIELD_FOR_SETTING[name]) == value


@pytest.mark.parametrize(("name", "value"), MALFORMED)
def test_a_malformed_identifier_is_refused_and_the_setting_is_named(name: str, value: str) -> None:
    with pytest.raises(ProductionConfigError) as refused:
        load_config({**COMPLETE, name: value})

    assert str(refused.value).startswith(f"{name} must be ")


def test_every_malformed_identifier_is_named_together() -> None:
    with pytest.raises(ProductionConfigError) as refused:
        load_config(
            {**COMPLETE, ENV_RUN_LOCATION: "US-CENTRAL1", ENV_RUN_JOB: "w" + "x" * 61 + "9"}
        )

    message = str(refused.value)
    assert message.startswith(f"{ENV_RUN_LOCATION} must be ")
    assert f"; {ENV_RUN_JOB} must be " in message


@pytest.mark.parametrize(("name", "value"), MALFORMED)
def test_a_malformed_identifier_opens_no_client_at_startup(name: str, value: str) -> None:
    calls: dict[str, Any] = {}

    with pytest.raises(ProductionConfigError):
        production_adapters(
            MagicMock(name="policy"), {**COMPLETE, name: value}, sdk=recording_sdk(calls)
        )

    assert calls == {}


@pytest.mark.parametrize("value", ["Open", "true", "1", "opened"])
def test_an_admissions_value_other_than_open_or_closed_refuses(value: str) -> None:
    with pytest.raises(ProductionConfigError, match=ENV_ADMISSIONS):
        load_config({**COMPLETE, ENV_ADMISSIONS: value})


@pytest.mark.parametrize(("value", "expected"), [("open", True), ("closed", False)])
def test_the_admissions_setting_selects_the_start_state(
    value: str, expected: bool
) -> None:
    assert load_config({**COMPLETE, ENV_ADMISSIONS: value}).admissions_open is expected


# Admissions switch


def test_the_admissions_switch_changes_only_when_told_to_in_this_process() -> None:
    switch = EnvironmentAdmissionSwitch(open_=True)
    assert switch.is_open() is True

    switch.set_open(False)
    assert switch.is_open() is False

    switch.set_open(True)
    assert switch.is_open() is True


def test_a_fresh_build_reads_the_admissions_setting_from_the_environment_again() -> (
    None
):
    """A drain lasts for one process only. A restart reads the environment again (gap 55)."""
    config = load_config(COMPLETE)
    first = build_adapters(config, recording_sdk({}))
    first.admissions.set_open(False)

    restarted = build_adapters(config, recording_sdk({}))

    assert restarted.admissions.is_open() is True


# Signer


def test_a_missing_signer_url_refuses_at_startup_and_opens_no_client() -> None:
    calls: dict[str, Any] = {}
    environ = {key: value for key, value in COMPLETE.items() if key != ENV_SIGNER_URL}

    with pytest.raises(ProductionConfigError) as refused:
        production_adapters(MagicMock(name="policy"), environ, sdk=recording_sdk(calls))

    assert str(refused.value) == f"missing required configuration: {ENV_SIGNER_URL}"
    assert calls == {}


# Wiring


def test_build_adapters_wires_each_port_from_the_configuration() -> None:
    calls: dict[str, Any] = {}

    adapters = build_adapters(load_config(COMPLETE), recording_sdk(calls))

    assert calls["firestore"] == ("mao-dev", "mcao-cloud")
    assert calls["bucket"] == ("mao-dev", "mao-dev-objects")
    assert calls["jobs"] is True
    assert isinstance(adapters, Adapters)
    assert isinstance(adapters.store, FirestoreRunStore)
    assert isinstance(adapters.objects, GcsObjectStore)
    assert isinstance(adapters.signer, HttpSignerClient)
    assert isinstance(adapters.launcher, CloudRunJobLauncher)
    assert isinstance(adapters.admissions, EnvironmentAdmissionSwitch)
    assert isinstance(adapters.clock, SystemClock)


def test_building_adapters_makes_no_call_to_any_backend() -> None:
    calls: dict[str, Any] = {}

    build_adapters(load_config(COMPLETE), recording_sdk(calls))

    assert calls["bucket_client"].method_calls == []
    assert calls["session"].method_calls == []


def test_the_built_launcher_refuses_every_launch_and_never_calls_jobs_run() -> None:
    calls: dict[str, Any] = {}
    adapters = build_adapters(load_config(COMPLETE), recording_sdk(calls))

    with pytest.raises(LaunchFailed, match="the launch gate is closed in production"):
        adapters.launcher.launch(worker_bootstrap(), "https://storage.invalid/intake?fake=1")

    assert calls["session"].method_calls == []


def test_the_admissions_switch_starts_closed_when_the_environment_says_closed() -> None:
    adapters = build_adapters(
        load_config({**COMPLETE, ENV_ADMISSIONS: "closed"}), recording_sdk({})
    )

    assert adapters.admissions.is_open() is False


@pytest.mark.parametrize("failing", ["firestore_client", "bucket", "jobs_session"])
def test_a_client_that_cannot_open_refuses_and_withholds_its_detail(
    failing: str,
) -> None:
    def boom(**_: Any) -> Any:
        raise RuntimeError("secret-credential-detail")

    sdk = replace(recording_sdk({}), **{failing: boom})

    with pytest.raises(ProductionAdaptersUnavailable) as refused:
        build_adapters(load_config(COMPLETE), sdk)

    assert str(refused.value) == "could not open a production client (RuntimeError)"
    assert "secret-credential-detail" not in str(refused.value)
    assert isinstance(refused.value.__cause__, RuntimeError)
