# Mutation dispositions

A disposition record states that one surviving mutant is equivalent, or that one excluded path has no workable mutation surface.
`bin/fm-mutation-dispositions.sh` reads these records and prints the raw counts and the adjusted counts side by side.
The consumer is report-only.
`bin/fm-mutation-verdict.sh` and the Mutation Verdict check do not read its output, so no merge is blocked or allowed by it.

## Trust basis

A record counts only after it is on `origin/main`, because the consumer reads records from that ref's objects and never from the working tree or a pull request branch.
A record reaches `origin/main` only through a human merge of a pull request under the default-branch ruleset.
That merge is the whole trust basis, and it is merge-trust, not cryptographic proof.
The `signer` field is a text claim.
Nothing in the record, its digests, or the consumer proves who wrote or approved it.
Anyone able to land a commit on the default branch can write a record that names the sign-off owner.
Do not treat a record as unforgeable.
Its protection is the review and merge that put it on the default branch, and the residual risks below apply to that protection.

## Residual risks

These risks are known and are not fixed by this design.

- A pull request can merge without an approving review, so a merge may have had no human review.
- The identity that agents use to open and merge pull requests is a repository admin and a member of the code-owner team.
- A pull request can change the default-branch ruleset or the workflow files, so the gate can be edited in the same change as the record it protects.
- Auto-merge is enabled for the repository, so a pull request can land as soon as its checks pass.
- The digests show that the source bytes and the tool rows match at the time the consumer runs.
- The digests do not show that the evidence was produced correctly.

## Record format

A record is a regular file named `<name>.disposition` in `docs/mutation-dispositions/` on the ref, stored with git mode 100644.
Other files in that directory are ignored.
A record is `key=value` lines, one key per line, with no blank lines, no repeated keys, and no unknown keys.
Every key listed for the record's kind must be present, even when its value is empty.

| Key | Required for | Rule |
| --- | --- | --- |
| `format` | every record | `fm-mutation-disposition/1` |
| `kind` | every record | `equivalent-mutant` or `unsupported-surface` |
| `status` | every record | `pending`, `signed`, or `lapsed` |
| `source_path` | every record | Repository-relative path, not absolute, with no `..` |
| `source_sha256` | every record | Lowercase hex sha256 of that file in the measured tree |
| `tool_versions_sha256` | every record | Lowercase hex sha256 of the evidence record's tool rows |
| `rationale` | every record | Non-empty reason the claim holds |
| `evidence_command` | every record | Non-empty command that reproduces the evidence |
| `evidence_result` | every record | Non-empty result that the command produced |
| `signer` | every record | Empty when `pending`; otherwise equal to the sign-off owner named by `fm_mutation_equivalence_signoff_owner` in `bin/fm-mutation-surface-lib.sh` |
| `signed_on` | every record | Empty when `pending`; otherwise a `YYYY-MM-DD` date |
| `mutant_id` | `equivalent-mutant` | `L<line>-<operator>` |
| `operator` | `equivalent-mutant` | The operator part of `mutant_id`, which must agree with it |
| `surface` | `unsupported-surface` | The surface name in the runner's exclusion row for the path |

An `equivalent-mutant` record must not carry `surface`.
An `unsupported-surface` record must not carry `mutant_id` or `operator`.

### Statuses

- `pending` is a claim that is not signed yet, so it adjusts nothing.
- A `pending` record must leave `signer` and `signed_on` empty, and a record with either one set is malformed.
- `signed` is a claim the sign-off owner has signed, and it adjusts only when every check in the outcome table passes.
- `lapsed` is a claim that is no longer valid, so it adjusts nothing.

### Digests

Compute `source_sha256` from the root of the measured tree:

```sh
shasum -a 256 <source_path>
```

Take the first field of the output.
Use `sha256sum` where `shasum` is not installed.

Compute `tool_versions_sha256` from the evidence record the runner wrote:

```sh
awk -F '\t' '$1 == "tool"' <evidence> | shasum -a 256
```

A change to the source file or to any tool row makes the matching claim lapse.

## Consumer

```sh
git fetch origin main
bin/fm-mutation-dispositions.sh --root <measured-tree> <evidence>
```

The default ref is the remote-tracking `origin/main` as it was last fetched, so fetch before reading.

| Option | Default | Meaning |
| --- | --- | --- |
| `--root` | `.` | The work tree whose source files the digests are checked against |
| `--ref` | `origin/main` | The git ref the records are read from |
| `--records` | `docs/mutation-dispositions` | The records directory inside the ref; it must be relative and inside the ref |

The output has one `records` line, one `raw` line, one `adjusted` line, one `claims` line, and one line per record file in name order:

```text
records   ref=<ref> dir=<dir> files=<n>
raw       killed=N survived=N timeout=N error=N unresolved=N excluded=N
adjusted  killed=N survived=N timeout=N error=N unresolved=N excluded=N equivalent-removed=N unsupported-signed=N
claims    applied=N pending=N lapsed=N unsigned=N malformed=N unmatched=N not-survivor=N duplicate=N
record <file>: <outcome>: <reason>
```

The adjusted `survived` count is the raw `survived` count minus `equivalent-removed`.
A record never changes `killed`, `timeout`, `error`, `unresolved`, or `excluded`.
`unsupported-signed` is shown beside `excluded` and never changes it.

Exit codes: 0 when the output is printed, including when claims are malformed, unmatched, or not adjusting; 1 when the evidence record, the ref, or a record cannot be read, or when signed equivalent claims exceed the survived count; 2 for a usage error.

## Outcomes

| Outcome | Meaning | Effect |
| --- | --- | --- |
| `applied` | Signed by the sign-off owner, matched to the evidence, and the source and configuration digests match | An equivalent claim removes one survived mutant; an unsupported claim is counted as `unsupported-signed` |
| `pending` | Not signed yet | Nothing |
| `lapsed` | Recorded as `lapsed`, or the source file changed or is missing, or the tool rows changed | Nothing |
| `unsigned` | The signer is not the sign-off owner | Nothing |
| `malformed` | Fails a format rule, or is not a regular file in the ref | Nothing |
| `unmatched` | No mutant row or exclusion row matches, or the exclusion row's surface differs | Nothing |
| `not-survivor` | The runner recorded the mutant as killed, timeout, error, or anything other than survived | Nothing; a timeout or error is never reclassified |
| `duplicate` | The same mutant or path was already applied from an earlier record in name order | Nothing |

An unsupported-surface record matches through the exclusion row's surface, using the same format, status, signer, and digest rules as an equivalent-mutant record.

## Out of scope

No tooling in this repository signs a record, and no signature tooling is planned here.
`docs/mutation-promotion-criteria.md` stays a blank template with no numbers; this consumer does not read it and sets no threshold.
This consumer does not change `bin/fm-mutation-verdict.sh`, the Mutation Verdict check, or any merge gate.
