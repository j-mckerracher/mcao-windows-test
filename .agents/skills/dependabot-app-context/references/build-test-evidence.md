# Build and test evidence configuration

Read this reference whenever adding or refreshing the caller-owned build job in
`.github/workflows/dependabot-ai-impact.yml`.

## Evidence hierarchy

Choose configuration from the strongest available source, in this order:

1. A default-branch CI job that already builds the relevant application with no secrets.
2. A checked-in wrapper or task entrypoint used by developers, such as a project wrapper,
   Make target, task-runner target, or package script.
3. Manifest and toolchain declarations that unambiguously define the build.
4. User confirmation when multiple modules, profiles, runners, or commands remain plausible.

Do not infer a command only from the programming language. A manifest proves the tool
exists, not which profiles, generated sources, workspace filters, or build target this
application requires.

## Decide whether a build is safe and meaningful

Enable evidence only when all of these are known:

- the application/module affected by its dependency updates;
- a compatible, non-privileged runner;
- the required runtime/toolchain version and setup;
- dependency restoration plus a real compile/package/build command;
- whether the build requires credentials or network access unavailable to an untrusted PR.

Do not pass secrets to make a private dependency restore work. Report that build evidence
was omitted because the safe job cannot resolve private dependencies. Do not use a
persistent self-hosted runner unless repository policy explicitly establishes that it is
isolated for untrusted pull-request code.

A dependency build can execute far more than source compilation: package lifecycle scripts,
build wrappers, compiler plugins, generated-code tools, container instructions, and newly
downloaded dependencies. Minimal GitHub permissions limit repository impact but do not make
the process a sandbox; PR code may still use the runner and its network.

## Ecosystem clues, not defaults

Use this table to locate evidence. It is not permission to select a command without reading
the repository.

| Signals | Verify before configuring |
|---|---|
| JVM manifests and Maven/Gradle wrappers | Runtime/toolchain/release version, wrapper, profiles, module selection, compile of test sources, JUnit/Surefire paths |
| `package.json` plus a package-manager lockfile | Declared package manager, runtime version, workspace filters, lifecycle scripts, build/typecheck script, test reporter and report path |
| Python project/requirements files | Supported Python range, environment/installer, package build or typecheck command, framework test command, pytest JUnit option, native build requirements |
| `go.mod`/workspace files | Go directive/toolchain, workspace/module scope, generation steps, compile-vs-test command, JUnit conversion already available in CI |
| .NET project/solution files | Target framework/SDK pin, solution or project scope, restore/build flags, test logger and results directory |
| Rust workspace manifests | Toolchain channel, feature flags, workspace/package scope, check/build command, test-report tooling already present |
| Ruby, PHP, Dart, or other package manifests | Runtime pin, repository wrapper, build/task target, native extensions, test reporter already configured |
| Docker, Terraform, Helm, or generated artifacts | Whether validation actually exercises the changed dependency, required initialization, plugins, network access, and whether PR code executes |

For interpreted applications with no compile/package/typecheck step, do not relabel the test
suite as a successful build. Either use a real repository-defined validation target or leave
`build-status` as `not-run` and pass test evidence only if the caller contract and job shape
support it accurately.

## Build step contract

The command recorded in `build-command` must be the exact command executed against both the
head and, on failure, the base. Include dependency restoration when a fresh checkout needs
it; otherwise the base worktree may fail merely because its dependencies were never
installed. Keep it a reproducible, non-interactive single command suitable for `bash -c`.

Capture all output in `build.log`, then print only a bounded tail to the Actions log. Use
`set -uo pipefail`, capture the exit code explicitly, write one of these statuses, and end
the step successfully so the assessment still runs:

- `success`: the intended compile/package/build completed;
- `failure`: it ran with the correct environment and the application did not build;
- `could-not-build`: the intended build could not start or complete meaningfully because
  the safe environment/toolchain/dependency source was unavailable;
- `not-run`: no build was attempted.

Do not turn missing credentials, an unavailable toolchain, or an unrecognized module into
`failure`; those are absences of evidence, not dependency regressions.

## Test evidence

Run tests only after a successful build unless the repository's normal command intentionally
combines the two. Record `passed`, `failed`, or `not-run` independently from build status.
Append test output to `build.log` without losing the compiler output.

JUnit XML is more valuable than the Boolean test result because it tells the assessment
which test classes actually ran. Reuse an installed/configured reporter or a native CLI
option. Do not edit application dependencies just to add a reporter. Copy only known test
report locations; avoid a repository-wide `*.xml` search that could treat schemas or config
as tests. In multi-module repositories, preserve relative paths beneath
`evidence/test-results/` so identical report filenames are not overwritten.

## Baseline attribution

Run the baseline only when the head build reports `failure`. Check out
`github.event.pull_request.base.sha` in a detached worktree and execute the same recorded
command with the same toolchain, environment preparation, working-directory relationship,
and network/credential constraints.

- base `success`: the head-only failure is attributable to the update;
- base `failure`: the breakage already exists and must not count against the update;
- `not-run`: attribution is unknown.

If the command embeds an absolute head-worktree path, depends on unshared generated state,
or cannot restore dependencies in the base worktree, fix the command or report `not-run`.
Never compare different commands and call the result a baseline.

## Artifact and assessment wiring

Upload one artifact named `build-evidence-${{ github.event.pull_request.number }}` with:

```text
build.log
test-results/
  ... JUnit XML, recursively ...
```

Collection and upload use `if: always()`. A missing build log becomes a short placeholder;
missing test reports do not fail the job. Set a short retention period unless repository
policy requires otherwise, because logs can contain source paths and dependency output.

The assessment job must depend on the build and use an `always()` guard. Pass the artifact
name plus all four caller outputs: `build-status`, `build-command`, `baseline-status`, and
`test-status`. Keep the default artifact paths unless the produced layout differs. Leave
`fail-on-build-failure` false unless the user chooses compiler gating; it is independent of
`fail-on-risk` and does not gate test failures.

## Final consistency check

Before finishing, verify all of these as one unit:

- root permissions are empty or job-scoped;
- build permissions are exactly `contents: read` and no secrets are referenced;
- the build has a finite timeout and the workflow cancels superseded runs for the same PR;
- checkout uses the PR head SHA, full history for baseline, and no persisted credentials;
- the base build reuses the exact recorded head command;
- build/test steps record outcomes instead of preventing assessment;
- artifact layout matches the reusable-workflow inputs;
- `assess` has `needs: build`, `always()`, and all evidence inputs;
- static assessment still works if evidence download is absent or malformed;
- commands, runtime versions, paths, and report globs are supported by repository files,
  not copied from an example.
