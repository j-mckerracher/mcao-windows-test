# Native Windows verification packet

This packet tells a coding agent on a managed Windows 11 laptop how to build and test the Rust `mcao` workspace natively, run the no-credential checks, and return one results folder. The agent does not fix code, does not push, and does not open a pull request. It records what happened, row by row, in the template at [`windows-native-verification-report-template.md`](windows-native-verification-report-template.md).

Branch under test: `windows-verification` of `https://github.com/j-mckerracher/mcao-windows-test` (a squashed single-commit snapshot of the DPES-751 integration branch at 7bb8ebf69915023259a9411feb949a9bce53c2d4, plus this packet adjustment).

Tested commit: the tip of branch `windows-verification` exactly as cloned (there is no separate pinned base in this copy; the earlier pinned base `1086480770dc606b1a6fdc2832aa36a62e8f79f4` is not in this repository). The agent records `git rev-parse HEAD` as `Commit tested`.

Repository: `https://github.com/j-mckerracher/mcao-windows-test`. Some rows may name files or counts written against an older commit; where a row disagrees with the code at this commit, report the observed result and do not edit anything.

## 1. Scope and limits

What this packet covers, without credentials:

- Toolchain pinned by the repository, MSVC host build, `cargo check`, `cargo clippy`, and `cargo test` on the host Windows target, with the per-crate filters in section 4.
- The `mcao` CLI offline behaviour: `--version`, `--help`, and `preflight` (Git, `gh` presence, `FM_HOME` length and OneDrive rules, default home unit tests).
- The Windows process backend tests and the console pipe tests, run as ordinary native tests.
- The `mcao-probe` stub-harness run that CI also performs (no harness, no credentials).
- Platform-leakage guard, state-encoding and task-id rules (DPES-889), and the audit zone gap.

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

## 2. Prerequisites

Assume nothing is installed except Git and a browser. Record each item in the environment block of the report.

| Item | Requirement | Source of the requirement |
| --- | --- | --- |
| Windows | Windows 11 x64. PowerShell 7 (`pwsh`) preferred. Windows PowerShell 5.1 (`powershell.exe`) is acceptable when `pwsh` is not installed or not installable without admin. Every block in this packet is written to run on 5.1 (see section 3). | Runner images used by CI: `.github/workflows/ci.yml:614` (`windows-2022`), `.github/workflows/rust-workspace-check.yml:63-64` (`windows-latest`) |
| Git | Git for Windows on `PATH`. | `preflight` exits 3 when Git is missing (`crates/mcao-core/src/preflight/prereq.rs:17`, remedy text; `crates/mcao-cli/tests/preflight.rs:105`) |
| GitHub CLI | `gh` on `PATH` (presence only; preflight never runs it). Optional for this packet: when it is absent, the rows that need it are `SKIPPED` or use the stub in section 4.6. | `crates/mcao-cli/src/preflight.rs:41-42` checks `gh` after Git and before `FM_HOME`; `crates/mcao-core/src/preflight/prereq.rs:49-50`; PR 414 ruling (PATH only, never executed) |
| Rust toolchain | `rustup` from the official source. Toolchain `1.95.0`, profile `minimal`, components `rustfmt` and `clippy`. | `rust-toolchain.toml:2-4` |
| Rust host target | `x86_64-pc-windows-msvc` (the MSVC host, the default on Windows x64). | `ci.yml:620` matrix target; `rust-workspace-check.yml:64` |
| MSVC Build Tools | "Desktop development with C++" workload with the x64 MSVC tools and a Windows SDK. Needed by the MSVC linker for test and build rows. | `rust-workspace-check.yml` runs on images that carry it; a bare laptop must be checked (section 2.1) |
| Long paths | Git `core.longpaths` and the Windows `LongPathsEnabled` registry value, read back. | `ci.yml:646` and `ci.yml:650-653` configure them on the runner. Laptop: read only, do not change the registry (record `yes` or `no`) |
| Line endings | Text files must check out with LF. | `.gitattributes:1-2` (`* text=auto eol=lf`); `ci.yml:625-640` (section 2.4) |

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

If the MSVC tools are absent, every Cargo row and every `mcao.exe` row (W-02 to W-18, W-19 to W-27, W-30, W-31 to W-38, W-40) is `SKIPPED` with the reason `no MSVC linker`, because each needs a built binary or a test binary. W-01 and W-29 do not need a linker and still run. Record the skips in section 5 of the report. Installing Build Tools needs admin and a download, so do not install it unless the owner has authorized it for this laptop.

### 2.2 Installing Rust (official source only)

Use the official rustup distribution. Do not use a package manager mirror, a copied installer, or any installer from another source. While this packet was written, `https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe.sha256` returned HTTP 200 next to the installer URL.

```powershell
$dist = 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe'
$work = Join-Path $env:LOCALAPPDATA 'mcao-winverify\tools'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$init = Join-Path $work 'rustup-init.exe'
Invoke-WebRequest -Uri $dist -OutFile $init -UseBasicParsing
$sha = Join-Path $work 'rustup-init.exe.sha256'
Invoke-WebRequest -Uri "$dist.sha256" -OutFile $sha -UseBasicParsing
$expected = ((Get-Content -LiteralPath $sha -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash -LiteralPath $init -Algorithm SHA256).Hash.ToLowerInvariant()
if ($expected -ne $actual) { throw "rustup-init checksum mismatch: expected $expected, got $actual" }
Get-AuthenticodeSignature -LiteralPath $init | Select-Object Status, @{ Name = 'Signer'; Expression = { $_.SignerCertificate.Subject } }
```

Stop if the checksum does not match or if the `.sha256` file is not published at that path. Record the Authenticode status in the report as information; the SHA-256 match is the gate. Then install without touching the user PATH:

```powershell
& $init -y --no-modify-path --default-toolchain none --profile minimal
if ($LASTEXITCODE -ne 0) { throw "rustup-init failed with $LASTEXITCODE" }
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
```

`--no-modify-path` stops rustup from writing the user PATH in the registry. The last line changes the PATH of this PowerShell session only. Do not add `.cargo\bin` to any persistent setting. If a new window is opened later, repeat the last line before any cargo row.

Proxy: if the corporate proxy blocks the downloads, stop and report `proxy blocked: <host name only>`. Do not set proxy credentials, do not paste a token into a command, and do not search for another download source.

### 2.3 Clone the branch

Clone into a short path outside OneDrive, for example `C:\mcao-winverify\src`. A deep path inside a synced folder causes path-length and sync-conflict failures that are not product defects.

```powershell
$Clone = 'C:\mcao-winverify\src'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Clone) | Out-Null
git clone --branch windows-verification --no-tags -- https://github.com/j-mckerracher/mcao-windows-test.git $Clone
git -C $Clone status --porcelain
```

Expected: the status output is empty. Any output means the checkout differs from the branch, so stop and report it.

Before the next command, run the setup block of section 3 (it defines `$Results`, `$RowsTsv`, and `Invoke-Row`), then copy this packet and the report template out of the clone,:

```powershell
Copy-Item -LiteralPath (Join-Path $Clone 'docs\windows-native-verification.md') -Destination (Join-Path $Results 'packet-copy.md')
Copy-Item -LiteralPath (Join-Path $Clone 'docs\windows-native-verification-report-template.md') -Destination (Join-Path $Results 'template-copy.md')
$base = git -C $Clone rev-parse HEAD
git -C $Clone status --porcelain
```

Every row runs on the cloned tip. The report's `Commit tested` is `$base`. Record `windows-verification` and the packet's own commit separately.

Toolchain in the clone (the pinned version is read from `rust-toolchain.toml`):

```powershell
Set-Location -LiteralPath $Clone
rustup show active-toolchain
rustup target add x86_64-pc-windows-msvc
rustup target add x86_64-unknown-linux-gnu
rustup target add aarch64-apple-darwin
```

The two non-host targets are used only by row W-18 `cargo check`. `rustup target add` writes only to the rustup home, not to the clone. `rustup show` installs the pinned toolchain automatically from `rust-toolchain.toml` if it is missing.

### 2.4 Line-ending check

This mirrors the CI check at `.github/workflows/ci.yml:625-640`, which fails when a text file is CRLF or when no text file reports LF.

```powershell
Set-Location -LiteralPath $Clone
$eol = @(git ls-files --eol)
$crlfText = @($eol | Where-Object { $_ -match '\bw/crlf\b' -and $_ -notmatch 'attr/-text' })
$lfText = @($eol | Where-Object { $_ -match '\bw/lf\b' -and $_ -match 'attr/text' })
"crlf-text=$($crlfText.Count) lf-text=$($lfText.Count)"
```

Expected: `crlf-text=0`, and `lf-text` is greater than 0. Any CRLF text file is a finding for the anomalies section; do not convert the files. Record the line as `LINE-ENDINGS`.

### 2.5 Recording versions

```powershell
[System.Environment]::OSVersion.VersionString
(Get-CimInstance Win32_OperatingSystem).Caption
(Get-CimInstance Win32_OperatingSystem).BuildNumber
$env:PROCESSOR_ARCHITECTURE
$PSVersionTable.PSVersion.ToString()
$PSVersionTable.PSEdition
git --version
rustc -Vv
cargo -V
rustup show active-toolchain
rustup target list --installed
```

Do not run `Get-ChildItem Env:`, `set`, or `printenv`. Environment dumps can contain tokens. Record only the values listed in the report's environment block. For `Proxy in use`, record `yes` or `no` from the operator's knowledge or from the browser settings; never print the proxy value.

## 3. Conventions for every row

- Shell: PowerShell (`pwsh` or `powershell.exe`, record which). Every block in this packet avoids PowerShell 7-only syntax: no ternary `? :`, no `??`, no `&&` or `||`, no `ForEach-Object -Parallel`, no `Get-Content -AsByteStream`, no `$PSStyle`, no three-argument `Join-Path`. Keep it that way when you adapt a block.
- Work from the clone root with `-LiteralPath` or `Push-Location`. No persistent `cd` is needed.
- Do not set `$ErrorActionPreference = 'Stop'` for the row helper: cargo writes progress to stderr, and `Stop` turns that into a terminating error.
- Every command finishes under the row's timeout. Long Cargo builds are the normal case: give the first build the full budget.
- Use `--locked` on every Cargo command so the committed `Cargo.lock` is used.
- Scripts written into the results folder run in a child shell with `-NoProfile -ExecutionPolicy Bypass -File`. If Group Policy refuses the script, record the message as an anomaly and mark the dependent rows `SKIPPED`.
- Child shells use the absolute path of the running shell: `$shell = (Get-Process -Id $PID).Path` (this is `pwsh.exe` or `powershell.exe`). Do not use `(Get-Command pwsh).Source`, which is `$null` on a 5.1-only machine.
- Expected test count: every Cargo test row states how many tests it must run. Cargo prints `running N tests` per test binary. A row whose log has no `running N tests` line with N of at least 1 is `FAIL (vacuous)`, even if the exit code is 0. A filter that matches nothing reports `running 0 tests` and exits 0; that is the case this rule catches.
- The helper below writes one line per row to `rows.tsv` in the results folder and marks a row vacuous when no test binary ran a test. For rows that are not Cargo test runs (`--version`, `preflight`, the probe), pass `-MinTests 0`.

```powershell
$RunId = Get-Date -Format 'yyyyMMdd-HHmmss'
$Results = Join-Path $env:LOCALAPPDATA "mcao-winverify\$RunId"
New-Item -ItemType Directory -Force -Path (Join-Path $Results 'logs') | Out-Null
$RowsTsv = Join-Path $Results 'rows.tsv'

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
  "$Id`t$code`t$secs`tran-binaries=$ran`tvacuous=$vacuous`t$Command" | Add-Content -LiteralPath $RowsTsv -Encoding utf8
  "$Id exit=$code duration=${secs}s binaries-with-tests=$ran vacuous=$vacuous"
}
```

Use it like this, with the row's command in the action:

```powershell
Invoke-Row -Id 'W-07' -Command 'cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation' -Action { cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation }
```

Do not change a row's command to make it pass. A failing row is evidence. A row is `PASS` only when its exit code is 0, its expected test count is met, and its expected output (section 4) appears in the log.

## 4. Test matrix

Columns: ID, command, expected tests run, expected result, capture, timeout, Jira acceptance, and Status. The Status column says how the row was checked when this packet was written:

- `VERIFIED-BY-READING`: the file, line, or CI step named in the row was read.
- `LISTED-ON-HOST`: the test name exists in the built test binary on macOS (`cargo` test binaries listed with `--list`). Windows-only (`cfg(windows)`) tests cannot be listed there, so their expected count is `at least 1`.
- `SPEC-ONLY`: the expected result is the packet's expectation and was not checked against code or a run. The agent records the actual result; it must not treat a SPEC-ONLY expectation as proof.

Failure rule for every row: do not edit code, do not change tests, do not add `#[ignore]`, do not rerun with `--ignored`. Capture the log, the exact error text, the versions in section 2.5, and continue to the next row.

Acceptance mapping key: `752-AC1` etc. refer to the Jira criteria listed in section 6.

### 4.1 Toolchain and build

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-01 | `rustup show active-toolchain`; `rustc -Vv` | none | Active toolchain `1.95.0-x86_64-pc-windows-msvc`; host line `host: x86_64-pc-windows-msvc` (`rust-toolchain.toml:2-4`) | Output | 2 min | 759 (toolchain pin) | VERIFIED-BY-READING |
| W-02 | `cargo build -p mcao-cli --locked --bin mcao` | none | Exit 0; `target\debug\mcao.exe` exists. Requires the MSVC linker (section 2.1). | Full log, `Get-Item target\debug\mcao.exe` | 45 min | 754-AC4 (partial), 752-AC1 (partial) | VERIFIED-BY-READING (bin `mcao`) |
| W-03 | `cargo build -p mcao-probe --locked --target x86_64-pc-windows-msvc` | none | Exit 0; `target\x86_64-pc-windows-msvc\debug\mcao-probe.exe` exists (`rust-workspace-check.yml:190`) | Log, file listing | 45 min | 752-AC1 (partial) | VERIFIED-BY-READING |
| W-04 | `cargo check --workspace --locked --target x86_64-pc-windows-msvc` | none | Exit 0 (`rust-workspace-check.yml:128-138`, Windows branch at `:136-148` runs the same per-target check) | Log | 30 min | 759-AC1, 752-AC4 | VERIFIED-BY-READING |
| W-05 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | none | Exit 0 (`rust-workspace-check.yml:145-148`) | Log | 30 min | 759-AC1, 759-AC5 | VERIFIED-BY-READING |

### 4.2 Workspace and per-crate tests on the host

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-06 | `cargo test --workspace --locked` | at least one `running N tests` line with N of at least 1 per crate test binary | Exit 0; `test result: ok` for every binary (`rust-workspace-check.yml:153-156`; the x86_64 leg runs the same command at `ci.yml:752-764`). Record each `running 0 tests` line in anomalies; they are normal for doc-test or empty binaries. | Full log, `grep "test result"` and `grep "running"` output to a file | 60 min | 759-AC2, 755-AC1 | VERIFIED-BY-READING |
| W-07 | `cargo test -p mcao-core --locked --test error a_task_id_refused_at_creation` | 1 (`running 1 test`) | Pass: `a_task_id_refused_at_creation_exits_with_usage_code_two` (`crates/mcao-core/tests/error.rs:9`) refuses `con` at creation with exit 2 and `Usage:` text. Replaces the former `--lib state::id` row, which matched no tests (`src/state/id.rs` has no `#[test]`). | Log | 20 min | 889-AC5 | VERIFIED-BY-READING; LISTED-ON-HOST (`running 1 test`) |
| W-08 | `cargo test -p mcao-core --locked --test state_encoding` | at least 1 (48 on the macOS host; the Windows count differs by `cfg(unix)` tests) | Pass: path verbatim write and opaque read, line-ending normalization, format version stamping, and the reserved-name loop `a_new_task_id_that_is_a_windows_device_name_is_refused` (`crates/mcao-core/tests/state_encoding.rs:355`), trailing-mark refusal `a_device_name_with_an_extension_or_trailing_mark_is_refused_as_a_bad_character` (`:385`), and the 64-byte creation boundary `state_task_id_length_boundary_is_64_bytes_for_creation` (`:458`). | Log | 20 min | 889-AC1, AC2, AC3, AC5, AC6, AC7 | LISTED-ON-HOST (names); content SPEC-ONLY |
| W-09 | `cargo test -p mcao-core --locked --test home` | at least 1 (30 on the macOS host; `names_match_case_insensitively_on_windows` is `cfg(windows)`) | Pass. Includes `names_match_case_insensitively_on_windows` (`crates/mcao-core/tests/home.rs:834`, gated by `#[cfg(windows)]` at `:833`). | Log | 20 min | 889-AC5 (case folding), 892-AC4 (partial) | VERIFIED-BY-READING; LISTED-ON-HOST (macOS names) |
| W-10 | `cargo test -p mcao-core --locked --test windows_registry` | at least 1 (the file is `#![cfg(windows)]`, `crates/mcao-core/tests/windows_registry.rs:5`) | Pass. The test writes only a throwaway `HKCU` subkey that it deletes on drop, never `HKCU\Environment` (`:1-3`). If it fails, record the error; do not run it elevated. | Log | 20 min | 892-AC3 (partial, registry edit only; no broadcast checked) | VERIFIED-BY-READING |
| W-11 | `cargo test -p mcao-core --locked --test preflight` | at least 1 (37 on the macOS host; `cfg(unix)` tests at `crates/mcao-core/tests/preflight.rs:260,272` do not run on Windows) | Pass. OneDrive root refusal `a_home_under_onedrive_is_refused_with_the_remedy` (`:144`), business root `a_business_onedrive_root_is_refused_too` (`:158`), profile-folder warning `a_home_under_the_profile_onedrive_folder_warns_without_a_root_variable` (`:198`), profile sync folder that a root also names `a_profile_sync_folder_that_a_root_also_names_is_refused` (`:228`), and `a_device_namespace_home_is_refused_not_compared` (`:436`). Includes `a_disk_prefixed_home_under_the_root_is_still_refused` (`:449`, `cfg` on the next line). | Log | 20 min | 892-AC4, AC5 | VERIFIED-BY-READING (line refs); LISTED-ON-HOST (names) |
| W-12 | `cargo test -p mcao-core --locked --lib preflight` | at least 1 (14 names match `preflight` on the macOS host; the Windows count differs) | Pass. Git and `gh` lookup unit tests `preflight::prereq::tests::*` (for example `preflight_git_is_missing_without_a_path_and_names_its_remedy`). These test the lookup function only, not the default home. | Log | 20 min | 754-AC2 (unit level) | LISTED-ON-HOST |
| W-13 | `cargo test -p mcao-cli --locked --test preflight` | at least 1 (14 on the macOS host) | Pass. Includes Git-missing exit 3 `preflight_exits_three_and_names_git_when_it_is_missing_from_path` (`crates/mcao-cli/tests/preflight.rs:105`), spaces and non-ASCII `FM_HOME` `preflight_prints_an_fm_home_with_spaces_and_non_ascii_exactly` (`:161`), overlong home exit 3 (`:186`, `cfg(windows)`), OneDrive root exit 3 `preflight_exits_three_naming_onedrive_for_a_home_under_a_onedrive_root` (`:400`), and script-shim rejection for Git and `gh` (`:296` and `:312`, both `cfg(windows)`). | Log | 20 min | 754-AC2, AC3, 892-AC4 | VERIFIED-BY-READING; LISTED-ON-HOST (names) |
| W-14 | `cargo test -p mcao-cli --locked --test cli` | at least 1 | Pass. Includes the Windows-only case at `crates/mcao-cli/tests/cli.rs:32` (`#[cfg(windows)]` at `:32`). | Log | 20 min | 754-AC5 (partial) | VERIFIED-BY-READING (`cli.rs:32`) |
| W-15 | `cargo test -p mcao-cli --locked --test toolchain` | at least 1 (8 on the macOS host) | Pass. `no_mistakes_is_refused_on_every_host` (`crates/mcao-cli/tests/toolchain.rs:118`). | Log | 20 min | 759 (gate evidence) | VERIFIED-BY-READING; LISTED-ON-HOST |
| W-16 | `cargo test -p mcao-cli --locked --test herdr_lab_viewer` | at least 1 | Pass. `lab_viewer_is_unsupported_on_windows` (`crates/mcao-cli/tests/herdr_lab_viewer.rs:371-373`, `cfg(windows)`) expects the documented Windows refusal. A failure here is a real finding; record it. | Log | 20 min | 754 (regression evidence; no matching AC) | VERIFIED-BY-READING |
| W-17 | `cargo test -p mcao-cli --locked --test audit_validate --test audit_activity_map` | at least 1 per binary (15 and 12 on the macOS host); zone-bearing cases are not run on Windows | Pass on Windows with the zone-bearing module absent. The zone-bearing module is `#[cfg(unix)] mod zone_bearing` (`crates/mcao-cli/tests/audit_activity_map.rs:88-89`). The Windows refusal of zone-bearing records is the documented gap at `crates/mcao-cli/src/audit/tzif.rs:8-10` (no zoneinfo on Windows). Record the count of `ok` lines and which zone cases were skipped by `cfg`. | Log, count of `ok` lines | 20 min | 759 (gate evidence), audit gap (see section 5) | VERIFIED-BY-READING |
| W-18 | `cargo check --workspace --locked --target x86_64-unknown-linux-gnu`, then `cargo check --workspace --locked --target aarch64-apple-darwin` | none | Exit 0 for each (`rust-workspace-check.yml:128-138`). `cargo check` does not link, so no cross linker is needed. | Two logs | 30 min each | 752-AC4, 754-AC4 (second half), 755-AC5 | VERIFIED-BY-READING |
| W-40 | `cargo test -p mcao-core --locked --test state_encoding a_new_task_id_with_uppercase` | 1 (`running 1 test`) | Pass: `a_new_task_id_with_uppercase_is_refused_and_a_legacy_one_still_reads` (`crates/mcao-core/tests/state_encoding.rs:407`) refuses uppercase at creation and still reads the legacy id. This is the DPES-889 owner ruling (new task ids are lowercase `[a-z0-9-]`; uppercase refused at creation). The CLI path that calls `TaskId::parse_for_creation` is not separately checked here. | Log | 20 min | 889-AC5 (ruling) | VERIFIED-BY-READING; LISTED-ON-HOST (`running 1 test` on macOS) |

Notes on W-06: the workspace test run includes the Unix-only crate test binaries as compiled on Windows; record whatever the output shows and do not interpret it beyond the `test result` and `running` lines.

### 4.3 Windows process backend and console (native tests)

These tests spawn child processes and use Job Objects, named pipes, and ConPTY. Run them in a normal interactive PowerShell window. Do not run them elevated. Do not run tests marked `#[ignore]` with `--ignored`: those are re-exec stubs that only the owning test starts (`crates/mcao-backend-windows/tests/backend.rs:78-115`, `conpty.rs:125-219`, `interrupt.rs` stub tests, `job.rs` stub tests).

Owner ruling (DPES-755, comment of 2026-10-26): on Windows `interrupt` means stop-or-kill (Ctrl+C, poll, then `TerminateJobObject`). A child that is gone after the ladder is the expected passing outcome.

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-19 | `cargo test -p mcao-backend-windows --locked --test job` | at least 1 (`crates/mcao-backend-windows/tests/job.rs:7`, `#![cfg(windows)]`) | Pass. Job Object tree termination and child-in-job checks. | Log | 20 min | 755-AC1 (Job Object cleanup) | VERIFIED-BY-READING (cfg line) |
| W-20 | `cargo test -p mcao-backend-windows --locked --test backend` | at least 1 (`tests/backend.rs:8`, `#![cfg(windows)]`) | Pass. Launch, state, and cleanup through `WindowsBackend`. | Log | 20 min | 755-AC1, AC3 | VERIFIED-BY-READING (cfg and `#[ignore]` lines) |
| W-21 | `cargo test -p mcao-backend-windows --locked --test interrupt` | at least 1 (`tests/interrupt.rs:15`, `#![cfg(windows)]`) | Pass, or a documented escalation failure. The interrupt ladder is Ctrl+C, then poll, then job termination (owner ruling above). A child that is gone after the ladder passes. A child that survives is the Unsupported/Unresolved case from 752-AC3: record the failing test name and the error; do not treat it as a harness bug. | Log | 20 min | 755-AC2, 752-AC3 | VERIFIED-BY-READING (cfg line); ladder behaviour SPEC-ONLY |
| W-22 | `cargo test -p mcao-backend-windows --locked --test conpty` | at least 1 (`tests/conpty.rs:9`, `#![cfg(windows)]`) | Pass in an interactive console. ConPTY needs a real console host; if the test reports the console or pseudo console API is unavailable, mark `SKIPPED` with the error text. | Log | 20 min | 755 (terminal), 885 (partial) | VERIFIED-BY-READING |
| W-23 | `cargo test -p mcao-backend-windows --locked --test console_pipe` | at least 1 (`crates/mcao-backend-windows/tests/console_pipe.rs:6`, `#![cfg(windows)]`) | Pass. The CI console lane runs the same command (`ci.yml:838`); the CI guard fails on `matched no native tests` (`ci.yml:834`), which this row repeats. Covers pipe sessions, partial frames, stalled input, and idle disconnect. | Log | 20 min | 883-AC4, AC5, AC6, 885-AC1-AC3 (partial) | VERIFIED-BY-READING |
| W-24 | `cargo test -p mcao-backend-windows --locked --test pipe` | at least 1 | Pass (`ci.yml:839`). | Log | 20 min | 883-AC4-AC6 (pipe lifecycle) | VERIFIED-BY-READING (CI line) |
| W-25 | `cargo test -p mcao-backend-windows --locked --lib console_server` | 13 (`console_server::tests::*`, listed on the macOS host) | Pass (`ci.yml:840`). Ring buffer, codec, and ACL unit tests in `crates/mcao-backend-windows/src/console_server.rs`. | Log | 20 min | 883-AC1-AC3, AC7-AC9, AC22 | LISTED-ON-HOST (13) |
| W-26 | `cargo test -p mcao-backend-windows --locked --lib` | 18 on the macOS host (13 `console_server` and 5 `pipe::tests`); the Windows count may differ | Pass (all library tests). | Log | 20 min | 883 (unit) | LISTED-ON-HOST |
| W-27 | `cargo test -p mcao-probe --locked --test native` | at least 1 (6 on the macOS host; Windows-gated cases at `crates/mcao-probe/tests/native.rs:18`, `:29`, `:36`) | Pass. Native probe test file. | Log | 20 min | 752-AC1 (partial) | VERIFIED-BY-READING (cfg lines); LISTED-ON-HOST (count) |

### 4.4 Probe run with the stub harness and bash-free PATH

No Claude Code, no credentials, no network. This repeats the CI probe step (`rust-workspace-check.yml:190-250`) on the laptop, including the bash-free PATH removal. The CI step drops every PATH directory that holds `bash.exe`, `sh.exe`, or `wsl.exe`, and every MSYS or Cygwin directory. It runs the probe by absolute path inside the reduced PATH. Then it checks seven operation records against an expected support table.

The probe script is written to the results folder and run in a child shell. The parent session PATH is not changed.

```powershell
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
| W-28 | `& $shell -NoProfile -ExecutionPolicy Bypass -File bash-free-probe.ps1 -ProbeExe <probe> -OutDir <results>` where `$shell = (Get-Process -Id $PID).Path` and `<probe>` is `$Clone\target\x86_64-pc-windows-msvc\debug\mcao-probe.exe` | not a Cargo test | Exit 0. The output line `probe expectations matched` appears. `probe-evidence.json` has seven records in this order: `spawn` demonstrated, `capture` unsupported, `send_text` unsupported, `interrupt` unsupported, `agent_state` demonstrated, `reconnect` unsupported, `cleanup` demonstrated (`rust-workspace-check.yml:238-246`). Any other support value for an operation is a FAIL. | `logs\W-28.log`, `probe-evidence.json`, `bash-free-path.json` | 5 min | 752-AC1 (partial), 752-AC3 | VERIFIED-BY-READING (CI steps and expected table) |
| W-29 | Read `bash-free-path.json` written by W-28 and check that `command_missing_after` has `true` for `bash`, `sh`, and `wsl` | not a Cargo test | Pass when all three values are `true`. Record the counts `removed_directories` and `kept_directories` and the executable names in `forbidden_found_on_path`; never record directory paths. | `bash-free-path.json` (counts only) | 2 min | 759-AC4 (partial, via the probe run) | VERIFIED-BY-READING (CI snippet `rust-workspace-check.yml:191-229`) |

Run W-28 with the probe's own flags exactly as CI does. It does not contact any harness. Do not add `-ErrorAction` changes to the probe script.

### 4.5 Platform leakage guard

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-30 | `cargo test -p mcao-probe --locked --test platform_leakage` | 12 on the macOS host (the `cfg(windows)` gate cases are the same binary) | Pass. The guard parses dependency gates and rejects any gate other than `cfg(unix)` or `cfg(windows)` (`crates/mcao-probe/tests/platform_leakage.rs:282`). A version-pinned support record covers third-party crates (PR 418). | Log | 20 min | 752-AC4 (guard test), 759 | VERIFIED-BY-READING (`:282`); LISTED-ON-HOST (12) |

### 4.6 CLI offline behaviour and FM_HOME rules (no network)

Run these from a PowerShell window. Each `preflight` row runs in a child shell with the environment set for that row only, so the parent session keeps its own PATH, `FM_HOME`, and OneDrive variables. The child shell helper is written to the results folder.

Preflight order, read from code: Git first (`crates/mcao-cli/src/preflight.rs:39`), then `gh` (`:41`), then `FM_HOME` (`:44`). Git and `gh` are looked up on `PATH` only as `.exe` or `.com` files (`crates/mcao-core/src/preflight/prereq.rs:23-26,120-125`). A `.cmd`, `.bat`, or `.ps1` file is never found. Preflight never runs `gh` or `git`.

Therefore a `gh` stub for the preflight rows must be a file named `gh.exe`. Its content does not matter because preflight never executes it. The `cli` tests use the same fixture (`crates/mcao-cli/tests/preflight.rs:31-50`, which writes `name.EXE` with the text `not a program`).

```powershell
$mcao = Join-Path $Clone 'target\debug\mcao.exe'
$GhStub = Join-Path $Results 'gh-stub'
New-Item -ItemType Directory -Force -Path $GhStub | Out-Null
Set-Content -LiteralPath (Join-Path $GhStub 'gh.exe') -Value 'not a program' -Encoding ascii
```

Use the stub only when the real `gh` is absent (`Get-Command gh -ErrorAction SilentlyContinue` returns nothing). The stub is for the preflight rows W-35 and W-37 only. Never put it on `PATH` for any other command, and never use it to run anything that would contact GitHub. Remove `$GhStub` at the end of section 4.6.

```powershell
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
Invoke-Row -Id 'W-35' -MinTests 0 -Command 'mcao preflight, spaces and non-ASCII FM_HOME' -Action { & $shell -NoProfile -ExecutionPolicy Bypass -File $preflightPath -Mcao $mcao -PathValue $pathValue -FmHome $fmHome -OneDriveVar '' -OneDriveValue '' -UserProfile '' }
```

Set `$pathValue` to the session `PATH` with the stub directory in front when the stub is used, and to the session `PATH` otherwise. The `git` directory must stay on `PATH` for every row except W-34.

| ID | Command | Expected tests run | Expected result | Capture | Timeout | Acceptance | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-31 | `& $mcao --version` | not a Cargo test | Exit 0, one version line. No file is created under the home. Test `version_flag_prints_the_package_version_and_exits_zero` exists on the macOS host. | Output, exit code | 1 min | 754-AC5 | LISTED-ON-HOST (name) |
| W-32 | `& $mcao --help` | not a Cargo test | Exit 0, help text. Test `help_flag_lists_util_and_succeeds_without_a_home` exists on the macOS host. | Output, exit code | 1 min | 754-AC5 | LISTED-ON-HOST |
| W-33 | `& $mcao preflight` with Git and a real `gh` on `PATH`, `FM_HOME` absent | not a Cargo test | Exit 0. Lines `Git: ok (...)`, `Gh: ok (...)` (capital `G` as printed by `crates/mcao-cli/src/preflight.rs:42`), and `Preflight: all checks passed`. **SKIPPED with the reason `gh absent` when the real `gh` is not installed.** The missing-`gh` branch is covered by `preflight_exits_three_and_names_gh_when_it_is_missing_from_path` (`crates/mcao-cli/tests/preflight.rs:119`) in W-13. | Output, exit code | 1 min | 754-AC1 (partial) | VERIFIED-BY-READING |
| W-34 | Git missing: run the child with `-PathValue "$env:SystemRoot\System32"`, `-ExpectGitMissing`, and no `gh` stub. The child script stops with exit 9 if `git` is still found. | not a Cargo test | Exit 3. Output names `Git: MISSING` and the remedy `install Git from https://git-scm.com and open a new terminal` (`crates/mcao-core/src/preflight/prereq.rs:17`; test `crates/mcao-cli/tests/preflight.rs:105`). Output does not contain `Gh:`, because `gh` is checked after Git (`crates/mcao-cli/src/preflight.rs:39-41`). | Output, exit code | 2 min | 754-AC2 | VERIFIED-BY-READING |
| W-35 | FM_HOME with spaces and non-ASCII: `$fmHome = Join-Path $env:TEMP 'mcao winverify ÄÖ 工作'`; create the folder; run the child with `-PathValue` including Git and the `gh` stub or real `gh`, and `-FmHome $fmHome` | not a Cargo test | Exit 0. The line `FM_HOME path length: ok (<path>)` and `FM_HOME OneDrive: ok (<path>)` print the path unchanged (`crates/mcao-cli/src/preflight.rs:84,95`; test `preflight_prints_an_fm_home_with_spaces_and_non_ascii_exactly`, `crates/mcao-cli/tests/preflight.rs:161`). Record the exact printed path line with the user name replaced by `<user>`. Remove the folder afterwards. **SKIPPED with the reason `gh absent and no stub` only if neither a real `gh` nor the stub is used.** | Output, exit code | 2 min | 754-AC3 | VERIFIED-BY-READING |
| W-36 | FM_HOME as UNC path: `\\<server>\<share>\mcao-winverify` only if an accessible share exists on the laptop; otherwise `SKIPPED` | not a Cargo test | Recorded result only (no expected pass or fail until the owner rules on UNC homes). | Output, exit code | 2 min | none (open question, see section 5) | SPEC-ONLY |
| W-37 | OneDrive root, for each variable name in `OneDrive`, `OneDriveCommercial`, `OneDriveConsumer` in turn: set that variable to `Join-Path $env:TEMP 'OneDrive-winverify'` in the child, create that folder, and set `FM_HOME` to a folder under it | not a Cargo test | Exit 3 for each of the three variables. Output names `FM_HOME OneDrive` and the remedy `%LOCALAPPDATA%\mcao\home` (`crates/mcao-core/src/preflight.rs:33-35`, `ONEDRIVE_RELOCATION`; test `preflight_exits_three_naming_onedrive_for_a_home_under_a_onedrive_root`, `crates/mcao-cli/tests/preflight.rs:400`). This is the owner ruling: a home under a positively detected OneDrive root is an error. **Needs the real `gh` or the stub, otherwise SKIPPED** (preflight stops at `Gh:` before `FM_HOME`). | Output, exit code (one per variable) | 2 min | 892-AC4 | VERIFIED-BY-READING |
| W-37b | OneDrive profile folder (warning only): child with `-UserProfile` set to `Join-Path $env:TEMP 'mcao-winverify-profile'`, `FM_HOME` set to `<profile>\OneDrive - winverify\home`, and the three OneDrive variables removed | not a Cargo test | Exit 0. Output contains `FM_HOME OneDrive: warning:` and `beneath USERPROFILE` (`crates/mcao-cli/tests/preflight.rs:328`, test `preflight_warns_and_passes_for_a_home_beneath_the_profile_sync_folder`). A profile folder name alone only warns; this is not an error. Needs the real `gh` or the stub. | Output, exit code | 2 min | 892-AC4 | VERIFIED-BY-READING |
| W-38 | `cargo test -p mcao-core --locked --test preflight the_default_home` | 2 (`running 2 tests`) | Pass: `the_default_home_is_the_mcao_home_under_localappdata` (`crates/mcao-core/tests/preflight.rs:108`) and `the_default_home_needs_an_absolute_localappdata` (`:121`). Unit tests only: `mcao preflight` prints no home line (`crates/mcao-cli/src/preflight.rs:44-47` prints checks only), so there is no CLI observation of the default home. | Log | 2 min | 754-AC5, 892-AC8 (partial, unit level) | VERIFIED-BY-READING; LISTED-ON-HOST (names) |
| W-39 | Overlong home: covered by W-13 (`preflight_exits_three_naming_the_path_length_check_for_an_overlong_home`, `crates/mcao-cli/tests/preflight.rs:186`, `cfg(windows)`). Record W-13 as a reference here, no separate run. | reference only | Reference only | none | 0 | 754 (path budget) | VERIFIED-BY-READING |

Do not run `mcao` commands that write to a home (anything other than `--version`, `--help`, `preflight`) in this packet.

After section 4.6, remove the stub and the temporary folders:

```powershell
Remove-Item -LiteralPath $GhStub -Recurse -Force -ErrorAction SilentlyContinue
```

## 5. Live behaviour without credentials

Only the stub-harness probe (W-28) and the native test binaries (W-19 to W-27) exercise live process behaviour here. They start a trivial child, observe it, and stop it. No row starts a real worker, a harness, or a service.

Items that need a credential or a managed laptop install are not live rows. They appear in the anomalies section if the agent sees them, and otherwise in section 6 as not covered.

Explicit list of what must NOT be attempted:

- Running Claude Code, or any other agent harness, in any form, including `--version` as a smoke test.
- Logging in to any cloud, GitHub, or service account, or using any token, key, or certificate.
- Cloud execution, cloud admission, or any `gcp` execution target.
- Signing binaries, reading or importing any certificate, or running any signer.
- Running an installer, an uninstaller, `msiexec`, `winget`, or any downloaded `.exe` other than the official `rustup-init.exe` in section 2.2. The `gh` stub in section 4.6 is a text file that is never run.
- Editing the registry, the user PATH, or any system setting. The `rustup-init` run uses `--no-modify-path`. W-10 writes only a throwaway `HKCU` subkey that the test deletes; do not run it elevated.
- Elevating to administrator at any point.
- Running `cargo install`, `cargo publish`, or any `git` write command (`commit`, `push`, `tag`, `merge`, `reset`, `checkout` in section 2.3).
- Opening, commenting on, or merging any pull request, and any Jira action.
- Running `--ignored` tests or re-exec stubs.
- Copying any file out of the results folder or the clone, and uploading anything.
- Modifying a tracked file in the clone. If a command modifies one (check `git status --porcelain`), record it and do not commit.

## 6. Jira acceptance mapping

These are the Jira criteria the rows above provide evidence for. The criterion text is in the work item, and the owner decides acceptance.

| Work item | Criteria with evidence from this packet | Rows | Not covered by this packet |
| --- | --- | --- | --- |
| DPES-752 | AC1 (partial: build, probe, and backend tests on native Windows; the no-WSL-on-PATH condition is W-29), AC4 (partial: `cargo check` on the three pinned targets, W-04 and W-18) | W-01, W-03, W-04, W-18, W-27, W-28, W-29, W-30 | AC2, AC3, AC5 (decision record review, owner); the evidence record schema from the decision record |
| DPES-754 | AC2 (Git missing, exit 3), AC3 (non-ASCII FM_HOME, exit path printed), AC5 (`--help` and `--version` offline, no home created), AC1 partial (Windows build only) | W-31 to W-35, W-38 | AC1 (clean-host install, no `bash.exe`/`wsl.exe`/`sh.exe` observed during install), AC4 second half and the statically linked clean Windows 10/11 claim |
| DPES-755 | AC1 (Job Object cleanup, partial), AC2 (interrupt, stop-or-kill ladder, partial), AC3 (state classification, partial via backend tests), AC5 (compile gate) | W-19 to W-22, W-18 | AC4 reconnection across controller restart (no row); live Blocked/Idle classification for a real worker |
| DPES-759 | AC1 (format, lint, test on Windows host), AC2 (tests pass on native OS, host target only), AC4 (no bash.exe on PATH, W-29 partial), AC5 (clippy warnings fail the gate, W-05) | W-01, W-04, W-05, W-06, W-15, W-17, W-29 | AC3 redacted artifacts on failure (CI-only), AC6 missing-infrastructure behaviour of the GitHub runner, the required-check configuration (owner and admin), and the Python-free gate (DPES-884) |
| DPES-883 | AC1-AC3 (ring buffer unit tests), AC4-AC6 (pipe lifecycle), AC7-AC9 (codec unit tests, if present under `console_server`), AC22 (ACL) | W-23 to W-26 | AC10-AC15 `mcao peek` and read-only attach from a real terminal; AC16-AC21 live attach behaviour; AC23 backlog disconnect under real load |
| DPES-885 | AC1-AC4 (slot management, unit or pipe), AC5-AC9 (input and resize at the frame level, partial) | W-22, W-23 | Live keystroke delivery and resize in a real console window; AC10-AC13 Unix routing (not a Windows row); AC17 Ctrl+] at a terminal; the Windows steer end-to-end test (owner note, DPES-885 comment of 2026-10-30) |
| DPES-889 | AC1 (verbatim write), AC2 (opaque read), AC3 (format version and backend token), AC5 (task-id validation, including the uppercase creation ruling in W-40), AC6 (line-ending normalization), AC7 (legacy reader), AC10 (matrix; partial), AC11 (bash compatibility is a Unix-side check, not run here) | W-07, W-08, W-09, W-40 | AC11 bash read of Windows-written records (needs a Unix host); cloud keys (execution target) beyond local |
| DPES-892 | AC4 (OneDrive: a positively detected home is an error, exit 3; a profile folder name alone only warns, exit 0), AC5 (no error for a safe home), AC8 (default home under `%LOCALAPPDATA%`, unit level only), AC3 (registry edit, test only) | W-10, W-11, W-13, W-37, W-37b, W-38 | AC1 (installer copies binary and writes manifest), AC2 (uninstaller removes only tracked files), AC3 broadcast, AC6-AC7 retry on `ERROR_SHARING_VIOLATION`, AC9 signing (blocked on the OPEN signer) |

Owner note: DPES-752 stays open after this packet. The packet is evidence for AC1 and AC4 only.

## 7. Results format

Write everything to the results folder created in section 3 (`$env:LOCALAPPDATA\mcao-winverify\<run-id>`). Layout:

```text
<run-id>\
  SUMMARY.md            the report, filled from windows-native-verification-report-template.md
  rows.tsv              one line per row: ID, exit code, seconds, binaries-with-tests, vacuous flag, command
  logs\W-01.log ...     full output of each row, one file per row
  probe-evidence.json   the W-28 evidence file
  bash-free-path.json   W-28/W-29 counts only
  packet-copy.md        this packet, copied from the clone
  template-copy.md      the report template, copied from the clone
  anomalies.txt         findings the template asks for, if any
```

Rows that were not run are listed in `SUMMARY.md` with `SKIPPED` and the reason. There is no log file for those rows.

Zip the whole `<run-id>` folder into `<run-id>.zip` in the same parent folder:

```powershell
Compress-Archive -LiteralPath $Results -DestinationPath "$Results.zip" -Force
```

Then tell the human operator the full path of `<run-id>.zip` and nothing else. Do not upload it, email it, or attach it to anything. `Compress-Archive` is available on Windows PowerShell 5.1 and PowerShell 7.

## 8. Safety

- Output can contain user names, machine names, and paths. Before saving a log, replace the Windows user name with `<user>`, the machine name with `<host>`, and any path under the OneDrive profile with `<onedrive>`. Do this for each log in the results folder:

```powershell
Get-ChildItem -LiteralPath (Join-Path $Results 'logs') -File | ForEach-Object {
  $text = Get-Content -LiteralPath $_.FullName -Raw
  $text = $text.Replace($env:USERNAME, '<user>').Replace($env:COMPUTERNAME, '<host>')
  Set-Content -LiteralPath $_.FullName -Value $text -Encoding utf8
}
```

- Never save a token, key, password, or credential, and never print one. If a log line contains one, replace the line with `<redacted>` and note the row in anomalies.
- Never print environment variables wholesale. Only the names and values listed in section 2.5 and the report's environment block.
- Do not copy anything outside the results folder, except the probe evidence file is written there directly.
- The packet never requires a secret. If a step seems to need one, stop that row, mark it `SKIPPED`, and continue.
- If a step would change the laptop beyond the results folder and the temporary folders this packet creates (`%TEMP%\mcao winverify ...`, `%TEMP%\OneDrive-winverify`, `%TEMP%\mcao-winverify-profile`, `%LOCALAPPDATA%\mcao-winverify`), stop and report.

## 9. Review and reading order

1. Owner gives the agent this file and the template.
2. Agent runs sections 2 and 3, then rows in section 4 in order, then section 5 where it applies.
3. Agent fills `SUMMARY.md` from the template, zips the folder, and reports the zip path.
4. Owner reads the anomalies first, then the table, then maps rows to Jira criteria with section 6.

A row that passes is evidence for the criteria in section 6 only as far as section 6 states. A passing packet does not close DPES-752, DPES-892, or any child item.
