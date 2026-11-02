"""Writes cases.json and the Python reference output for every prose case.

Run from any directory:
  python3 -I -B crates/mcao-core/tests/fixtures/assignment-reassess-prose/generate.py <ref-root>
where <ref-root> is an extracted tree of main d80bcd15 (git -C <repo> archive d80bcd15 | tar -x -C <ref-root>).
Each case builds a repository with fixed identity and dates, runs
`python3 -I -B -X utf8 <ref-root>/bin/fm-assignment-reassess.py reassess --repo <repo> --request <file>`
with only the generator environment, and writes stdout to <case>.json. The Rust replay in
tests/assignment_reassess_prose.rs rebuilds the same repositories and compares byte for byte.
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
GUIDE = "# Guide\n\nIntro line.\n\n```sh\necho one\n```\n\nClosing text.\n"
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


def request(surfaces, triggers=(), conditions=None):
    body = {
        "task_id": "dpes-884",
        "pinned_policy_revision": "rev-1",
        "target_branch": "DPES-751",
        "comparison_base_commit": "$BASE",
        "assignment_intent": "port the prose line checks",
        "expected_surfaces": list(surfaces),
        "risk_triggers": list(triggers),
    }
    if conditions is not None:
        body["small_low_risk_conditions"] = conditions
    return body


def text(path, content):
    return {"path": path, "text": content}


EDIT_INTRO = GUIDE.replace("Intro line.", "Intro line, edited.")
CASES = [
    {"name": "prose-clean-edit", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", EDIT_INTRO)],
     "request": request(["docs/guide.md"])},
    {"name": "prose-fenced-edit-with-risk-trigger", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("echo one", "echo two"))],
     "request": request(["docs/guide.md"], ["breaking-public-contract"])},
    {"name": "prose-clean-edit-with-risk-trigger", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", EDIT_INTRO)],
     "request": request(["docs/guide.md"], ["breaking-public-contract"])},
    {"name": "prose-fenced-edit-standard", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("echo one", "echo two"))],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-fenced-edit-small-low-risk", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("echo one", "echo two"))],
     "request": request(["docs/guide.md"], [], ALL_TRUE)},
    {"name": "prose-edit-after-closing-fence", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("Closing text.", "Closing text, edited."))],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-fence-opened-by-edit", "base": [text("docs/guide.md", "Intro\n")],
     "worktree": [text("docs/guide.md", "Intro\n\n```\nnew block\n```\n")],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-blank-lines-added", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("Intro line.\n", "Intro line.\n\n\n"))],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-trailing-whitespace", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("Closing text.", "Closing text.   "))],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-crlf-only", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("\n", "\r\n"))],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-unicode-edit", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", GUIDE.replace("Intro line.", "Intro caf\u00e9 \u2615 \u65e5\u672c."))],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-undecodable-worktree", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [{"path": "docs/guide.md", "hex": "ff0a"}],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-and-code-small-low-risk", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/guide.md", EDIT_INTRO), text("scripts/x.sh", "#!/bin/sh\n")],
     "request": request(["docs/guide.md", "scripts/x.sh"], [], ALL_TRUE)},
    {"name": "prose-deleted-fenced-file", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [{"path": "docs/guide.md", "delete": True}],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-very-long-line", "base": [text("docs/guide.md", "a" * 10000 + "\n")],
     "worktree": [text("docs/guide.md", "b" * 10000 + "\n")],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-indented-fence", "base": [text("docs/guide.md", "   ```\nx\n   ```\n")],
     "worktree": [text("docs/guide.md", "   ```\ny\n   ```\n")],
     "request": request(["docs/guide.md"], [])},
    {"name": "prose-mixed-clean-and-fenced", "base": [text("docs/a.md", "one\n"), text("docs/b.md", GUIDE)],
     "worktree": [text("docs/a.md", "one edited\n"), text("docs/b.md", GUIDE.replace("echo one", "echo two"))],
     "request": request(["docs/a.md", "docs/b.md"], [])},
    {"name": "prose-untracked-new-file", "base": [text("docs/guide.md", GUIDE)],
     "worktree": [text("docs/new.md", "New page.\n")],
     "request": request(["docs/new.md"], [])},
]


def inventory(paths):
    return text(
        "docs/documentation-audiences.json",
        json.dumps(
            {"surfaces": [{"path": path, "audience": "public-product"} for path in paths]},
            indent=2,
        )
        + "\n",
    )


INVENTORY = inventory(("docs/guide.md", "docs/a.md", "docs/b.md", "docs/new.md"))
for _case in CASES:
    _case["base"] = [INVENTORY, *_case["base"]]

# One case per surface name, each with an inventory that lists only that path as public-product, so
# the name alone decides whether the change is prose. The case names are file names, the paths are not.
PATH_NAMES = (
    ("path-dot-md", ".md"),
    ("path-dotdot-md", "..md"),
    ("path-triple-dot-md", "...md"),
    ("path-dotdot-upper-md", "..MD"),
    ("path-subdir-dotdot-md", "x/..md"),
    ("path-dotdot-sh", "..sh"),
    ("path-md", "md"),
    ("path-readme", "README"),
    ("path-dotted-stem-md", "a.b.md"),
    ("path-dir-dot-md-file", "dir.md/file"),
    ("path-trailing-dot", "a."),
    ("path-trailing-dot-in-dir", "docs/guide.md."),
    ("path-hidden-dot-md", ".hidden.md"),
    ("path-upper-md", "UPPER.MD"),
    ("path-unicode-md", "caf\u00e9.md"),
    ("path-unicode-upper-md", "docs/\u00fcber.MD"),
    ("path-unicode-leading-dots-md", "..r\u00e9sum\u00e9.md"),
    ("path-unicode-dotdot-only", "..\u00e9"),
)
CASES.extend(
    {"name": name, "base": [inventory([path]), text(path, GUIDE)],
     "worktree": [text(path, EDIT_INTRO)], "request": request([path], [])}
    for name, path in PATH_NAMES
)
ENV_KEYS = {
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_AUTHOR_NAME": "fixture",
    "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
    "GIT_COMMITTER_NAME": "fixture",
    "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
    "GIT_AUTHOR_DATE": "2026-10-16T02:00:00Z",
    "GIT_COMMITTER_DATE": "2026-10-16T02:00:00Z",
}


def child_env():
    env = dict(ENV_KEYS)
    env["PATH"] = os.environ["PATH"]
    return env


def write_entry(root, entry):
    target = root / entry["path"]
    if entry.get("delete"):
        target.unlink()
        return
    target.parent.mkdir(parents=True, exist_ok=True)
    data = bytes.fromhex(entry["hex"]) if "hex" in entry else entry["text"].encode("utf-8")
    target.write_bytes(data)


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], env=child_env(), check=True,
                          capture_output=True).stdout.decode("utf-8").strip()


def run_case(ref_root, scratch, case):
    repo = scratch / case["name"]
    subprocess.run(["git", "-c", "init.defaultBranch=main", "init", "-q", str(repo)],
                   env=child_env(), check=True)
    for entry in case["base"]:
        write_entry(repo, entry)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    base = git(repo, "rev-parse", "HEAD")
    for entry in case["worktree"]:
        write_entry(repo, entry)
    body = json.dumps(case["request"]).replace("$BASE", base)
    request_file = scratch / f"{case['name']}.request.json"
    request_file.write_text(body, encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, "-I", "-B", "-X", "utf8", str(ref_root / "bin" / "fm-assignment-reassess.py"),
         "reassess", "--repo", str(repo), "--request", str(request_file)],
        env=child_env(), capture_output=True, check=False,
    )
    if proc.returncode != 0 or proc.stderr:
        raise SystemExit(f"{case['name']}: exit {proc.returncode}: {proc.stderr.decode('utf-8')}")
    return proc.stdout


def main():
    ref_root = pathlib.Path(sys.argv[1]).resolve()
    (HERE / "cases.json").write_text(json.dumps(CASES, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="prose-cases-"))
    try:
        for case in CASES:
            (HERE / f"{case['name']}.json").write_bytes(run_case(ref_root, scratch, case))
            print(case["name"], "written")
    finally:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    main()
