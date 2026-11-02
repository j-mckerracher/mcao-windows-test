# Pre-port reference baseline fixtures (DPES-957, DPES-884)

These fixtures record what the seven Python reference utilities print at the replacement pre-port
baseline. They are the expected outputs that a future Rust port must match.

## Owner decision

Main commit `d80bcd15f06b79fed639cf028e273306567ffbfd` is the replacement pre-port Python reference
baseline. It supersedes the unavailable DPES-803 QA-tag baseline (Jira DPES-884 and DPES-957 comments,
2026-11-01). The baseline is not a recovery of that tag. Nothing here asserts historical equivalence
or passing parity. No tag was created for it.

The commit is reachable from `origin/main`: `git merge-base --is-ancestor d80bcd15... origin/main`
succeeds, and `origin/main` is `d80bcd15` at generation time.

## Layout

- `generate.py` produces everything in this directory. It is the only generator. It takes a clean
  checkout of the baseline commit as `--reference`, so every expected output is produced by the
  Python utilities at that commit. No Rust code computes an expected value.
- `manifest.json` records the baseline commit, the blob and SHA-256 of each utility, the generator
  SHA-256, the interpreter and Unicode and OS versions, the comparison with `origin/main` and with the
  integration ref, every step's normalized command line, exit status, and input hashes, and the SHA-256
  and size of every file under `outputs/`.
- `outputs/<utility>/<case>/` holds one set of files per step: `<step>.command` (normalized command
  line), `<step>.stdout`, `<step>.stderr`, and `<step>.exit`. A case may also hold `inputs/` (inputs
  written by the generator) and `store/` (the audit store after the writer steps). For the birds-eye
  server, `<step>.exit` holds the HTTP status code.

Committed inputs are read in place and are not copied: audit-validate envelopes under
`crates/mcao-cli/tests/fixtures/audit-validate/` and assignment-profile cases and code roots under
`crates/mcao-core/tests/fixtures/assignment_profile/`. The manifest records the SHA-256 of every input
used. Synthetic sanitizer secrets are built inside `generate.py` at run time, as in
`tests/fm-audit-sanitize.test.sh`.

## Utilities and what each case covers

| Utility | Cases |
| --- | --- |
| `bin/fm-audit-writer.py` | `chain`: three `append` steps, one refused `append` (envelope with `seq`), `verify`, `project-ledger`; `store/` holds the committed segment and checkpoint |
| `bin/fm-audit-envelope.py` | `validate` over all 19 audit-validate JSON fixtures (`envelope` and `fleet_ledger_v1`) |
| `bin/fm-audit-sanitize.py` | `seeded`: sanitize of a seeded event with a closed config; `config-extra-key`: refusal |
| `bin/fm-audit-activity-map.py` | `projection`: map of the accepted envelope; map of an envelope with no resolvable identity |
| `bin/fm-assignment-reassess.py` | `repo`: `collect` on a clean and a dirty synthetic git repository; `reassess` with a high-risk and a no-trigger request |
| `bin/fm-assignment-profile.py` | one case per directory in `assignment_profile/cases/` (52): the exact pins and command each case declares |
| `bin/fm-birds-eye-view-serve.py` | `assets`: `GET /`, `/app.css`, `/app.js` and an unknown route, against the reference server on `127.0.0.1` |

These cases map to DPES-884 AC1 (writer, canonical line bytes in `store/`), AC3 (validate), AC5
(sanitize), AC6 (activity-map), AC7 (assignment reassess), AC8 (the loopback server routes), and
the assignment-profile cases.

## Comparison with the then-current main and the integration ref

- `origin/main` at generation time equals the baseline. None of the seven utilities differs from main.
- `origin/DPES-751` (integration) differs in one utility, `bin/fm-audit-activity-map.py`: blob
  `560f33880c` against `87bf5849`. The change uses `record.get("task_id")` instead of
  `record["task_id"]`. The baseline is main, so this change is not in it. A port of the activity
  projection must decide that difference explicitly.
- The 52 assignment-profile cases give the same stdout, stderr, and exit status at the baseline as the
  committed expected files on the integration branch. This is evidence that those cases do not depend
  on the activity-map difference.

## Generation environment (pinned)

- Host: macOS 26.7.1 (Darwin 25.6.0), arm64.
- Interpreter: CPython 3.14.6, `/opt/homebrew/bin/python3`; `unicodedata` 16.0.0.
- Git 2.53.0.
- Invocation for every utility: `python3 -I -B -X utf8 <utility> ...`. `-I` ignores `PYTHON*`
  environment variables, so `PYTHONHASHSEED` is not pinned. Utility output is compared byte for byte
  across two regenerations, which would expose any hash-order dependence.
- Environment for each step: `FM_HOME`, `FM_STATE_OVERRIDE`, `FM_DATA_OVERRIDE`, `FM_CONFIG_OVERRIDE`,
  and `FM_PROJECTS_OVERRIDE` point at a temporary directory. The git repository for reassess is
  created with fixed author and committer identity and dates, and with global and system git config
  disabled.
- Paths that differ between checkouts are replaced with tokens before hashing: `<reference>`,
  `<inputs>`, `<out>`, `<tmp>`, and `python3`. The birds-eye port is replaced with `<port>`.

## Regeneration and independent verification

```sh
git -C <repo> worktree add --detach <ref-a> d80bcd15f06b79fed639cf028e273306567ffbfd
git -C <repo> worktree add --detach <ref-b> d80bcd15f06b79fed639cf028e273306567ffbfd
python3 -I tests/fixtures/pre-port-baseline/generate.py --reference <ref-a> --inputs <repo> \
  --main-repo <repo> --out tests/fixtures/pre-port-baseline
python3 -I tests/fixtures/pre-port-baseline/generate.py --reference <ref-b> --inputs <repo> \
  --main-repo <repo> --out <tmp-regen>
diff -r tests/fixtures/pre-port-baseline/outputs <tmp-regen>/outputs
cmp tests/fixtures/pre-port-baseline/manifest.json <tmp-regen>/manifest.json
```

The generator refuses a reference that is not at the baseline commit or is not clean. A regeneration
that differs in any byte fails the diff. Both steps must pass before a fixture is accepted.

Result of the run recorded with this change: the two regenerations were byte-identical across all
370 files under `outputs/` and in `manifest.json`. Both reference checkouts stayed clean with no
ignored files.

## Not covered (residual)

- No Rust parity test uses these fixtures. The DPES-957 and DPES-884 differential and golden tests
  remain open and are not part of this change.
- The seven utilities are not compared with the DPES-803 QA tag, which is not available. Any
  equivalence to that tag remains unverified.
- Python 3.12 and 3.13 are not covered. Their Unicode tables differ from 3.14 (DPES-965), so the
  redaction outputs here are specific to 3.14.6 and Unicode 16.0.
- Rejection records written by `fm-audit-envelope.py` and `fm-audit-writer.py` to the state directory
  carry a wall-clock time. Only the stdout and exit status of those steps are fixed.
- The birds-eye server's `/api/view` and `/api/capture` are not captured. They depend on a live
  collector cycle and on process state, so their output is not stable. The server's own stdout and
  stderr are not captured.
- The integration-branch activity-map difference is recorded above but not evaluated.
