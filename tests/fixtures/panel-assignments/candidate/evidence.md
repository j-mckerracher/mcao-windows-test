# Candidate panel-assignment evidence

This file records command results for DPES-781 AC8 and T9 on the candidate fixtures in this directory.
It shows that the fixtures parse and seat through the landed resolver.
It does not prove that a panel ran or that any seat completed.

## Status

The fixtures are approved as authored on 2026-10-20 in the DPES-762 policy owner and DPES-764 mapping owner roles.
That approval covers the fixture files as written and nothing else.
DPES-762, DPES-764, DPES-765, and DPES-769 remain Draft.
The per-class mapping is derived and remains BLOCKING in DPES-764.
The model ids in `model-capabilities.md` are placeholders, so the resolution results below say nothing about any real model.

## Commit anchors

- Base commit for these checks: 012b86bcee3e18c566068aa6b0d4bd98a7bbacb8, committed 2026-10-20T14:08:19-06:00.
- Fixture landing commit: 0d2c6b8fc2e51a8f59454a362826f263c767971f, committed 2026-10-20T13:50:51-06:00 (DPES-781 candidate fixtures, PR #110).
- The diff of `model-capabilities.md` and `small-low-risk/panel-request.tsv` between 0d2c6b8 and 012b86b is empty, so the capability source and the selected request are unchanged since landing.

## Command results

Each command ran from the repository root.
The captured output for each command is in `evidence-output.txt` beside this file, byte for byte, in command order.
Each section of that file starts with a `=== output N of 7 ===` header line, which is not part of the captured output.

Capability source validation:

```text
bash bin/fm-capability-schema.sh validate tests/fixtures/panel-assignments/candidate/model-capabilities.md
```

Captured output: section 1 of `evidence-output.txt`.

Capability lookup for one placeholder model:

```text
bash bin/fm-capability-schema.sh lookup tests/fixtures/panel-assignments/candidate/model-capabilities.md placeholder-model-a
```

Captured output: section 2 of `evidence-output.txt`.

Resolution of the one panel request in the fixtures (`small-low-risk`):

```text
bash bin/fm-panel-resolve.sh resolve tests/fixtures/panel-assignments/candidate/model-capabilities.md tests/fixtures/panel-assignments/candidate/small-low-risk/panel-request.tsv
```

Captured output: section 3 of `evidence-output.txt`.

Refusal of a zero-seat request, using an empty request file outside the repository:

```text
bash bin/fm-panel-resolve.sh resolve tests/fixtures/panel-assignments/candidate/model-capabilities.md <empty-request-file>
```

Captured output: section 4 of `evidence-output.txt`.

Validation of an unavailable copy of the capability source, made by `sed 's/available: true/available: false/'` in a scratch directory:

```text
bash bin/fm-capability-schema.sh validate <unavailable-copy>
```

Captured output: section 5 of `evidence-output.txt`.

Resolution of the selected request against that unavailable copy:

```text
bash bin/fm-panel-resolve.sh resolve <unavailable-copy> tests/fixtures/panel-assignments/candidate/small-low-risk/panel-request.tsv
```

Captured output: section 6 of `evidence-output.txt`.

The resolver exits 0 for an unavailable capability source and reports each seat as unresolved with a reason.
The T9 assertion therefore checks the seat count, the seated roles, and the unresolved reason, and it does not rely on the exit code.
An earlier suite run checked only the exit code for this case, which would have passed vacuously; that assertion was replaced before the final run.

Suite run:

```text
bash bin/fm-test-run.sh tests/fm-panel-assignments.test.sh
```

Captured output: section 7 of `evidence-output.txt`.

## Fixture coverage

- `documentation-only`: no panel request, because the resolver refuses a zero-seat request.
- `small-low-risk`: one request with two seats (a writer candidate and a reviewer judge of the writer); resolved above and seated by the T9 assertion.
- `standard`: no panel request, because no retrieved text names panel roles for this class.
- `high-risk-broad`: no panel request, because the required rigor is undefined in retrieved text.
- `escalation-authorization-one-line`: no panel request and no reassessment run, because `bin/fm-assignment-reassess.py` needs concrete paths, host pins, and classifier inputs that the fixture does not contain.

## Checks run

- `bin/fm-lint.sh tests/fm-panel-assignments.test.sh`: exit 0.
- `bin/fm-doc-audience-check.sh`: the three Markdown files in this directory are classified as `maintainer-verification` in `docs/documentation-audiences.json`.

## Still open for AC8 and T9

- The per-class mapping is BLOCKING in DPES-764, so the selected seat roles are not an approved mapping.
- The eligibility criteria for `small-low-risk`, `standard`, and `high-risk-broad` are not defined in any retrieved issue.
- The `documentation-only` wording differs between two retrievals (DPES-761 epic and DPES-762 section 5), so its authoritative source needs confirming.
- The escalation example is not exercised by any run, and the DPES-765 owner has not approved it.
- No real model is resolved, because every model id is a placeholder.
- No panel has run and no seat has completed.
- The DPES-769 approved profile fixtures are not on main, so the brief's T9 reference to them is not met.
