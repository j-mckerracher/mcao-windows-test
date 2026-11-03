# Native Windows verification packet

## Current request

The agent reads this block at the start of every run. Keep the four lines in this exact form (`KEY: value`, one per line, no extra text on the key lines). Only the fenced block is parsed; the bullets below it describe the fields.

```text
RUN_ID: 20261103-r3
MODE: full
ROWS: ALL
NOTES: none
```

- A `RUN_ID` of `none` is the idle state. The agent does nothing else while it reads `none`.
- Any other `RUN_ID` is a new run request. The id is a single token such as `20261103-091500`.
- `MODE: full` runs every row in section 4 except W-49 and W-50. `MODE: targeted` runs only the rows named in `ROWS` plus W-01. W-49 and W-50 are excluded from both and run only under the rerun bullet below.
- `ROWS: ALL`, or a comma-separated list of row ids such as `W-06,W-21`. Ignored for `MODE: full`. `ROWS: ALL` does not select W-49 or W-50, and naming either in `ROWS` does not run it.
- W-49 and W-50, the explicit reruns of the UNRESOLVED W-21 test, run only when the `NOTES` key line is exactly `NOTES: rerun W-21`, in either mode. The match is literal and case-sensitive: `NOTES: rerun W-21` must be the whole line, and any other `NOTES` value does not trigger it. `ROWS` naming W-49 or W-50 does not trigger it. Without that exact line W-49 and W-50 are each recorded `SKIPPED`, reason `not requested`. W-21 is never retried automatically; W-49 and W-50 are its only reruns.
- `NOTES:` is free text. Read it before running rows.
- This block is committed in the idle state. The repository owner fills it when publishing a run request.
- A request cannot contain the hash of the commit it asks to test. The commit under test is the tip of `origin/windows-verification` that the agent fetched in step 1 of the loop, recorded with `git rev-parse HEAD` before any row runs. The run's `<RUN_ID>.started` file and the report's `Commit tested` field carry that hash.

## 1. Scope and limits

This packet is run in a loop. The agent on the Windows 11 laptop polls branch `windows-verification` of `https://github.com/j-mckerracher/mcao-windows-test.git` every 60 seconds, reads this file at the start of every run, runs the requested rows, and pushes one results folder back to that branch (section 2.4 and section 7). The loop procedure, the push rules, and the safety rules are the standing instructions for every run.

What this packet covers, without credentials:

- Toolchain pinned by the repository, MSVC host build, `cargo check`, `cargo clippy`, and `cargo test` on the host Windows target, with the per-crate filters in section 4.
- The `mcao` CLI offline behaviour: `--version`, `--help`, `preflight` (Git, `gh` presence, `FM_HOME` length, OneDrive rules, the UNC home warning, default home unit tests), and the audit writer refusal and rejection-log home on Windows.
- The Windows process backend tests and the console pipe tests, run as ordinary native tests.
- The `mcao-probe` stub-harness run that CI also performs (no harness, no credentials).
- Platform-leakage guard, state-encoding and task-id rules (DPES-889), and the audit zone gap.
- Diagnostic probes (W-51 to W-57) for the open Windows questions from the first loop run: temp directory under a cleared environment, direct `.sh` launch, bash path forms, Python zone data, directory open, git under a cleared environment, and environment presence.

What this packet cannot cover, and must not be reported as covered:

- Claude Code runs, login, or any pilot-harness evidence (needs live Claude Code credentials).
- Cloud execution, cloud admission, or any network service beyond the Rust toolchain and crate downloads.
- Organization signing of binaries, the signing mechanism, or signature verification (DPES-892 AC9 is blocked on it; the signer is OPEN in DPES-752).
- Installer, uninstaller, registry PATH edit, `WM_SETTINGCHANGE` broadcast, and retry-on-sharing-violation behaviour (DPES-892 AC1-AC3, AC6-AC8). Those need a managed laptop install and are not run here.
- A clean-laptop install of `mcao` and the "no `bash.exe`, `wsl.exe`, or `sh.exe` processes observed during install" check (DPES-754 AC1).
- Statically linked binary proof on clean Windows 10 and 11 (DPES-754 AC4 first half).
- Decision-record sign-off (DPES-752 AC2, AC5), which is an owner review, not a test.
- Live interactive attach to a worker console from a real terminal, `mcao peek`, and `mcao attach` (DPES-883 AC10-AC21; DPES-885 AC5-AC9 at the terminal level). The automated console tests in section 4 cover the pipe and server code paths only.
- Windows steer end-to-end (DPES-885 owner note: `crates/mcao-core/tests/steer_tmux_target.rs` is `cfg(unix)`; no Windows end-to-end steer row exists). Only the unit tests in the CI check job are covered, and they are not run again here.

If a row in section 4 needs something from this list, mark it `SKIPPED` with that reason. Never mark it `PASS`.

### 1.1 Evidence base

The first native run, `20261102-150124`, tested commit `b324a8145dc218c5de3d4e593f89f0cfb39d79bc` of the public test repository. That commit is not in this repository's history, so its results are mapped to this packet by reading the code at `origin/DPES-751` (`4967a20d`, which includes PR 456). Each row in section 4 states what the first run showed. Where the first run failed because of a defect fixed later (PR 456 fixed the Windows-only clippy failures and the rejection-log home regression), the row keeps the observed failure and its expectation for the current commit is `SPEC-ONLY` until a run shows it.

The diagnosis rerun, `20261102-r1`, tested commit `c96d86f232373de7d40d007ad9c69200c6552454` with `MODE: full` and `NOTES: rerun W-21`. Where section 4 cites its figures it names `20261102-r1`; they are not figures from the first run.

The second run, `20261102-r2`, tested commit `37a2078dc62874448470360fb01d29241f5874cf` with `MODE: targeted`, `ROWS: W-06,W-17,W-21`, and `NOTES: rerun W-21`; it also ran W-49 (20 reruns). It used Windows PowerShell 5.1. That commit had PRs 460 and 463 but not PR 464: its `hook_session_start` and `hook_host_mirror` rows ran as Windows tests that PR 464 gates off. Where section 4 cites its figures it names `20261102-r2`. The packet version of that run had no W-50 row, so W-50 has not been run.

Status values used in section 4:

- `VERIFIED-ON-WINDOWS (run 20261102-150124)`: the first native run executed the row and its result matched the expectation written here.
- `OBSERVED-FAIL (run 20261102-150124)`: the first native run executed the row and it failed. The row says what the current expectation is.
- `UNRESOLVED`: a recorded failure whose cause the packet does not settle. The row stays a failure in the report until a run settles it.
- `KNOWN OPEN`: a recorded failure whose cause is not established. The packet does not assign a cause; the row is reported as a failure until a run settles it (W-06, `assignment_reassess_prose`).
- `VERIFIED-BY-READING`: the file, line, or CI step named in the row was read.
- `LISTED-ON-HOST`: the test name exists in the built test binary on macOS (`cargo test ... -- --list`). Windows-only (`cfg(windows)`) tests cannot be listed there.
- `SPEC-ONLY`: the expected result is the packet's expectation and no Windows run has shown it. The agent records the actual result; it must not treat a SPEC-ONLY expectation as proof.

These labels are evidence labels for the Status column of section 4. The status a run records for each row is a separate vocabulary: `PASS`, `FAIL`, `FAIL (vacuous)`, `SKIPPED`, `TIMEOUT`, `SPEC-ONLY observed`, defined in the report template, section 3.

## 2. Loop procedure

### 2.1 Checking the MSVC Build Tools

Run in PowerShell. The check uses only `vswhere.exe`. Do not use `where.exe link.exe`: Windows ships `C:\Windows\System32\link.exe` (the hard-link tool), so a PATH lookup always finds something.

```powershell
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path -LiteralPath $vswhere) {
  & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
} else {
  Write-Output 'vswhere.exe not found'
}
Get-Command link.exe -All -ErrorAction SilentlyContinue | Where-Object { $_.Source -notlike "$env:SystemRoot*" } | Select-Object -ExpandProperty Source
```

A printed installation path means the x64 MSVC tools are present. Record it as `yes, path redacted to <vs>`. If nothing is printed, record `no`. The last line prints nothing on a stock machine; a printed path there means a Visual Studio `link.exe` is on `PATH` outside the system folder. Record it as `link.exe on PATH: yes` and do not treat it as proof of the MSVC tools.

If the MSVC tools are absent, every Cargo row and every `mcao.exe` row that needs a built binary is `SKIPPED` with the reason `no MSVC linker`. W-01 and W-29 do not need a linker and still run. Record the skips in section 5 of the report. Do not install Build Tools: that needs admin and a download.

### 2.2 Rust toolchain: use the installed rustup only

The standing instructions forbid downloading or running an installer. Use the rustup, git, and Visual Studio build tools already on the laptop. Downloads this section allows are the ones rustup and cargo make themselves: the pinned toolchain (fetched by `rustup show` when it is missing), the two cross targets named by W-18 (fetched by `rustup target add`), and the crates cargo fetches for the `--locked` builds. rustup fetching the pinned toolchain and the W-18 targets is allowed and is not an installer. `rustup-init.exe`, MSI packages, any other installer, and any `.exe` fetched for this packet are not allowed.

```powershell
rustup --version
rustup show active-toolchain
```

If `rustup` is not on `PATH`, write `results/<RUN_ID>-BLOCKED.md` with the reason `rustup missing`, push it (section 2.6), and stop the run. Do not download `rustup-init.exe`. The pinned toolchain comes from `rust-toolchain.toml` in the test checkout. `rustup show` installs that pinned toolchain from the official channel when it is missing; if that fails, record the exact error as a blocker and skip the Cargo rows.

The two non-host targets are used only by row W-18 `cargo check`. Install them with `rustup target add` only when they are missing, and record the result. `rustup target add` writes only to the rustup home, not to a checkout.

```powershell
rustup target add x86_64-unknown-linux-gnu
rustup target add aarch64-apple-darwin
```

If one of those downloads is blocked (proxy, TLS, DNS, or network), record the exact error, with a proxy block written as `proxy blocked: <host name only>`, and then: if the pinned toolchain cannot be installed, record it as the blocker and skip every Cargo row and every row that needs a built binary; if only a cross target cannot be added, mark W-18 `SKIPPED` with that reason and run the other rows; if a crate download blocks a Cargo build, mark the row that needs it `SKIPPED` with the same reason. Do not set proxy credentials, do not search for another download source, and do not run `rustup-init` or an installer as a workaround.

### 2.3 Folders, clones, and the build directory

The loop uses two clones of the same branch, plus one build directory outside both clones. Use these exact values; they are not placeholders.

- Repository: `https://github.com/j-mckerracher/mcao-windows-test.git`
- Branch: `windows-verification`. Never use `main`. Never create a pull request, tag, or other branch.
- Test checkout: `%LOCALAPPDATA%\mcao-winverify\src`. Detached checkout of `origin/windows-verification` each run. Never edited.
- Results checkout: `%LOCALAPPDATA%\mcao-winverify\push`. A second, separate clone of the same branch, used only to publish results (section 2.6).
- Build directory: `%LOCALAPPDATA%\mcao-winverify\target`, set through `CARGO_TARGET_DIR` for every cargo command. It is outside both clones.
- Run folder: `%LOCALAPPDATA%\mcao-winverify\<RUN_ID>`, outside both clones. Logs and the report are written here before they are copied to the results checkout.
- Do not use `C:\mcao-winverify`. A non-admin session cannot create it.

Setup, once, before the first run (create the folders and both clones if they are missing):

```powershell
$Repo = 'https://github.com/j-mckerracher/mcao-windows-test.git'
$Branch = 'windows-verification'
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Push = Join-Path $Root 'push'
$Target = Join-Path $Root 'target'
New-Item -ItemType Directory -Force -Path $Root | Out-Null
if (-not (Test-Path -LiteralPath (Join-Path $Clone '.git'))) {
  git clone --no-tags --branch $Branch -- $Repo $Clone
}
if (-not (Test-Path -LiteralPath (Join-Path $Push '.git'))) {
  git clone --no-tags --branch $Branch -- $Repo $Push
}
$env:CARGO_TARGET_DIR = $Target
```

`$env:CARGO_TARGET_DIR = $Target` must be set again in every new PowerShell session or child shell before a cargo command runs. Every cargo command in this packet is run after that line.

### 2.4 The loop: one run

The agent repeats this procedure forever. Between checks it waits 60 seconds and never busy-loops. It exits only when told to.

1. Fetch the test checkout: `git -C $Clone fetch origin $Branch`. If the tip is the same as the last check, wait 60 seconds and repeat.
2. Otherwise check out the tip detached and read the request:

   ```powershell
   git -C $Clone checkout --detach origin/$Branch
   git -C $Clone status --porcelain
   $tip = git -C $Clone rev-parse HEAD
   ```

   `git status --porcelain` must be empty. Any output means the checkout was changed; record it, write `results/<RUN_ID>-BLOCKED.md` if a RUN_ID is in the request, and stop the run.

   Read the `Current request` block of `docs/windows-native-verification.md` in the test checkout. Take the `RUN_ID`, `MODE`, `ROWS`, and `NOTES` values from the four key lines. If `RUN_ID` is `none`, wait 60 seconds and repeat.
3. Skip a request that has results already. Pull the results checkout (`git -C $Push pull --rebase`). If `results/<RUN_ID>.zip` already exists on the branch, skip the run and go to step 1. Do not write a second result for the same RUN_ID.
4. Record the commit under test (`$tip`). In the results checkout write `results/<RUN_ID>.started` with the UTC start time and `$tip`, publish it (section 2.6), and continue. A `.started` file is published before any row runs.
5. Run what the request names, in the order of section 4: `MODE: full` runs every row except W-49 and W-50; `MODE: targeted` runs only the rows in `ROWS` plus W-01; W-49 and W-50 run only when the `NOTES` key line is exactly `NOTES: rerun W-21` (Current request section, either mode); naming W-49 or W-50 in `ROWS` does not trigger them, and otherwise each is recorded `SKIPPED`, reason `not requested`. Follow the standing procedure in sections 3 and 4. Each row's log goes to `%LOCALAPPDATA%\mcao-winverify\<RUN_ID>\logs\`. Fill in the report template (`docs/windows-native-verification-report-template.md` in the test checkout, copied to the run folder at the start of the run).
6. After the last row record `git -C $Clone status --porcelain` in the report. It must be empty.
7. Write the report to `%LOCALAPPDATA%\mcao-winverify\<RUN_ID>\<RUN_ID>-SUMMARY.md`: run id, commit tested, start and end times, a table of every row (PASS, FAIL, FAIL (vacuous), SKIPPED, TIMEOUT, or SPEC-ONLY observed; exit code; tests run), each failure quoted exactly (test name, panic message with file and line, last 30 log lines), surprises and differences, questions. Use only those six row statuses; the report template, section 3, defines them.
8. Redact the run folder (section 8), zip it to `%LOCALAPPDATA%\mcao-winverify\<RUN_ID>.zip`, copy `<RUN_ID>.zip` and `<RUN_ID>-SUMMARY.md` into `results/` of the results checkout, write the run id alone to `results/LATEST` as the LAST file, and publish (section 2.6) with the message `results: <RUN_ID>`.
9. Go to step 1.

### 2.5 Recording versions

```powershell
[System.Environment]::OSVersion.VersionString
(Get-CimInstance Win32_OperatingSystem).Caption
(Get-CimInstance Win32_OperatingSystem).BuildNumber
$env:PROCESSOR_ARCHITECTURE
$PSVersionTable.PSVersion.ToString()
$PSVersionTable.PSEdition
git --version
Push-Location -LiteralPath $Clone
rustup show active-toolchain
rustc -V
cargo -V
rustc -Vv
Pop-Location
rustup target list --installed
```

The `rustup show active-toolchain`, `rustc`, and `cargo` lines run inside the test checkout, because `rust-toolchain.toml` pins the toolchain there. The default toolchain outside the checkout can differ. A difference is an anomaly to record in the report, not a failure.

Do not run `Get-ChildItem Env:`, `set`, or `printenv`. Environment dumps can contain tokens. Record only the values listed in the report's environment block. For `Proxy in use`, record `yes` or `no` from the operator's knowledge or from the browser settings; never print the proxy value.

### 2.6 Publishing results

Only files under `results/` of the results checkout are ever committed and pushed. Never push anything else, never force, never push to `main`, and never create a pull request, tag, or branch. Use only the git credentials already configured on this laptop.

The first publish, once, is `results/AGENT-READY.md`: UTC time, Windows version, git, rustc, cargo, and whether `gh` is on `PATH`. Redact it (section 8) and publish it with the message `results: agent ready` before any other step. If that push fails, stop and report the exact error with credentials removed.

```powershell
$ready = Join-Path $Push 'results\AGENT-READY.md'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $ready) | Out-Null
@(
  '# Agent ready',
  "- UTC: $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))",
  "- Windows: $([System.Environment]::OSVersion.VersionString)",
  "- git: $(git --version)",
  "- rustc: $(rustc -V)",
  "- cargo: $(cargo -V)",
  "- gh on PATH: $([bool](Get-Command gh -ErrorAction SilentlyContinue))"
) | Set-Content -LiteralPath $ready -Encoding utf8
```

The publish step, used for every results push, is this function. It stages only `results/`, commits, then pulls with rebase and pushes. It retries the pull and push up to three times and stops after that.

```powershell
function Publish-Results {
  param([string]$Message)
  git -C $Push add -- results
  $staged = @(git -C $Push diff --cached --name-only)
  if ($staged.Count -eq 0) { throw 'nothing staged under results/' }
  if (@($staged | Where-Object { $_ -notlike 'results/*' }).Count -gt 0) { throw 'staged path outside results/; stop' }
  git -C $Push commit -m $Message
  if ($LASTEXITCODE -ne 0) { throw 'commit failed' }
  for ($attempt = 1; $attempt -le 3; $attempt++) {
    git -C $Push pull --rebase
    if ($LASTEXITCODE -eq 0) {
      git -C $Push push origin $Branch
      if ($LASTEXITCODE -eq 0) { return }
    }
  }
  throw 'push refused after three attempts; stop and report the error without credentials'
}
```

Never use `--force`, `--force-with-lease`, `git reset`, or `git commit --amend` on the results checkout. If a rebase conflicts, stop and record it in `results/<RUN_ID>-BLOCKED.md` only after the publish retries are exhausted.

## 3. Conventions for every row

- Shell: run every block in PowerShell 7 (`pwsh.exe`) when `pwsh.exe` is on `PATH`; otherwise run it in Windows PowerShell 5.1 (`powershell.exe`). Record which one ran and its `$PSVersionTable.PSVersion` in the report. Every block must work in both. Blocks avoid PowerShell 7-only syntax: no ternary `? :`, no `??`, no `?.`, no `&&` or `||` as operators, no `ForEach-Object -Parallel`, no `Get-Content -AsByteStream`, no `$PSStyle`, no `ConvertFrom-Json -AsHashtable`, no `-Encoding utf8NoBOM`, no three-argument `Join-Path`. `[ordered]` is fine. Keep it that way when you adapt a block.
- A block saved as a `.ps1` file must be UTF-8 with a BOM. Windows PowerShell 5.1 reads a file without a BOM as ANSI and garbles the non-ASCII `FM_HOME` in W-35 and W-36.
- Work from the clone root with `-LiteralPath` or `Push-Location`. No persistent `cd` is needed.
- Hashing: `Get-FileHash` can be missing from a session. Where a file hash is needed, use .NET: `[System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash([System.IO.File]::ReadAllBytes($path))).Replace('-', '').ToLowerInvariant()`.
- Do not set `$ErrorActionPreference = 'Stop'` for the row helper: cargo writes progress to stderr, and `Stop` turns that into a terminating error.
- Every command finishes under the row's timeout, and no row runs longer than 30 minutes. A row that exceeds 30 minutes is stopped, recorded as `TIMEOUT`, and the run goes on to the next row. Long Cargo builds are the normal case: give the first build the full budget.
- Use `--locked` on every Cargo command so the committed `Cargo.lock` is used. Set `CARGO_TARGET_DIR` first (section 2.3).
- Scripts written into the run folder run in a child shell with `-NoProfile -ExecutionPolicy Bypass -File`. If Group Policy refuses the script, record the message as an anomaly and mark the dependent rows `SKIPPED`.
- Child shells use the absolute path of the running shell: `$shell = (Get-Process -Id $PID).Path` (this is `pwsh.exe` or `powershell.exe`). Do not use `(Get-Command pwsh).Source`, which is `$null` on a 5.1-only machine.
- Expected test count: every Cargo test row states how many tests it must run. Cargo prints `running N tests` per test binary. A row whose log has no `running N tests` line with N of at least 1 is `FAIL (vacuous)`, even if the exit code is 0. A filter that matches nothing reports `running 0 tests` and exits 0; that is the case this rule catches. A row that states an expected count of `0` for a file that is gated `cfg(unix)` is a PASS when its exit code is 0 and its log shows `running 0 tests`; that row sets `MinTests` to 0.
- The helper below writes one line per row to `rows.tsv` in the run folder and marks a row vacuous when no test binary ran a test. For rows that are not Cargo test runs (`--version`, `preflight`, the probe, the audit refusals), pass `-MinTests 0`.

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null

function Invoke-Row {
  param([string]$Id, [string]$Command, [int]$MinTests = 1, [scriptblock]$Action)
  $log = Join-Path (Join-Path $Results 'logs') "$Id.log"
  $start = Get-Date
  $lines = @(& $Action 2>&1 | ForEach-Object { "$_" })
  $code = $LASTEXITCODE
  $lines | Out-File -FilePath $log -Encoding utf8
  $secs = [int]((Get-Date) - $start).TotalSeconds
  $ran = @($lines | Where-Object { $_ -match '^running [1-9][0-9]* tests?$' }).Count
  $vacuous = ($MinTests -gt 0) -and ($ran -eq 0)
  "$Id`t$code`t$secs`tbinaries-with-tests=$ran`tvacuous=$vacuous`t$Command" | Add-Content -LiteralPath $RowsTsv -Encoding utf8
  "$Id exit=$code duration=${secs}s binaries-with-tests=$ran vacuous=$vacuous"
}
```

Use it like this, with the row's command in the action:

```powershell
Invoke-Row -Id 'W-07' -Command 'cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation' -Action { cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation }
```

Do not change a row's command to make it pass. A failing row is evidence. A row is `PASS` only when its exit code is 0, its expected test count is met, and its expected output (section 4) appears in the log.

## 4. Test matrix

Columns: ID, command, expected tests run, expected result, capture, timeout, Jira acceptance, and Status. The Status column is the packet's status of the row (section 1.1), not the result of a run. The result of a run is recorded in the report.

Failure rule for every row: do not edit code, do not change tests, do not add `#[ignore]`, do not rerun with `--ignored`, and do not retry a failing row unless the request NOTES say so. The only reruns this packet defines are W-49 and W-50 (section 4.8), both gated by the `NOTES` key line of the Current request section; W-50 is a separate three-run row, not a retry. Capture the log, the exact error text, the versions in section 2.5, and continue to the next row.

Acceptance mapping key: `752-AC1` etc. refer to the Jira criteria listed in section 6.

Build paths: the binaries are in the build directory, not in the clone. `$Target\debug\mcao.exe` is `mcao.exe` and `$Target\x86_64-pc-windows-msvc\debug\mcao-probe.exe` is the probe.

### 4.1 Toolchain and build

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-01 | Inside the test checkout (`Push-Location -LiteralPath $Clone`): `rustup show active-toolchain`; `rustc -V`; `cargo -V`; `rustc -Vv` | none | The pinned toolchain (`rust-toolchain.toml:2`, `channel = "1.95.0"`): active toolchain `1.95.0-x86_64-pc-windows-msvc`; `rustc -V` and `cargo -V` report 1.95.0; host line `host: x86_64-pc-windows-msvc`. Run 20261102-150124: `1.95.0-x86_64-pc-windows-msvc`, `rustc 1.95.0 (59807616e 2026-04-14)`. Run 20261102-r2: the log shows `release: 1.95.0`. `rustup show active-toolchain`, `rustc`, and `cargo` read outside the checkout gave the default `stable` toolchain (1.96.0). That is an anomaly to record, not a failure; the version of record is the one read inside the checkout. | Output | 2 min | 759 (toolchain pin) | VERIFIED-ON-WINDOWS (run 20261102-150124); run 20261102-r2 PASS with the outside-checkout anomaly recorded |
| W-02 | `cargo build -p mcao-cli --locked --bin mcao` | none | Exit 0; `$Target\debug\mcao.exe` exists. Requires the MSVC linker (section 2.1). Run 20261102-150124: exit 0 in 102 s. | Full log, `Get-Item` of the binary | 30 min | 754-AC4 (partial), 752-AC1 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-03 | `cargo build -p mcao-probe --locked --target x86_64-pc-windows-msvc` | none | Exit 0; `$Target\x86_64-pc-windows-msvc\debug\mcao-probe.exe` exists (`rust-workspace-check.yml:190`). Run 20261102-150124: exit 0 in 55 s. | Log, file listing | 30 min | 752-AC1 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-04 | `cargo check --workspace --locked --target x86_64-pc-windows-msvc` | none | Exit 0 (`rust-workspace-check.yml:128-138`, Windows branch at `:136-148`). Run 20261102-150124: exit 0 in 27 s. | Log | 30 min | 759-AC1, 752-AC4 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-05 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | none | Exit 0 (`rust-workspace-check.yml:145-148`). Run 20261102-150124 exited 101: `unused import: std::io::Write`, `unused import: Stdio`, `function assert_result is never used` (`audit_validate`), `unused import: Child` (`hook_host_mirror`). PR 456 (`4967a20d`) gates those items on the cfg that uses them. The current expectation is exit 0; the packet has not seen it on Windows. | Log | 30 min | 759-AC1, 759-AC5 | OBSERVED-FAIL (run 20261102-150124); current expectation SPEC-ONLY |

### 4.2 Workspace and per-crate tests on the host

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-06 | `cargo test --workspace --locked --no-fail-fast` | at least one `running N tests` line with N of at least 1 across the binaries | Exit 0 with `test result: ok` for every binary (`rust-workspace-check.yml:153-156`; the x86_64 leg runs the same command at `ci.yml:752-764`). `--no-fail-fast` is required: the first run used `cargo test --workspace --locked` and stopped at the first failing binary, `mcao-backend-windows --test interrupt` (W-21), so the later binaries were never run, including the OneDrive, hook, and audit writer tests added after that run. If any binary fails, the report lists every failing binary by its `Running` line and its `test result: FAILED` line, the panic text of each failing test, and the run's total of binaries executed. Each `running 0 tests` line is listed in anomalies; doc-test and `cfg`-gated binaries show it normally. Expected after PRs 463 (`58b79e35`) and 464 (`5fb482ef`), both on `origin/DPES-751` and in this packet branch: every class that PR 464 gates off Windows is absent from the failing set, so `hook_host_mirror`, `hook_session_start`, `util_durable`, `audit_writer_chain_baseline`, and the `mcao-core` lib `audit_writer` tests do not fail. Per binary on Windows: `hook_host_mirror` 4 passed (W-45); `hook_session_start` 0 tests (file `#![cfg(unix)]`); `util_durable` 2 passed; `mcao-core` lib 2 `audit_writer` tests run (empty store, rejected envelope) and no failure; `audit_writer_chain_baseline` 3 passed; `audit_validate` 7 (W-17). Run 20261102-r2 showed `assignment_reassess` 3 passed, `assignment_reassess_previous` 2 passed (PR 463 forwards `TEMP`, `TMP`, and `USERPROFILE` to their `env_clear()` children), and `interrupt` 16 passed, 7 ignored, 0 failed (W-21). The only known remaining failure is `assignment_reassess_prose`, **KNOWN OPEN**: its `every_prose_case_matches_the_python_reference` test fails on 2 of 36 cases, `path-trailing-dot` and `path-trailing-dot-in-dir`, whose replayed and recorded bytes differ by one byte (SPEC-ONLY observed in run 20261102-r2). The cause is not established and this packet does not assign one. Any other failing binary is a new finding. Run 20261102-r2 (commit `37a2078d`, before PR 464) recorded 1531 passed and 38 `FAILED` test lines (37 test-level, plus one nested child-process test in `assignment_reassess_prose`): `hook_host_mirror` 10, `hook_session_start` 17, `util_durable` 2, `mcao-core` lib 6, `audit_writer_chain_baseline` 1, and `assignment_reassess_prose` 1. The first five are the classes PR 464 gates off Windows. Run 20261102-r2 did not run W-50. | Full log; `grep` of `Running`, `test result`, and `running` lines into `W-06-summary.txt` | 30 min | 759-AC2, 755-AC1 | SPEC-ONLY. Run 20261102-r1 (before PRs 463 and 464): 9 failing binaries, 14 `test result: FAILED` lines. Run 20261102-r2 (before PR 464): see the expected set below; `assignment_reassess_prose` is KNOWN OPEN. |
| W-07 | `cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation` | 1 (`running 1 test`) | Pass: `a_task_id_refused_at_creation_exits_with_usage_code_two` (`crates/mcao-core/tests/error.rs:9`) refuses `con` at creation with exit 2 and `Usage:` text. Run 20261102-150124: 1 passed. | Log | 20 min | 889-AC5 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-08 | `cargo test -p mcao-core --locked --test state_encoding` | 46 on Windows (the macOS host lists 48; the difference is `cfg(unix)` tests). Run 20261102-150124: 46 passed. | Pass: path verbatim write and opaque read, line-ending normalization, format version stamping, the reserved-name loop `a_new_task_id_that_is_a_windows_device_name_is_refused` (`crates/mcao-core/tests/state_encoding.rs:355`), trailing-mark refusal `a_device_name_with_an_extension_or_trailing_mark_is_refused_as_a_bad_character` (`:385`), and the 64-byte creation boundary `state_task_id_length_boundary_is_64_bytes_for_creation` (`:458`). | Log | 20 min | 889-AC1, AC2, AC3, AC5, AC6, AC7 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-09 | `cargo test -p mcao-core --locked --test home` | 22 on Windows (the macOS host lists 30). Run 20261102-150124: 22 passed. | Pass. Includes `names_match_case_insensitively_on_windows` (`crates/mcao-core/tests/home.rs:834`, gated by `#[cfg(windows)]` at `:833`). | Log | 20 min | 889-AC5 (case folding), 892-AC4 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-10 | `cargo test -p mcao-core --locked --test windows_registry` | 3 (three `#[test]` functions in the file, counted by reading it; `#![cfg(windows)]` at `crates/mcao-core/tests/windows_registry.rs:5` means the macOS host lists 0). Run 20261102-150124: 3 passed. | Pass. The test creates `HKEY_CURRENT_USER\Software\mcao-test-<pid>-<n>` (`<pid>` is the test process id, `<n>` a counter from 0 in `ThrowawayKey::new`) with a `Path` value under it, and deletes that key on drop, even when a test fails. It never touches `HKCU\Environment` or any HKLM key. It is the only registry write this packet allows (sections 5 and 8). If it fails, record the error; do not run it elevated. | Log | 20 min | 892-AC3 (partial, registry edit only; no broadcast checked) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-11 | `cargo test -p mcao-core --locked --test preflight` | 39 on Windows (the macOS host lists 40). PR 467 (`f79b2c59`) added three ungated UNC tests at `crates/mcao-core/tests/preflight.rs:460`, `:478`, and `:497`, so each count rose by three. The `cfg(unix)` tests at `crates/mcao-core/tests/preflight.rs:260,272` do not run on Windows. Run 20261102-150124: 36 passed (before PR 467). | Pass. OneDrive root refusal `a_home_under_onedrive_is_refused_with_the_remedy` (`:144`), business root `a_business_onedrive_root_is_refused_too` (`:158`), profile-folder warning `a_home_under_the_profile_onedrive_folder_warns_without_a_root_variable` (`:198`), profile sync folder that a root also names `a_profile_sync_folder_that_a_root_also_names_is_refused` (`:228`), and `a_device_namespace_home_is_refused_not_compared` (`:436`). | Log | 20 min | 892-AC4, AC5 | VERIFIED-ON-WINDOWS (run 20261102-150124) for the 36 pre-467 tests; the three PR 467 tests SPEC-ONLY |
| W-12 | `cargo test -p mcao-core --locked --lib preflight` | 13 on Windows. The macOS host lists 14 (`cargo test -p mcao-core --locked --lib preflight -- --list`); the one host-only test is `notify::unix::tests::missing_handler_is_a_preflight_failure` (`crates/mcao-core/src/notify/unix.rs:257`), and its module is `#[cfg(unix)]` at `crates/mcao-core/src/notify/mod.rs:8`, so Windows does not compile it. Run 20261102-150124: 13 passed. | Pass. Git and `gh` lookup unit tests `preflight::prereq::tests::*` (for example `preflight_git_is_missing_without_a_path_and_names_its_remedy`). These test the lookup function only, not the default home. | Log | 20 min | 754-AC2 (unit level) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-13 | `cargo test -p mcao-cli --locked --test preflight` | 15 on Windows (14 on the macOS host). PR 467 (`f79b2c59`) added two `cfg(windows)` UNC tests at `crates/mcao-cli/tests/preflight.rs:423` and `:441`; the 13 pre-467 tests are unchanged. Run 20261102-150124: 13 passed. | Pass. Includes Git-missing exit 3 `preflight_exits_three_and_names_git_when_it_is_missing_from_path` (`crates/mcao-cli/tests/preflight.rs:105`), spaces and non-ASCII `FM_HOME` `preflight_prints_an_fm_home_with_spaces_and_non_ascii_exactly` (`:161`), overlong home exit 3 (`:186`, `cfg(windows)`), OneDrive root exit 3 `preflight_exits_three_naming_onedrive_for_a_home_under_a_onedrive_root` (`:400`), and script-shim rejection for Git and `gh` (`:296` and `:312`, both `cfg(windows)`). | Log | 20 min | 754-AC2, AC3, 892-AC4 | VERIFIED-ON-WINDOWS (run 20261102-150124) for the 13 pre-467 tests; the two PR 467 UNC tests SPEC-ONLY |
| W-14 | `cargo test -p mcao-cli --locked --test cli` | 8 (the macOS host lists 8). Run 20261102-150124: 8 passed. | Pass. Includes the Windows-only case at `crates/mcao-cli/tests/cli.rs:32` (`#[cfg(windows)]` at `:32`). | Log | 20 min | 754-AC5 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-15 | `cargo test -p mcao-cli --locked --test toolchain` | 8. Run 20261102-150124: 8 passed. | Pass. `no_mistakes_is_refused_on_every_host` (`crates/mcao-cli/tests/toolchain.rs:118`). | Log | 20 min | 759 (gate evidence) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-16 | `cargo test -p mcao-cli --locked --test herdr_lab_viewer` | 4 on Windows (the macOS host lists 9). Run 20261102-150124: 4 passed. | Pass. `lab_viewer_is_unsupported_on_windows` (`crates/mcao-cli/tests/herdr_lab_viewer.rs:371-373`, `cfg(windows)`) expects the documented Windows refusal. A failure here is a real finding; record it. | Log | 20 min | 754 (regression evidence; no matching AC) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-17 | `cargo test -p mcao-cli --locked --test audit_validate --test audit_activity_map` | `audit_validate`: 7 on Windows (6 run without a gate, plus `a_rejection_is_recorded_under_the_windows_default_home`, `crates/mcao-cli/tests/audit_validate.rs:318-320`, `cfg(windows)`); the 9 tests in `mod zone_bearing` (`audit_validate.rs:65`, `cfg(unix)`) do not run. `audit_activity_map`: 8 on Windows (12 on the host, minus the 4 in `mod zone_bearing`, `audit_activity_map.rs:84-85`). First run: `audit_validate` 6 (5 passed, 1 failed); the first run's SUMMARY reported `audit_activity_map` as 8 in one place and 6 in another, so the actual count is recorded, not assumed. | Exit 0 expected. Run 20261102-r2 (exit 0 in 11 s): `audit_activity_map` 8 and `audit_validate` 7 tests ran, all passed. After PR 463 (`58b79e35`) the Windows default-home test uses `envelope-actor-mismatch.json`, so `audit_validate` runs 7 tests (6 ungated and that one) and all must pass; `audit_activity_map` runs 8. The first run failed `a_rejection_without_a_state_directory_is_not_printed` (test at `audit_validate.rs:297`). Its failing assertion was the `FM_HOME or FM_STATE_OVERRIDE` text check, panicked at `audit_validate.rs:298:5` on the first run's commit, where an unset `FM_HOME` selects the default home on Windows. PR 456 made that text check `cfg!(unix)`-only (at the current head, `audit_validate.rs:311-312`) and set `LOCALAPPDATA` to a scratch directory. Record the `ok` line count of `audit_activity_map` and the zone cases skipped by `cfg`. | Log, count of `ok` lines, `running` lines | 20 min | 759 (gate evidence), audit gap (section 5) | VERIFIED-ON-WINDOWS (run 20261102-r2: `audit_validate` 7, `audit_activity_map` 8); first run OBSERVED-FAIL (run 20261102-150124) |
| W-18 | `cargo check --workspace --locked --target x86_64-unknown-linux-gnu`, then `cargo check --workspace --locked --target aarch64-apple-darwin` | none | Exit 0 for each (`rust-workspace-check.yml:128-138`). `cargo check` does not link, so no cross linker is needed. Run 20261102-150124: exit 0 for both (9 s and 10 s). | Two logs | 30 min each | 752-AC4, 754-AC4 (second half), 755-AC5 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-40 | `cargo test -p mcao-core --locked --test state_encoding a_new_task_id_with_uppercase` | 1 (`running 1 test`). Run 20261102-150124: 1 passed. | Pass: `a_new_task_id_with_uppercase_is_refused_and_a_legacy_one_still_reads` (`crates/mcao-core/tests/state_encoding.rs:407`) refuses uppercase at creation and still reads the legacy id. This is the DPES-889 owner ruling (new task ids are lowercase `[a-z0-9-]`; uppercase refused at creation). The CLI path that calls `TaskId::parse_for_creation` is not separately checked here. | Log | 20 min | 889-AC5 (ruling) | VERIFIED-ON-WINDOWS (run 20261102-150124) |

### 4.3 Windows process backend and console (native tests)

These tests spawn child processes and use Job Objects, named pipes, and ConPTY. Run them in a normal interactive PowerShell window. Do not run them elevated. Do not run tests marked `#[ignore]` with `--ignored`: those are re-exec stubs that only the owning test starts (`crates/mcao-backend-windows/tests/backend.rs:78-115`, `conpty.rs:125-219`, `interrupt.rs` stub tests, `job.rs` stub tests).

Owner ruling (DPES-755, comment of 2026-10-26): on Windows `interrupt` means stop-or-kill (Ctrl+C, poll, then `TerminateJobObject`). A child that is gone after the ladder is the expected passing outcome.

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-19 | `cargo test -p mcao-backend-windows --locked --test job` | 4 `running` lines totalling 10 tests on Windows: the `job` binary's own run prints `running 7 tests` (4 passed, 3 ignored), and three `running 1 test` lines follow from the `#[ignore]` re-exec stubs that the owning tests start. Run 20261102-150124: `7+1+1+1` over 4 `running` lines, exit 0. | Pass. Job Object tree termination and child-in-job checks (`crates/mcao-backend-windows/tests/job.rs:7`, `#![cfg(windows)]`). | Log | 20 min | 755-AC1 (Job Object cleanup) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-20 | `cargo test -p mcao-backend-windows --locked --test backend` | 14 (10 passed, 4 ignored). Run 20261102-150124: 14 counted, exit 0. | Pass. Launch, state, and cleanup through `WindowsBackend` (`tests/backend.rs:8`, `#![cfg(windows)]`). | Log | 20 min | 755-AC1, AC3 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-21 | `cargo test -p mcao-backend-windows --locked --test interrupt` | `running 23 tests; 16 passed; 7 ignored (re-exec stubs); 0 failed` (run 20261102-r2). First run 20261102-150124: 15 passed, 7 ignored, 1 failed. | Pass expected: exit 0 with `test result: ok. 16 passed; 0 failed; 7 ignored`. The 7 ignored tests are the `#[ignore]` re-exec stubs that only their owning tests start, so they are not run and do not count as passes. PR 460 (`45932bce`, on `origin/DPES-751`) is the escalation fix for this test. Run 20261102-r2 (MODE targeted, `NOTES: rerun W-21`) showed exactly that: exit 0 in 20 s, 16 passed, 0 failed, 7 ignored. W-50 reruns the whole binary under the same gate (not run in r2). The owner ruling above applies: a child gone after the ladder passes. First run: exit 101; `escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait` (`crates/mcao-backend-windows/tests/interrupt.rs:512`) panicked at `interrupt.rs:523` with `assertion left == right failed; left: Exited; right: TerminatedJob`. The state recorded after the stop-or-kill ladder was `Exited`, not `TerminatedJob`. The packet does not settle whether this is a harness defect or the Unsupported/Unresolved case from 752-AC3, so the row stays a recorded failure, marked UNRESOLVED. The test is not retried automatically: this row runs it once. W-49 and W-50 are the only reruns (section 4.8); each runs only when the `NOTES` key line is exactly `NOTES: rerun W-21`, in either mode, and `ROWS` naming either does not trigger it. Record the failing test name, the panic text, and the state. Do not treat it as a harness bug or as a known failure. | Log | 20 min | 755-AC2, 752-AC3 | VERIFIED-ON-WINDOWS (run 20261102-r2: 16 passed, 0 failed, 7 ignored); first run UNRESOLVED (run 20261102-150124) |
| W-22 | `cargo test -p mcao-backend-windows --locked --test conpty` | 25 (20 passed, 5 ignored). Run 20261102-150124: exit 0, under pwsh 7 in a console host, not elevated. | Pass in an interactive console. ConPTY needs a real console host; if the test reports the console or pseudo console API is unavailable, mark `SKIPPED` with the error text. | Log, console host type | 20 min | 755 (terminal), 885 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-23 | `cargo test -p mcao-backend-windows --locked --test console_pipe` | 7. Run 20261102-150124: 7 passed. | Pass. The CI console lane runs the same command (`ci.yml:838`); the CI guard fails on `matched no native tests` (`ci.yml:834`), which this row repeats. Covers pipe sessions, partial frames, stalled input, and idle disconnect (`crates/mcao-backend-windows/tests/console_pipe.rs:6`, `#![cfg(windows)]`). | Log | 20 min | 883-AC4, AC5, AC6, 885-AC1-AC3 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-24 | `cargo test -p mcao-backend-windows --locked --test pipe` | 3. Run 20261102-150124: 3 passed. | Pass (`ci.yml:839`). | Log | 20 min | 883-AC4-AC6 (pipe lifecycle) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-25 | `cargo test -p mcao-backend-windows --locked --lib console_server` | 13 (`console_server::tests::*`, the same 13 the macOS host lists). Run 20261102-150124: 13 passed. | Pass (`ci.yml:840`). Ring buffer, codec, and ACL unit tests in `crates/mcao-backend-windows/src/console_server.rs`. | Log | 20 min | 883-AC1-AC3, AC7-AC9, AC22 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-26 | `cargo test -p mcao-backend-windows --locked --lib` | 88 on Windows (87 passed, 1 ignored in run 20261102-150124). The macOS host lists 18; the Windows library binary has more `cfg(windows)` tests. | Pass (all library tests, zero failures). | Log | 20 min | 883 (unit) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-27 | `cargo test -p mcao-probe --locked --test native` | 6. Run 20261102-150124: 6 passed. | Pass. Native probe test file (Windows-gated cases at `crates/mcao-probe/tests/native.rs:18`, `:29`, `:36`). | Log | 20 min | 752-AC1 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |

### 4.4 Probe run with the stub harness and bash-free PATH

No Claude Code, no credentials, no network. This repeats the CI probe step (`rust-workspace-check.yml:190-250`) on the laptop, including the bash-free PATH removal. The CI step drops every PATH directory that holds `bash.exe`, `sh.exe`, or `wsl.exe`, and every MSYS or Cygwin directory. It runs the probe by absolute path inside the reduced PATH. Then it checks seven operation records against an expected support table.

The probe script is written to the run folder and run in a child shell. The parent session PATH is not changed.

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$probeScript = @'
param([string]$ProbeExe, [string]$OutDir)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$forbidden = 'bash.exe', 'sh.exe', 'wsl.exe'
$found = [System.Collections.Generic.SortedSet[string]]::new()
$kept = @()
$removed = 0
foreach ($dir in ($env:Path -split ';' | Where-Object { $_ })) {
  $hits = @($forbidden | Where-Object { Test-Path -LiteralPath (Join-Path $dir $_) })
  $unixlike = $dir -match '(?i)[\\/](msys64|msys2|cygwin|cygwin64)([\\/]|$)'
  if ($hits.Count -gt 0 -or $unixlike) {
    $removed++
    foreach ($hit in $hits) { [void]$found.Add($hit) }
  } else {
    $kept += $dir
  }
}
$env:Path = $kept -join ';'
$missing = [ordered]@{}
foreach ($name in 'bash', 'sh', 'wsl') {
  $missing[$name] = $null -eq (Get-Command $name -ErrorAction SilentlyContinue)
}
$proof = [ordered]@{
  removed_directories     = $removed
  kept_directories        = $kept.Count
  forbidden_found_on_path = @($found)
  command_missing_after   = $missing
}
$proof | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $OutDir 'bash-free-path.json')
if ($missing.Values -contains $false) { throw 'a forbidden shell is still on PATH' }
$evidence = Join-Path $OutDir 'probe-evidence.json'
& $ProbeExe --backend native --harness stub --scenario cooperative --evidence-out $evidence
if ($LASTEXITCODE -ne 0) { throw "mcao-probe exited $LASTEXITCODE" }
$expected = [ordered]@{
  spawn       = 'demonstrated'
  capture     = 'unsupported'
  send_text   = 'unsupported'
  interrupt   = 'unsupported'
  agent_state = 'demonstrated'
  reconnect   = 'unsupported'
  cleanup     = 'demonstrated'
}
$records = @(Get-Content -Raw $evidence | ConvertFrom-Json)
if ($records.Count -ne $expected.Count) { throw "expected $($expected.Count) records, got $($records.Count)" }
$i = 0
foreach ($name in $expected.Keys) {
  $record = $records[$i++]
  if ($record.operation -ne $name -or $record.support -ne $expected[$name]) {
    throw "operation $($record.operation): support $($record.support), expected $name $($expected[$name])"
  }
  if ($record.execution -ne 'native' -or $record.harness -ne 'stub' -or $record.backend -ne 'windows-job') {
    throw "operation $($record.operation) is not native windows-job stub evidence"
  }
}
Write-Output 'probe expectations matched'
'@
$probeScriptPath = Join-Path $Results 'bash-free-probe.ps1'
Set-Content -LiteralPath $probeScriptPath -Value $probeScript -Encoding utf8
```

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-28 | `& $shell -NoProfile -ExecutionPolicy Bypass -File bash-free-probe.ps1 -ProbeExe <probe> -OutDir <results>` where `$shell = (Get-Process -Id $PID).Path` and `<probe>` is `$Target\x86_64-pc-windows-msvc\debug\mcao-probe.exe` | not a Cargo test | Exit 0. The output line `probe expectations matched` appears. `probe-evidence.json` has seven records in this order: `spawn` demonstrated, `capture` unsupported, `send_text` unsupported, `interrupt` unsupported, `agent_state` demonstrated, `reconnect` unsupported, `cleanup` demonstrated (`rust-workspace-check.yml:238-246`). Any other support value for an operation is a FAIL. Run 20261102-150124: exit 0, all seven records matched, every record `native`, `stub`, `windows-job`. | `logs\W-28.log`, `probe-evidence.json`, `bash-free-path.json` | 5 min | 752-AC1 (partial), 752-AC3 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-29 | Read `bash-free-path.json` written by W-28 and check that `command_missing_after` has `true` for `bash`, `sh`, and `wsl` | not a Cargo test | Pass when all three values are `true`. Record the counts `removed_directories` and `kept_directories` and the executable names in `forbidden_found_on_path`; never record directory paths. Run 20261102-150124: `removed_directories=5 kept_directories=49`, `forbidden_found_on_path=bash.exe, wsl.exe`, all three commands missing after. | `bash-free-path.json` (counts only) | 2 min | 759-AC4 (partial, via the probe run) | VERIFIED-ON-WINDOWS (run 20261102-150124) |

Run W-28 with the probe's own flags exactly as CI does. It does not contact any harness. Do not add `-ErrorAction` changes to the probe script.

### 4.5 Platform leakage guard

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-30 | `cargo test -p mcao-probe --locked --test platform_leakage` | 12 (the macOS host lists 12). Run 20261102-150124: 12 passed. | Pass. The guard parses dependency gates and rejects any gate other than `cfg(unix)` or `cfg(windows)` (`crates/mcao-probe/tests/platform_leakage.rs:282`). A version-pinned support record covers third-party crates (PR 418). | Log | 20 min | 752-AC4 (guard test), 759 | VERIFIED-ON-WINDOWS (run 20261102-150124) |

### 4.6 Audit writer and rejection log on Windows

These rows cover the DPES-753 writer and DPES-884 validator behaviour that PR 456 changed after the first run. They run only test binaries or the `mcao.exe` CLI on temporary folders; no row writes to a real home.

The per-binary rows below state the number of tests a Windows run executes. A count of `0` is the expected result for a binary that is entirely `cfg(unix)`: it is a PASS with exit 0 and `running 0 tests`. The counts come from reading the gates and from the host listing (`cargo test -p mcao-cli --locked --test <name> -- --list`); the Windows-side counts are SPEC-ONLY until a run shows them.

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-41 | `cargo test -p mcao-cli --locked --no-fail-fast --test audit_writer_cli` | 0 on Windows (`running 0 tests`). The file is `#![cfg(unix)]` (`crates/mcao-cli/tests/audit_writer_cli.rs:14`); 20 tests on the macOS host (`cargo test -p mcao-cli --locked --test audit_writer_cli -- --list` lists 20). | PASS = exit 0 with `running 0 tests`. Confirms the writer CLI tests do not run on Windows. Any `running N` with N of at least 1 is an unexpected change and is recorded. | Log | 20 min | 753 (writer CLI, unix) | SPEC-ONLY (count from `#![cfg(unix)]` and host listing) |
| W-42 | `cargo test -p mcao-cli --locked --no-fail-fast --test audit_writer_parity` | 0 on Windows (`running 0 tests`). `#![cfg(unix)]` at `crates/mcao-cli/tests/audit_writer_parity.rs:15`; 1 test on the macOS host. | PASS = exit 0 with `running 0 tests`. | Log | 20 min | 753 (writer parity, unix) | SPEC-ONLY (count from `#![cfg(unix)]` and host listing) |
| W-43 | `cargo test -p mcao-cli --locked --no-fail-fast --test hook_onedrive_guard` | 1 on Windows. 5 tests on the host. Four are `cfg(unix)`: `a_onedrive_home_stands_down_session_start_without_writing` (`hook_onedrive_guard.rs:120-122`), `a_refused_session_start_keeps_the_pi_prerequisite_stand_down_code` (`:149-151`), `a_name_only_sync_folder_does_not_stop_session_start` (`:175-177`), and `a_name_only_sync_folder_still_records_host_mirror` (`:241-243`). The ungated one is `a_onedrive_home_stands_down_host_mirror_without_recording` (`:207`). | PASS = exit 0 and `running 1 test`. | Log | 20 min | 892-AC4 (hook), 753 | SPEC-ONLY (count from the `cfg(unix)` gates read at the lines given) |
| W-44 | `cargo test -p mcao-cli --locked --no-fail-fast --test onedrive_guard` | 6 on Windows (no `cfg` gate on any test; 6 on the host). In `an_unset_home_keeps_the_unix_rejection_message` (`crates/mcao-cli/tests/onedrive_guard.rs:231`) the assertions sit under `if cfg!(unix)` (`:244`), so on Windows that test runs the command and asserts only its exit path, not the rejection text. | PASS = exit 0 and `running 6 tests`. The Windows run does not check the unset-home rejection text; the report says so. | Log | 20 min | 892-AC4, AC5 | SPEC-ONLY (count from the host listing; `cfg!(unix)` branch read at `:244`) |
| W-45 | `cargo test -p mcao-cli --locked --no-fail-fast --test hook_host_mirror` | 4 on Windows (21 on the host). After PR 464 (`5fb482ef`) 18 host tests are `cfg(unix)` and one is `cfg(not(unix))`: `prompt_submit_records_nothing_when_the_mirror_lock_is_unsupported`, the Windows lock-stub case. The Windows set is that test plus the three negative tests (`unreadable_mirror…`, `claude_foreign…`, `stands_down…`). Run 20261102-r1 (before PR 464) ran 13 and failed 10; run 20261102-150124 never ran W-45. | PASS = exit 0, `running 4 tests`, and `test result: ok. 4 passed`. | Log | 20 min | 753, 894 (host mirror writer) | SPEC-ONLY (count from the `cfg(unix)` gates read in the source) |
| W-46 | `cargo test -p mcao-cli --locked --no-fail-fast --test audit_baseline_parity` | 0 on Windows (`running 0 tests`). `#![cfg(unix)]` at `crates/mcao-cli/tests/audit_baseline_parity.rs:22`; 1 test on the macOS host. | PASS = exit 0 with `running 0 tests`. | Log | 20 min | 753 (baseline parity, unix) | SPEC-ONLY (count from `#![cfg(unix)]` and host listing) |
| W-47 | `mcao.exe audit append`, `audit verify`, and `audit project-ledger` on Windows. Each with a `--store` path that does not exist, under `$Root`, and an input file from the clone (`crates/mcao-cli/tests/fixtures/audit-validate/envelope-day-key-mismatch.json`, read only). Run each as `& $mcao audit append --store $store $fixture` (and the same shape for `verify --store $store` and `project-ledger --store $store`). Run each command as its own `Invoke-Row -MinTests 0` with IDs `W-47-append`, `W-47-verify`, and `W-47-project-ledger`. | not a Cargo test | Exit 7 for each command. The line on stderr is `mcao audit: Unsupported: windows does not support the audit store writer, whose lock needs flock`. The store directory is not created: `Test-Path -LiteralPath $store` is `False` afterwards. The refusal runs before any input is read or store is touched (`crates/mcao-cli/src/audit/writer.rs:55-56` for `run_append`, `:113-114` for `run_verify`, `:134-135` for `run_project_ledger`; `unlocked_platform_refusal`, the exit code 7 from `crates/mcao-core/src/error.rs:47`, the `Unsupported` message at `error.rs:62-63`). This is the documented Windows refusal: the store lock is `flock`, which only unix provides. | `logs\W-47-append.log`, `W-47-verify.log`, `W-47-project-ledger.log`, `Test-Path` result | 5 min | 753 (writer, Windows refusal) | SPEC-ONLY (read from `writer.rs` and `error.rs`; not run) |
| W-48 | `mcao.exe audit validate` with `FM_HOME` and `FM_STATE_OVERRIDE` unset and a scratch `LOCALAPPDATA` under `$Root`. Run it in two cases through the child script below (`-NoProfile -ExecutionPolicy Bypass -File`), which removes `FM_HOME` and `FM_STATE_OVERRIDE` and sets `LOCALAPPDATA`. Case A (`W-48-a`): `$Root\w48-a\mcao\home\state` exists. Case B (`W-48-b`): it does not. | not a Cargo test | Case A: exit 1; stdout is exactly `{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/actor/id"]}` (fixture `crates/mcao-cli/tests/fixtures/audit-validate/envelope-actor-mismatch.json`, PR 463; the Python reference gives the same line on macOS, and the unix Rust binary is byte-identical to it); the file `$Root\w48-a\mcao\home\state\audit-rejections.jsonl` exists afterwards. Case B: exit 2; stdout is empty; stderr contains `rejection not recorded`, and nothing is written to `$Root\w48-b`. On Windows an unset `FM_HOME` selects the default home `%LOCALAPPDATA%\mcao\home` (`crates/mcao-cli/src/audit.rs:239-250`; test `a_rejection_is_recorded_under_the_windows_default_home`, `audit_validate.rs:318-320`; `docs/audit-envelope.md:88`). The row never writes to the real `%LOCALAPPDATA%`. | `logs\W-48-a.log`, `logs\W-48-b.log`, the rejection file (counts only) | 5 min | 884 (validator on Windows), 759 | SPEC-ONLY (read from `audit.rs` and the Windows-only test; not run) |
| W-48-c | `mcao.exe audit validate` on `envelope-day-key-mismatch.json` with the W-48 child script and a state directory present (run as `W-48-c`) | not a Cargo test | Observation only, not a pass criterion. The fixture is zone-bearing and Windows has no zoneinfo database, so the expected result is exit 1 with stdout `{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/user/timezone"]}`. This is the owner-accepted gap (DPES-884 AC3 stays partial; no Windows zone support is planned). Run 20261102-r1 showed the same line for the same fixture in its W-48 case A (its SUMMARY, section 6); run 20261102-150124 did not show that line, because its W-17 failed earlier, at `audit_validate.rs:298`, on the stderr text check. | `logs\W-48-c.log` | 5 min | 884 AC3 (owner-accepted gap) | SPEC-ONLY (observation) |

The child script for W-48, written to the run folder:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$auditChildScript = @'
param([string]$Mcao, [string]$LocalAppData, [string]$Document)
Remove-Item Env:FM_HOME -ErrorAction SilentlyContinue
Remove-Item Env:FM_STATE_OVERRIDE -ErrorAction SilentlyContinue
$env:LOCALAPPDATA = $LocalAppData
& $Mcao audit validate $Document
exit $LASTEXITCODE
'@
$auditChildPath = Join-Path $Results 'audit-validate-child.ps1'
Set-Content -LiteralPath $auditChildPath -Value $auditChildScript -Encoding utf8
```

Run the two cases and remove their scratch folders afterwards. Both folders are under `$Root`, never the real `%LOCALAPPDATA%`:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$auditChildPath = Join-Path $Results 'audit-validate-child.ps1'
if (-not (Test-Path -LiteralPath $auditChildPath)) { throw 'write the W-48 child script first (the block above)' }
if (-not (Get-Command Invoke-Row -ErrorAction SilentlyContinue)) { throw 'load the section 3 helper in this session first' }
$mcao = Join-Path $Target 'debug\mcao.exe'
$shell = (Get-Process -Id $PID).Path
$fixture = Join-Path $Clone 'crates\mcao-cli\tests\fixtures\audit-validate\envelope-actor-mismatch.json'
$dayKeyFixture = Join-Path $Clone 'crates\mcao-cli\tests\fixtures\audit-validate\envelope-day-key-mismatch.json'
New-Item -ItemType Directory -Force -Path (Join-Path $Root 'w48-a\mcao\home\state') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $Root 'w48-b') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $Root 'w48-c\mcao\home\state') | Out-Null
Invoke-Row -Id 'W-48-a' -MinTests 0 -Command 'mcao audit validate, unset FM_HOME, state directory present' -Action { & $shell -NoProfile -ExecutionPolicy Bypass -File $auditChildPath -Mcao $mcao -LocalAppData (Join-Path $Root 'w48-a') -Document $fixture }
Invoke-Row -Id 'W-48-b' -MinTests 0 -Command 'mcao audit validate, unset FM_HOME, state directory absent' -Action { & $shell -NoProfile -ExecutionPolicy Bypass -File $auditChildPath -Mcao $mcao -LocalAppData (Join-Path $Root 'w48-b') -Document $fixture }
Invoke-Row -Id 'W-48-c' -MinTests 0 -Command 'mcao audit validate, day-key fixture, unset FM_HOME, state directory present (observation)' -Action { & $shell -NoProfile -ExecutionPolicy Bypass -File $auditChildPath -Mcao $mcao -LocalAppData (Join-Path $Root 'w48-c') -Document $dayKeyFixture }
Remove-Item -LiteralPath (Join-Path $Root 'w48-a'), (Join-Path $Root 'w48-b'), (Join-Path $Root 'w48-c') -Recurse -Force -ErrorAction SilentlyContinue
```

### 4.7 CLI offline behaviour and FM_HOME rules (no network)

Run these from a PowerShell window. Each `preflight` row runs in a child shell with the environment set for that row only, so the parent session keeps its own PATH, `FM_HOME`, and OneDrive variables. The child shell helper is written to the run folder.

Preflight order, read from code: Git first (`crates/mcao-cli/src/preflight.rs:39`), then `gh` (`:41`), then `FM_HOME` (`:44`). Git and `gh` are looked up on `PATH` only as `.exe` or `.com` files (`crates/mcao-core/src/preflight/prereq.rs:23-26,120-125`). A `.cmd`, `.bat`, or `.ps1` file is never found. Preflight never runs `gh` or `git`. Preflight checks the text of `FM_HOME` (length, OneDrive rules); it does not require the folder to exist.

Therefore a `gh` stub for the preflight rows must be a file named `gh.exe`. Its content does not matter because preflight never executes it. The `cli` tests use the same fixture (`crates/mcao-cli/tests/preflight.rs:31-50`, which writes `name.EXE` with the text `not a program`).

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$mcao = Join-Path $Target 'debug\mcao.exe'
$GhStub = Join-Path $Results 'gh-stub'
New-Item -ItemType Directory -Force -Path $GhStub | Out-Null
Set-Content -LiteralPath (Join-Path $GhStub 'gh.exe') -Value 'not a program' -Encoding ascii
```

Use the stub only when the real `gh` is absent (`Get-Command gh -ErrorAction SilentlyContinue` returns nothing). The stub is for the preflight rows W-35, W-36, and W-37 only. Never put it on `PATH` for any other command, and never use it to run anything that would contact GitHub. Remove `$GhStub` at the end of section 4.7.

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$shell = (Get-Process -Id $PID).Path
$preflightScript = @'
param([string]$Mcao, [string]$PathValue, [string]$FmHome, [string]$OneDriveVar, [string]$OneDriveValue, [string]$UserProfile, [switch]$ExpectGitMissing)
$env:Path = $PathValue
if ($ExpectGitMissing -and (Get-Command git -ErrorAction SilentlyContinue)) { Write-Output 'git still found on PATH; stop'; exit 9 }
Remove-Item Env:FM_HOME -ErrorAction SilentlyContinue
foreach ($name in 'OneDrive', 'OneDriveCommercial', 'OneDriveConsumer') { Remove-Item -Path ("Env:" + $name) -ErrorAction SilentlyContinue }
if ($FmHome -ne '') { $env:FM_HOME = $FmHome }
if ($OneDriveVar -ne '') { Set-Item -Path ("Env:" + $OneDriveVar) -Value $OneDriveValue }
if ($UserProfile -ne '') { $env:USERPROFILE = $UserProfile }
& $Mcao preflight
exit $LASTEXITCODE
'@
$preflightPath = Join-Path $Results 'preflight-child.ps1'
Set-Content -LiteralPath $preflightPath -Value $preflightScript -Encoding utf8
```

Run a row through the helper, with the arguments named in the row. The child's exit code is the row's exit code:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
if (-not (Get-Command Invoke-Row -ErrorAction SilentlyContinue)) { throw 'load the section 3 helper in this session first' }
$shell = (Get-Process -Id $PID).Path
$mcao = Join-Path $Target 'debug\mcao.exe'
$preflightPath = Join-Path $Results 'preflight-child.ps1'
if (-not (Test-Path -LiteralPath $preflightPath)) { throw 'write the preflight child script first (section 4.7 block above)' }
$GhStub = Join-Path $Results 'gh-stub'
$fmHome = Join-Path $env:TEMP 'mcao winverify 有人 ÄÖ'
$pathValue = $env:Path
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { $pathValue = "$GhStub;$env:Path" }
New-Item -ItemType Directory -Force -Path $fmHome | Out-Null
Invoke-Row -Id 'W-35' -MinTests 0 -Command 'mcao preflight, spaces and non-ASCII FM_HOME' -Action { & $shell -NoProfile -ExecutionPolicy Bypass -File $preflightPath -Mcao $mcao -PathValue $pathValue -FmHome $fmHome -OneDriveVar '' -OneDriveValue '' -UserProfile '' }
Remove-Item -LiteralPath $fmHome -Recurse -Force -ErrorAction SilentlyContinue
```

`$pathValue` is the session `PATH`, with the stub directory in front only when the real `gh` is absent. The `git` directory must stay on `PATH` for every row except W-34.

W-36 uses the same child helper with a UNC `FM_HOME`. It runs after W-35 in the same session. The one share read is the existence check described in its row:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
if (-not (Get-Command Invoke-Row -ErrorAction SilentlyContinue)) { throw 'load the section 3 helper in this session first' }
$shell = (Get-Process -Id $PID).Path
$mcao = Join-Path $Target 'debug\mcao.exe'
$preflightPath = Join-Path $Results 'preflight-child.ps1'
if (-not (Test-Path -LiteralPath $preflightPath)) { throw 'write the preflight child script first (section 4.7 block above)' }
$GhStub = Join-Path $Results 'gh-stub'
$uncHome = '\\localhost\c$\Users\' + $env:USERNAME + '\AppData\Local\Temp\mcao-winverify-unc'
$pathValue = $env:Path
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { $pathValue = "$GhStub;$env:Path" }
if (-not (Test-Path -LiteralPath '\\localhost\c$\Users')) {
  "W-36`tSKIPPED`t`t`tvacuous=n/a`tadmin share unreachable" | Add-Content -LiteralPath $RowsTsv -Encoding utf8
} else {
  Invoke-Row -Id 'W-36' -MinTests 0 -Command 'mcao preflight, UNC FM_HOME as text only' -Action { & $shell -NoProfile -ExecutionPolicy Bypass -File $preflightPath -Mcao $mcao -PathValue $pathValue -FmHome $uncHome -OneDriveVar '' -OneDriveValue '' -UserProfile '' }
}
```

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-31 | `& $mcao --version` | not a Cargo test | Exit 0, one version line. No file is created under the home. Run 20261102-150124: exit 0. Test `version_flag_prints_the_package_version_and_exits_zero` exists on the macOS host. | Output, exit code | 1 min | 754-AC5 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-32 | `& $mcao --help` | not a Cargo test | Exit 0, help text. Run 20261102-150124: exit 0. Test `help_flag_lists_util_and_succeeds_without_a_home` exists on the macOS host. | Output, exit code | 1 min | 754-AC5 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-33 | `& $mcao preflight` with Git and a real `gh` on `PATH`, `FM_HOME` absent | not a Cargo test | Exit 0. Lines `Git: ok (...)`, `Gh: ok (...)` (capital `G` as printed by `crates/mcao-cli/src/preflight.rs:42`), and `Preflight: all checks passed`. Run 20261102-150124: exit 0 with the real `gh` (`C:\Program Files\GitHub CLI\gh.exe`). **SKIPPED with the reason `gh absent` when the real `gh` is not installed.** The missing-`gh` branch is covered by `preflight_exits_three_and_names_gh_when_it_is_missing_from_path` (`crates/mcao-cli/tests/preflight.rs:119`) in W-13. | Output, exit code | 1 min | 754-AC1 (partial) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-34 | Git missing: run the child with `-PathValue "$env:SystemRoot\System32"`, `-ExpectGitMissing`, and no `gh` stub. The child script stops with exit 9 if `git` is still found. | not a Cargo test | Exit 3. Output names `Git: MISSING` and the remedy `install Git from https://git-scm.com and open a new terminal` (`crates/mcao-core/src/preflight/prereq.rs:17`; test `crates/mcao-cli/tests/preflight.rs:105`). Output does not contain `Gh:`, because `gh` is checked after Git (`crates/mcao-cli/src/preflight.rs:39-41`). Run 20261102-150124: exit 3, `Preflight check failed: Git: MISSING (not found on PATH); install Git from https://git-scm.com and open a new terminal`, no `Gh:` line. | Output, exit code | 2 min | 754-AC2 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-35 | FM_HOME with spaces and non-ASCII: `$fmHome = Join-Path $env:TEMP 'mcao winverify 有人 ÄÖ'`; create the folder; run the child with `-PathValue` including Git and the real `gh` or the stub, and `-FmHome $fmHome` | not a Cargo test | Exit 0. The line `FM_HOME path length: ok (<path>)` and `FM_HOME OneDrive: ok (<path>)` print the path unchanged (`crates/mcao-cli/src/preflight.rs:83,106`; test `preflight_prints_an_fm_home_with_spaces_and_non_ascii_exactly`, `crates/mcao-cli/tests/preflight.rs:161`). Run 20261102-150124 printed `FM_HOME path length: ok (C:\Users\<user>\AppData\Local\Temp\mcao winverify 有人 ÄÖ)` with exit 0 and the real `gh`. Record the printed path line with the user name replaced by `<user>`. Remove the folder afterwards. **SKIPPED with the reason `gh absent and no stub` only if neither a real `gh` nor the stub is used.** | Output, exit code | 2 min | 754-AC3 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-36 | FM_HOME as a UNC path inside the user's own temp folder, through the local administrative share name: `\\localhost\c$\Users\<user>\AppData\Local\Temp\mcao-winverify-unc`, where `<user>` is `$env:USERNAME`. The row runs one existence check on the admin share, `Test-Path -LiteralPath '\\localhost\c$\Users'`: a read of the share root, with no listing, open, create, or write on the share and no check of the UNC folder itself. If that is `False`, the row is `SKIPPED` with the reason `admin share unreachable`. The UNC `FM_HOME` is passed to `mcao preflight` as text only. Preflight does no filesystem access on `FM_HOME` (its FM_HOME checks read the path text and the environment; no create, read, or metadata call in `crates/mcao-cli/src/preflight.rs` or `crates/mcao-core/src/preflight.rs`), so the row creates nothing. | not a Cargo test | Expected exit 0, with stderr empty and these lines in order: `FM_HOME path length: ok (<path>)`, `FM_HOME OneDrive: ok (<path>)`, `FM_HOME UNC: warning: operational home <path> is a UNC share path; state, locks, and atomic renames over a network share are not safe to assume; set FM_HOME to %LOCALAPPDATA%\mcao\home or another local folder`, then `Preflight: all checks passed`. The UNC warning comes from PR 467 (`f79b2c59`, on `origin/DPES-751`, merged into this packet branch): `crates/mcao-cli/src/preflight.rs:109-111` prints it after the OneDrive line, from `check_home_not_unc` (`crates/mcao-core/src/preflight.rs:373`) and `unc_warning` (`crates/mcao-core/src/preflight.rs:393`). The warning does not change the exit code (test `preflight_warns_and_passes_for_a_unc_home_without_touching_the_share`, `crates/mcao-cli/tests/preflight.rs:423`). A missing warning line is a FAIL of this row. Run 20261102-r1 ran before PR 467 and showed exit 0 and the two `ok` lines with no warning line; that is the pre-warning behaviour and is not evidence for the warning. The first run (`\\localhost\c$\mcao-winverify-fmhome`, a folder created on the system drive root and then removed) showed exit 0 with the two `ok` lines; that run created a folder on the admin share, which this row no longer does. | Output, exit code | 2 min | 892 (UNC warning, PR 467) | SPEC-ONLY (expects the PR 467 warning; never run natively with it) |
| W-37 | OneDrive root, for each variable name in `OneDrive`, `OneDriveCommercial`, `OneDriveConsumer` in turn: set that variable to `Join-Path $env:TEMP 'OneDrive-winverify'` in the child, create that folder, and set `FM_HOME` to a folder under it | not a Cargo test | Exit 3 for each of the three variables. Output names `FM_HOME OneDrive` and the remedy `%LOCALAPPDATA%\mcao\home` (`crates/mcao-core/src/preflight.rs:33-35`, `ONEDRIVE_RELOCATION`; test `preflight_exits_three_naming_onedrive_for_a_home_under_a_onedrive_root`, `crates/mcao-cli/tests/preflight.rs:400`). This is the owner ruling: a home under a positively detected OneDrive root is an error. Run 20261102-150124: exit 3 for each of the three variables. **Needs the real `gh` or the stub, otherwise SKIPPED** (preflight stops at `Gh:` before `FM_HOME`). | Output, exit code (one per variable) | 2 min | 892-AC4 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-37b | OneDrive profile folder (warning only): child with `-UserProfile` set to `Join-Path $env:TEMP 'mcao-winverify-profile'`, `FM_HOME` set to `<profile>\OneDrive - winverify\home`, and the three OneDrive variables removed | not a Cargo test | Exit 0. Output contains `FM_HOME OneDrive: warning:` and `beneath USERPROFILE` (`crates/mcao-cli/tests/preflight.rs:328`, test `preflight_warns_and_passes_for_a_home_beneath_the_profile_sync_folder`). Run 20261102-150124: exit 0 with the warning line. A profile folder name alone only warns; this is not an error. Needs the real `gh` or the stub. | Output, exit code | 2 min | 892-AC4 | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-38 | `cargo test -p mcao-core --locked --test preflight the_default_home` | 2 (`running 2 tests`). Run 20261102-150124: 2 passed. | Pass: `the_default_home_is_the_mcao_home_under_localappdata` (`crates/mcao-core/tests/preflight.rs:108`) and `the_default_home_needs_an_absolute_localappdata` (`:121`). Unit tests only: `mcao preflight` prints no home line (`crates/mcao-cli/src/preflight.rs:44-47` prints checks only), so there is no CLI observation of the default home. | Log | 2 min | 754-AC5, 892-AC8 (partial, unit level) | VERIFIED-ON-WINDOWS (run 20261102-150124) |
| W-39 | Overlong home: covered by W-13 (`preflight_exits_three_naming_the_path_length_check_for_an_overlong_home`, `crates/mcao-cli/tests/preflight.rs:186`, `cfg(windows)`). Record W-13 as a reference here, no separate run. | reference only | Reference only | none | 0 | 754 (path budget) | VERIFIED-BY-READING |

After section 4.7, remove the stub and the temporary folders:

```powershell
Remove-Item -LiteralPath $GhStub -Recurse -Force -ErrorAction SilentlyContinue
```

### 4.8 Reruns of the UNRESOLVED interrupt test (W-49, W-50)

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-49 | Twenty runs of one test, each one a separate `cargo test` process: `cargo test -p mcao-backend-windows --locked --test interrupt escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait -- --exact` | 1 per run (`running 1 test`) | Expected 20 of 20 passing after PR 460 (`45932bce`, on `origin/DPES-751`). Run 20261102-r2 (`NOTES: rerun W-21`) recorded 20 of 20 passing, exit 0 each, no TIMEOUT and no panic text. A failure means the PR 460 fix did not work: record every panic text. Runs only when the `NOTES` key line of the Current request section is exactly `NOTES: rerun W-21`, in either mode; `ROWS` naming W-49 does not trigger it. Otherwise record `SKIPPED`, reason `not requested`. When it runs, record for each run its number, exit code, and the `test result` line. A run killed at its 10-minute limit is recorded with exit `TIMEOUT`, has no `test result` line, and counts as a failed run. Count the passes and failures. For every failing run, record the panic text: the `panicked at` line with its file and line, and the `left:` and `right:` lines. The test name and its assertion are at `crates/mcao-backend-windows/tests/interrupt.rs:512` and `:523`. Do not add `--ignored`, change the test, or stop after the first failure. The run keeps the failures recorded, marked UNRESOLVED, until a later run settles it. Write the counts into `W-49-summary.txt` in the run folder, and one `rows.tsv` line per run (`W-49-run<n>`), as every other row gets. Timing: each run has a 10-minute limit. The row's 30-minute budget starts with run 1 and also caps the last run. A run that reaches its limit has its process tree killed (`taskkill /T /F`, which also stops the `interrupt` test process it started), and the loop goes on to the next run. When the budget ends the loop, the loop stops, writes a `W-49-row` TIMEOUT line to `rows.tsv` and a line to `W-49-summary.txt`, and the row ends with the runs completed so far. | `logs\W-49-rerun.txt` (all runs), `W-49-summary.txt` (counts and failure text), `rows.tsv` (W-49-run lines) | 30 min | 755-AC2, 752-AC3 | VERIFIED-ON-WINDOWS (run 20261102-r2: 20 of 20 passing, after PR 460); the first-run W-21 failure is UNRESOLVED (run 20261102-150124) |
| W-50 | `cargo test -p mcao-backend-windows --locked --test interrupt` (the whole binary, default test threads), three separate runs: `W-50-run1` to `W-50-run3` | 23 per run (`running 23 tests`) | Pass: each run exits 0 with `test result: ok. 16 passed; 0 failed; 7 ignored`. Gate: the same as W-49. Runs only when the `NOTES` key line is exactly `NOTES: rerun W-21`, in either mode; `ROWS` naming W-50 does not trigger it, and otherwise W-50 is recorded `SKIPPED`, reason `not requested`. When it runs, one run of this binary took 13 s in run 20261102-150124 (W-21), so the three runs take about 40 s. This row checks the escalation fix (PR 460) at the default thread count on the whole binary. A failing run keeps its panic text; it is not retried. | `logs\W-50-run<n>.log`, `rows.tsv` (W-50-run lines) | 3 min | 755-AC2, 752-AC3 | SPEC-ONLY (PR 460 fix; not run in run 20261102-r2, whose packet had no W-50 row) |

Use this loop shape. It writes one line per run and keeps the failure text:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path $Results | Out-Null
$env:CARGO_TARGET_DIR = $Target
$packetNotes = @(Get-Content -LiteralPath (Join-Path $Clone 'docs\windows-native-verification.md') -ErrorAction Stop | Where-Object { $_ -cmatch '^NOTES: ' })
$RerunRequested = ($packetNotes.Count -gt 0) -and ($packetNotes[0] -ceq 'NOTES: rerun W-21')
$summary = Join-Path $Results 'W-49-summary.txt'
if (-not $RerunRequested) {
  'W-49 SKIPPED: reason not requested'
} else {
  $rerunLog = Join-Path $Results 'logs\W-49-rerun.txt'
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $rerunLog) | Out-Null
  "W-49 log file: $rerunLog" | Add-Content -LiteralPath $summary -Encoding utf8
  $rerunArgs = @('test', '-p', 'mcao-backend-windows', '--locked', '--test', 'interrupt', 'escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait', '--', '--exact')
  $rerunCommand = 'cargo test -p mcao-backend-windows --locked --test interrupt escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait -- --exact'
  $rowBudgetMs = 1800000
  $runLimitMs = 600000
  $passes = 0
  $failures = 0
  $rowTimedOut = $false
  $rowStart = Get-Date
  for ($run = 1; $run -le 20; $run++) {
    $spentMs = [int]((Get-Date) - $rowStart).TotalMilliseconds
    $limitMs = [Math]::Min($runLimitMs, $rowBudgetMs - $spentMs)
    if ($limitMs -le 0) { $rowTimedOut = $true; break }
    $outFile = Join-Path $Results "W-49-run$run.out"
    $errFile = Join-Path $Results "W-49-run$run.err"
    $runStart = Get-Date
    $proc = Start-Process -FilePath 'cargo' -ArgumentList $rerunArgs -WorkingDirectory $Clone -NoNewWindow -PassThru -RedirectStandardOutput $outFile -RedirectStandardError $errFile
    $finished = $proc.WaitForExit($limitMs)
    if (-not $finished) {
      & taskkill.exe /PID $proc.Id /T /F | Out-Null
      [void]$proc.WaitForExit(10000)
    }
    $secs = [int]((Get-Date) - $runStart).TotalSeconds
    $out = @()
    foreach ($file in $outFile, $errFile) {
      if (Test-Path -LiteralPath $file) {
        $out += @(Get-Content -LiteralPath $file)
        Remove-Item -LiteralPath $file
      }
    }
    $code = if ($finished) { $proc.ExitCode } else { 'TIMEOUT' }
    $ran = @($out | Where-Object { $_ -match '^running [1-9][0-9]* tests?$' }).Count
    if ($finished -and $code -eq 0) { $passes++ } else { $failures++ }
    "run=$run exit=$code" | Add-Content -LiteralPath $rerunLog -Encoding utf8
    $out | Add-Content -LiteralPath $rerunLog -Encoding utf8
    $out | Where-Object { $_ -match 'panicked at|left:|right:|test result' } | ForEach-Object { "run=$run $_" } | Add-Content -LiteralPath $summary -Encoding utf8
    if (-not $finished) { "run=$run TIMEOUT: killed with its process tree at $limitMs ms" | Add-Content -LiteralPath $summary -Encoding utf8 }
    "W-49-run$run`t$code`t$secs`tbinaries-with-tests=$ran`tvacuous=$($ran -eq 0)`t$rerunCommand" | Add-Content -LiteralPath $RowsTsv -Encoding utf8
    if (-not $finished -and $limitMs -lt $runLimitMs) { $rowTimedOut = $true; break }
  }
  if ($rowTimedOut) {
    "row TIMEOUT: the 30-minute row budget ended the loop at run $run" | Add-Content -LiteralPath $summary -Encoding utf8
    "W-49-row`tTIMEOUT`t`tbinaries-with-tests=0`tvacuous=n/a`trow stopped at its 30-minute budget at run $run" | Add-Content -LiteralPath $RowsTsv -Encoding utf8
  }
  "passes=$passes failures=$failures" | Add-Content -LiteralPath $summary -Encoding utf8
}
```

W-50 runs the whole binary three times, each as its own row so that each log and `rows.tsv` line is kept. Run it only when its gate is open (the W-50 row), in the same session after the section 3 helper:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$env:CARGO_TARGET_DIR = $Target
$packetNotes = @(Get-Content -LiteralPath (Join-Path $Clone 'docs\windows-native-verification.md') -ErrorAction Stop | Where-Object { $_ -cmatch '^NOTES: ' })
$RerunRequested = ($packetNotes.Count -gt 0) -and ($packetNotes[0] -ceq 'NOTES: rerun W-21')
if (-not $RerunRequested) {
  'W-50 SKIPPED: reason not requested'
} else {
  if (-not (Get-Command Invoke-Row -ErrorAction SilentlyContinue)) { throw 'load the section 3 helper in this session first' }
  Push-Location -LiteralPath $Clone
  foreach ($run in 1..3) {
    Invoke-Row -Id "W-50-run$run" -MinTests 1 -Command 'cargo test -p mcao-backend-windows --locked --test interrupt' -Action { cargo test -p mcao-backend-windows --locked --test interrupt }
  }
  Pop-Location
}
```

### 4.9 Diagnostic probes for the first run's open questions

Rows W-51 to W-57 are read-only observations. None has a pass or fail beyond its own recording. Each runs in a child process where stated, writes only under the run folder, and installs nothing (no `pip install`, no package, no installer). Each `Invoke-Row` is `-MinTests 0`. Each action ends by setting `$global:LASTEXITCODE` (`0`, the child's exit code, or `TIMEOUT`), so the recorded exit code is the one the action sets, not a stale value.

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-51 | child pwsh started with an environment that holds only `PATH` and `TMPDIR`: `[System.IO.Path]::GetTempPath()`, compared with the parent | not a Cargo test | Record only. Expected: the child reports the Windows directory (`C:\Windows\`), because `TEMP`, `TMP`, and `USERPROFILE` are absent and Windows ignores `TMPDIR`. This is the mechanism behind PR 463's `windows_temp_env()`. | `logs\W-51.log` | 2 min | 759 (assignment_reassess mechanism) | SPEC-ONLY observed |
| W-52 | start the comment-only `w52-probe.sh` in the run folder directly with `UseShellExecute` false (no shell) and read `Win32Exception.NativeErrorCode`. The file holds one comment line, so a launch that succeeds runs nothing; `bin\fm-sessionstart-nudge.sh` is not launched | not a Cargo test | Record only. Expected: `NativeErrorCode` 193 (`%1 is not a valid Win32 application`), the result for any `.sh` file that is not a Windows program, so the check is the same for the hook script. This is the hook launcher gap (`run_child`, product follow-up P3). A launch that starts and does not exit within 60 s is killed with its process tree and recorded `TIMEOUT`. | `logs\W-52.log` | 2 min | 759 (hook launcher, latent) | SPEC-ONLY observed |
| W-53 | `where.exe bash`, `bash --version`, and `test -r` on the backslash path and on the forward-slash path of `bin\fm-sessionstart-nudge.sh` (read only, the script is not run) | not a Cargo test | Record only. Expected: the backslash form is not readable from `bash` and the forward-slash form is. This decides the hook oracle fix in `hook_session_start.rs`. Record which `bash` ran (the full path printed as `bash used (full path)`). The Git for Windows `bash.exe` under `Program Files` is preferred and run by full path. `C:\Windows\System32\bash.exe` is the WSL launcher and is never started: when no Git for Windows or non-System32 bash exists, the bash steps are `SKIPPED` with the reason `WSL launcher only`. `wsl.exe` is never invoked. Each bash step is bounded (30 s): a step that does not exit is killed with its process tree and recorded `TIMEOUT`. | `logs\W-53.log` | 2 min | 759 (hook oracle) | SPEC-ONLY observed |
| W-54 | python: `zoneinfo.TZPATH`, whether the `tzdata` package is present (checked, never installed), `ZoneInfo('America/Chicago')`, and the reference `bin\fm-audit-envelope.py validate` on `envelope-day-key-mismatch.json` with `FM_STATE_OVERRIDE` under the run folder | not a Cargo test | Record only. If `python --version` does not exit 0 with a `Python x.y` line (the Microsoft Store alias stub fails that check), the row is `SKIPPED` with `python absent`. Each python step is bounded (30 s for `--version`, 60 s for the probe and the reference): a step that does not exit is killed with its process tree and the row is recorded `TIMEOUT`. Expected on a stock Windows Python without `tzdata`: `ZoneInfo` fails and the reference prints `schema_invalid /user/timezone`, which would match the Rust result. Not verified until this row runs. | `logs\W-54.log` | 2 min | 884 AC3 (zone gap) | SPEC-ONLY observed (or SKIPPED) |
| W-55 | `[System.IO.File]::Open` on a directory with read access, the open that `sync_dir` needs | not a Cargo test | Record only. Expected: `UnauthorizedAccessException` (access denied, os error 5). This confirms the mechanism behind PR 464's `audit_writer` gate and the owner decision about `sync_dir` on Windows. | `logs\W-55.log` | 2 min | 753 (writer, owner decision) | SPEC-ONLY observed |
| W-56 | child pwsh with an environment of `PATH`, `GIT_CONFIG_NOSYSTEM=1`, and `GIT_CONFIG_GLOBAL=/dev/null` only: `git init`, add, commit, then `git config --show-origin --list`; the exit code of each git step is recorded | not a Cargo test | Record only. Expected: every git step exits 0 and `git config --show-origin --list` names no file under the user profile, because `USERPROFILE` is absent in the child. This is the input to the W-06 `assignment_reassess*` question. | `logs\W-56.log` | 2 min | 759 (assignment_reassess, SPEC-ONLY) | SPEC-ONLY observed |
| W-57 | presence, length, and drive-rooted shape of `USERPROFILE`, `LOCALAPPDATA`, `OneDrive`, `OneDriveCommercial`, and `OneDriveConsumer` in the test process; no path text is printed | not a Cargo test | Record only. Expected: the three OneDrive variables are empty on this laptop, as the first run recorded. This is the hermetic environment reason in PR 464's `Command` changes. | `logs\W-57.log` | 1 min | 892 (guard environment) | SPEC-ONLY observed |

The probe block. Run it after the W-50 block, in the same session, after the section 3 helper:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
if (-not (Get-Command Invoke-Row -ErrorAction SilentlyContinue)) { throw 'load the section 3 helper in this session first' }
$shell = (Get-Process -Id $PID).Path
$nativeSh = Join-Path $Clone 'bin\fm-sessionstart-nudge.sh'
$pyProbePath = Join-Path $Results 'w54-zone-probe.py'
$gitChildPath = Join-Path $Results 'w56-git-child.ps1'
$harmlessSh = Join-Path $Results 'w52-probe.sh'
Set-Content -LiteralPath $harmlessSh -Value '# harmless probe file: a comment only, never run as a script' -Encoding ascii

function Invoke-ChildCapture {
  param([string]$FileName, [string]$Arguments, [hashtable]$ChildEnv, [int]$TimeoutMs = 120000)
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $FileName
  $psi.Arguments = $Arguments
  $psi.UseShellExecute = $false
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $psi.EnvironmentVariables.Clear()
  foreach ($key in $ChildEnv.Keys) { $psi.EnvironmentVariables[$key] = $ChildEnv[$key] }
  $proc = [System.Diagnostics.Process]::Start($psi)
  $stdoutTask = $proc.StandardOutput.ReadToEndAsync()
  $stderrTask = $proc.StandardError.ReadToEndAsync()
  if ($proc.WaitForExit($TimeoutMs)) {
    $global:LASTEXITCODE = $proc.ExitCode
    "child exit=$($proc.ExitCode)"
  } else {
    & taskkill.exe /PID $proc.Id /T /F | Out-Null
    [void]$proc.WaitForExit(10000)
    $global:LASTEXITCODE = 'TIMEOUT'
    "child exit=TIMEOUT: killed with its process tree at $TimeoutMs ms"
  }
  [void]$stdoutTask.Wait(10000)
  [void]$stderrTask.Wait(10000)
  "child stdout: $(if ($stdoutTask.IsCompleted) { $stdoutTask.Result })"
  "child stderr: $(if ($stderrTask.IsCompleted) { $stderrTask.Result })"
}

function Invoke-Bounded {
  param([string]$FileName, [string[]]$Arguments, [int]$TimeoutMs = 60000)
  $quoted = @($Arguments | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } })
  $base = Join-Path $Results ('bounded-' + [guid]::NewGuid().ToString('N'))
  try {
    $proc = Start-Process -FilePath $FileName -ArgumentList $quoted -NoNewWindow -PassThru -RedirectStandardOutput "$base.out" -RedirectStandardError "$base.err"
  } catch {
    "START FAILED: $($_.Exception.Message)"
    $global:LASTEXITCODE = 1
    return
  }
  if ($proc.WaitForExit($TimeoutMs)) {
    $global:LASTEXITCODE = $proc.ExitCode
  } else {
    & taskkill.exe /PID $proc.Id /T /F | Out-Null
    [void]$proc.WaitForExit(10000)
    $global:LASTEXITCODE = 'TIMEOUT'
    "TIMEOUT: killed with its process tree at $TimeoutMs ms"
  }
  foreach ($file in "$base.out", "$base.err") {
    if (Test-Path -LiteralPath $file) {
      Get-Content -LiteralPath $file
      Remove-Item -LiteralPath $file
    }
  }
}

$gitChildScript = @'
param([string]$Repo)
Set-Location -LiteralPath $Repo
git init -q . ; "init exit=$LASTEXITCODE"
Set-Content -LiteralPath f.txt -Value 'probe' -Encoding ascii
git add f.txt ; "add exit=$LASTEXITCODE"
git -c user.name=probe -c user.email=probe@example.invalid commit -q -m probe ; "commit exit=$LASTEXITCODE"
git config --show-origin --list ; "config-list exit=$LASTEXITCODE"
'@
Set-Content -LiteralPath $gitChildPath -Value $gitChildScript -Encoding utf8

$pyProbeScript = @'
import importlib.util
import sys
import zoneinfo
print("python", sys.version.split()[0], sys.platform)
print("TZPATH", zoneinfo.TZPATH)
print("tzdata installed:", importlib.util.find_spec("tzdata") is not None)
try:
    zoneinfo.ZoneInfo("America/Chicago")
    print("ZoneInfo America/Chicago: loads")
except Exception as error:
    print("ZoneInfo America/Chicago: fails", type(error).__name__, error)
'@
Set-Content -LiteralPath $pyProbePath -Value $pyProbeScript -Encoding utf8

Invoke-Row -Id 'W-51' -MinTests 0 -Command 'child pwsh, env PATH and TMPDIR only: GetTempPath' -Action {
  "parent GetTempPath: $([System.IO.Path]::GetTempPath())"
  Invoke-ChildCapture -FileName $shell -Arguments '-NoProfile -Command "[System.IO.Path]::GetTempPath()"' -ChildEnv @{ PATH = $env:Path; TMPDIR = (Join-Path $Root 'w51-tmpdir') }
}

Invoke-Row -Id 'W-52' -MinTests 0 -Command 'direct launch of the comment-only w52-probe.sh in the run folder, no shell' -Action {
  try {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $harmlessSh
    $psi.UseShellExecute = $false
    $proc = [System.Diagnostics.Process]::Start($psi)
    if ($proc.WaitForExit(60000)) {
      "started; exit=$($proc.ExitCode)"
      $global:LASTEXITCODE = 0
    } else {
      & taskkill.exe /PID $proc.Id /T /F | Out-Null
      [void]$proc.WaitForExit(10000)
      'TIMEOUT: started and did not exit within 60000 ms; killed with its process tree'
      $global:LASTEXITCODE = 'TIMEOUT'
    }
  } catch [System.ComponentModel.Win32Exception] {
    "Win32Exception NativeErrorCode=$($_.Exception.NativeErrorCode): $($_.Exception.Message)"
    $global:LASTEXITCODE = 0
  }
}

Invoke-Row -Id 'W-53' -MinTests 0 -Command 'bash: where.exe, bash --version, test -r on backslash and forward-slash paths (bounded; Git bash by full path; WSL launcher never started)' -Action {
  $timedOut = $false
  $whereLines = @(Invoke-Bounded -FileName 'where.exe' -Arguments @('bash') -TimeoutMs 30000)
  $whereLines
  if ("$LASTEXITCODE" -eq 'TIMEOUT') { $timedOut = $true }
  $bashExe = @("$env:ProgramFiles\Git\bin\bash.exe", "${env:ProgramFiles(x86)}\Git\bin\bash.exe", "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe") | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
  if (-not $bashExe) {
    $bashExe = $whereLines | Where-Object { $_ -match '\\bash\.exe$' -and $_ -notlike "$env:SystemRoot\*" } | Select-Object -First 1
  }
  if (-not $bashExe) {
    'SKIPPED: WSL launcher only (no Git for Windows bash and no bash outside System32); bash not started'
    $global:LASTEXITCODE = 0
    return
  }
  "bash used (full path): $bashExe"
  $versionLines = @(Invoke-Bounded -FileName $bashExe -Arguments @('--version') -TimeoutMs 30000)
  $versionLines
  if ("$LASTEXITCODE" -eq 'TIMEOUT') { $timedOut = $true }
  $fwd = $nativeSh -replace '\\', '/'
  foreach ($form in @($nativeSh, $fwd)) {
    "form=$form"
    $readLines = @(Invoke-Bounded -FileName $bashExe -Arguments @('-c', "test -r '$form' && echo readable || echo unreadable") -TimeoutMs 30000)
    $readLines
    if ("$LASTEXITCODE" -eq 'TIMEOUT') { $timedOut = $true }
  }
  if ($timedOut) { $global:LASTEXITCODE = 'TIMEOUT' } else { $global:LASTEXITCODE = 0 }
}

Invoke-Row -Id 'W-54' -MinTests 0 -Command 'python zone facts and reference on the day-key fixture (bounded)' -Action {
  $pyCommand = Get-Command python -ErrorAction SilentlyContinue
  if (-not $pyCommand) {
    'SKIPPED: python absent (no python on PATH)'
    $global:LASTEXITCODE = 0
    return
  }
  $pyExe = $pyCommand.Source
  $versionLines = @(Invoke-Bounded -FileName $pyExe -Arguments @('--version') -TimeoutMs 30000)
  $versionLines
  if ("$LASTEXITCODE" -eq 'TIMEOUT') { $global:LASTEXITCODE = 'TIMEOUT'; return }
  if ("$LASTEXITCODE" -ne '0' -or (($versionLines -join ' ') -notmatch 'Python \d')) {
    'SKIPPED: python absent (python --version did not exit 0 with a Python x.y line; the Microsoft Store alias fails this check)'
    $global:LASTEXITCODE = 0
    return
  }
  $zoneLines = @(Invoke-Bounded -FileName $pyExe -Arguments @($pyProbePath) -TimeoutMs 60000)
  $zoneLines
  if ("$LASTEXITCODE" -eq 'TIMEOUT') { $global:LASTEXITCODE = 'TIMEOUT'; return }
  $stateDir = Join-Path $Results 'w54-state'
  New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
  $fixture = Join-Path $Clone 'crates\mcao-cli\tests\fixtures\audit-validate\envelope-day-key-mismatch.json'
  $oldState = $env:FM_STATE_OVERRIDE
  $env:FM_STATE_OVERRIDE = $stateDir
  try {
    $refLines = @(Invoke-Bounded -FileName $pyExe -Arguments @((Join-Path $Clone 'bin\fm-audit-envelope.py'), 'validate', $fixture) -TimeoutMs 60000)
    $refLines
    $refCode = $LASTEXITCODE
    "reference exit=$refCode"
  } finally {
    $env:FM_STATE_OVERRIDE = $oldState
    Remove-Item -LiteralPath $stateDir -Recurse -Force -ErrorAction SilentlyContinue
  }
  if ("$refCode" -eq 'TIMEOUT') { $global:LASTEXITCODE = 'TIMEOUT' } else { $global:LASTEXITCODE = 0 }
}

Invoke-Row -Id 'W-55' -MinTests 0 -Command '[System.IO.File]::Open on a directory' -Action {
  $dir = Join-Path $Root 'w55-dir'
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  try {
    $stream = [System.IO.File]::Open($dir, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read)
    $stream.Dispose()
    'opened a directory handle'
  } catch {
    "exception: $($_.Exception.GetType().FullName): $($_.Exception.Message)"
  }
  Remove-Item -LiteralPath $dir -Recurse -Force
  $global:LASTEXITCODE = 0
}

Invoke-Row -Id 'W-56' -MinTests 0 -Command 'child git init, add, commit, config origins with PATH, GIT_CONFIG_NOSYSTEM=1, GIT_CONFIG_GLOBAL=/dev/null only' -Action {
  $repo = Join-Path $Root 'w56-repo'
  Remove-Item -LiteralPath $repo -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path $repo | Out-Null
  Invoke-ChildCapture -FileName $shell -Arguments "-NoProfile -ExecutionPolicy Bypass -File `"$gitChildPath`" -Repo `"$repo`"" -ChildEnv @{ PATH = $env:Path; GIT_CONFIG_NOSYSTEM = '1'; GIT_CONFIG_GLOBAL = '/dev/null' }
  Remove-Item -LiteralPath $repo -Recurse -Force -ErrorAction SilentlyContinue
}

Invoke-Row -Id 'W-57' -MinTests 0 -Command 'presence, length, and drive-rooted shape of profile and OneDrive variables (no path text)' -Action {
  foreach ($name in 'USERPROFILE', 'LOCALAPPDATA', 'OneDrive', 'OneDriveCommercial', 'OneDriveConsumer') {
    $value = [System.Environment]::GetEnvironmentVariable($name)
    if ([string]::IsNullOrEmpty($value)) { "${name}: empty" } else { "${name}: set, length $($value.Length), drive-rooted: $($value -match '^[A-Za-z]:\\')" }
  }
  $global:LASTEXITCODE = 0
}
```

## 5. Live behaviour without credentials

Only the stub-harness probe (W-28) and the native test binaries (W-19 to W-27, W-41 to W-46, W-49, W-50) exercise live process behaviour here. They start a trivial child, observe it, and stop it. No row starts a real worker, a harness, or a service. The diagnostic rows W-51 to W-57 start child processes (pwsh, a direct `.sh` launch expected to fail, bash, python, git) and read environment variable presence; they install nothing and write only under the run folder.

Items that need a credential or a managed laptop install are not live rows. They appear in the anomalies section if the agent sees them, and otherwise in section 6 as not covered.

Explicit list of what must NOT be attempted:

- Running Claude Code, or any other agent harness, in any form, including `--version` as a smoke test.
- Logging in to any cloud, GitHub, or service account, or using any token, key, or certificate.
- Cloud execution, cloud admission, or any `gcp` execution target.
- Signing binaries, reading or importing any certificate, or running any signer.
- Installing any Python package (`pip install`, including `tzdata`), any Python package manager action, or any Rust one-off program for the probes; W-54 only reads `tzdata` presence.
- Running an installer, an uninstaller, `msiexec`, `winget`, `rustup-init`, or any `.exe` downloaded for this packet. The toolchain, the W-18 cross targets, and the crates that rustup and cargo fetch under section 2.2 are allowed. The `gh` stub in section 4.7 is a text file that is never run.
- Editing the registry, the user PATH, or any system setting. The single registry exception is the throwaway `HKCU` subkey that W-10's test creates and deletes (exact path in section 8). No other registry write is allowed, and W-10 never runs elevated.
- Elevating to administrator at any point.
- Running `cargo install`, `cargo publish`, or any `git` write command in the test checkout. The only git write in the loop is the results publish in section 2.6, which commits only `results/` in the results checkout.
- Opening, commenting on, or merging any pull request, and any Jira action.
- Running `--ignored` tests or re-exec stubs (W-49 runs the owning test, not the stub).
- Creating any folder on an admin share, a network share, or a drive root. Work only under `%LOCALAPPDATA%\mcao-winverify` and the system temp folder.
- Copying any file out of the run folder or the clones, and uploading anything except the results publish in section 2.6.
- Modifying a tracked file in the test checkout. If a command modifies one (check `git status --porcelain`), record it, do not commit, and stop the run.

## 6. Jira acceptance mapping

These are the Jira criteria the rows above provide evidence for. The criterion text is in the work item, and the owner decides acceptance.

| Work item | Criteria with evidence from this packet | Rows | Not covered by this packet |
| --- | --- | --- | --- |
| DPES-752 | AC1 (partial: build, probe, and backend tests on native Windows; the no-WSL-on-PATH condition is W-29), AC3 (interrupt escalation: W-21 UNRESOLVED on the first run, passing once in run 20261102-r2; W-49 rerun 20 of 20 in run 20261102-r2), AC4 (partial: `cargo check` on the three pinned targets, W-04 and W-18) | W-01, W-03, W-04, W-18, W-21, W-27, W-28, W-29, W-30, W-49, W-50, W-51 to W-57 (diagnostic) | AC2, AC5 (decision record review, owner); the evidence record schema from the decision record |
| DPES-753 | Audit writer on Windows: refusal exit 7 for the writer commands (W-47); unix-only writer tests run 0 here (W-41, W-42, W-46) | W-41, W-42, W-46, W-47 | Writer behaviour on unix; the hash chain on Windows (no Windows run of the writer commands) |
| DPES-754 | AC2 (Git missing, exit 3), AC3 (non-ASCII FM_HOME, exit path printed), AC5 (`--help` and `--version` offline, no home created), AC1 partial (Windows build only) | W-31 to W-35, W-38 | AC1 (clean-host install, no `bash.exe`/`wsl.exe`/`sh.exe` observed during install), AC4 second half and the statically linked clean Windows 10/11 claim |
| DPES-755 | AC1 (Job Object cleanup, partial), AC2 (interrupt, stop-or-kill ladder, UNRESOLVED on the first run; W-49 reruns 20 of 20 passing in run 20261102-r2), AC3 (state classification, partial via backend tests), AC5 (compile gate) | W-18, W-19 to W-22, W-49, W-50 | AC4 reconnection across controller restart (no row); live Blocked/Idle classification for a real worker |
| DPES-759 | AC1 (format, lint, test on Windows host), AC2 (tests pass on native OS, host target only, with `--no-fail-fast`), AC4 (no bash.exe on PATH, W-29 partial), AC5 (clippy warnings fail the gate, W-05) | W-01, W-04, W-05, W-06, W-15, W-17, W-29 | AC3 redacted artifacts on failure (CI-only), AC6 missing-infrastructure behaviour of the GitHub runner, the required-check configuration (owner and admin), and the Python-free gate (DPES-884) |
| DPES-883 | AC1-AC3 (ring buffer unit tests), AC4-AC6 (pipe lifecycle), AC7-AC9 (codec unit tests, if present under `console_server`), AC22 (ACL) | W-23 to W-26 | AC10-AC15 `mcao peek` and read-only attach from a real terminal; AC16-AC21 live attach behaviour; AC23 backlog disconnect under real load |
| DPES-884 | Validator on Windows: unset-home rejection recorded under the default home (W-48); the `cfg(unix)` zone cases do not run (W-17) | W-17, W-48, W-48-c | The zone-bearing cases on Windows (no zoneinfo, documented gap); the reference parity on Windows |
| DPES-885 | AC1-AC4 (slot management, unit or pipe), AC5-AC9 (input and resize at the frame level, partial) | W-22, W-23 | Live keystroke delivery and resize in a real console window; AC10-AC13 Unix routing (not a Windows row); AC17 Ctrl+] at a terminal; the Windows steer end-to-end test (owner note, DPES-885 comment of 2026-10-30) |
| DPES-889 | AC1 (verbatim write), AC2 (opaque read), AC3 (format version and backend token), AC5 (task-id validation, including the uppercase creation ruling in W-40), AC6 (line-ending normalization), AC7 (legacy reader), AC10 (matrix; partial), AC11 (bash compatibility is a Unix-side check, not run here) | W-07, W-08, W-09, W-40 | AC11 bash read of Windows-written records (needs a Unix host); cloud keys (execution target) beyond local |
| DPES-892 | AC4 (OneDrive: a positively detected home is an error, exit 3; a profile folder name alone only warns, exit 0), AC5 (no error for a safe home), AC8 (default home under `%LOCALAPPDATA%`, unit level only), AC3 (registry edit, test only), UNC home warning and exit 0 (PR 467, merged into this packet branch; SPEC-ONLY until a Windows run shows it: W-36) | W-10, W-11, W-13, W-36, W-37, W-37b, W-38, W-43, W-44 | AC1 (installer copies binary and writes manifest), AC2 (uninstaller removes only tracked files), AC3 broadcast, AC6-AC7 retry on `ERROR_SHARING_VIOLATION`, AC9 signing (blocked on the OPEN signer); the UNC warning until a Windows run shows it (W-36) |

Owner note: DPES-752 stays open after this packet. The packet is evidence for the criteria named above only as far as each row states. A passing packet does not close DPES-752, DPES-892, or any child item.

## 7. Results layout on the branch

Each run writes a folder outside both clones, `%LOCALAPPDATA%\mcao-winverify\<RUN_ID>\`, and publishes only the results files under `results/` of the results checkout. The layout of the run folder:

```text
<RUN_ID>\
  <RUN_ID>-SUMMARY.md   the report, filled from windows-native-verification-report-template.md
  rows.tsv              one line per row and per W-49 run: ID, exit code, seconds, binaries-with-tests, vacuous flag, command
  W-06-summary.txt      the W-06 binary list (`Running`, `test result`, `running` lines)
  W-49-summary.txt      the W-49 rerun counts and failure text
  logs\W-01.log ...     full output of each row, one file per row
  probe-evidence.json   the W-28 evidence file
  bash-free-path.json   W-28/W-29 counts only
  packet-copy.md        this packet, copied at the start of the run from the test checkout
  template-copy.md      the report template, copied at the start of the run from the test checkout
  anomalies.txt         findings the template asks for, if any
```

The published results on the branch:

```text
results\AGENT-READY.md          first publish, once
results\<RUN_ID>.started        UTC start time and the commit tested
results\<RUN_ID>.zip            zip of the run folder (after redaction)
results\<RUN_ID>-SUMMARY.md     the report
results\LATEST                  the run id alone; written last
results\<RUN_ID>-BLOCKED.md     a state this packet does not cover
```

Rows that were not run are listed in `<RUN_ID>-SUMMARY.md` with `SKIPPED` and the reason. There is no log file for those rows.

Zip the whole `<RUN_ID>` folder into `<RUN_ID>.zip` in the parent folder, after the report is written and the run folder is redacted:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
Compress-Archive -LiteralPath $Results -DestinationPath "$Results.zip" -Force
```

Copy `<RUN_ID>.zip` and `<RUN_ID>-SUMMARY.md` into `results\` of the results checkout, write `LATEST` last (section 2.4, step 8), and publish with section 2.6. The agent does not upload anything else, and does not send the zip anywhere outside the branch.

## 8. Safety

- Output can contain user names, machine names, and paths. Before a log, the report, or the zip leaves the run folder, replace the Windows user name with `<user>`, the machine name with `<host>`, and any path under the OneDrive profile with `<onedrive>`. Do this for every text file in the run folder, including `logs\`, the report, `rows.tsv`, and the JSON files:

```powershell
$RunId = '<RUN_ID from the request>'
if ($RunId -notmatch '^[A-Za-z0-9][A-Za-z0-9-]*$') { throw 'set $RunId to the RUN_ID of the Current request block' }
$Root = Join-Path $env:LOCALAPPDATA 'mcao-winverify'
$Clone = Join-Path $Root 'src'
$Target = Join-Path $Root 'target'
$Results = Join-Path $Root $RunId
$RowsTsv = Join-Path $Results 'rows.tsv'
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$oneDriveRoots = @($env:OneDrive, $env:OneDriveCommercial, $env:OneDriveConsumer) | Where-Object { $_ }
Get-ChildItem -LiteralPath $Results -Recurse -File | Where-Object { $_.Extension -in '.log', '.md', '.txt', '.tsv', '.json', '.ps1' } | ForEach-Object {
  $text = Get-Content -LiteralPath $_.FullName -Raw
  foreach ($oneDriveRoot in $oneDriveRoots) {
    $text = $text -replace [regex]::Escape($oneDriveRoot), '<onedrive>'
    $text = $text -replace [regex]::Escape($oneDriveRoot.Replace('\', '\\')), '<onedrive>'
  }
  $text = $text -replace [regex]::Escape($env:USERNAME), '<user>'
  $text = $text -replace [regex]::Escape($env:COMPUTERNAME), '<host>'
  Set-Content -LiteralPath $_.FullName -Value $text -Encoding utf8
}
```

- Never save a token, key, password, or credential, and never print one. If a log line contains one, replace the line with `<redacted>` and note the row in anomalies.
- Never print environment variables wholesale. Only the names and values listed in section 2.5 and the report's environment block, and the variables a row names.
- Never create anything on an admin share, a network share, or a drive root. Work only under `%LOCALAPPDATA%\mcao-winverify` and the system temp folder (`%TEMP%`). That folder rule covers files and folders. Registry use is limited to the W-10 exception above. W-36 is the only row that touches an admin share: it runs one `Test-Path` read on `\\localhost\c$\Users` and creates nothing.
- Never elevate. Never download or run an installer, and never run `rustup-init`. The rustup and cargo downloads named in section 2.2 are allowed.
- The loop's only push is a results push: the files under `results/` of the results checkout (section 2.6), on branch `windows-verification`. Every other packet rule stands. Never push anything else, never force-push, never push to `main`, and never create a pull request, tag, or branch. Use only the git credentials already configured on this laptop; never type, print, paste, or search for a token, key, or password.
- The packet never requires a secret. If a step seems to need one, stop that row, mark it `SKIPPED`, and continue.
- If a step would change the laptop beyond the run folder, the two clones, the build directory, and the temporary folders this packet creates (`%TEMP%\mcao winverify ...`, `%TEMP%\OneDrive-winverify`, `%TEMP%\mcao-winverify-profile`, `%LOCALAPPDATA%\mcao-winverify`), stop and report. The one registry change this packet allows is the W-10 key in the next bullet.
- Registry exception (W-10 only): `crates/mcao-core/tests/windows_registry.rs` runs as an ordinary, non-elevated test. It creates `HKEY_CURRENT_USER\Software\mcao-test-<pid>-<n>` (`<pid>` the test process id, `<n>` a counter from 0), writes a `Path` value under it, and deletes that key on drop. It is HKCU only, never HKLM and never `HKCU\Environment`, and nothing persists after the run. If a leftover `Software\mcao-test-*` key from an earlier killed run is present before W-10 runs, record its name in anomalies and stop that row; do not delete keys by hand.

## 9. Reading order

1. The owner publishes a run request: the `Current request` block of this file on `windows-verification`, with `RUN_ID`, `MODE`, `ROWS`, and `NOTES` filled in.
2. The agent runs the loop in section 2.4. Sections 2 and 3 apply to every run. Section 4 rows run in order, then section 5 where it applies.
3. The agent fills `<RUN_ID>-SUMMARY.md` from the template, redacts, zips the run folder, and publishes (section 2.4 step 8).
4. The owner reads the anomalies first, then the table, then maps rows to Jira criteria with section 6.

A row that passes is evidence for the criteria in section 6 only as far as section 6 states. A passing run does not close DPES-752, DPES-892, or any child item.
