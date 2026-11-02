#!/usr/bin/env python3
"""Generate the DPES-957 / DPES-884 pre-port reference baseline fixtures.

Run with the stdlib only, from any directory:

    python3 -I tests/fixtures/pre-port-baseline/generate.py \
        --reference <clean checkout of BASELINE_SHA> \
        --inputs <this repository root> \
        --main-repo <repository with origin/main> \
        --out tests/fixtures/pre-port-baseline

Every expected output is the stdout, stderr, and exit status of one of the seven Python reference
utilities run from the clean reference checkout. Nothing here or in the Rust code computes an
expected value. The checkout must be at BASELINE_SHA and must have no modified, staged, or
untracked file; the generator refuses anything else. Inputs are either committed fixtures read in
place (their hashes are recorded) or inputs written by this file (copied into the output tree).

The owner-approved replacement baseline is main commit BASELINE_SHA. It is not the unavailable
DPES-803 QA tag, and these outputs assert no historical equivalence or passing parity.
Paths that differ between two checkouts (reference, inputs, output, temporary) are replaced
with fixed tokens before hashing, so two regenerations in different directories must match
byte for byte. docs/ and README.md in this directory explain the layout and the evidence.
"""

import argparse
import hashlib
import json
import os
import platform
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unicodedata
import urllib.error
import urllib.request
from pathlib import Path

BASELINE_SHA = "d80bcd15f06b79fed639cf028e273306567ffbfd"
MAIN_REF = "origin/main"
DPES751_REF = "origin/DPES-751"
GENERATOR_REL = "tests/fixtures/pre-port-baseline/generate.py"
UTILITIES = (
    "bin/fm-audit-writer.py",
    "bin/fm-audit-sanitize.py",
    "bin/fm-audit-envelope.py",
    "bin/fm-audit-activity-map.py",
    "bin/fm-assignment-reassess.py",
    "bin/fm-birds-eye-view-serve.py",
    "bin/fm-assignment-profile.py",
)
AUDIT_VALIDATE_REL = "crates/mcao-cli/tests/fixtures/audit-validate"
ASSIGNMENT_CASES_REL = "crates/mcao-core/tests/fixtures/assignment_profile/cases"
ASSIGNMENT_CODE_ROOT_REL = "crates/mcao-core/tests/fixtures/assignment_profile"
ENVELOPE_ACCEPTED_REL = f"{AUDIT_VALIDATE_REL}/envelope-accepted.json"
ENVELOPE_MISSING_USER_REL = f"{AUDIT_VALIDATE_REL}/envelope-missing-user.json"
ENVELOPE_SEQ_ONE_REL = f"{AUDIT_VALIDATE_REL}/envelope-seq-one-null-prev.json"
BIRDS_EYE_ASSETS_REL = "assets/birds-eye-view"
BIRDS_EYE_ROUTES = ("/", "/app.css", "/app.js", "/missing")

# Synthetic seeded values, copied from tests/fm-audit-sanitize.test.sh. They are not credentials.
HOME_KEY = "5e8f1c2a7b9d4e6f0a3b5c7d9e1f2a4b6c8d0e2f4a6b8c0d1e3f5a7b9c1d2e3f"  # gitleaks:allow - synthetic test key
SEEDED = {
    "gh": "ghp_SyntheticTokenAbcdefghij0123",
    "pat": "github_pat_SyntheticPatAbcdefghij0123",
    "aws": "AKIASYNTHETIC0123456",  # gitleaks:allow - synthetic seeded value
    "google": "AIzaSyntheticGoogleKeyForTests000000000",  # gitleaks:allow - synthetic seeded value
    "slack": "xoxb-1234567890-SyntheticSlackToken",  # gitleaks:allow - synthetic seeded value
    "jwt": "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzeW50aGV0aWMifQ.c3ludGhldGljLXNpZw",  # gitleaks:allow - synthetic seeded value
    "bearer": "Bearer SyntheticBearerValue0000",  # gitleaks:allow - synthetic seeded value
    "pk": "-----BEGIN RSA PRIVATE KEY-----\nSYNTHETICKEYBODYLINE1\n-----END RSA PRIVATE KEY-----",  # gitleaks:allow - synthetic seeded value
    "cred": "password=SyntheticPassw0rd",  # gitleaks:allow - synthetic seeded value
    "email": "synthetic.patient@example.test",
    "ssn": "987-65-4321",
    "phone": "555-555-0142",
    "dob": "DOB: 1979-03-14",
    "mrn": "MRN: 00123456",
}

GIT_ENV = {
    "PATH": os.environ.get("PATH", ""),
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": os.devnull,
}


def reference_python_argv(interpreter):
    """Reference utilities run as production invokes them (-I), with bytecode writes and encoding pinned."""
    return [interpreter, "-I", "-B", "-X", "utf8"]


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    return sha256_bytes(Path(path).read_bytes())


def git(repo, *args, env=None):
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, check=True, env=env or GIT_ENV,
    )
    return result.stdout.decode("utf-8").strip()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def preflight(reference):
    head = git(reference, "rev-parse", "HEAD")
    if head != BASELINE_SHA:
        raise SystemExit(f"reference checkout is at {head}, expected {BASELINE_SHA}")
    dirty = git(reference, "status", "--porcelain", "--ignored")
    if dirty:
        raise SystemExit(f"reference checkout is not clean:\n{dirty}")
    for rel in UTILITIES:
        if not (reference / rel).is_file():
            raise SystemExit(f"reference utility missing: {rel}")


def comparison_to_main(main_repo):
    main_commit = git(main_repo, "rev-parse", MAIN_REF)
    dpes751_commit = git(main_repo, "rev-parse", DPES751_REF)
    main_diff = []
    dpes751_diff = []
    utilities = []
    for rel in UTILITIES:
        base_blob = git(main_repo, "rev-parse", f"{BASELINE_SHA}:{rel}")
        main_blob = git(main_repo, "rev-parse", f"{MAIN_REF}:{rel}")
        dpes751_blob = git(main_repo, "rev-parse", f"{DPES751_REF}:{rel}")
        utilities.append({"path": rel, "git_blob": base_blob})
        if main_blob != base_blob:
            main_diff.append({"path": rel, "main_blob": main_blob})
        if dpes751_blob != base_blob:
            dpes751_diff.append({"path": rel, "dpes751_blob": dpes751_blob})
    return {
        "main_ref": MAIN_REF,
        "main_commit": main_commit,
        "main_commit_equals_baseline": main_commit == BASELINE_SHA,
        "utilities_differing_from_main": main_diff,
        "integration_ref": DPES751_REF,
        "integration_commit": dpes751_commit,
        "utilities_differing_on_integration_ref": dpes751_diff,
        "note": (
            "The baseline is main. Differences on the integration ref are informational and are "
            "not part of the baseline."
        ),
    }, utilities


class Baseline:
    def __init__(self, reference, inputs, out, workdir):
        self.reference = reference
        self.inputs = inputs
        self.out = out
        self.outputs = out / "outputs"
        self.workdir = workdir
        self.steps = []
        self.python = sys.executable
        roots = (
            (reference, "<reference>"),
            (inputs, "<inputs>"),
            (out, "<out>"),
            (workdir, "<tmp>"),
        )
        pairs = []
        for root, token in roots:
            for variant in sorted({str(root), os.path.realpath(root)}):
                pairs.append((variant + "/", token + "/"))
                pairs.append((variant, token))
        pairs.append((self.python, "python3"))
        pairs.sort(key=lambda pair: len(pair[0]), reverse=True)
        self.replacements = [(old.encode("utf-8"), new.encode("utf-8")) for old, new in pairs]

    def normalize(self, data):
        for old, new in self.replacements:
            data = data.replace(old, new)
        return data

    def env(self):
        for name in ("home", "state", "data", "config", "projects"):
            (self.workdir / name).mkdir(parents=True, exist_ok=True)
        return {
            "PATH": os.environ.get("PATH", ""),
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONHASHSEED": "0",
            "PYTHONUTF8": "1",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "FM_HOME": str(self.workdir / "home"),
            "FM_STATE_OVERRIDE": str(self.workdir / "state"),
            "FM_DATA_OVERRIDE": str(self.workdir / "data"),
            "FM_CONFIG_OVERRIDE": str(self.workdir / "config"),
            "FM_PROJECTS_OVERRIDE": str(self.workdir / "projects"),
        }

    def utility(self, rel):
        return str(self.reference / rel)

    def record_step(self, case_dir, name, command_text, stdout, stderr, exit_code, inputs):
        case_dir.mkdir(parents=True, exist_ok=True)
        (case_dir / f"{name}.command").write_bytes(command_text.encode("utf-8") + b"\n")
        (case_dir / f"{name}.stdout").write_bytes(stdout)
        (case_dir / f"{name}.stderr").write_bytes(stderr)
        (case_dir / f"{name}.exit").write_text(f"{exit_code}\n", encoding="utf-8")
        self.steps.append({
            "path": str((case_dir / name).relative_to(self.out)),
            "command": command_text,
            "exit": exit_code,
            "inputs": inputs,
            "stdout_sha256": sha256_bytes(stdout),
            "stderr_sha256": sha256_bytes(stderr),
        })

    def input_record(self, path):
        path = Path(path).resolve()
        for root in (self.out, self.inputs):
            try:
                rel = path.relative_to(root)
            except ValueError:
                continue
            return {str(rel): sha256_file(path)}
        raise SystemExit(f"input outside known roots: {path}")

    def run(self, case_dir, name, argv_tail, inputs, cwd=None, env=None):
        argv = [*reference_python_argv(self.python), *argv_tail]
        proc = subprocess.run(
            argv, cwd=str(cwd or self.reference), env=env or self.env(),
            capture_output=True, check=False,
        )
        command_text = self.normalize(shlex.join(argv).encode("utf-8")).decode("utf-8")
        stdout = self.normalize(proc.stdout)
        stderr = self.normalize(proc.stderr)
        self.record_step(case_dir, name, command_text, stdout, stderr, proc.returncode, inputs)
        return proc.returncode, stdout

    def stage_input(self, case_dir, name, value):
        path = case_dir / "inputs" / name
        write_json(path, value)
        return path


def write_writer_inputs(baseline, case_dir):
    """Three envelopes without seq or prev_hash, derived from the committed accepted envelope."""
    envelope = json.loads((baseline.inputs / ENVELOPE_ACCEPTED_REL).read_text(encoding="utf-8"))
    envelope.pop("seq")
    envelope.pop("prev_hash")
    appends = []
    for index in range(3):
        doc = dict(envelope)
        doc["event_id"] = f"{envelope['event_id'][:-2]}{90 + index}"
        doc["ts"] = f"2026-10-16T02:00:0{index}.000Z"
        appends.append(baseline.stage_input(case_dir, f"append-{index}.json", doc))
    return appends


def write_sanitize_inputs(baseline, case_dir):
    """Closed sanitizer config, a config with an unknown key, and one seeded event."""
    config = baseline.stage_input(case_dir, "sanitize-config.json", {"home_key": HOME_KEY})
    bad_config = baseline.stage_input(
        case_dir, "sanitize-config-extra-key.json", {"home_key": HOME_KEY, "unexpected": True})
    s = SEEDED
    seeded = {
        "event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
        "seq": 0,
        "ts": "2026-10-16T02:00:00.000Z",
        "prev_hash": None,
        "event": "task.status",
        "status": " ".join([s["gh"], s["pat"], s["aws"], s["google"], s["slack"]]),
        "detail": {
            "note": "key block " + s["pk"],
            "auth": " ".join([s["jwt"], s["bearer"]]),
            "nested": {"contact": "reach " + s["email"] + " or " + s["phone"]},
        },
        "notes": [
            "slack " + s["slack"],
            "ssn " + s["ssn"],
            s["dob"] + " " + s["mrn"],
            "cred " + s["cred"],
        ],
        s["gh"]: "value under a secret-bearing key",
        s["mrn"]: "record " + s["mrn"],
    }
    event = baseline.stage_input(case_dir, "sanitize-event-seeded.json", seeded)
    return config, bad_config, event


def run_writer(baseline):
    case_dir = baseline.outputs / "fm-audit-writer" / "chain"
    appends = write_writer_inputs(baseline, case_dir)
    store = baseline.workdir / "store"
    store.mkdir()
    env = baseline.env()
    tool = baseline.utility("bin/fm-audit-writer.py")
    for index, path in enumerate(appends):
        baseline.run(case_dir, f"append-{index}", [tool, "append", "--store", str(store), str(path)],
                     baseline.input_record(path), env=env)
    refused = baseline.inputs / ENVELOPE_SEQ_ONE_REL
    baseline.run(case_dir, "append-refused-seq", [tool, "append", "--store", str(store), str(refused)],
                 baseline.input_record(refused), env=env)
    baseline.run(case_dir, "verify", [tool, "verify", "--store", str(store)], {}, env=env)
    baseline.run(case_dir, "project-ledger", [tool, "project-ledger", "--store", str(store)], {}, env=env)
    for entry in sorted(store.iterdir()):
        if entry.is_file():
            target = case_dir / "store" / entry.name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(entry, target)


def run_envelope(baseline):
    tool = baseline.utility("bin/fm-audit-envelope.py")
    fixtures = sorted((baseline.inputs / AUDIT_VALIDATE_REL).glob("*.json"))
    for path in fixtures:
        definition = "fleet_ledger_v1" if path.name.startswith("ledger-") else "envelope"
        case_dir = baseline.outputs / "fm-audit-envelope" / path.stem
        baseline.run(case_dir, "validate", [tool, "validate", "--definition", definition, str(path)],
                     baseline.input_record(path))


def run_sanitize(baseline):
    case_dir = baseline.outputs / "fm-audit-sanitize" / "seeded"
    config, bad_config, event = write_sanitize_inputs(baseline, case_dir)
    tool = baseline.utility("bin/fm-audit-sanitize.py")
    baseline.run(case_dir, "sanitize", [tool, "sanitize", "--config", str(config), str(event)],
                 {**baseline.input_record(config), **baseline.input_record(event)})
    refused = baseline.outputs / "fm-audit-sanitize" / "config-extra-key"
    baseline.run(refused, "sanitize", [tool, "sanitize", "--config", str(bad_config), str(event)],
                 {**baseline.input_record(bad_config), **baseline.input_record(event)})


def run_activity_map(baseline):
    case_dir = baseline.outputs / "fm-audit-activity-map" / "projection"
    config, _, _ = write_sanitize_inputs(baseline, case_dir)
    tool = baseline.utility("bin/fm-audit-activity-map.py")
    accepted = baseline.inputs / ENVELOPE_ACCEPTED_REL
    baseline.run(case_dir, "map-accepted", [tool, "map", "--config", str(config), str(accepted)],
                 {**baseline.input_record(config), **baseline.input_record(accepted)})
    missing = baseline.inputs / ENVELOPE_MISSING_USER_REL
    baseline.run(case_dir, "map-missing-user", [tool, "map", "--config", str(config), str(missing)],
                 {**baseline.input_record(config), **baseline.input_record(missing)})


def git_commit_base(repo):
    env = {
        **GIT_ENV,
        "GIT_AUTHOR_NAME": "fixture",
        "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
        "GIT_COMMITTER_NAME": "fixture",
        "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        "GIT_AUTHOR_DATE": "2026-10-16T02:00:00Z",
        "GIT_COMMITTER_DATE": "2026-10-16T02:00:00Z",
    }
    subprocess.run(["git", "-c", "init.defaultBranch=main", "init", "-q", str(repo)],
                   check=True, env=env, capture_output=True)
    files = {
        "README.md": "# fixture\n",
        "docs/guide.md": "guide\n",
        "bin/tool.sh": "#!/bin/sh\necho tool\n",
    }
    for rel, text in files.items():
        (repo / rel).parent.mkdir(parents=True, exist_ok=True)
        (repo / rel).write_text(text, encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True, env=env, capture_output=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-q", "-m", "base"], check=True, env=env,
                   capture_output=True)
    return git(repo, "rev-parse", "HEAD", env=env)


def run_reassess(baseline):
    case_dir = baseline.outputs / "fm-assignment-reassess" / "repo"
    repo = baseline.workdir / "reassess-repo"
    base = git_commit_base(repo)
    tool = baseline.utility("bin/fm-assignment-reassess.py")
    env = baseline.env()
    baseline.run(case_dir, "collect-clean", [tool, "collect", "--repo", str(repo), "--base-commit", base],
                 {}, env=env)
    (repo / "README.md").write_text("# fixture changed\n", encoding="utf-8")
    (repo / "docs" / "new.md").write_text("untracked\n", encoding="utf-8")
    baseline.run(case_dir, "collect-dirty", [tool, "collect", "--repo", str(repo), "--base-commit", base],
                 {}, env=env)
    request = {
        "task_id": "dpes-765-demo",
        "assignment_intent": "Reassess the profile from the actual change set.",
        "expected_surfaces": ["README.md", "docs/guide.md", "bin/tool.sh"],
        "pinned_policy_revision": "rev-test-1",
        "target_branch": "main",
        "comparison_base_commit": base,
    }
    high = baseline.stage_input(case_dir, "request-high-risk.json", {
        **request, "risk_triggers": ["security-authorization-secrets-or-sensitive-data"],
    })
    standard = baseline.stage_input(case_dir, "request-no-trigger.json", {**request, "risk_triggers": []})
    baseline.run(case_dir, "reassess-high-risk", [tool, "reassess", "--repo", str(repo), "--request", str(high)],
                 baseline.input_record(high), env=env)
    baseline.run(case_dir, "reassess-no-trigger", [tool, "reassess", "--repo", str(repo), "--request", str(standard)],
                 baseline.input_record(standard), env=env)


def run_assignment_profile(baseline):
    tool = baseline.utility("bin/fm-assignment-profile.py")
    cases_root = baseline.inputs / ASSIGNMENT_CASES_REL
    code_root_base = baseline.inputs / ASSIGNMENT_CODE_ROOT_REL
    matches = {}
    for case in sorted(p for p in cases_root.iterdir() if p.is_dir()):
        spec = json.loads((case / "case.json").read_text(encoding="utf-8"))
        code_root = code_root_base / spec["code_root"]
        record = case / "record.json"
        pins = spec.get("pins") or {}
        argv = [tool, spec["command"], "--code-root", str(code_root), "--record", str(record)]
        if pins.get("task") is not None:
            argv += ["--task", pins["task"]]
        if pins.get("target_branch") is not None:
            argv += ["--target-branch", pins["target_branch"]]
        if pins.get("base_commit") is not None:
            argv += ["--base-commit", pins["base_commit"]]
        if pins.get("require_current"):
            argv.append("--require-current")
        case_dir = baseline.outputs / "fm-assignment-profile" / case.name
        inputs = {**baseline.input_record(record), **baseline.input_record(case / "case.json")}
        for rel in sorted(p for p in code_root.rglob("*") if p.is_file()):
            inputs.update(baseline.input_record(rel))
        returncode, stdout = baseline.run(case_dir, "run", argv, inputs)
        committed = {
            "stdout": (case / "expected.stdout").read_bytes(),
            "stderr": (case / "expected.stderr").read_bytes(),
            "exit": (case / "expected.exit").read_text(encoding="utf-8"),
        }
        step = baseline.steps[-1]
        matches[case.name] = {
            "stdout": step["stdout_sha256"] == sha256_bytes(committed["stdout"]),
            "stderr": step["stderr_sha256"] == sha256_bytes(committed["stderr"]),
            "exit": f"{returncode}\n" == committed["exit"],
        }
    return matches


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def http_get(port, route):
    url = f"http://127.0.0.1:{port}{route}"
    try:
        with urllib.request.urlopen(url, timeout=10) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def run_birds_eye(baseline):
    case_dir = baseline.outputs / "fm-birds-eye-view-serve" / "assets"
    port = free_port()
    argv = [
        *reference_python_argv(baseline.python), baseline.utility("bin/fm-birds-eye-view-serve.py"),
        "--assets", str(baseline.reference / BIRDS_EYE_ASSETS_REL),
        "--model", baseline.utility("bin/fm-birds-eye-model.sh"),
        "--capture", baseline.utility("bin/fm-birds-eye-capture.sh"),
        "--interval", "5",
        "--port", str(port),
    ]
    server = subprocess.Popen(
        argv, cwd=str(baseline.reference), env=baseline.env(),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    try:
        deadline = time.monotonic() + 30
        while True:
            if server.poll() is not None:
                raise SystemExit(f"birds-eye server exited early with {server.returncode}")
            try:
                http_get(port, "/")
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise SystemExit("birds-eye server did not answer within 30 seconds")
                time.sleep(0.2)
        asset_inputs = {
            f"{BIRDS_EYE_ASSETS_REL}/{asset.name}": sha256_file(asset)
            for asset in sorted((baseline.reference / BIRDS_EYE_ASSETS_REL).iterdir())
            if asset.is_file()
        }
        command = ["fm-birds-eye-view-serve.py", "--assets", f"{BIRDS_EYE_ASSETS_REL}",
                   "--model", "bin/fm-birds-eye-model.sh", "--capture", "bin/fm-birds-eye-capture.sh",
                   "--interval", "5", "--port", "<port>"]
        command_text = "python3 -I -B -X utf8 " + " ".join(shlex.quote(part) for part in command)
        for route in BIRDS_EYE_ROUTES:
            status, body = http_get(port, route)
            name = "root" if route == "/" else route.lstrip("/").replace(".", "-")
            body = baseline.normalize(body.replace(str(port).encode("utf-8"), b"<port>"))
            baseline.record_step(
                case_dir, name, f"GET {route} (command: {command_text})",
                body, b"", status, asset_inputs,
            )
    finally:
        try:
            os.killpg(server.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            server.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(server.pid, signal.SIGKILL)
            server.wait()


def tree_hashes(root):
    files = {}
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        files[str(path.relative_to(root.parent))] = {
            "sha256": sha256_file(path),
            "bytes": path.stat().st_size,
        }
    return files


def build_manifest(baseline, reference, main_repo, assignment_matches):
    comparison, utilities = comparison_to_main(main_repo)
    utility_entries = []
    for entry in utilities:
        path = reference / entry["path"]
        utility_entries.append({**entry, "sha256": sha256_file(path)})
    files = tree_hashes(baseline.outputs)
    return {
        "schema": "mcao.pre-port-baseline.v1",
        "owner_decision": (
            "DPES-884 and DPES-957 owner decision (2026-11-01): main commit "
            f"{BASELINE_SHA} is the replacement pre-port Python reference baseline, superseding "
            "the unavailable DPES-803 QA-tag baseline. It asserts no tag recovery, historical "
            "equivalence, or passing parity."
        ),
        "baseline": {
            "commit": BASELINE_SHA,
            "reference_checkout": "detached clean checkout of the baseline commit",
            "utilities": utility_entries,
        },
        "comparison_to_main": comparison,
        "generator": {"path": GENERATOR_REL, "sha256": sha256_file(Path(__file__).resolve())},
        "environment": {
            "python_executable": "python3",
            "python_version": sys.version,
            "python_implementation": platform.python_implementation(),
            "unicodedata_version": unicodedata.unidata_version,
            "platform": platform.platform(),
            "machine": platform.machine(),
            "kernel": f"{os.uname().sysname} {os.uname().release}",
            "git_version": subprocess.run(["git", "--version"], capture_output=True, check=True,
                                          env=GIT_ENV).stdout.decode("utf-8").strip(),
            "invocation": "python3 -I -B -X utf8 <utility> (python -I ignores PYTHON* environment variables)",
        },
        "assignment_profile_matches_committed_dpes751_expected": assignment_matches,
        "steps": baseline.steps,
        "files": files,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--reference", required=True, help="clean checkout at BASELINE_SHA")
    parser.add_argument("--inputs", required=True, help="repository root holding committed inputs")
    parser.add_argument("--main-repo", required=True, help="repository whose origin/main is compared")
    parser.add_argument("--out", required=True, help="directory receiving manifest.json and outputs/")
    args = parser.parse_args(argv)

    reference = Path(args.reference).resolve()
    inputs = Path(args.inputs).resolve()
    main_repo = Path(args.main_repo).resolve()
    out = Path(args.out).resolve()
    preflight(reference)

    workdir = Path(tempfile.mkdtemp(prefix="pre-port-baseline-"))
    try:
        out.mkdir(parents=True, exist_ok=True)
        if (out / "outputs").exists():
            shutil.rmtree(out / "outputs")
        baseline = Baseline(reference, inputs, out, workdir)
        run_writer(baseline)
        run_envelope(baseline)
        run_sanitize(baseline)
        run_activity_map(baseline)
        run_reassess(baseline)
        assignment_matches = run_assignment_profile(baseline)
        run_birds_eye(baseline)
        manifest = build_manifest(baseline, reference, main_repo, assignment_matches)
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n",
                                           encoding="utf-8")
    finally:
        shutil.rmtree(workdir, ignore_errors=True)
    print(f"generated {len(manifest['steps'])} steps and {len(manifest['files'])} files into {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
