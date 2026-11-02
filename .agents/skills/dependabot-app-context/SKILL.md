---
name: dependabot-app-context
description: Configure this repository's Dependabot AI assessment and human-gated remediation caller by inferring application context, review and coding constraints, scan paths, protected paths, and safe caller-owned build/test evidence. Use when setting up a new repo from the template or refreshing it after the application or CI changes.
user-invocable: true
allowed-tools: Read Glob Grep Edit Write AskUserQuestion Bash(actionlint*)
---

# Configure Dependabot AI

Configure `.github/workflows/dependabot-ai-impact.yml` from evidence in the current
repository. It contains an automatic read-only assessment path and, when present, a
human-gated remediation path that writes code only to a companion branch and PR. Do not
assume a language, framework, build tool, source layout, runtime version, test reporter, or
protected path from an example.

Do not run remediation, merge, approve, push, change repository settings, or modify
application code. This skill only configures the caller workflow and
`.github/ai-app-context.md`.

## Required files

Read `.github/workflows/dependabot-ai-impact.yml` and `.github/ai-app-context.md`. If the
workflow is absent, stop and point the user to
`mc-innersource/mc-ci-reusable-workflows`. If the context file is absent, create it and
replace an inline `app-context` with `app-context-file: .github/ai-app-context.md`. Do not
leave both: the file input wins and a stale inline block misleads maintainers.

Treat an existing real configuration as a refresh. Preserve accurate constraints and any
working build details unless repository evidence shows they changed.

If the workflow advertises `remediation-command`, require a matching `issue_comment`
trigger and `remediate` job. If remediation is intentionally absent, remove the advertised
command. Never leave a sticky-comment command that does nothing.

## Invocation outcome

When called against a repository created from this template, complete this configuration as
one coherent edit:

1. Replace the placeholder prose in `.github/ai-app-context.md` with repository facts.
2. Add or refresh assessment `extra-instructions` and, when useful, `usage-scan-paths`.
3. If build evidence is trustworthy, convert the commented `build:` scaffold into an
   active repository-specific job and enable all matching assessment wiring. If it is not
   trustworthy, leave both the scaffold and evidence inputs commented.
4. Add or refresh remediation `extra-instructions` and repository-specific `deny-paths`.
5. Preserve the assessment/remediation trigger, permission, concurrency, command-matching,
   and patch-guard invariants.
6. Validate the resulting workflow and report configuration that still requires a human or
   repository-setting choice.

Do not stop after filling only the context file. Conversely, do not activate template
placeholders merely to make every feature appear enabled. Every active runner, command,
path, version, reporter, instruction, and deny pattern must have repository evidence or a
user answer behind it.

Do not select or change the reusable-workflow release. Preserve both pinned `uses:` lines and
verify they identify the same compatible released revision. Report any missing, mutable, or
mismatched pin instead of changing it.

## Infer the application, not a template example

Inspect the files that actually exist:

- Manifests and toolchain pins: package/build manifests, lockfiles, wrapper configuration,
  runtime-version files, container base images, and workspace or multi-module definitions.
- Existing CI and developer entrypoints: inspect workflows, Makefiles, task runners, and
  documented commands. Prefer a command already used by CI over one merely conventional for
  a manifest, but verify its runner and credentials are safe for untrusted PR-head code.
- Architecture: read the entry point and representative primary source. Look for async or
  reactive execution, native/AOT builds, worker/event-loop constraints, generated code,
  private registries, special profiles, and startup-sensitive deployment.
- Deployment and contracts: inspect containers, deployment workflows, Helm/Kubernetes,
  Terraform, serverless configuration, API schemas, protocols, migrations, and publishing
  metadata.
- Dependency risks: identify runtime limits, hand-pinned versions, public contracts, and
  failure modes that could compile successfully but change behavior.

For a monorepo or multi-module build, determine whether the existing CI safely builds the
whole repository or whether jobs must be selected by changed ecosystem/path. Do not build an
arbitrary first manifest. If selection cannot be made from default-branch workflow data,
ask the user or leave build evidence off.

Record exact versions only when the repository establishes them. Do not invent SLAs,
ownership, deployment targets, commands, test coverage, or organizational constraints.

Ask at most two or three questions, and only when the missing answer materially affects the
configuration and cannot be inferred. A private-registry credential requirement, ambiguous
choice between multiple application modules, or undocumented downstream contract can
qualify. Skip questions when repository evidence is sufficient.

## Write the model context

Replace the placeholder in `.github/ai-app-context.md` with concise, plain text describing:

- runtime/framework, build tool, and exact supported versions;
- what the application does;
- architectural constraints relevant to dependency updates;
- primary source/test layout and the test framework, when established;
- deployment target and runtime/base-image bounds;
- public API, protocol, schema, database, or library-consumer contracts;
- noteworthy hand-pinned dependencies and failure-sensitive integrations or adapters.

Aim for roughly 8–20 lines, but prefer a shorter accurate description over padding. This
file is facts and constraints shared by assessment workflows, not a dependency inventory
or marketing description. Do not include credentials, secret values, internal tokens, or
environment-variable contents.

Put assessment-only emphasis in `extra-instructions` under the caller's `with:` block, not
in the shared context file. Use short directions such as risks to pay particular attention
to, grounded in repository evidence. Omit the input if there is nothing useful to add.

Put code-changing constraints in the remediation job's separate `extra-instructions`.
These are not the same prose: assessment instructions direct a reviewer toward risks;
remediation instructions bound what an agent may edit. Keep shared facts in the context
file so the two jobs cannot drift.

Set `usage-scan-paths` only when narrowing the default `.` materially reduces noise without
hiding relevant source, manifests, generated-code inputs, or configuration. Derive paths
from the repository; never default blindly to `src`, `lib`, or another conventional name.

### Assessment field mapping

Under `jobs.assess.with`:

- keep `app-context-file: .github/ai-app-context.md`;
- insert `extra-instructions: |` after it when repository evidence supports concrete risks;
- set `usage-scan-paths` to a space-separated string only when a narrower scan is complete;
- preserve `fail-on-risk: never` unless the user explicitly chooses a model-risk gate;
- preserve `remediation-command: /apply-fixes` while the matching remediation job exists;
- leave model, CLI version, labels, markers, and token fallback at their defaults unless the
  repository or user has deliberately configured them.

Assessment instructions should name observable failure modes and where they matter. Useful
categories include a runtime version increase, protocol/API drift, DI or auto-configuration
changes, serialization/default changes, concurrency violations, generated-code changes, and
observability failures that tests may miss. Include only categories supported by the code
or deployment. Do not paste the context file back into `extra-instructions`.

## Configure build and test evidence when trustworthy

Add or refresh a separate `build` job only when the repository reveals a credible setup,
compile/build command, and application module. If it does not, leave static assessment
working and tell the user what information is missing. Do not guess merely to enable the
feature.

Before editing the build scaffold, read
[references/build-test-evidence.md](references/build-test-evidence.md). It contains the
status contract, command-selection rules, ecosystem clues, baseline requirements, and
artifact-layout checks needed for this mode.

The template scaffold is executable YAML stored as comments. When enabling it:

- uncomment and customize the entire `build:` job, not isolated steps;
- replace both angle-bracket placeholders and the JUnit-copy comment with real values;
- add repository-specific toolchain/cache/setup steps after checkout;
- remove superseded instructional comments that read like unconfigured placeholders;
- uncomment `needs: build`, use the shown `always()` assessment guard, and remove the old
  one-line guard;
- uncomment all five evidence inputs as a unit, including
  `fail-on-build-failure: false`;
- leave no active or commented `<infer ...>` / `<the repository-specific ...>` placeholder
  inside the configured job.

If an active build job already exists, refresh it in place instead of rebuilding it from the
commented example. If both an active job and the old scaffold remain, preserve the active
configuration and remove the redundant scaffold once all its safety mechanics are present.

When leaving build evidence disabled, do not uncomment `needs`, `always()`, artifact names,
or status inputs. Static assessment must remain valid and the handoff must explain why no
safe, meaningful build could be inferred.

The build job executes code from the pull-request head. Preserve these security invariants:

- Give it only `permissions: contents: read`; do not pass secrets or grant comment, label,
  push, package-write, or `copilot-requests` access.
- Check out `github.event.pull_request.head.sha` with `persist-credentials: false`. Use full
  history when a baseline build needs the base commit.
- Keep it separate from the privileged reusable-workflow call. The assessment job only
  downloads and reads its artifact.
- Use the repository's own wrapper and toolchain pin where available. Match existing action
  pinning conventions; prefer immutable action SHAs with readable release comments.
- Set a finite timeout informed by ordinary CI duration so a hung installer, test, or
  dependency hook cannot occupy the runner indefinitely.
- Never weaken compilation or tests to obtain a green result, and never make a private
  credential available to PR-head code.
- Do not copy a persistent or privileged self-hosted runner from ordinary CI without
  explicit evidence that it is isolated for untrusted pull-request builds. Prefer an
  ephemeral hosted runner when compatible; otherwise ask rather than silently expanding
  the attack surface.

Shape the job around the repository rather than forcing a universal script:

1. Expose `build-status`, `build-command`, `test-status`, and `baseline-status` as job
   outputs. Use the reusable workflow's documented values: build is `success`, `failure`,
   `could-not-build`, or `not-run`; tests are `passed`, `failed`, or `not-run`; baseline is
   `success`, `failure`, or `not-run`.
2. Record the exact compile/build command. Capture its full output in `build.log`, print a
   bounded tail for the Actions log, record failure as an output, and let the step finish
   successfully so assessment still runs.
3. If the build succeeds and a reliable test command exists, run it and append its output
   to `build.log`. Capture JUnit XML already produced by the project. Enable an existing
   reporter when straightforward, but do not add an application dependency solely for this
   workflow. A test status without XML is still useful but provides no coverage inventory.
4. If the head build fails, run the same build at the base SHA when that can be done with
   the same setup. Record whether the failure is introduced or pre-existing. Do not run a
   baseline with a different command or toolchain.
5. Under `if: always()`, collect `build.log` at the artifact root and any JUnit XML below
   `test-results/`, then upload one PR-numbered artifact. Missing reports must not make
   evidence collection fail. Preserve relative directories when report names can collide.

When the build job exists, update `assess` together with it:

- add `needs: build`;
- make its guard start with `always()` so a failed or skipped build cannot suppress the
  assessment;
- pass `build-artifact`, `build-status`, `build-command`, `baseline-status`, and
  `test-status` from `needs.build.outputs`;
- leave `fail-on-build-failure: false` unless the user explicitly wants compiler failures
  to gate dependency PRs.

Keep test failures advisory in this assessment job unless the repository already has a
separate required test check or the user asks to gate on them. `fail-on-build-failure` does
not gate on tests. Do not quietly turn a previously advisory Dependabot workflow into a
required test gate.

If refreshing a workflow whose build can no longer be trusted, remove the stale build job,
`needs`, and build-evidence inputs together rather than reporting misleading evidence.

## Configure human-gated remediation

When the caller includes remediation or advertises `/apply-fixes`, read
[references/remediation.md](references/remediation.md) and verify the whole write path. The
command in assessment input and the `startsWith` caller guard must match exactly.

Infer remediation `extra-instructions` from supported runtime versions, architectural
invariants, public interfaces, code-generation boundaries, and repository conventions.
Require the minimum update-related change and reject unrelated refactoring. Do not claim the
agent can compile or test its patch: it has no shell, and companion-PR CI is the validator.

Replace the template's generic two-line remediation block with a concise constraint block
that retains its minimum-change/no-unrelated-refactor intent and adds only inferred rules.
Examples of rule categories—not fixed wording—are: stay within the supported runtime,
preserve an async/reactive execution model, do not change a named public contract, edit the
source of generated output rather than the output, and follow the repository's established
module/export/API conventions.

Add `deny-paths` only for repository-specific files the agent must never change, such as a
manually governed contract, migration directory, generated source, or executable hook. The
reusable workflow already denies workflows, Dependabot configuration, CODEOWNERS, lockfiles,
and git metadata. Do not duplicate that list, deny ordinary source broadly, or enable
manifest changes without an explicit reason and user authorization.

If protected paths are found, uncomment `deny-paths` and replace its example value with one
space-separated, quoted pattern string. If none are justified, keep the line commented and
remove no built-in protections. Keep `allow-manifest-changes`, `require-write-permission`,
`require-dependabot-actor`, and `comment-on-dependabot-pr` explicitly set to their safe
values in the template.

Preserve `require-write-permission: true`, the four-part caller guard, job-scoped write
permissions, and `cancel-in-progress: false`. Do not add an App or PAT credential merely to
avoid the default CI approval click; that is a separate security/operations choice for the
user.

## Verify and report

Run:

```bash
actionlint -ignore 'unknown permission scope "copilot-requests"' .github/workflows/dependabot-ai-impact.yml
```

Fix errors caused by the edit. Then show the user:

- the application context and assessment-specific risks written;
- the remediation constraints and protected paths written;
- the files that support those inferences;
- the inferred build/test commands and report paths, or why build evidence was left off;
- remediation prerequisites the user must verify in repository settings and companion-PR
  CI branch filters;
- any remaining `TODO(new repo)`, especially the reusable-workflow SHA, which this skill
  must not silently choose without a requested release/pinning decision.

Before reporting success, search the active workflow for template residue. `TODO(new repo)`
is allowed only for unresolved setup such as the two release SHA pins, and the commented
build placeholders are allowed only while build evidence remains disabled. No activated
build job may retain angle-bracket placeholders, example deny paths, invented commands, or
an assessment/remediation command mismatch.
