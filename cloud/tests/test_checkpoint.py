"""Checkpoint schema v1 and the pre-launch restore gate (DPES-869 Q2, T3/T4 at the pure level)."""

import json
import traceback
from typing import Any

import pytest

from fakes import EPOCH, run_record
from mao_cloud.checkpoint import (
    PUBLICATION_BOUNDARIES,
    CheckpointError,
    CheckpointErrorCode,
    CheckpointManifest,
    CommitMarker,
    ObjectRef,
    ObservedObject,
    PublicationBoundary,
    admit_restore,
    check_commit_matches,
    check_compatibility,
    check_component_limits,
    check_restore_paths,
    check_run_budget,
    parse_manifest,
    select_checkpoint,
    validate_for_restore,
    verify_component,
)
from mao_cloud.protocol import ErrorCode, ProtocolError, RunRecord, RunState, sha256_hex

RUN_ID = "run_" + "a" * 32
OTHER_RUN_ID = "run_" + "b" * 32
CLAIM_ID = "claim_" + "e" * 32
OTHER_CLAIM_ID = "claim_" + "f" * 32
SHA = "a" * 64
OTHER_SHA = "b" * 64
BASE_SHA = "1" * 40
IMAGE = "sha256:" + "c" * 64
OTHER_IMAGE = "sha256:" + "d" * 64
HARNESS_VERSION = "1.3.0"
MIB = 1024 * 1024
GIB = 1024 * MIB


def _ref(name: str, *, size: int = 10, generation: int = 1) -> dict[str, Any]:
    return {
        "object_name": name,
        "generation": generation,
        "sha256": SHA,
        "size_bytes": size,
    }


def _payload() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "run_id": RUN_ID,
        "attempt": 2,
        "fence": 3,
        "claim_id": CLAIM_ID,
        "base_sha": BASE_SHA,
        "source_digest": SHA,
        "harness": "pi",
        "harness_version": HARNESS_VERSION,
        "image_digest": IMAGE,
        "harness_state": _ref(f"runs/{RUN_ID}/attempts/000002/checkpoints/harness.bin"),
        "workspace_state": _ref(
            f"runs/{RUN_ID}/attempts/000002/checkpoints/workspace.tar.gz"
        ),
        "workspace_entries": [
            {"path": "src/app.py", "kind": "file", "executable": False},
            {"path": "scripts/run.sh", "kind": "file", "executable": True},
            {"path": "old.txt", "kind": "deleted", "executable": False},
        ],
    }


def _bytes(payload: dict[str, Any]) -> bytes:
    return json.dumps(payload).encode("utf-8")


def _manifest() -> CheckpointManifest:
    return parse_manifest(_bytes(_payload()))


def _code_of(call: Any, *args: Any, **kwargs: Any) -> CheckpointErrorCode:
    with pytest.raises(CheckpointError) as info:
        call(*args, **kwargs)
    return info.value.code


def test_error_codes_are_exactly_the_six_contract_strings() -> None:
    assert {code.value for code in CheckpointErrorCode} == {
        "checkpoint_incompatible",
        "checkpoint_integrity_failed",
        "checkpoint_missing",
        "checkpoint_limit_exceeded",
        "checkpoint_commit_failed",
        "restore_unsafe_path",
    }


def test_valid_manifest_parses_with_its_identity_and_entries() -> None:
    manifest = _manifest()
    assert manifest.run_id == RUN_ID
    assert (manifest.attempt, manifest.fence) == (2, 3)
    assert manifest.harness_state.generation == 1
    assert [entry.path for entry in manifest.workspace_entries] == [
        "src/app.py",
        "scripts/run.sh",
        "old.txt",
    ]


@pytest.mark.parametrize(
    "raw", [b"", b"not json", b"[]", b"null", b'{"schema_version": 1, "x": NaN}']
)
def test_bytes_that_are_not_a_json_object_fail_integrity(raw: bytes) -> None:
    assert _code_of(parse_manifest, raw) is CheckpointErrorCode.INTEGRITY_FAILED


@pytest.mark.parametrize("version", [2, 0])
def test_other_schema_version_is_incompatible(version: int) -> None:
    payload = _payload()
    payload["schema_version"] = version
    assert _code_of(parse_manifest, _bytes(payload)) is CheckpointErrorCode.INCOMPATIBLE


def test_missing_schema_version_is_incompatible() -> None:
    payload = _payload()
    del payload["schema_version"]
    assert _code_of(parse_manifest, _bytes(payload)) is CheckpointErrorCode.INCOMPATIBLE


def test_unknown_extra_key_fails_integrity() -> None:
    payload = _payload()
    payload["unreviewed"] = "x"
    assert (
        _code_of(parse_manifest, _bytes(payload))
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_component_outside_the_run_prefix_fails_integrity() -> None:
    payload = _payload()
    payload["harness_state"] = _ref(
        f"runs/{OTHER_RUN_ID}/attempts/000002/checkpoints/harness.bin"
    )
    assert (
        _code_of(parse_manifest, _bytes(payload))
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_component_name_with_traversal_fails_integrity() -> None:
    payload = _payload()
    payload["workspace_state"] = _ref(f"runs/{RUN_ID}/../other/workspace.tar.gz")
    assert (
        _code_of(parse_manifest, _bytes(payload))
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_zero_generation_fails_integrity() -> None:
    payload = _payload()
    payload["harness_state"]["generation"] = 0
    assert (
        _code_of(parse_manifest, _bytes(payload))
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_duplicate_workspace_path_fails_integrity() -> None:
    payload = _payload()
    payload["workspace_entries"].append(
        {"path": "src/app.py", "kind": "file", "executable": False}
    )
    assert (
        _code_of(parse_manifest, _bytes(payload))
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_deleted_entry_with_executable_bit_fails_integrity() -> None:
    payload = _payload()
    payload["workspace_entries"] = [
        {"path": "gone.sh", "kind": "deleted", "executable": True}
    ]
    assert (
        _code_of(parse_manifest, _bytes(payload))
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_harness_version_mismatch_is_incompatible() -> None:
    assert (
        _code_of(
            check_compatibility,
            _manifest(),
            harness_version="1.4.0",
            image_digest=IMAGE,
        )
        is CheckpointErrorCode.INCOMPATIBLE
    )


def test_image_digest_mismatch_is_incompatible() -> None:
    assert (
        _code_of(
            check_compatibility,
            _manifest(),
            harness_version=HARNESS_VERSION,
            image_digest=OTHER_IMAGE,
        )
        is CheckpointErrorCode.INCOMPATIBLE
    )


def test_matching_pins_pass_compatibility() -> None:
    check_compatibility(
        _manifest(), harness_version=HARNESS_VERSION, image_digest=IMAGE
    )


def _manifest_with_sizes(harness: int, workspace: int) -> CheckpointManifest:
    payload = _payload()
    payload["harness_state"]["size_bytes"] = harness
    payload["workspace_state"]["size_bytes"] = workspace
    return parse_manifest(_bytes(payload))


def test_harness_state_above_64_mib_exceeds_its_limit() -> None:
    manifest = _manifest_with_sizes(64 * MIB + 1, 10)
    assert (
        _code_of(check_component_limits, manifest) is CheckpointErrorCode.LIMIT_EXCEEDED
    )


def test_harness_state_at_64_mib_passes() -> None:
    check_component_limits(_manifest_with_sizes(64 * MIB, 10))


def test_workspace_above_1_gib_exceeds_its_limit() -> None:
    manifest = _manifest_with_sizes(10, GIB + 1)
    assert (
        _code_of(check_component_limits, manifest) is CheckpointErrorCode.LIMIT_EXCEEDED
    )


def test_workspace_at_1_gib_passes() -> None:
    check_component_limits(_manifest_with_sizes(10, GIB))


def test_run_budget_past_2_gib_refuses_the_next_checkpoint() -> None:
    assert _code_of(check_run_budget, 2 * GIB, 1) is CheckpointErrorCode.LIMIT_EXCEEDED


def test_run_budget_exactly_at_2_gib_is_allowed() -> None:
    check_run_budget(2 * GIB - 1, 1)


@pytest.mark.parametrize(
    "path",
    [
        "/etc/passwd",
        "../x",
        "a/../b",
        "a//b",
        "a/",
        "./a",
        "a/./b",
        "a\\b",
        "a\x00b",
        "a\nb",
        "",
    ],
)
def test_unsafe_restore_path_is_refused_with_its_typed_code(path: str) -> None:
    payload = _payload()
    payload["workspace_entries"] = [{"path": path, "kind": "file", "executable": False}]
    manifest = parse_manifest(_bytes(payload))
    assert (
        _code_of(check_restore_paths, manifest)
        is CheckpointErrorCode.RESTORE_UNSAFE_PATH
    )


@pytest.mark.parametrize("path", ["dir/file.txt", ".hidden", "a.b", "a..b"])
def test_safe_relative_restore_path_passes(path: str) -> None:
    payload = _payload()
    payload["workspace_entries"] = [{"path": path, "kind": "file", "executable": False}]
    check_restore_paths(parse_manifest(_bytes(payload)))


def _observed(*, generation: int = 1, size: int = 10, sha: str = SHA) -> ObservedObject:
    return ObservedObject(generation=generation, size_bytes=size, sha256=sha)


def _component() -> ObjectRef:
    return ObjectRef(
        object_name=f"runs/{RUN_ID}/attempts/000002/checkpoints/harness.bin",
        generation=1,
        sha256=SHA,
        size_bytes=10,
    )


def test_missing_component_is_reported_as_missing() -> None:
    assert _code_of(verify_component, _component(), None) is CheckpointErrorCode.MISSING


@pytest.mark.parametrize(
    "observed",
    [
        _observed(generation=2),
        _observed(size=11),
        _observed(sha=OTHER_SHA),
    ],
)
def test_component_that_differs_from_its_recorded_identity_fails_integrity(
    observed: ObservedObject,
) -> None:
    assert (
        _code_of(verify_component, _component(), observed)
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_component_matching_its_recorded_identity_passes() -> None:
    verify_component(_component(), _observed())


def _marker(
    *, attempt: int = 2, fence: int = 3, claim_id: str = CLAIM_ID
) -> CommitMarker:
    return CommitMarker(
        run_id=RUN_ID,
        attempt=attempt,
        fence=fence,
        claim_id=claim_id,
        manifest=ObjectRef(
            object_name=f"runs/{RUN_ID}/attempts/000002/manifest.json",
            generation=1,
            sha256=SHA,
            size_bytes=10,
        ),
        committed=True,
    )


def test_commit_marker_for_another_attempt_fails_integrity() -> None:
    assert (
        _code_of(check_commit_matches, _marker(attempt=1), _manifest())
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_commit_marker_for_another_fence_fails_integrity() -> None:
    assert (
        _code_of(check_commit_matches, _marker(fence=4), _manifest())
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_commit_marker_for_another_claim_fails_integrity() -> None:
    assert (
        _code_of(check_commit_matches, _marker(claim_id=OTHER_CLAIM_ID), _manifest())
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_manifest_without_claim_id_fails_integrity() -> None:
    payload = _payload()
    del payload["claim_id"]
    assert (
        _code_of(parse_manifest, _bytes(payload))
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_commit_marker_naming_the_manifest_identity_passes() -> None:
    check_commit_matches(_marker(), _manifest())


def test_restore_gate_refuses_a_checkpoint_from_another_logical_run() -> None:
    assert (
        _code_of(
            validate_for_restore,
            _bytes(_payload()),
            run_id=OTHER_RUN_ID,
            harness_version=HARNESS_VERSION,
            image_digest=IMAGE,
        )
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_restore_gate_returns_a_valid_manifest() -> None:
    manifest = validate_for_restore(
        _bytes(_payload()),
        run_id=RUN_ID,
        harness_version=HARNESS_VERSION,
        image_digest=IMAGE,
    )
    assert manifest.run_id == RUN_ID


def test_restore_gate_checks_compatibility_before_limits() -> None:
    payload = _payload()
    payload["harness_version"] = "1.4.0"
    payload["harness_state"]["size_bytes"] = 64 * MIB + 1
    assert (
        _code_of(
            validate_for_restore,
            _bytes(payload),
            run_id=RUN_ID,
            harness_version=HARNESS_VERSION,
            image_digest=IMAGE,
        )
        is CheckpointErrorCode.INCOMPATIBLE
    )


def test_restore_gate_checks_limits_before_paths() -> None:
    payload = _payload()
    payload["harness_state"]["size_bytes"] = 64 * MIB + 1
    payload["workspace_entries"] = [
        {"path": "../x", "kind": "file", "executable": False}
    ]
    assert (
        _code_of(
            validate_for_restore,
            _bytes(payload),
            run_id=RUN_ID,
            harness_version=HARNESS_VERSION,
            image_digest=IMAGE,
        )
        is CheckpointErrorCode.LIMIT_EXCEEDED
    )


def test_restore_gate_refuses_unsafe_path_in_an_otherwise_valid_checkpoint() -> None:
    payload = _payload()
    payload["workspace_entries"] = [
        {"path": "../x", "kind": "file", "executable": False}
    ]
    assert (
        _code_of(
            validate_for_restore,
            _bytes(payload),
            run_id=RUN_ID,
            harness_version=HARNESS_VERSION,
            image_digest=IMAGE,
        )
        is CheckpointErrorCode.RESTORE_UNSAFE_PATH
    )


# --- Selection, bootstrap gate, and stale-fence refusal (DPES-869 AC3, AC4, AC6; T3, T4, T6) ----

SENTINEL = "SIGNED-URL-SENTINEL-7f3a"
CKPT = "ck-0001"


def _names(ckpt: str) -> dict[str, str]:
    base = f"runs/{RUN_ID}/attempts/000001/checkpoints/{ckpt}"
    return {
        "harness": f"{base}/harness.bin",
        "workspace": f"{base}/workspace.tar.gz",
        "manifest": f"{base}/manifest.json",
    }


def _current(**overrides: Any) -> RunRecord:
    """The controller's run record: the only source of current attempt, fence, and claim."""
    values: dict[str, Any] = {
        "state": RunState.RUNNING,
        "claim_id": CLAIM_ID,
        "attempt": 1,
        "fence": 1,
        "version": 4,
        "last_seen_at": EPOCH,
    }
    values.update(overrides)
    return run_record(**values)


def _publication(
    *,
    ckpt: str = CKPT,
    harness: str | None = None,
    workspace: str | None = None,
    attempt: int = 1,
    fence: int = 1,
    claim_id: str = CLAIM_ID,
    workspace_path: str = "src/app.py",
) -> tuple[CommitMarker, bytes, dict[str, ObservedObject]]:
    """A complete, matching checkpoint: all three objects written and the marker committed."""
    names = _names(ckpt)
    harness_name = harness or names["harness"]
    workspace_name = workspace or names["workspace"]
    payload = _payload()
    payload.update(
        attempt=attempt,
        fence=fence,
        claim_id=claim_id,
        harness_state=_ref(harness_name),
        workspace_state=_ref(workspace_name),
        workspace_entries=[
            {"path": workspace_path, "kind": "file", "executable": False}
        ],
    )
    data = _bytes(payload)
    marker = CommitMarker(
        run_id=RUN_ID,
        attempt=attempt,
        fence=fence,
        claim_id=claim_id,
        manifest=ObjectRef(
            object_name=names["manifest"],
            generation=1,
            sha256=sha256_hex(data),
            size_bytes=len(data),
        ),
        committed=True,
    )
    observed = {
        harness_name: ObservedObject(generation=1, size_bytes=10, sha256=SHA),
        workspace_name: ObservedObject(generation=1, size_bytes=10, sha256=SHA),
        names["manifest"]: ObservedObject(
            generation=1, size_bytes=len(data), sha256=sha256_hex(data)
        ),
    }
    return marker, data, observed


def _select(
    run: RunRecord,
    marker: CommitMarker | None,
    data: bytes | None,
    observed: dict[str, ObservedObject],
    **overrides: Any,
) -> Any:
    values: dict[str, Any] = {
        "marker": marker,
        "manifest_bytes": data,
        "observed": observed,
        "harness_version": HARNESS_VERSION,
        "image_digest": IMAGE,
        "expected_attempt": 1,
        "expected_version": 4,
    }
    values.update(overrides)
    return select_checkpoint(run, **values)


def test_a_complete_matching_checkpoint_is_selected() -> None:
    marker, data, observed = _publication()
    selection = _select(_current(), marker, data, observed)
    assert selection.marker == marker
    assert selection.manifest.harness_state.object_name == _names(CKPT)["harness"]


@pytest.mark.parametrize("boundary", PUBLICATION_BOUNDARIES)
def test_a_fault_at_any_publication_boundary_selects_nothing(
    boundary: PublicationBoundary,
) -> None:
    """Objects land before the pointer transaction, so a fault at any boundary leaves no marker."""
    _, data, observed = _publication()
    names = _names(CKPT)
    written = PUBLICATION_BOUNDARIES[: PUBLICATION_BOUNDARIES.index(boundary) + 1]
    visible: dict[str, ObservedObject] = {}
    if PublicationBoundary.HARNESS_STATE_WRITTEN in written:
        visible[names["harness"]] = observed[names["harness"]]
    if PublicationBoundary.WORKSPACE_STATE_WRITTEN in written:
        visible[names["workspace"]] = observed[names["workspace"]]
    if PublicationBoundary.MANIFEST_WRITTEN in written:
        visible[names["manifest"]] = observed[names["manifest"]]
    manifest_bytes = data if PublicationBoundary.MANIFEST_WRITTEN in written else None
    assert (
        _code_of(_select, _current(), None, manifest_bytes, visible)
        is CheckpointErrorCode.MISSING
    )


@pytest.mark.parametrize("missing", ["harness", "workspace", "manifest"])
def test_a_committed_pointer_with_a_component_missing_selects_nothing(
    missing: str,
) -> None:
    marker, data, observed = _publication()
    del observed[_names(CKPT)[missing]]
    assert (
        _code_of(_select, _current(), marker, data, observed)
        is CheckpointErrorCode.MISSING
    )


@pytest.mark.parametrize("which", ["harness", "workspace"])
def test_a_mixed_harness_and_workspace_pair_is_never_selected(which: str) -> None:
    other = _names("ck-0002")[which]
    marker, data, observed = _publication(**{which: other})
    assert (
        _code_of(_select, _current(), marker, data, observed)
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_a_component_with_another_generation_is_refused() -> None:
    marker, data, observed = _publication()
    observed[_names(CKPT)["workspace"]] = ObservedObject(
        generation=2, size_bytes=10, sha256=SHA
    )
    assert (
        _code_of(_select, _current(), marker, data, observed)
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


def test_manifest_bytes_that_differ_from_the_committed_digest_are_refused() -> None:
    marker, data, observed = _publication()
    tampered = data.replace(b'"src/app.py"', b'"src/bad.py"')
    assert (
        _code_of(_select, _current(), marker, tampered, observed)
        is CheckpointErrorCode.INTEGRITY_FAILED
    )


@pytest.mark.parametrize(
    ("run_overrides", "expected_attempt", "expected_version", "marker_overrides"),
    [
        pytest.param(
            {"attempt": 2, "fence": 2, "version": 5},
            1,
            4,
            {},
            id="publisher-holds-old-cas",
        ),
        pytest.param({"fence": 2}, 1, 4, {}, id="old-fence-with-current-cas"),
        pytest.param({}, 1, 4, {"claim_id": OTHER_CLAIM_ID}, id="old-claim"),
    ],
)
def test_a_stale_attempt_checkpoint_cannot_replace_the_selection(
    run_overrides: dict[str, Any],
    expected_attempt: int,
    expected_version: int,
    marker_overrides: dict[str, Any],
) -> None:
    marker, data, observed = _publication(**marker_overrides)
    with pytest.raises(ProtocolError) as info:
        _select(
            _current(**run_overrides),
            marker,
            data,
            observed,
            expected_attempt=expected_attempt,
            expected_version=expected_version,
        )
    assert info.value.code is ErrorCode.STALE_ATTEMPT


def test_a_publish_against_a_moved_version_is_a_version_conflict() -> None:
    marker, data, observed = _publication()
    with pytest.raises(ProtocolError) as info:
        _select(_current(version=5), marker, data, observed)
    assert info.value.code is ErrorCode.VERSION_CONFLICT


def test_a_stale_refusal_names_no_payload_or_credential() -> None:
    marker, data, observed = _publication(workspace_path=f"{SENTINEL}.txt")
    with pytest.raises(ProtocolError) as info:
        _select(_current(fence=2), marker, data, observed, expected_attempt=1)
    assert SENTINEL not in str(info.value)
    assert SENTINEL not in repr(info.value.detail)


def _gate_case(
    case: str,
) -> tuple[bytes, dict[str, ObservedObject], CheckpointErrorCode]:
    """One corrupted bootstrap input, with the typed error it must raise."""
    names = _names(CKPT)
    _, data, observed = _publication()
    payload = json.loads(data)
    if case == "corrupt":
        return b"{not json", observed, CheckpointErrorCode.INTEGRITY_FAILED
    if case == "digest":
        observed[names["harness"]] = ObservedObject(
            generation=1, size_bytes=10, sha256=OTHER_SHA
        )
        return data, observed, CheckpointErrorCode.INTEGRITY_FAILED
    if case == "generation":
        observed[names["harness"]] = ObservedObject(
            generation=2, size_bytes=10, sha256=SHA
        )
        return data, observed, CheckpointErrorCode.INTEGRITY_FAILED
    if case == "size":
        observed[names["workspace"]] = ObservedObject(
            generation=1, size_bytes=11, sha256=SHA
        )
        return data, observed, CheckpointErrorCode.INTEGRITY_FAILED
    if case == "oversized":
        payload["harness_state"]["size_bytes"] = 64 * MIB + 1
        observed[names["harness"]] = ObservedObject(
            generation=1, size_bytes=64 * MIB + 1, sha256=SHA
        )
        return _bytes(payload), observed, CheckpointErrorCode.LIMIT_EXCEEDED
    if case == "schema_version":
        payload["schema_version"] = 2
        return _bytes(payload), observed, CheckpointErrorCode.INCOMPATIBLE
    if case == "harness_version":
        payload["harness_version"] = "9.9.9"
        return _bytes(payload), observed, CheckpointErrorCode.INCOMPATIBLE
    if case == "path":
        payload["workspace_entries"][0]["path"] = "../escape.txt"
        return _bytes(payload), observed, CheckpointErrorCode.RESTORE_UNSAFE_PATH
    if case == "symlink":
        payload["workspace_entries"] = [
            {"path": "link", "kind": "symlink", "executable": False}
        ]
        return _bytes(payload), observed, CheckpointErrorCode.RESTORE_UNSAFE_PATH
    if case == "missing":
        del observed[names["harness"]]
        return data, observed, CheckpointErrorCode.MISSING
    raise AssertionError(case)


GATE_CASES = [
    "corrupt",
    "digest",
    "generation",
    "size",
    "oversized",
    "schema_version",
    "harness_version",
    "path",
    "symlink",
    "missing",
]


@pytest.mark.parametrize("case", GATE_CASES)
def test_bootstrap_refuses_each_bad_checkpoint_before_any_launch(case: str) -> None:
    data, observed, expected = _gate_case(case)
    launched: list[CheckpointManifest] = []
    with pytest.raises(CheckpointError) as info:
        admit_restore(
            data,
            run_id=RUN_ID,
            harness_version=HARNESS_VERSION,
            image_digest=IMAGE,
            observed=observed,
            launch=launched.append,
        )
    assert info.value.code is expected
    assert launched == []


def test_a_verified_checkpoint_launches_exactly_once() -> None:
    _, data, observed = _publication()
    launched: list[CheckpointManifest] = []
    admit_restore(
        data,
        run_id=RUN_ID,
        harness_version=HARNESS_VERSION,
        image_digest=IMAGE,
        observed=observed,
        launch=launched.append,
    )
    assert [manifest.run_id for manifest in launched] == [RUN_ID]


def test_an_unexpected_field_value_never_reaches_the_formatted_traceback() -> None:
    """Pydantic renders rejected input values. The refusal must not chain them into a traceback."""
    sentinel = "SIGNED-URL-SENTINEL-unexpected-field"
    payload = _payload()
    payload["unexpected_field"] = sentinel
    with pytest.raises(CheckpointError) as info:
        parse_manifest(_bytes(payload))
    assert info.value.code is CheckpointErrorCode.INTEGRITY_FAILED
    rendered = "".join(traceback.format_exception(info.value))
    assert sentinel not in rendered
    assert info.value.__suppress_context__ is True
