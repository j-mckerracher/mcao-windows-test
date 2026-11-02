"""Static checks over the cloud CI and image workflow definitions (DPES-819 AC1, AC2, AC3, AC5).

These read the workflow text. They cannot prove a run passes, a scan gate trips, or an attestation
is accepted by Binary Authorization; those need GitHub runs and GCP and are stated gaps.
The permission pins reject any scope line they cannot read exactly, comments included, but they match
text rather than parse YAML, so a quoted permissions key beside the real block and a repeated identical
scope line still pass.
"""

import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github" / "workflows"
CI = WORKFLOWS / "cloud-ci.yml"
IMAGES = WORKFLOWS / "cloud-images.yml"
RECORD = WORKFLOWS / "cloud-record-digests.yml"

# Reusable workflow pins are the ones the reference trial image uses. Changing them is a
# deliberate review decision, so the test names them.
BUILD_IMAGE_USES = (
    "mclm-dps/reusable-workflow-build-image/.github/workflows/build-image.yml"
    "@bab88aea9cbd42773ccfeb888a6825b9c729312c"
)
PUSH_IMAGE_USES = (
    "mclm-dps/reusable-workflow-push-gcr-image/.github/workflows/push-gcr-image.yml"
    "@b56e3b7dec2f90c9de0b35fec9b68c232b6e63c8"
)
AR_PREFIX = "us-central1-docker.pkg.dev/dpe-platform-d/artifact-repository"


def _text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _uses_lines(text: str) -> list[str]:
    return [m.group(1) for m in re.finditer(r"^\s*(?:- )?uses:\s*(\S+)", text, re.MULTILINE)]


def _job_block(text: str, job: str) -> str:
    match = re.search(
        rf"^  {re.escape(job)}:\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)", text, re.MULTILINE | re.DOTALL
    )
    assert match, f"job {job!r} not found"
    return match.group(1)


IMAGE_PERMISSIONS = {
    "actions": "read",
    "artifact-metadata": "write",
    "attestations": "write",
    "contents": "read",
    "id-token": "write",
    "packages": "write",
}
RECORDER_PERMISSIONS = {"contents": "read"}


def _top_level_permissions(text: str) -> dict[str, str]:
    # Every permissions key must be the one top-level block, and every line in it must parse exactly.
    # A line the pattern cannot read fails the pin instead of silently leaving the dict.
    lines = text.splitlines()
    keys = [line for line in lines if re.match(r"[ \t]*permissions[ \t]*:", line)]
    assert keys == ["permissions:"], f"expected exactly one top-level permissions key, found {keys}"
    scopes: dict[str, str] = {}
    for line in lines[lines.index("permissions:") + 1 :]:
        if not line.strip():
            continue
        if not line.startswith(" "):
            break
        match = re.fullmatch(r"  ([a-z-]+): (read|write|none)", line)
        assert match, f"unparsed permissions line: {line!r}"
        scopes[match.group(1)] = match.group(2)
    return scopes


def _assert_permission_pin(text: str, expected: dict[str, str]) -> None:
    assert _top_level_permissions(text) == expected


def _assert_every_reusable_call_inherits_caller_secrets(text: str) -> None:
    calls = (
        "build-controller", "build-worker", "build-signer",
        "push-controller", "push-worker", "push-signer",
        "record-digests",
    )
    for job in calls:
        assert re.search(r"^    secrets: inherit$", _job_block(text, job), re.MULTILINE), job


# A literal named read of the secrets context: secrets.NAME, secrets['NAME'], or secrets["NAME"].
# Context and property names are case-insensitive in Actions expressions, so these match any case.
SECRET_NAMED_READ = re.compile(
    r"\bsecrets(?:\.([A-Za-z0-9_]+)|\s*\[\s*(['\"])(.+?)\2\s*\])", re.IGNORECASE
)
# Any spelling of the secrets word, and the one line form allowed: a reusable call inheriting them.
SECRETS_WORD = re.compile(r"\bsecrets\b", re.IGNORECASE)
SECRETS_INHERIT_LINE = re.compile(r"^[ \t]*secrets:[ \t]*inherit[ \t]*$", re.MULTILINE)


def _assert_recorder_reads_only_the_impersonation_secret(recorder: str) -> None:
    # Every occurrence of the secrets context must be a literal named read. A bare read such as
    # toJSON(secrets) or a computed index such as secrets[env.NAME] is not a named read, so it fails.
    occurrences = len(SECRETS_WORD.findall(recorder))
    reads = [(m.group(1) or m.group(3)).upper() for m in SECRET_NAMED_READ.finditer(recorder)]
    assert len(reads) == occurrences, (
        f"secrets context read without a literal name in {occurrences - len(reads)} place(s)"
    )
    assert set(reads) == {"GS2_IMPERSONATION_ACCOUNT_SECRET"}, reads


def test_cloud_ci_and_image_workflows_exist() -> None:
    assert CI.is_file()
    assert IMAGES.is_file()


def test_cloud_ci_runs_the_python_suite_and_the_export_drift_check() -> None:
    text = _text(CI)
    assert "uv sync --locked" in text
    assert re.search(r"uv run (--frozen )?pytest", text)
    assert "uv export --frozen --no-dev --extra controller" in text
    assert "requirements/controller.txt" in text


def test_cloud_ci_lints_workflows_through_the_owner_script_not_bare_actionlint() -> None:
    # bin/fm-lint-workflows.sh owns the workflow lint, including the copilot-requests ignore for the
    # pinned actionlint. A bare actionlint step reports that known false positive and fails.
    text = _text(CI)
    assert "bin/fm-install-actionlint.sh" in text
    lint = _job_block(text, "workflows")
    assert "run: bin/fm-lint-workflows.sh" in lint
    assert "run: actionlint" not in lint


def test_every_action_reference_in_the_cloud_workflows_is_pinned_to_a_full_sha() -> None:
    for path in (CI, IMAGES, RECORD):
        for uses in _uses_lines(_text(path)):
            if uses.startswith("./"):
                continue
            assert re.search(r"@[0-9a-f]{40}$", uses), f"{path.name}: unpinned {uses}"


def test_cloud_ci_has_read_only_token_permissions_and_no_pull_request_target() -> None:
    text = _text(CI)
    assert re.search(r"^permissions:\n  contents: read\n", text, re.MULTILINE)
    assert "pull_request_target" not in text
    assert "write" not in text


def test_image_workflow_calls_the_reference_reusable_build_and_push_pins() -> None:
    text = _text(IMAGES)
    assert BUILD_IMAGE_USES in _uses_lines(text)
    assert PUSH_IMAGE_USES in _uses_lines(text)


def test_controller_worker_and_signer_are_built_from_their_own_dockerfiles_at_the_repo_root_context() -> None:
    text = _text(IMAGES)
    assert "file: cloud/Dockerfile.controller" in text
    assert "file: cloud/Dockerfile\n" in text
    assert "file: cloud/Dockerfile.signer" in text


def test_all_three_images_publish_to_the_artifact_repository_tagged_by_commit() -> None:
    text = _text(IMAGES)
    for image in ("mao-controller", "mao-worker", "mao-signer"):
        assert f"image_dest: {AR_PREFIX}/{image}:${{{{ github.sha }}}}" in text, image


def test_critical_and_high_cve_gate_is_enforced_and_never_disabled() -> None:
    text = _text(IMAGES)
    # Every build job and every push job must set the gate explicitly: the reference build
    # workflow defaults to disabled, so an omitted input would silently drop the gate.
    assert text.count("disable_container_scanning_results: 'false'") == 6
    assert "disable_container_scanning_results: 'true'" not in text


def test_attestation_and_sbom_are_generated_for_all_three_pushed_images() -> None:
    text = _text(IMAGES)
    assert text.count("generate_attestation: true") == 3
    assert text.count("generate_sbom: true") == 3


def test_build_push_and_digest_calls_all_use_the_nonprod_environment() -> None:
    # Three build and three push calls plus the digest-recording call, which also needs GCP auth.
    text = _text(IMAGES)
    assert text.count("environment: nonprod") == 7


def test_image_builds_pushes_and_digest_recording_never_run_on_pull_requests() -> None:
    text = _text(IMAGES)
    jobs = (
        "build-controller", "build-worker", "build-signer",
        "push-controller", "push-worker", "push-signer",
        "record-digests",
    )
    for job in jobs:
        assert "github.event_name != 'pull_request'" in _job_block(text, job), job


def _assert_no_secrets_context_read(text: str) -> None:
    # Any spelling of the secrets word, in any expression, condition, or bracket form, is a read this pin
    # forbids. Matching the word rather than the ${{ }} wrapper also catches bare if: conditions.
    assert not SECRETS_WORD.search(SECRETS_INHERIT_LINE.sub("", text)), "secrets read outside `secrets: inherit`"


def test_secrets_are_inherited_not_written_into_the_workflow() -> None:
    text = _text(IMAGES)
    assert len(re.findall(r"^    secrets: inherit$", text, re.MULTILINE)) == 7
    _assert_no_secrets_context_read(text)
    _assert_no_secrets_context_read(_text(CI))


def test_no_secrets_context_pin_allows_other_contexts() -> None:
    _assert_no_secrets_context_read("        run: echo ${{ github.sha }}\n")


def test_no_secrets_context_pin_allows_only_the_inherit_line() -> None:
    _assert_no_secrets_context_read("    secrets: inherit\n")


def test_no_secrets_context_pin_rejects_a_bare_condition_read() -> None:
    with pytest.raises(AssertionError):
        _assert_no_secrets_context_read("    if: secrets.OTHER_SECRET != ''\n")


@pytest.mark.parametrize(
    "expression",
    [
        "secrets.OTHER_SECRET",
        "secrets['OTHER_SECRET']",
        "toJSON(secrets)",
        "secrets[env.NAME]",
        "SECRETS.OTHER_SECRET",
        "format('{0}', secrets.OTHER_SECRET)",
        "fromJSON('{\"k\": 1}').k || secrets.OTHER_SECRET",
    ],
    ids=[
        "dot-read",
        "bracket-read",
        "whole-context-read",
        "computed-index-read",
        "upper-case-read",
        "format-braces-read",
        "fromjson-braces-read",
    ],
)
def test_no_secrets_context_pin_rejects_each_read_shape(expression: str) -> None:
    with pytest.raises(AssertionError):
        _assert_no_secrets_context_read("        run: echo ${{ " + expression + " }}\n")


def test_image_workflow_top_level_permissions_are_pinned_to_the_reviewed_scopes() -> None:
    # The write scopes stay at workflow level on DPES-751, as on main. Moving them into the caller
    # jobs is a reviewed change, and this pin makes an unreviewed edit fail here.
    _assert_permission_pin(_text(IMAGES), IMAGE_PERMISSIONS)


def test_digest_recorder_is_read_only_and_declares_no_secrets_of_its_own() -> None:
    recorder = _text(RECORD)
    _assert_permission_pin(recorder, RECORDER_PERMISSIONS)
    # The recorder reads the GCP impersonation secret the caller passes through secrets: inherit.
    assert not re.search(r"secrets\s*:", recorder, re.IGNORECASE)
    _assert_recorder_reads_only_the_impersonation_secret(recorder)


def test_every_reusable_call_in_the_image_workflow_inherits_the_caller_secrets() -> None:
    _assert_every_reusable_call_inherits_caller_secrets(_text(IMAGES))


def test_the_workflow_never_touches_the_reference_repository() -> None:
    assert "internal-sandbox" not in _text(IMAGES)
    assert "internal-sandbox" not in _text(CI)


def test_digests_are_recorded_for_all_three_images_after_push() -> None:
    # The reusable push workflows expose no digest output, so a local reusable reads the digests
    # back from Artifact Registry after all three pushes have completed.
    text = _text(IMAGES)
    record = _job_block(text, "record-digests")
    assert "needs: [push-controller, push-worker, push-signer]" in record
    assert "uses: ./.github/workflows/cloud-record-digests.yml" in record
    assert "mao-controller" in record and "mao-worker" in record and "mao-signer" in record
    recorder = _text(RECORD)
    assert "gcloud artifacts docker images describe" in recorder
    assert "--show-all-metadata" in recorder
    assert "sha256:" in recorder
    assert "image_summary.digest" in recorder
    assert f"signer_image: {AR_PREFIX}/mao-signer:${{{{ github.sha }}}}" in text


def test_the_digest_recorder_gates_scan_state_through_the_release_evidence_module() -> None:
    recorder = _text(RECORD)
    assert "PYTHONPATH=cloud/src python3 -m mao_cloud.release_evidence" in recorder
    for flag in ("--controller-describe", "--worker-describe", "--signer-describe"):
        assert flag in recorder, flag
    assert "GITHUB_STEP_SUMMARY" in recorder
    assert "exit \"$gate\"" in recorder
    assert "actions/checkout@" in recorder
    assert "signer_image:" in recorder


def test_the_digest_recorder_normalizes_each_describe_response_before_any_jq_read() -> None:
    # gcloud can emit raw control characters that jq rejects; the normalizer must rewrite every
    # describe response inside describe(), ahead of the first jq read of the file.
    recorder = _text(RECORD)
    describe_body = recorder[recorder.index("describe() {") : recorder.index("describe controller ")]
    gcloud_read = describe_body.index("gcloud artifacts docker images describe")
    normalize = describe_body.index("PYTHONPATH=cloud/src python3 -m mao_cloud.describe_normalize")
    assert gcloud_read < normalize
    assert normalize < recorder.index("jq -r")


def test_the_cloud_ci_builds_and_smoke_tests_the_signer_image_without_pushing() -> None:
    text = _text(CI)
    images = _job_block(text, "images")
    assert "docker build --platform linux/amd64 -f cloud/Dockerfile.signer -t mao-signer:ci ." in images
    assert "docker run --rm --network none --entrypoint /bin/sh mao-signer:ci" in images
    assert 'test "$(id -u)" = 10003' in images
    assert "import mao_cloud.signer_serve" in images
    assert "push" not in images.lower().replace("pushing", "")


def test_deploy_is_not_in_the_image_workflow() -> None:
    text = _text(IMAGES)
    for word in ("gcloud run", "terraform", "gcloud scheduler", "azure-pipelines"):
        assert word not in text


def _with_top_level_line(text: str, line: str) -> str:
    mutated = re.sub(r"^permissions:\n", f"permissions:\n{line}\n", text, count=1, flags=re.MULTILINE)
    assert mutated != text
    return mutated


# Negative cases: each edit below is a bypass the strict pins must reject.


def _reviewed_permissions_block(expected: dict[str, str]) -> str:
    return "permissions:\n" + "".join(f"  {k}: {v}\n" for k, v in expected.items()) + "jobs:\n"


@pytest.mark.parametrize(
    ("expected", "scope"),
    [
        (IMAGE_PERMISSIONS, "  security-events: write # added"),
        (RECORDER_PERMISSIONS, "  id-token: write # oidc"),
    ],
    ids=["image-scope-with-comment", "recorder-scope-with-comment"],
)
def test_permission_pin_rejects_a_scope_line_it_cannot_read_exactly(
    expected: dict[str, str], scope: str
) -> None:
    # Built from a fixed block, not the live file, so the case stays valid even if a workflow is edited.
    block = _reviewed_permissions_block(expected)
    _assert_permission_pin(block, expected)
    with pytest.raises(AssertionError, match="unparsed permissions line"):
        _assert_permission_pin(_with_top_level_line(block, scope), expected)


@pytest.mark.parametrize(
    ("job", "scope"),
    [
        ("build-controller", "      contents: write"),
        ("record-digests", "      contents: write\n      id-token: write"),
    ],
    ids=["build-controller-job-write", "record-digests-job-write"],
)
def test_permission_pin_rejects_a_job_level_permissions_block(job: str, scope: str) -> None:
    text = _text(IMAGES)
    mutated = re.sub(
        rf"^  {re.escape(job)}:\n", f"  {job}:\n    permissions:\n{scope}\n", text, count=1, flags=re.MULTILINE
    )
    assert mutated != text
    with pytest.raises(AssertionError, match="expected exactly one top-level permissions key"):
        _assert_permission_pin(mutated, IMAGE_PERMISSIONS)


def test_commented_out_secrets_inherit_fails_the_reusable_call_pin() -> None:
    # Built from a fixed block, not the live file, so the case stays valid even if a workflow is edited.
    jobs = (
        "build-controller", "build-worker", "build-signer",
        "push-controller", "push-worker", "push-signer",
        "record-digests",
    )
    block = "".join(f"  {job}:\n    uses: ./x\n    secrets: inherit\n" for job in jobs)
    _assert_every_reusable_call_inherits_caller_secrets(block)
    mutated = block.replace(
        "  push-worker:\n    uses: ./x\n    secrets: inherit\n",
        "  push-worker:\n    uses: ./x\n    # secrets: inherit\n",
    )
    assert mutated != block
    with pytest.raises(AssertionError, match="push-worker"):
        _assert_every_reusable_call_inherits_caller_secrets(mutated)


@pytest.mark.parametrize(
    ("extra_line", "match"),
    [
        ("other: ${{ secrets.OTHER_SECRET }}", "OTHER_SECRET"),
        ("other: ${{ secrets['OTHER_SECRET'] }}", "OTHER_SECRET"),
        ("other: ${{ toJSON(secrets) }}", "without a literal name"),
        ("other: ${{ secrets[env.NAME] }}", "without a literal name"),
        ("other: ${{ SECRETS.OTHER_SECRET }}", "OTHER_SECRET"),
        ("other: ${{ format('{0}', secrets.OTHER_SECRET) }}", "OTHER_SECRET"),
        ("other: ${{ fromJSON('{\"k\": 1}').k || secrets.OTHER_SECRET }}", "OTHER_SECRET"),
    ],
    ids=[
        "dot-read",
        "bracket-read",
        "whole-context-read",
        "computed-index-read",
        "upper-case-read",
        "format-braces-read",
        "fromjson-braces-read",
    ],
)
def test_recorder_secret_pin_rejects_each_other_secret_read_shape(extra_line: str, match: str) -> None:
    # Built from a fixed line, not the live file, so the case stays valid even if the recorder is edited.
    base = "credentials_json: ${{ secrets.GS2_IMPERSONATION_ACCOUNT_SECRET }}\n"
    _assert_recorder_reads_only_the_impersonation_secret(base)
    with pytest.raises(AssertionError, match=re.escape(match)):
        _assert_recorder_reads_only_the_impersonation_secret(base + f"          {extra_line}\n")
