"""The one-shot admission-layout command (DPES-809): python3 -m mao_cloud.provision.

Mocks only. The command takes an injected client factory, so no test opens Firestore, and every test
here runs against the in-memory Firestore fake.
"""

from collections.abc import Callable, Mapping
from typing import Any

import pytest
from firestore_fake import FakeFirestore
from mao_cloud import provision
from mao_cloud.firestore_store import (
    ADMISSION_SHARDS,
    LAYOUT,
    FirestoreRunStore,
    FirestoreStoreError,
    provision_admission_layout,
)

PROJECT_ENV = {"MAO_CLOUD_PROJECT": "mao-dev"}


class Factory:
    """Records each open. Returns the fake, or raises the given error instead."""

    def __init__(self, fake: FakeFirestore, error: Exception | None = None) -> None:
        self.fake = fake
        self.error = error
        self.calls: list[dict[str, Any]] = []

    def __call__(self, **kwargs: Any) -> FakeFirestore:
        self.calls.append(kwargs)
        if self.error is not None:
            raise self.error
        return self.fake


def run(environ: Mapping[str, str], factory: Callable[..., Any]) -> int:
    return provision.main(environ, client_factory=factory)


@pytest.mark.parametrize(
    "environ",
    [
        pytest.param({}, id="unset"),
        pytest.param({"MAO_CLOUD_PROJECT": ""}, id="empty"),
        pytest.param({"MAO_CLOUD_PROJECT": "   "}, id="blank"),
    ],
)
def test_a_missing_project_exits_2_and_opens_nothing(
    environ: Mapping[str, str], capsys: pytest.CaptureFixture[str]
) -> None:
    fake = FakeFirestore()
    factory = Factory(fake)

    assert run(environ, factory) == provision.EXIT_CONFIG

    assert factory.calls == []
    assert fake.document(LAYOUT, "current") is None
    assert "MAO_CLOUD_PROJECT is required" in capsys.readouterr().err


def test_provisions_the_default_database_for_the_serving_shard_count() -> None:
    fake = FakeFirestore()
    factory = Factory(fake)

    assert run(PROJECT_ENV, factory) == provision.EXIT_PROVISIONED

    assert factory.calls == [{"project": "mao-dev", "database": "mcao-cloud"}]
    assert fake.document(LAYOUT, "current") == {"shards": ADMISSION_SHARDS}


def test_a_blank_database_name_falls_back_to_the_default() -> None:
    factory = Factory(FakeFirestore())

    run({**PROJECT_ENV, "MAO_CLOUD_FIRESTORE_DATABASE": "   "}, factory)

    assert factory.calls == [{"project": "mao-dev", "database": "mcao-cloud"}]


def test_an_explicit_database_name_is_used() -> None:
    factory = Factory(FakeFirestore())

    assert (
        run({**PROJECT_ENV, "MAO_CLOUD_FIRESTORE_DATABASE": "mcao-dev-2"}, factory)
        == provision.EXIT_PROVISIONED
    )

    assert factory.calls == [{"project": "mao-dev", "database": "mcao-dev-2"}]


def test_running_again_with_the_same_shard_count_succeeds_and_changes_nothing(
    capsys: pytest.CaptureFixture[str],
) -> None:
    fake = FakeFirestore()
    factory = Factory(fake)
    run(PROJECT_ENV, factory)
    provisioned = fake.document(LAYOUT, "current")

    assert run(PROJECT_ENV, factory) == provision.EXIT_PROVISIONED

    assert fake.document(LAYOUT, "current") == provisioned
    assert "admission layout provisioned" in capsys.readouterr().out


def test_a_layout_for_another_shard_count_exits_4_and_changes_nothing(
    capsys: pytest.CaptureFixture[str],
) -> None:
    fake = FakeFirestore()
    provision_admission_layout(fake, shards=8)
    before = fake.document(LAYOUT, "current")

    assert run(PROJECT_ENV, Factory(fake)) == provision.EXIT_LAYOUT_CONFLICT

    assert fake.document(LAYOUT, "current") == before == {"shards": 8}
    assert "nothing was changed" in capsys.readouterr().err


def test_a_client_failure_exits_3_and_reports_only_the_exception_type(
    capsys: pytest.CaptureFixture[str],
) -> None:
    fake = FakeFirestore()
    factory = Factory(fake, error=RuntimeError("credential detail at /secret/key.json"))

    assert run(PROJECT_ENV, factory) == provision.EXIT_NOT_CONFIRMED

    err = capsys.readouterr().err
    assert "RuntimeError" in err
    assert "run the command again" in err
    assert "credential detail" not in err
    assert "/secret/key.json" not in err
    assert fake.document(LAYOUT, "current") is None


def test_an_unconfirmed_transaction_exits_3_and_leaves_no_layout(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    fake = FakeFirestore()

    def unavailable(work: Any) -> None:
        raise TimeoutError("deadline exceeded")

    monkeypatch.setattr(fake, "run_transaction", unavailable)

    assert run(PROJECT_ENV, Factory(fake)) == provision.EXIT_NOT_CONFIRMED

    assert "TimeoutError" in capsys.readouterr().err
    assert fake.document(LAYOUT, "current") is None


def test_after_provisioning_the_run_store_serves_operations_and_before_it_refuses() -> (
    None
):
    fake = FakeFirestore()
    with pytest.raises(FirestoreStoreError):
        FirestoreRunStore(fake).admission_snapshot()

    assert run(PROJECT_ENV, Factory(fake)) == provision.EXIT_PROVISIONED

    assert FirestoreRunStore(fake).admission_snapshot() is not None
