#!/usr/bin/env python3
"""fm-assignment-reassess.py - reassess an assignment profile from the actual change set.

docs/assignment-profile-policy.md owns the profiles, precedence, path classes, and stage
mapping. docs/assignment-profile-decision.v1.schema.json owns the record shape. This script
is the pure core: it collects the change set against a comparison base, derives the change
identity, classifies under the approved precedence, and emits a superseding decision record.
It does not gate completion, persist records, or pin the base. Those wait on DPES-763 and
DPES-764, and the host supplies the pinned values through the request.

Subcommands:
  collect   --repo DIR --base-commit SHA
            Print coverage categories, gaps, the changed paths, and the change identity
            (present only when collection is complete).
  reassess  --repo DIR --request FILE
            Print {"status", "decision", "transition"}. The request carries the host pins,
            the classifier inputs, and the previous decision when one exists.

Exit codes: 0 recorded or unchanged, 1 blocked (invalid or contradictory input, no record
created), 2 usage or unreadable input.

Change collection, compared against the comparison base commit:
  committed             git diff --name-status -M BASE HEAD
  tracked-working-tree  git diff --name-status -M HEAD
  untracked             git ls-files --others --exclude-standard
  renamed, deleted      covered when committed and tracked-working-tree both succeed
A category counts as covered only when its git query succeeded. A failed query is reported
as incomplete change_coverage, never as an empty change set. Every changed path is read from
the comparison base (old side) and from the worktree (new side).

Change identity: "sha256:<hex>" over the base commit, the binary patch from BASE to the
worktree, and the sha256 of each untracked file's bytes in sorted path order. It is computed
only when collection is complete and every untracked path is a regular file.

Path classes use the inventory docs/documentation-audiences.json as committed at the
comparison base, so a worktree edit cannot reclassify its own change:
  prose               .md, .mdx, .rst, .txt with a prose audience
  behavioral          audience agent-runtime
  executable-example  paths under docs/examples/
  code                .sh, .bash, .py, .json, .toml, .yaml, .yml
  unknown             everything else, including a file with no inventory entry
A prose change that touches a line inside a fenced block, in either version, is executable.
Undecodable prose cannot be line-checked, so it is unavailable evidence and stays unresolved.

Interim choices, each tied to a policy open item: the code extension list (open item 1),
an empty change set is unresolved, and a resolved decision keeps the high-risk level of an
earlier unresolved decision because version 1 has no lowering path (open item 4).
The small-low-risk attestation keys are this script's encoding of the policy's conditions,
and risk triggers and attestations are supplied by the classifier, not derived from paths.
"""

import argparse
import difflib
import hashlib
import json
import os
import re
import stat
import subprocess
import sys

CONTRACT_VERSION = 1
PROFILES = ("documentation-only", "small-low-risk", "standard", "high-risk")
PROFILE_RANK = {name: rank for rank, name in enumerate(PROFILES)}
PROFILE_STAGES = {
    "documentation-only": (
        "ground-scope", "implement", "focused-review", "verify", "change-reassessment", "delivery-handoff",
    ),
    "small-low-risk": (
        "ground-scope", "short-plan", "implement", "focused-review", "verify", "change-reassessment",
        "delivery-handoff",
    ),
    "standard": (
        "ground-scope", "design", "implement", "focused-review", "verify", "change-reassessment",
        "delivery-handoff",
    ),
    "high-risk": (
        "ground-scope", "design", "design-critique", "implement", "independent-change-review",
        "risk-verify", "change-reassessment", "delivery-handoff",
    ),
}
RISK_TRIGGERS = (
    "security-authorization-secrets-or-sensitive-data",
    "destructive-operation-or-data-migration",
    "dispatch-lifecycle-worktree-delivery-merge-or-proof-gate-authority",
    "breaking-public-contract",
    "cross-component-shared-state-concurrency-or-recovery",
    "broad-change-needing-coordinated-design-and-verification",
)
COVERAGE_CATEGORIES = ("committed", "tracked-working-tree", "untracked", "renamed", "deleted")
SMALL_LOW_RISK_CONDITIONS = (
    "single_bounded_behavior_change",
    "no_security_or_authorization_change",
    "no_sensitive_data_change",
    "no_destructive_operation",
    "no_merge_delivery_authority_change",
    "no_dispatch_lifecycle_worktree_or_proof_gate_change",
    "no_public_contract_incompatibility",
    "no_data_migration",
    "no_unresolved_cross_component_impact",
    "relevant_focused_verification_identified",
)
PROSE_EXTENSIONS = (".md", ".mdx", ".rst", ".txt")
PROSE_AUDIENCES = frozenset(
    ("public-product", "operator-current", "maintainer-architecture", "maintainer-verification")
)
CODE_EXTENSIONS = (".sh", ".bash", ".py", ".json", ".toml", ".yaml", ".yml")
EXAMPLE_PREFIX = "docs/examples/"
INVENTORY_PATH = "docs/documentation-audiences.json"
DECISION_STAGES = ("provisional", "final")
CLASSIFICATION_STATUSES = ("resolved", "unresolved")
REQUIRED_REQUEST_KEYS = (
    "task_id", "pinned_policy_revision", "target_branch", "comparison_base_commit",
    "assignment_intent", "expected_surfaces",
)
OPTIONAL_REQUEST_KEYS = ("risk_triggers", "small_low_risk_conditions", "previous")
RECORD_KEYS = frozenset((
    "contract_version", "decision_id", "task_id", "assignment_intent", "expected_surfaces",
    "pinned_policy_revision", "profile", "decision_stage", "classification_status",
    "risk_triggers", "change_coverage", "reasons", "required_stages", "evidence_obligations",
    "target_branch", "comparison_base_commit",
))
OPTIONAL_RECORD_KEYS = frozenset(("evaluated_change_identity", "supersedes"))
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DECISION_ID_RE = re.compile(r"^[^/]+/[1-9][0-9]*$")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")


class GitError(Exception):
    pass


def git(cwd, *args):
    try:
        proc = subprocess.run(
            ["git", *args], cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False
        )
    except OSError as exc:
        raise GitError(f"git unavailable: {exc}")
    if proc.returncode != 0:
        raise GitError(f"git {args[0]} exited {proc.returncode}")
    return proc.stdout


def parse_name_status(output):
    tokens = output.split(b"\0")
    paths = set()
    index = 0
    while index < len(tokens):
        status = tokens[index]
        index += 1
        if not status:
            continue
        width = 2 if status[:1] in (b"R", b"C") else 1
        for token in tokens[index:index + width]:
            paths.add(os.fsdecode(token))
        index += width
    return paths


def parse_listing(output):
    return {os.fsdecode(token) for token in output.split(b"\0") if token}


def read_worktree(top, path):
    full = os.path.join(top, path)
    try:
        info = os.lstat(full)
    except (FileNotFoundError, NotADirectoryError):
        return "absent", None
    if not stat.S_ISREG(info.st_mode):
        return "other", None
    with open(full, "rb") as handle:
        return "file", handle.read()


def read_base(top, base, path):
    spec = f"{base}:{path}"
    try:
        kind = git(top, "cat-file", "-t", spec).strip()
    except GitError:
        return None
    return git(top, "cat-file", "blob", spec) if kind == b"blob" else None


def load_audiences(top, base):
    try:
        document = json.loads(git(top, "show", f"{base}:{INVENTORY_PATH}"))
        return {entry["path"]: entry["audience"] for entry in document["surfaces"]}
    except (GitError, ValueError, KeyError, TypeError):
        return None


def change_identity(top, base, untracked):
    try:
        patch = git(top, "diff", "--binary", "--no-color", "--no-ext-diff", "--no-textconv", "-M", base, "--")
    except GitError:
        return None
    digest = hashlib.sha256()
    digest.update(b"assignment-change-identity/v1\0")
    digest.update(base.encode("ascii") + b"\0")
    digest.update(b"patch\0" + hashlib.sha256(patch).digest())
    for path in untracked:
        kind, data = read_worktree(top, path)
        if kind != "file":
            return None
        digest.update(b"untracked\0" + os.fsencode(path) + b"\0" + hashlib.sha256(data).digest())
    return "sha256:" + digest.hexdigest()


def collect(repo, base):
    result = {
        "base_ok": False, "complete": False, "categories": [], "gaps": [], "records": [],
        "identity": None, "audiences": None,
    }
    try:
        top = os.fsdecode(git(repo, "rev-parse", "--show-toplevel").strip())
    except GitError as exc:
        result["gaps"].append(f"not a git worktree: {exc}")
        return result
    base_ok = bool(SHA_RE.match(base))
    if base_ok:
        try:
            git(top, "cat-file", "-e", f"{base}^{{commit}}")
        except GitError:
            base_ok = False
    result["base_ok"] = base_ok

    def query(label, args, parse):
        try:
            return parse(git(top, *args))
        except GitError as exc:
            result["gaps"].append(f"{label}: {exc}")
            return None

    committed = tracked = None
    if base_ok:
        committed = query(
            "committed",
            ["diff", "--name-status", "-z", "-M", "--no-ext-diff", base, "HEAD", "--"],
            parse_name_status,
        )
    tracked = query(
        "tracked-working-tree",
        ["diff", "--name-status", "-z", "-M", "--no-ext-diff", "HEAD", "--"],
        parse_name_status,
    )
    untracked = query("untracked", ["ls-files", "-z", "--others", "--exclude-standard"], parse_listing)

    covered = set()
    if committed is not None:
        covered.add("committed")
    if tracked is not None:
        covered.add("tracked-working-tree")
    if untracked is not None:
        covered.add("untracked")
    if committed is not None and tracked is not None:
        covered.update(("renamed", "deleted"))
    result["categories"] = [name for name in COVERAGE_CATEGORIES if name in covered]
    result["complete"] = len(result["categories"]) == len(COVERAGE_CATEGORIES)

    paths = set()
    for found in (committed, tracked, untracked):
        if found is not None:
            paths |= found
    for path in sorted(paths):
        new_kind, new = read_worktree(top, path)
        old = read_base(top, base, path) if base_ok else None
        result["records"].append({"path": path, "old": old, "new": new, "new_kind": new_kind})
    if base_ok:
        result["audiences"] = load_audiences(top, base)
        if result["complete"]:
            result["identity"] = change_identity(top, base, sorted(untracked))
    return result


def path_class(path, audiences):
    if path.startswith(EXAMPLE_PREFIX):
        return "executable-example"
    audience = (audiences or {}).get(path)
    if audience == "agent-runtime":
        return "behavioral"
    extension = os.path.splitext(path)[1].lower()
    if extension in PROSE_EXTENSIONS and audience in PROSE_AUDIENCES:
        return "prose"
    if extension in CODE_EXTENSIONS:
        return "code"
    return "unknown"


def fenced_lines(lines):
    fenced = set()
    marker = None
    for number, line in enumerate(lines, 1):
        match = FENCE_RE.match(line)
        if marker is None:
            if match:
                marker = match.group(1)
                fenced.add(number)
            continue
        fenced.add(number)
        if match and match.group(1)[0] == marker[0] and len(match.group(1)) >= len(marker):
            if not line.strip()[len(match.group(1)):]:
                marker = None
    return fenced


def prose_verdict(old, new):
    try:
        old_lines = old.decode("utf-8").splitlines() if old is not None else []
        new_lines = new.decode("utf-8").splitlines() if new is not None else []
    except UnicodeDecodeError:
        return "unverifiable"
    old_fenced = fenced_lines(old_lines)
    new_fenced = fenced_lines(new_lines)
    matcher = difflib.SequenceMatcher(None, old_lines, new_lines, autojunk=False)
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            continue
        if any(n in old_fenced for n in range(i1 + 1, i2 + 1)):
            return "executable"
        if any(n in new_fenced for n in range(j1 + 1, j2 + 1)):
            return "executable"
    return "clean"


def small_low_risk_gaps(conditions):
    if conditions is None:
        return list(SMALL_LOW_RISK_CONDITIONS)
    return [name for name in SMALL_LOW_RISK_CONDITIONS if conditions.get(name) is not True]


def classify(collection, triggers, conditions):
    """Return (profile, status, reasons) for the current change set alone."""
    unresolved = []
    reasons = []
    records = collection["records"]
    classes = []
    prose_clean = True
    for record in records:
        path = record["path"]
        cls = path_class(path, collection["audiences"])
        classes.append(cls)
        if record["new_kind"] == "other":
            unresolved.append(f"non-regular worktree path: {path}")
        elif cls == "prose":
            verdict = prose_verdict(record["old"], record["new"])
            if verdict == "unverifiable":
                unresolved.append(f"prose cannot be decoded for line checks: {path}")
            elif verdict != "clean":
                prose_clean = False
                reasons.append(f"{path}: {verdict} change in prose")
        elif cls == "unknown":
            unresolved.append(f"unknown path: {path}")
        else:
            reasons.append(f"{path}: {cls} change")
    if not records:
        unresolved.append("no changed paths were collected")
    if not collection["base_ok"]:
        unresolved.append("comparison base is not resolvable in the worktree")
    missing = [name for name in COVERAGE_CATEGORIES if name not in collection["categories"]]
    if missing:
        unresolved.append("change collection incomplete, missing: " + ", ".join(missing))
    unresolved.extend(f"collection gap: {gap}" for gap in collection["gaps"])
    if collection["audiences"] is None and collection["base_ok"]:
        unresolved.append("policy inventory is unavailable at the comparison base")
    if collection["complete"] and collection["base_ok"] and collection["identity"] is None:
        unresolved.append("change identity could not be computed")
    if triggers is None:
        unresolved.append("risk is not assessed: risk_triggers is absent")

    triggers = triggers or []
    gaps = small_low_risk_gaps(conditions)
    all_prose = bool(classes) and all(cls == "prose" for cls in classes)
    if unresolved:
        profile, status = "high-risk", "unresolved"
    elif triggers:
        profile, status = "high-risk", "resolved"
    elif all_prose and prose_clean:
        profile, status = "documentation-only", "resolved"
    elif records and not gaps:
        profile, status = "small-low-risk", "resolved"
    else:
        profile, status = "standard", "resolved"

    reasons = unresolved + [f"risk trigger: {name}" for name in triggers] + reasons
    if profile == "documentation-only":
        reasons.append("every changed path is prose with no executable or unverifiable lines")
    elif profile == "small-low-risk":
        reasons.append("all small-low-risk conditions attested for a single bounded change")
    elif profile == "standard" and not unresolved and not triggers:
        reasons.append("default standard; small-low-risk not met: " + ", ".join(gaps))
    return profile, status, reasons


def dedupe(items):
    seen = set()
    result = []
    for item in items:
        if item not in seen:
            seen.add(item)
            result.append(item)
    return result


def nonempty_str(value):
    return isinstance(value, str) and value.strip() != ""


def known(value, options):
    return isinstance(value, str) and value in options


def trigger_list_valid(value):
    return (
        isinstance(value, list)
        and all(isinstance(item, str) and item in RISK_TRIGGERS for item in value)
        and len(set(value)) == len(value)
    )


def category_list_valid(value):
    return (
        isinstance(value, list)
        and all(isinstance(item, str) and item in COVERAGE_CATEGORIES for item in value)
        and len(set(value)) == len(value)
    )


def validate_request(request):
    if not isinstance(request, dict):
        return ["request must be a JSON object"]
    problems = []
    unknown = sorted(set(request) - set(REQUIRED_REQUEST_KEYS) - set(OPTIONAL_REQUEST_KEYS))
    if unknown:
        problems.append("request has unknown keys: " + ", ".join(unknown))
    for key in REQUIRED_REQUEST_KEYS:
        if key not in request:
            problems.append(f"request is missing {key}")
    task_id = request.get("task_id")
    if not nonempty_str(task_id) or "/" in task_id:
        problems.append("task_id must be a non-empty string without '/'")
    for key in ("pinned_policy_revision", "target_branch", "assignment_intent"):
        if key in request and not nonempty_str(request[key]):
            problems.append(f"{key} must be a non-empty string")
    base = request.get("comparison_base_commit")
    if not (isinstance(base, str) and SHA_RE.match(base)):
        problems.append("comparison_base_commit must be a 40-hex commit")
    surfaces = request.get("expected_surfaces")
    if "expected_surfaces" in request and not (
        isinstance(surfaces, list)
        and all(nonempty_str(path) for path in surfaces)
        and len(set(surfaces)) == len(surfaces)
    ):
        problems.append("expected_surfaces must be a unique list of non-empty paths")
    triggers = request.get("risk_triggers")
    if triggers is not None and not trigger_list_valid(triggers):
        problems.append("risk_triggers must be null or a unique list of known trigger names")
    conditions = request.get("small_low_risk_conditions")
    if conditions is not None:
        if not isinstance(conditions, dict):
            problems.append("small_low_risk_conditions must be an object")
        else:
            extra = sorted(set(conditions) - set(SMALL_LOW_RISK_CONDITIONS))
            if extra:
                problems.append("small_low_risk_conditions has unknown keys: " + ", ".join(extra))
            if any(not isinstance(value, bool) for value in conditions.values()):
                problems.append("small_low_risk_conditions values must be booleans")
    if request.get("previous") is not None:
        problems.extend(validate_record(request["previous"], task_id, request))
    return problems


def validate_record(record, task_id, request):
    if not isinstance(record, dict):
        return ["previous must be a decision record object"]
    problems = []
    unknown = sorted(set(record) - RECORD_KEYS - OPTIONAL_RECORD_KEYS)
    if unknown:
        problems.append("previous has unknown keys: " + ", ".join(unknown))
    missing = sorted(RECORD_KEYS - set(record))
    if missing:
        problems.append("previous is missing keys: " + ", ".join(missing))
    version = record.get("contract_version")
    if isinstance(version, bool) or version != CONTRACT_VERSION:
        problems.append(f"previous contract_version must be {CONTRACT_VERSION}")
    decision_id = record.get("decision_id")
    if not (
        isinstance(decision_id, str)
        and DECISION_ID_RE.match(decision_id)
        and decision_id.split("/")[0] == task_id
    ):
        problems.append("previous decision_id must be <task_id>/<sequence> for the request task_id")
    if record.get("task_id") != task_id:
        problems.append("previous task_id differs from the request task_id")
    if record.get("assignment_intent") != request.get("assignment_intent"):
        problems.append("previous assignment_intent differs from the request")
    if record.get("expected_surfaces") != request.get("expected_surfaces"):
        problems.append("previous expected_surfaces differ from the request")
    if record.get("target_branch") != request.get("target_branch"):
        problems.append("previous target_branch differs from the host-pinned target_branch")
    if not nonempty_str(record.get("pinned_policy_revision")):
        problems.append("previous pinned_policy_revision must be a non-empty string")
    base = record.get("comparison_base_commit")
    if not (isinstance(base, str) and SHA_RE.match(base)):
        problems.append("previous comparison_base_commit must be a 40-hex commit")

    profile = record.get("profile")
    stage = record.get("decision_stage")
    status = record.get("classification_status")
    if not known(profile, PROFILE_RANK):
        problems.append("previous profile is not a known profile id")
    if not known(stage, DECISION_STAGES):
        problems.append("previous decision_stage must be provisional or final")
    if not known(status, CLASSIFICATION_STATUSES):
        problems.append("previous classification_status must be resolved or unresolved")
    triggers = record.get("risk_triggers")
    if not trigger_list_valid(triggers):
        problems.append("previous risk_triggers must be a unique list of known trigger names")
    elif triggers and profile != "high-risk":
        problems.append("previous risk_triggers require profile high-risk")
    coverage = record.get("change_coverage")
    if not (
        isinstance(coverage, dict)
        and set(coverage) == {"complete", "categories"}
        and isinstance(coverage["complete"], bool)
        and category_list_valid(coverage["categories"])
    ):
        problems.append("previous change_coverage must be {complete, categories} with known categories")
    else:
        full = sorted(coverage["categories"]) == sorted(COVERAGE_CATEGORIES)
        if coverage["complete"] != full:
            problems.append("previous change_coverage.complete disagrees with its categories")
        if status == "resolved" and not coverage["complete"]:
            problems.append("previous resolved decision cannot have incomplete change_coverage")
    reasons = record.get("reasons")
    if not (isinstance(reasons, list) and reasons and all(nonempty_str(item) for item in reasons)):
        problems.append("previous reasons must be a non-empty list of strings")
    if known(profile, PROFILE_RANK):
        if record.get("required_stages") != list(PROFILE_STAGES[profile]):
            problems.append("previous required_stages do not match its profile")
        expected = {
            "tests_required": profile != "documentation-only",
            "independent_change_review_required": profile == "high-risk",
        }
        if record.get("evidence_obligations") != expected:
            problems.append("previous evidence_obligations do not match its profile")
    if stage == "final":
        if status != "resolved":
            problems.append("final decision must be resolved")
        if not nonempty_str(record.get("evaluated_change_identity")):
            problems.append("final decision requires evaluated_change_identity")
    if status == "unresolved" and (stage != "provisional" or profile != "high-risk"):
        problems.append("unresolved decision must be provisional high-risk")
    if "evaluated_change_identity" in record and not nonempty_str(record["evaluated_change_identity"]):
        problems.append("previous evaluated_change_identity must be a non-empty string")
    if "supersedes" in record:
        sup = record["supersedes"]
        if not (
            isinstance(sup, dict)
            and set(sup) == {"decision_id", "escalation_reasons"}
            and isinstance(sup["decision_id"], str)
            and DECISION_ID_RE.match(sup["decision_id"])
            and isinstance(sup["escalation_reasons"], list)
            and sup["escalation_reasons"]
            and all(nonempty_str(item) for item in sup["escalation_reasons"])
        ):
            problems.append("previous supersedes is malformed")
    return problems


def reassess(repo, request):
    previous = request.get("previous")
    base = request["comparison_base_commit"]
    collection = collect(repo, base)
    profile, status, reasons = classify(collection, request.get("risk_triggers"), request.get("small_low_risk_conditions"))

    requested = set(request.get("risk_triggers") or [])
    inherited = set(previous["risk_triggers"]) if previous else set()
    triggers = [name for name in RISK_TRIGGERS if name in requested | inherited]
    effective = profile
    if previous and PROFILE_RANK[previous["profile"]] > PROFILE_RANK[effective]:
        effective = previous["profile"]
    if triggers:
        effective = "high-risk"
    stage = "final" if status == "resolved" else "provisional"
    identity = collection["identity"]
    coverage = {"complete": collection["complete"], "categories": collection["categories"]}

    transition_reasons = []
    if previous:
        if effective != previous["profile"]:
            transition_reasons.append(f"profile escalated from {previous['profile']} to {effective}")
        added = [name for name in triggers if name not in previous["risk_triggers"]]
        if added:
            transition_reasons.append("risk triggers recorded: " + ", ".join(added))
        if (stage, status) != (previous["decision_stage"], previous["classification_status"]):
            transition_reasons.append(f"decision is now {stage} and {status}")
        if identity != previous.get("evaluated_change_identity"):
            transition_reasons.append("change identity changed")
        prior_coverage = previous["change_coverage"]
        if (coverage["complete"], sorted(coverage["categories"])) != (
            prior_coverage["complete"], sorted(prior_coverage["categories"])
        ):
            transition_reasons.append("change coverage changed")
        if base != previous["comparison_base_commit"]:
            transition_reasons.append("comparison base changed")
        if request["pinned_policy_revision"] != previous["pinned_policy_revision"]:
            transition_reasons.append("pinned policy revision changed")

    transition = {
        "previous_profile": previous["profile"] if previous else None,
        "effective_profile": effective,
        "escalated": bool(previous) and PROFILE_RANK[effective] > PROFILE_RANK[previous["profile"]],
        "newly_required_stages": [
            name for name in PROFILE_STAGES[effective]
            if not previous or name not in PROFILE_STAGES[previous["profile"]]
        ],
        "evidence_invalidated": bool(
            previous
            and previous.get("evaluated_change_identity") is not None
            and previous["evaluated_change_identity"] != identity
        ),
    }
    if previous and not transition_reasons:
        return {"status": "unchanged", "decision": previous, "transition": transition}

    sequence = 1 if previous is None else int(previous["decision_id"].rsplit("/", 1)[1]) + 1
    record = {
        "contract_version": CONTRACT_VERSION,
        "decision_id": f"{request['task_id']}/{sequence}",
        "task_id": request["task_id"],
        "assignment_intent": request["assignment_intent"],
        "expected_surfaces": request["expected_surfaces"],
        "pinned_policy_revision": request["pinned_policy_revision"],
        "profile": effective,
        "decision_stage": stage,
        "classification_status": status,
        "risk_triggers": triggers,
        "change_coverage": coverage,
        "reasons": dedupe((previous["reasons"] if previous else []) + reasons + transition_reasons),
        "required_stages": list(PROFILE_STAGES[effective]),
        "evidence_obligations": {
            "tests_required": effective != "documentation-only",
            "independent_change_review_required": effective == "high-risk",
        },
        "target_branch": request["target_branch"],
        "comparison_base_commit": base,
    }
    if identity is not None:
        record["evaluated_change_identity"] = identity
    if previous:
        record["supersedes"] = {"decision_id": previous["decision_id"], "escalation_reasons": transition_reasons}
    return {"status": "recorded", "decision": record, "transition": transition}


def load_json(path):
    with open(path, "rb") as handle:
        return json.loads(handle.read().decode("utf-8"))


def emit(payload):
    json.dump(payload, sys.stdout, indent=2)
    sys.stdout.write("\n")


def cmd_collect(args):
    collection = collect(args.repo, args.base_commit)
    emit({
        "complete": collection["complete"],
        "categories": collection["categories"],
        "gaps": collection["gaps"],
        "base_ok": collection["base_ok"],
        "identity": collection["identity"],
        "paths": [
            {
                "path": record["path"],
                "in_base": record["old"] is not None,
                "in_worktree": record["new_kind"] == "file",
                "kind": record["new_kind"],
            }
            for record in collection["records"]
        ],
    })
    return 0


def cmd_reassess(args):
    try:
        request = load_json(args.request)
    except (OSError, ValueError) as exc:
        print(f"fm-assignment-reassess: cannot read request: {exc}", file=sys.stderr)
        return 2
    problems = validate_request(request)
    if problems:
        emit({"status": "blocked", "reasons": problems})
        return 1
    result = reassess(args.repo, request)
    emit(result)
    return 0


def build_parser():
    parser = argparse.ArgumentParser(description="Reassess an assignment profile from the actual change set.")
    commands = parser.add_subparsers(dest="command", required=True)
    collect_parser = commands.add_parser("collect", help="print the collected change set")
    collect_parser.add_argument("--repo", required=True)
    collect_parser.add_argument("--base-commit", required=True)
    collect_parser.set_defaults(handler=cmd_collect)
    reassess_parser = commands.add_parser("reassess", help="print a reassessed decision record")
    reassess_parser.add_argument("--repo", required=True)
    reassess_parser.add_argument("--request", required=True)
    reassess_parser.set_defaults(handler=cmd_reassess)
    return parser


def main(argv):
    args = build_parser().parse_args(argv)
    return args.handler(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
