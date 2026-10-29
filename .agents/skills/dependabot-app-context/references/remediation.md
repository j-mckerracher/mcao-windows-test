# Human-gated remediation configuration

Read this reference when the caller includes `dependabot-ai-remediation` or the assessment
sets a non-empty `remediation-command`.

## Keep the two paths distinct

The assessment path is automatic and advisory. It reads the Dependabot branch, comments,
and labels, but never changes code. The remediation path starts only after a maintainer
comments the configured command. It creates or updates a companion branch and opens a
companion pull request based on the Dependabot branch; it never changes the Dependabot PR's
own commits and never merges, approves, or reviews.

Use one context file for facts shared by both jobs. Use different `extra-instructions`:

- assessment: risks a reviewer should investigate, including behavioral changes that can
  compile and pass tests;
- remediation: hard constraints on edits, supported runtime, public interfaces, architecture,
  generated/manual boundaries, and the instruction to make the smallest necessary patch.

The remediation agent has no shell. It cannot build, run tests, regenerate lockfiles, or
prove a runtime-risk hypothesis. Tell it to leave ambiguous behavioral work documented
rather than guessing. Compiler errors from the assessment comment may guide it, but the
companion PR's CI validates the combined dependency bump and patch.

## Trigger and caller guard

Use `issue_comment: {types: [created]}`. The remediate job guard must require all of:

- `github.event_name == 'issue_comment'`;
- the issue is a pull request;
- the comment starts with the configured command;
- the pull request author is `dependabot[bot]`.

The reusable workflow separately resolves the target PR, verifies the Dependabot branch,
and checks that the commenter has write, maintain, or admin permission. Keep
`require-write-permission` and `require-dependabot-actor` enabled. The caller guard reduces
cost and blast radius; the reusable checks are the security boundary.

The command passed to assessment `remediation-command` must exactly match the prefix in the
remediate job guard. Do not advertise it when the remediation job is absent.

## Permissions, concurrency, and pinning

Keep root permissions empty and grant the remediation job only:

- `contents: write` for the companion branch;
- `pull-requests: write` for the companion PR and pointer comment;
- `issues: write` for acknowledgement and an optional pending label;
- `copilot-requests: write` for inference with the built-in token.

Use a per-PR remediation concurrency group with `cancel-in-progress: false`; cancellation
after a push can leave a branch without a companion PR. Pin the remediation reusable
workflow to an immutable released SHA. Use the same released revision as assessment when
compatible, but do not silently choose or update a release for the user. Never use `main`
for a write-capable workflow.

## Patch guards

Keep the reusable defaults unless repository evidence and the user's intent justify a
change:

- manifests read-only (`allow-manifest-changes: false`);
- lockfiles, workflows, Dependabot config, CODEOWNERS, and git metadata denied;
- bounded changed-file and patch-line counts;
- comment back to the Dependabot PR enabled.

Use `deny-paths` to add genuine repository boundaries: manually approved API/schema files,
migrations, generated outputs whose sources must be edited instead, release/signing files,
or executable hook directories. Do not deny all source, test, or configuration paths the
agent may legitimately need. Do not increase patch limits merely because a proposed patch
hit a guard; first determine why it sprawled.

Keep the assessment comment marker aligned if either workflow changes its default marker.
Choose a pending label only if repository automation or policy consumes it.

## Credentials and repository prerequisites

No extra credential is required by default. The built-in `GITHUB_TOKEN` creates the
companion PR, whose CI waits for a maintainer to click **Approve workflows to run**. Treat
that approval as the safer default for AI-written code.

The user must verify:

1. Repository Settings → Actions → General allows GitHub Actions to create pull
   requests; otherwise companion PR creation fails.
2. Companion-PR CI can trigger when its base is a `dependabot/**` branch. A workflow filtered
   to `pull_request.branches: [main]` will not run on the companion, regardless of token.
3. Rulesets permit creation/updates of the configured companion branch prefix.
4. The combined dependency update and remediation receive human review; remediation does
   not close runtime risks merely because code compiles.

An optional GitHub App or write PAT makes companion CI start automatically and changes the
actor/audit trail. Configure one only when the user requests that tradeoff. Never invent
secret names, create credentials, or weaken branch protections as part of this skill.

## Consistency check

Before finishing, verify:

- `issue_comment` is present and only the remediate job can run on it;
- assessment/build jobs explicitly require `pull_request_target` when both triggers exist;
- the advertised command and caller guard match;
- write permissions exist only on remediate;
- remediation concurrency never cancels an in-flight publication;
- assessment and remediation use the same context file and compatible comment marker;
- coding constraints are repository-supported and distinct from review instructions;
- deny paths add to, rather than redundantly restate, built-in guards;
- no optional App/PAT secret is active without the user's choice;
- both reusable workflows use immutable released pins;
- remaining repository-setting and CI-trigger prerequisites are reported to the user.
