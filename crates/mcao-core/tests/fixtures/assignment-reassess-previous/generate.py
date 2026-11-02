"""Writes cases.json and the Python reference output for every previous-record case.

Run from any directory:
  python3 -I -B -X utf8 crates/mcao-core/tests/fixtures/assignment-reassess-previous/generate.py <ref-root>
where <ref-root> is an extracted tree of main d80bcd15 (git -C <repo> archive d80bcd15 | tar -x -C <ref-root>).
Each case builds a repository with fixed identity and dates. A case with a seed first runs the reference
`reassess` on the seed state with no previous record, and the seed's decision becomes the previous record
after the case's patch. Then the reference runs `reassess --request` on the case state. Stdout goes to
<case>.json and the exit code to <case>.exit. cases.json holds the request and previous record inputs with
$BASE in place of the base commit. The Rust replay in tests/assignment_reassess_previous.rs rebuilds the
same repositories and compares stdout and exit code byte for byte.
"""

import copy
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
GUIDE = "# Guide\n\nIntro line.\n\n```sh\necho one\n```\n\nClosing text.\n"
EDIT_INTRO = GUIDE.replace("Intro line.", "Intro line, edited.")
ALL_TRUE = {
    name: True
    for name in (
        "single_bounded_behavior_change", "no_security_or_authorization_change",
        "no_sensitive_data_change", "no_destructive_operation", "no_merge_delivery_authority_change",
        "no_dispatch_lifecycle_worktree_or_proof_gate_change", "no_public_contract_incompatibility",
        "no_data_migration", "no_unresolved_cross_component_impact",
        "relevant_focused_verification_identified",
    )
}
INTENT = "port the previous record"
ENV = {
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_AUTHOR_NAME": "fixture",
    "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
    "GIT_COMMITTER_NAME": "fixture",
    "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
    "GIT_AUTHOR_DATE": "2026-10-16T02:00:00Z",
    "GIT_COMMITTER_DATE": "2026-10-16T02:00:00Z",
}


def text(path, content):
    return {"path": path, "text": content}


def inventory(paths):
    return text(
        "docs/documentation-audiences.json",
        json.dumps({"surfaces": [{"path": p, "audience": "public-product"} for p in paths]}, indent=2) + "\n",
    )


BASE = [inventory(["docs/guide.md"]), text("docs/guide.md", GUIDE)]
WT_PROSE = [text("docs/guide.md", EDIT_INTRO)]
WT_CODE = [text("scripts/x.sh", "#!/bin/sh\n")]


def request(triggers=(), conditions=None, pinned="rev-1", intent=INTENT):
    body = {
        "task_id": "dpes-884",
        "pinned_policy_revision": pinned,
        "target_branch": "DPES-751",
        "comparison_base_commit": "$BASE",
        "assignment_intent": intent,
        "expected_surfaces": ["docs/guide.md", "scripts/x.sh"],
        "risk_triggers": list(triggers),
    }
    if conditions is not None:
        body["small_low_risk_conditions"] = conditions
    return body


def write_entry(root, entry):
    target = root / entry["path"]
    if entry.get("delete"):
        target.unlink()
        return
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(entry["text"].encode("utf-8"))


def child_env():
    env = dict(ENV)
    env["PATH"] = os.environ["PATH"]
    return env


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], env=child_env(), check=True,
                          capture_output=True).stdout.decode("utf-8").strip()


def build(scratch, name, base, worktree):
    repo = scratch / name
    subprocess.run(["git", "-c", "init.defaultBranch=main", "init", "-q", str(repo)],
                   env=child_env(), check=True)
    for entry in base:
        write_entry(repo, entry)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    sha = git(repo, "rev-parse", "HEAD")
    for entry in worktree:
        write_entry(repo, entry)
    return repo, sha


def reference(ref_root, scratch, tag, repo, body, base_sha):
    request_file = scratch / f"{tag}.request.json"
    request_file.write_text(json.dumps(body).replace("$BASE", base_sha), encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, "-I", "-B", "-X", "utf8", str(ref_root / "bin" / "fm-assignment-reassess.py"),
         "reassess", "--repo", str(repo), "--request", str(request_file)],
        env=child_env(), capture_output=True, check=False,
    )
    if proc.stderr or proc.returncode not in (0, 1):
        raise SystemExit(f"{tag}: exit {proc.returncode}: {proc.stderr.decode('utf-8')}")
    return proc.returncode, proc.stdout


SEEDS = {
    # Unresolved high-risk, provisional: the base alone has no changed path.
    "unresolved": (BASE, [], request()),
    # Resolved small-low-risk, final: one attested code change.
    "small": (BASE, WT_CODE, request(conditions=ALL_TRUE)),
    # Unresolved with a risk trigger inherited by later decisions.
    "trigger": (BASE, [], request(triggers=["breaking-public-contract"])),
    # Resolved documentation-only, final.
    "doc": (BASE, WT_PROSE, request(conditions=ALL_TRUE)),
}


def seed_decision(ref_root, scratch, seed_name):
    base, worktree, body = SEEDS[seed_name]
    repo, sha = build(scratch, f"seed-{seed_name}", base, worktree)
    code, out = reference(ref_root, scratch, f"seed-{seed_name}", repo, body, sha)
    if code != 0:
        raise SystemExit(f"seed {seed_name} exited {code}")
    return json.loads(out)["decision"]


def patched(decision, **changes):
    record = copy.deepcopy(decision)
    for key, value in changes.items():
        if value is ...:
            record.pop(key, None)
        else:
            record[key] = value
    return record


def case(name, seed, state, body, previous_of=None, literal=None, has_previous=True):
    return {"name": name, "seed": seed, "state": state, "request": body,
            "previous_of": previous_of, "literal": literal, "has_previous": has_previous}


SUPERSEDE = [
    case("supersede-unresolved-to-documentation", "unresolved", WT_PROSE, request(conditions=ALL_TRUE)),
    case("supersede-unresolved-to-small-low-risk", "unresolved", WT_CODE, request(conditions=ALL_TRUE)),
    case("supersede-unresolved-to-standard", "unresolved", WT_CODE, request()),
    case("supersede-unresolved-adds-trigger", "unresolved", WT_CODE,
         request(triggers=["breaking-public-contract"], conditions=ALL_TRUE)),
    case("supersede-small-escalates-to-high-risk", "small", WT_CODE,
         request(triggers=["destructive-operation-or-data-migration"], conditions=ALL_TRUE)),
    case("supersede-small-unchanged", "small", WT_CODE, request(conditions=ALL_TRUE)),
    case("supersede-inherited-trigger-persists", "trigger", WT_PROSE, request(conditions=ALL_TRUE)),
    case("supersede-identity-invalidated", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, evaluated_change_identity="sha256:" + "0" * 64)),
    case("supersede-base-changed", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, comparison_base_commit="b" * 40)),
    case("supersede-policy-revision-changed", "unresolved", WT_CODE, request(conditions=ALL_TRUE, pinned="rev-2")),
    case("supersede-coverage-changed", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, change_coverage={"complete": False, "categories": ["tracked-working-tree"]})),
    case("supersede-sequence-increments", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, decision_id="dpes-884/7")),
    case("supersede-huge-sequence", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, decision_id="dpes-884/99999999999999999999999999")),
    case("supersede-trailing-newline-sequence", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, decision_id="dpes-884/1\n")),
    case("supersede-unicode-intent", "unresolved", WT_CODE,
         request(conditions=ALL_TRUE, intent="port caf\u00e9 \u2615 \u65e5\u672c \U0001F680"),
         previous_of=lambda d: patched(d, assignment_intent="port caf\u00e9 \u2615 \u65e5\u672c \U0001F680")),
    case("supersede-reasons-accumulate", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, reasons=["no changed paths were collected", "earlier note \u00e9"])),
    case("supersede-previous-supersedes-replaced", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, supersedes={"decision_id": "dpes-884/1", "escalation_reasons": ["x"]})),
    case("supersede-large-reason-list", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, reasons=["reason %d" % i for i in range(500)])),
    case("blocked-previous-identity-null", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
         previous_of=lambda d: patched(d, evaluated_change_identity=None)),
    case("previous-null-is-fresh", "unresolved", WT_CODE, request(conditions=ALL_TRUE), literal=None, has_previous=True),
]

BLOCKING = [
    ("unknown-key", lambda d: patched(d, extra=1)),
    ("missing-key", lambda d: patched(d, reasons=...)),
    ("contract-version-two", lambda d: patched(d, contract_version=2)),
    ("contract-version-true", lambda d: patched(d, contract_version=True)),
    ("contract-version-float-one-accepted", lambda d: patched(d, contract_version=1.0)),
    ("decision-id-other-task", lambda d: patched(d, decision_id="other/1")),
    ("decision-id-zero-sequence", lambda d: patched(d, decision_id="dpes-884/0")),
    ("decision-id-not-string", lambda d: patched(d, decision_id=7)),
    ("task-id-differs", lambda d: patched(d, task_id="other")),
    ("assignment-intent-differs", lambda d: patched(d, assignment_intent="other intent")),
    ("expected-surfaces-differ", lambda d: patched(d, expected_surfaces=["x"])),
    ("target-branch-differs", lambda d: patched(d, target_branch="main")),
    ("pinned-revision-empty", lambda d: patched(d, pinned_policy_revision="")),
    ("base-not-hex", lambda d: patched(d, comparison_base_commit="XYZ")),
    ("profile-unknown", lambda d: patched(d, profile="weird")),
    ("stage-unknown", lambda d: patched(d, decision_stage="maybe")),
    ("status-unknown", lambda d: patched(d, classification_status="maybe")),
    ("triggers-not-list", lambda d: patched(d, risk_triggers="breaking-public-contract")),
    ("triggers-need-high-risk", lambda d: patched(d, risk_triggers=["breaking-public-contract"], profile="small-low-risk", decision_stage="final", classification_status="resolved")),
    ("coverage-not-object", lambda d: patched(d, change_coverage="all")),
    ("coverage-complete-disagrees", lambda d: patched(d, change_coverage={"complete": True, "categories": ["committed"]})),
    ("resolved-with-incomplete-coverage", lambda d: patched(d, decision_stage="final", classification_status="resolved", change_coverage={"complete": False, "categories": []})),
    ("reasons-empty", lambda d: patched(d, reasons=[])),
    ("required-stages-mismatch", lambda d: patched(d, required_stages=["ground-scope"])),
    ("evidence-obligations-mismatch", lambda d: patched(d, evidence_obligations={"tests_required": False, "independent_change_review_required": True})),
    ("final-not-resolved", lambda d: patched(d, decision_stage="final")),
    ("final-without-identity", lambda d: patched(d, decision_stage="final", classification_status="resolved", evaluated_change_identity=...)),
    ("identity-empty", lambda d: patched(d, evaluated_change_identity="")),
    ("supersedes-malformed", lambda d: patched(d, supersedes={"decision_id": "x"})),
    ("previous-is-list", lambda d: []),
    ("previous-is-string", lambda d: "text"),
    ("previous-is-empty-object", lambda d: {}),
    ("reasons-unicode-empty-item", lambda d: patched(d, reasons=["caf\u00e9", ""])),
    ("huge-pinned-revision", lambda d: patched(d, pinned_policy_revision="r" * 5000)),
]
for _name, _patch in BLOCKING:
    SUPERSEDE.append(case(f"blocked-previous-{_name}", "unresolved", WT_CODE, request(conditions=ALL_TRUE),
                          previous_of=_patch))

SUPERSEDE.append(case("blocked-request-and-previous", "unresolved", WT_CODE,
                      dict(request(conditions=ALL_TRUE), task_id="a/b", extra_key=1),
                      previous_of=lambda d: patched(d, profile="weird")))
SUPERSEDE.append(case("blocked-empty-intent-request", "unresolved", WT_CODE,
                      request(conditions=ALL_TRUE, intent=""),
                      previous_of=lambda d: patched(d)))


def main():
    ref_root = pathlib.Path(sys.argv[1]).resolve()
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="previous-cases-"))
    cases = []
    try:
        seeds = {name: seed_decision(ref_root, scratch, name) for name in SEEDS}
        for item in SUPERSEDE:
            base = BASE
            repo, sha = build(scratch, item["name"], base, item["state"])
            body = copy.deepcopy(item["request"])
            previous = None
            if item["previous_of"] is not None:
                previous = item["previous_of"](copy.deepcopy(seeds[item["seed"]])) if item["seed"] else None
            elif item["seed"] is not None:
                previous = copy.deepcopy(seeds[item["seed"]])
            if item["name"] == "previous-null-is-fresh":
                previous = None
            if previous is not None or item["name"] == "previous-null-is-fresh":
                body["previous"] = previous
            body_text = json.dumps(body).replace(sha, "$BASE")
            code, out = reference(ref_root, scratch, item["name"], repo, json.loads(body_text), sha)
            (HERE / f"{item['name']}.json").write_bytes(out)
            (HERE / f"{item['name']}.exit").write_text(f"{code}\n", encoding="utf-8")
            cases.append({
                "name": item["name"],
                "base": base,
                "worktree": item["state"],
                "request": json.loads(body_text),
                "previous_source": item["seed"],
            })
            print(item["name"], code)
    finally:
        shutil.rmtree(scratch)
    (HERE / "cases.json").write_text(json.dumps(cases, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
