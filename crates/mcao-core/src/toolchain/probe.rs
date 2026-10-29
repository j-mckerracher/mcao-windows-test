//! Per-tool qualification probes and the evidence record they produce (DPES-890).
//!
//! Each probe reads the machine only through [`Host`]. A process starts only from a resolved
//! executable with argv, so no `cmd`, `sh`, `powershell`, or `npm` process starts, and a `.cmd`,
//! `.bat`, or `.ps1` shim is recorded as a blocker rather than run.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use super::discovery::{Found, find_executable};
use super::host::{Host, Platform, RunOutput};
use super::npm::resolve_npm_shim_argv;
use super::record::ToolRecord;

/// Launcher type for a tool that is not on PATH at all.
const NOT_FOUND: &str = "not_found";

/// Git settings whose values change how a checkout behaves on Windows, and what each one controls.
const GIT_SETTINGS: [(&str, &str); 3] = [
    ("core.autocrlf", "checkout line-ending conversion"),
    ("core.symlinks", "symlink materialization"),
    ("core.longpaths", "paths beyond MAX_PATH"),
];

/// The identity of one qualification run. The caller supplies it, so the probes stay pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContext {
    pub timestamp: String,
    pub runner: String,
    /// The CI job's URL, or empty outside CI. Written as each tool's `evidence_link` and as `ci_job`.
    pub ci_job_url: String,
}

/// The result of `gh auth status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhAuth {
    Authenticated,
    NotAuthenticated,
    Error,
}

impl GhAuth {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authenticated => "authenticated",
            Self::NotAuthenticated => "not_authenticated",
            Self::Error => "error",
        }
    }
}

/// The complete qualification record that `mcao toolchain qualify` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub tools: Vec<ToolRecord>,
    pub pilot_harness_launcher: PilotLauncher,
    pub gh_auth_status: GhAuth,
    /// True when `git-credential-manager` is a native executable on PATH.
    pub git_credential_manager: bool,
    pub timestamp: String,
    pub runner: String,
    /// The CI job's URL, written as `ci_job`.
    pub ci_job_url: String,
}

/// The pilot harness launcher record: `{harness, launcher_type, executable, evidence}`. It has no
/// verdict; the pilot is native-only, and the launcher type and evidence state what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotLauncher {
    pub harness: &'static str,
    /// `node_script_via_npm_shim` is the label for a missing native executable.
    /// It does not mean the npm shim ran, because that fallback is never attempted.
    /// The pilot has no verdict, so a consumer decides from a non-empty `executable`,
    /// never from this label alone.
    pub launcher_type: &'static str,
    /// The native executable that would start the harness, or empty when none was found.
    pub executable: String,
    pub evidence: String,
}

impl PilotLauncher {
    fn to_json(&self) -> Value {
        json!({
            "harness": self.harness,
            "launcher_type": self.launcher_type,
            "executable": self.executable,
            "evidence": self.evidence,
        })
    }
}

impl Evidence {
    /// The record as pretty JSON with a trailing newline.
    pub fn to_json_text(&self) -> String {
        let text =
            serde_json::to_string_pretty(&self.to_json()).expect("a JSON value always serializes");
        format!("{text}\n")
    }

    fn to_json(&self) -> Value {
        json!({
            "tools": self.tools.iter().map(ToolRecord::to_json).collect::<Vec<Value>>(),
            "pilot_harness_launcher": self.pilot_harness_launcher.to_json(),
            "gh_auth_status": self.gh_auth_status.as_str(),
            "git_credential_manager": self.git_credential_manager,
            "timestamp": self.timestamp,
            "runner": self.runner,
            "ci_job": self.ci_job_url,
        })
    }
}

/// Qualifies the governance toolchain on `host`. Probing reads and starts only the tools named here.
pub fn qualify(host: &dyn Host, ctx: &RunContext) -> Evidence {
    let mut tools = vec![
        probe_git(host),
        probe_gh(host),
        probe_no_mistakes(host),
        probe_npm_tool(host, "tasks-axi"),
        probe_npm_tool(host, "quota-axi"),
        probe_codex(host),
        probe_herdr(host),
    ];
    for tool in &mut tools {
        tool.evidence_link.clone_from(&ctx.ci_job_url);
    }
    Evidence {
        tools,
        pilot_harness_launcher: probe_pilot_launcher(host),
        gh_auth_status: gh_auth_status(host),
        git_credential_manager: matches!(
            find_executable(host, "git-credential-manager"),
            Found::Exe(_)
        ),
        timestamp: ctx.timestamp.clone(),
        runner: ctx.runner.clone(),
        ci_job_url: ctx.ci_job_url.clone(),
    }
}

/// The pilot harness, claude, started only as a native executable (decision d). The npm shim
/// fallback is not attempted, so a missing native executable is recorded as not found.
fn probe_pilot_launcher(host: &dyn Host) -> PilotLauncher {
    const HARNESS: &str = "claude";
    let (launcher_type, executable, evidence) = match find_executable(host, HARNESS) {
        Found::Exe(exe) => {
            let version = host
                .run(&exe, &[OsStr::new("--version")])
                .ok()
                .filter(|output| output.success)
                .and_then(|output| first_line(&output));
            let evidence = match version {
                Some(version) => format!("native executable on PATH; --version: {version}"),
                None => "native executable on PATH; --version did not run successfully".to_owned(),
            };
            ("native_exe", exe.display().to_string(), evidence)
        }
        Found::ShimOnly(shim) => (
            "script_shim",
            String::new(),
            format!(
                "{HARNESS} resolves only to the script shim {}; a native executable is required",
                shim.display()
            ),
        ),
        Found::Missing => (
            "node_script_via_npm_shim",
            String::new(),
            format!(
                "{HARNESS} is not on PATH as a native executable; the npm shim fallback is not attempted"
            ),
        ),
    };
    PilotLauncher {
        harness: HARNESS,
        launcher_type,
        executable,
        evidence,
    }
}

/// A PATH tool that is started directly as a native executable.
fn probe_native(host: &dyn Host, name: &'static str, install_method: &'static str) -> ToolRecord {
    match find_executable(host, name) {
        Found::Exe(exe) => {
            let mut record = ToolRecord::new(name, "native_exe", install_method);
            record.binary_exists = true;
            record.argv_only = true;
            with_version(record, host.run(&exe, &[OsStr::new("--version")])).settle(false)
        }
        Found::ShimOnly(shim) => {
            let mut record = ToolRecord::new(name, "script_shim", install_method);
            record.binary_exists = true;
            record.blockers.push(format!(
                "{name} resolves only to the script shim {}; a native executable is required",
                shim.display()
            ));
            record.settle(false)
        }
        Found::Missing => {
            let mut record = ToolRecord::new(name, NOT_FOUND, install_method);
            record.blockers.push(format!("{name} is not on PATH"));
            record.settle(false)
        }
    }
}

fn probe_gh(host: &dyn Host) -> ToolRecord {
    probe_native(host, "gh", "PATH executable")
}

fn probe_codex(host: &dyn Host) -> ToolRecord {
    probe_native(host, "codex", "PATH executable")
}

/// Git is a native executable. Its checkout settings are recorded as limitations and never block.
fn probe_git(host: &dyn Host) -> ToolRecord {
    let mut record = probe_native(host, "git", "PATH executable");
    if let Found::Exe(git) = find_executable(host, "git") {
        record.limitations.extend(
            GIT_SETTINGS
                .iter()
                .map(|&(key, note)| git_setting(host, &git, key, note)),
        );
    }
    record
}

fn git_setting(host: &dyn Host, git: &Path, key: &str, note: &str) -> String {
    let value = match run_argv(host, git, &["config", "--get", key]) {
        Ok(output) if output.success => output.stdout.trim().to_owned(),
        Ok(output) if output.code == Some(1) => "unset".to_owned(),
        _ => "unreadable".to_owned(),
    };
    format!("{key}={value} ({note})")
}

/// no-mistakes is a node script whose git-hook daemon is a shell wrapper. It is never started during
/// qualification, and its verdict is unsupported whenever it is installed, since it cannot run
/// argv-only. Its install method on managed Windows is unapproved.
fn probe_no_mistakes(host: &dyn Host) -> ToolRecord {
    let mut record = ToolRecord::new("no-mistakes", "node_script", "unapproved");
    match resolve_npm_shim_argv(host, "no-mistakes") {
        Ok(_) => {
            record.binary_exists = true;
            record
                .limitations
                .push("git_hook_daemon_shell_wrapper".to_owned());
        }
        Err(reason) => record.blockers.push(reason),
    }
    record.settle(false)
}

/// An npm global package, started as `node <script>` with argv. Its script internals are not
/// inspected, which the record states as a limitation.
fn probe_npm_tool(host: &dyn Host, name: &'static str) -> ToolRecord {
    let mut record = ToolRecord::new(name, "node_script", "npm global package");
    match resolve_npm_shim_argv(host, name) {
        Ok(argv) => {
            record.binary_exists = true;
            record.argv_only = true;
            record.limitations.push(
                "runs as node_script through node argv; script internals are not inspected"
                    .to_owned(),
            );
            let result = host.run(
                &argv.executable,
                &[argv.script.as_os_str(), OsStr::new("--version")],
            );
            with_version(record, result).settle(false)
        }
        Err(reason) => {
            record.blockers.push(reason);
            record.settle(false)
        }
    }
}

/// herdr is refused on Windows and dropped from the pilot, so it is never probed there.
fn probe_herdr(host: &dyn Host) -> ToolRecord {
    if host.platform() == Platform::Windows {
        let mut record = ToolRecord::new("herdr", "refused", "not supported on Windows");
        record.binary_exists = !matches!(find_executable(host, "herdr"), Found::Missing);
        record
            .blockers
            .push("herdr is not supported on Windows and is dropped from the pilot".to_owned());
        return record.settle(true);
    }
    probe_native(host, "herdr", "PATH executable")
}

fn gh_auth_status(host: &dyn Host) -> GhAuth {
    let Found::Exe(gh) = find_executable(host, "gh") else {
        return GhAuth::Error;
    };
    match run_argv(host, &gh, &["auth", "status"]) {
        Ok(output) if output.success => GhAuth::Authenticated,
        Ok(output) if output.code == Some(1) => GhAuth::NotAuthenticated,
        _ => GhAuth::Error,
    }
}

/// Records the `--version` result. A run that fails, times out, or prints nothing leaves no version and
/// a blocker that names why, so the verdict is never supported on an unverified version.
fn with_version(mut record: ToolRecord, result: io::Result<RunOutput>) -> ToolRecord {
    let version = match &result {
        Ok(output) if output.success => first_line(output),
        _ => None,
    };
    match (version, result) {
        (Some(version), _) => record.version = Some(version),
        (None, Err(error)) => record
            .blockers
            .push(format!("{} --version did not run: {error}", record.name)),
        (None, Ok(_)) => record.blockers.push(format!(
            "{} --version did not run successfully",
            record.name
        )),
    }
    record
}

fn first_line(output: &RunOutput) -> Option<String> {
    output
        .stdout
        .lines()
        .chain(output.stderr.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn run_argv(host: &dyn Host, exe: &Path, args: &[&str]) -> io::Result<RunOutput> {
    let argv: Vec<&OsStr> = args.iter().map(|arg| OsStr::new(*arg)).collect();
    host.run(exe, &argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::host::fake::{FakeHost, assert_no_shell_or_npm, failed, ok};
    use crate::toolchain::record::Verdict;

    const NODE: &str = "wbin/node.exe";
    const TASKS_SCRIPT: &str = "wappdata/npm/node_modules/tasks-axi/bin/cli.js";
    const QUOTA_SCRIPT: &str = "wappdata/npm/node_modules/quota-axi/dist/main.js";
    const NO_MISTAKES_SCRIPT: &str = "wappdata/npm/node_modules/no-mistakes/bin/nm.js";

    const JOB_URL: &str = "https://ci.example/runs/1/job/2";

    fn ctx() -> RunContext {
        RunContext {
            timestamp: "2026-10-25T18:00:00Z".to_owned(),
            runner: "test-runner".to_owned(),
            ci_job_url: JOB_URL.to_owned(),
        }
    }

    /// A Windows host with node, git, gh, and optionally a native claude, plus the npm globals.
    fn windows_host(with_native_claude: bool) -> FakeHost {
        let mut host = FakeHost::new(Platform::Windows)
            .env("APPDATA", "wappdata")
            .dir("wappdata/npm")
            .path_var(&["wbin"])
            .exe(NODE)
            .exe("wbin/git.exe")
            .exe("wbin/gh.exe")
            .script(
                "wbin/git.exe",
                &["--version"],
                ok("git version 2.45.0.windows.1"),
            )
            .script(
                "wbin/git.exe",
                &["config", "--get", "core.autocrlf"],
                ok("true"),
            )
            .script(
                "wbin/git.exe",
                &["config", "--get", "core.symlinks"],
                failed(1),
            )
            .script(
                "wbin/git.exe",
                &["config", "--get", "core.longpaths"],
                ok("true"),
            )
            .script("wbin/gh.exe", &["--version"], ok("gh version 2.60.0"))
            .script("wbin/gh.exe", &["auth", "status"], ok("Logged in"))
            .file(
                "wappdata/npm/node_modules/tasks-axi/package.json",
                r#"{"bin":"bin/cli.js"}"#,
            )
            .file(TASKS_SCRIPT, "")
            .script(NODE, &[TASKS_SCRIPT, "--version"], ok("tasks-axi 1.2.0"))
            .file(
                "wappdata/npm/node_modules/quota-axi/package.json",
                r#"{"bin":{"quota-axi":"dist/main.js"}}"#,
            )
            .file(QUOTA_SCRIPT, "")
            .script(NODE, &[QUOTA_SCRIPT, "--version"], ok("quota-axi 0.9.0"))
            .file(
                "wappdata/npm/node_modules/no-mistakes/package.json",
                r#"{"bin":"bin/nm.js"}"#,
            )
            .file(NO_MISTAKES_SCRIPT, "");
        if with_native_claude {
            host = host.exe("wbin/claude.exe").script(
                "wbin/claude.exe",
                &["--version"],
                ok("1.0.0 (Claude Code)"),
            );
        }
        host
    }

    fn find<'a>(evidence: &'a Evidence, name: &str) -> &'a ToolRecord {
        evidence
            .tools
            .iter()
            .find(|record| record.name == name)
            .expect("the record is present")
    }

    #[test]
    fn qualify_on_windows_starts_no_shell_or_npm_process() {
        // AC1: the probes never start cmd, sh, powershell, or npm, nor a script shim.
        let host = windows_host(true).file("wbin/codex.cmd", "");
        let _ = qualify(&host, &ctx());
        assert!(!host.runs().is_empty(), "the probes ran");
        assert_no_shell_or_npm(&host.runs());
    }

    #[test]
    fn evidence_root_keys_follow_ac2() {
        // AC2: the root keys are exactly these seven.
        let text = qualify(&windows_host(true), &ctx()).to_json_text();
        let json: Value = serde_json::from_str(&text).expect("the record is JSON");
        let mut keys: Vec<&str> = json
            .as_object()
            .expect("the root is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "ci_job",
                "gh_auth_status",
                "git_credential_manager",
                "pilot_harness_launcher",
                "runner",
                "timestamp",
                "tools",
            ]
        );
        assert_eq!(json["ci_job"], JOB_URL);
        assert_eq!(json["gh_auth_status"], "authenticated");
        assert_eq!(json["git_credential_manager"], false);
    }

    #[test]
    fn tool_records_carry_the_normative_shape_and_evidence_link() {
        // Jira section 5 item 8: each tool record has evidence_link and no argv_only field.
        let text = qualify(&windows_host(true), &ctx()).to_json_text();
        let json: Value = serde_json::from_str(&text).expect("the record is JSON");
        let tools = json["tools"].as_array().expect("the tools list");
        assert!(!tools.is_empty());
        for tool in tools {
            assert_eq!(tool["evidence_link"], JOB_URL, "{tool}");
            assert!(tool.get("argv_only").is_none(), "{tool}");
        }
    }

    #[test]
    fn pilot_launcher_has_the_normative_four_field_shape() {
        // Jira section 6: {harness, launcher_type, executable, evidence}, with no verdict.
        let text = qualify(&windows_host(true), &ctx()).to_json_text();
        let json: Value = serde_json::from_str(&text).expect("the record is JSON");
        let pilot = json["pilot_harness_launcher"]
            .as_object()
            .expect("the pilot object");
        let mut keys: Vec<&str> = pilot.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["evidence", "executable", "harness", "launcher_type"]);
    }

    #[test]
    fn no_mistakes_is_unsupported_node_script_with_shell_wrapper() {
        // AC6: launcher node_script, the wrapper limitation, verdict unsupported, install unapproved.
        let host = windows_host(true);
        let evidence = qualify(&host, &ctx());
        let record = find(&evidence, "no-mistakes");
        assert_eq!(record.launcher_type, "node_script");
        assert_eq!(record.limitations, ["git_hook_daemon_shell_wrapper"]);
        assert_eq!(record.verdict, Verdict::Unsupported);
        assert_eq!(record.install_method, "unapproved");
        assert!(
            !host
                .runs()
                .iter()
                .any(|(_, argv)| argv.iter().any(|arg| arg.contains("no-mistakes"))),
            "no-mistakes is never started"
        );
    }

    #[test]
    fn git_is_supported_with_checkout_setting_limitations() {
        // AC7: verdict supported; the limitations cover autocrlf, symlinks, and longpaths.
        let evidence = qualify(&windows_host(true), &ctx());
        let git = find(&evidence, "git");
        assert_eq!(git.verdict, Verdict::Supported);
        assert_eq!(git.version.as_deref(), Some("git version 2.45.0.windows.1"));
        assert!(
            git.limitations
                .iter()
                .any(|l| l.starts_with("core.autocrlf=true"))
        );
        assert!(
            git.limitations
                .iter()
                .any(|l| l.starts_with("core.symlinks=unset"))
        );
        assert!(
            git.limitations
                .iter()
                .any(|l| l.starts_with("core.longpaths=true"))
        );
    }

    #[test]
    fn npm_tool_runs_through_node_argv_and_is_supported() {
        let host = windows_host(true);
        let evidence = qualify(&host, &ctx());
        let tasks = find(&evidence, "tasks-axi");
        assert_eq!(tasks.launcher_type, "node_script");
        assert!(tasks.argv_only);
        assert_eq!(tasks.verdict, Verdict::Supported);
        assert!(
            host.runs()
                .iter()
                .any(|(exe, argv)| exe.ends_with("node.exe") && argv[0] == TASKS_SCRIPT),
            "tasks-axi runs as node argv"
        );
    }

    #[test]
    fn pilot_is_native_only() {
        // AC8 and decision (d): a native executable is native_exe with its path and version; a missing
        // one has no executable and the npm shim fallback is not attempted.
        let native = qualify(&windows_host(true), &ctx()).pilot_harness_launcher;
        assert_eq!(native.harness, "claude");
        assert_eq!(native.launcher_type, "native_exe");
        assert!(native.executable.ends_with("claude.exe"), "{native:?}");
        assert!(
            native.evidence.contains("1.0.0 (Claude Code)"),
            "{native:?}"
        );

        let missing = qualify(&windows_host(false), &ctx()).pilot_harness_launcher;
        assert_eq!(missing.launcher_type, "node_script_via_npm_shim");
        assert!(missing.executable.is_empty(), "{missing:?}");
        assert!(
            missing
                .evidence
                .contains("npm shim fallback is not attempted"),
            "{missing:?}"
        );

        // A shim is never a pilot launcher: it is recorded, not run, and has no executable.
        let shim_host = windows_host(false).file("wbin/claude.cmd", "");
        let shim = qualify(&shim_host, &ctx()).pilot_harness_launcher;
        assert_eq!(shim.launcher_type, "script_shim");
        assert!(shim.executable.is_empty(), "{shim:?}");
        assert!(
            !shim_host
                .runs()
                .iter()
                .any(|(exe, _)| exe.ends_with("claude.cmd")),
            "the shim is never run"
        );
    }

    #[test]
    fn version_run_that_does_not_start_is_unsupported_with_its_error() {
        // A --version run that errors (unscripted here, as a missing or wedged process would) leaves no
        // version, and the blocker names the failure instead of leaving it unexplained.
        let host = windows_host(true).exe("wbin/codex.exe");
        let evidence = qualify(&host, &ctx());
        let codex = find(&evidence, "codex");
        assert_eq!(codex.verdict, Verdict::Unsupported);
        assert!(codex.version.is_none());
        assert!(
            codex
                .blockers
                .iter()
                .any(|b| b.starts_with("codex --version did not run: ")),
            "{:?}",
            codex.blockers
        );
    }

    #[test]
    fn gh_auth_maps_exit_status_to_three_values() {
        // AC9: exit 0 authenticated, exit 1 not_authenticated, anything else or absent error.
        let host_with = |output: RunOutput| {
            FakeHost::new(Platform::Windows)
                .path_var(&["wbin"])
                .exe("wbin/gh.exe")
                .script("wbin/gh.exe", &["auth", "status"], output)
        };
        assert_eq!(gh_auth_status(&host_with(ok(""))), GhAuth::Authenticated);
        assert_eq!(
            gh_auth_status(&host_with(failed(1))),
            GhAuth::NotAuthenticated
        );
        assert_eq!(gh_auth_status(&host_with(failed(2))), GhAuth::Error);

        let absent = FakeHost::new(Platform::Windows).path_var(&["wbin"]);
        assert_eq!(gh_auth_status(&absent), GhAuth::Error);
    }

    #[test]
    fn codex_cmd_shim_is_unsupported_and_never_run() {
        let host = windows_host(true).file("wbin/codex.cmd", "");
        let evidence = qualify(&host, &ctx());
        let codex = find(&evidence, "codex");
        assert_eq!(codex.verdict, Verdict::Unsupported);
        assert!(codex.blockers.iter().any(|b| b.contains("shim")));
        assert!(
            !host
                .runs()
                .iter()
                .any(|(exe, _)| exe.ends_with("codex.cmd")),
            "the shim is never run"
        );
    }

    #[test]
    fn herdr_is_refused_on_windows_without_a_probe() {
        // Decision (c): herdr is unsupported on Windows, recorded with a refusal, and never probed.
        let host = windows_host(true).exe("wbin/herdr.exe");
        let evidence = qualify(&host, &ctx());
        let herdr = find(&evidence, "herdr");
        assert_eq!(herdr.verdict, Verdict::Unsupported);
        assert!(
            herdr
                .blockers
                .iter()
                .any(|b| b.contains("not supported on Windows"))
        );
        assert!(
            !host
                .runs()
                .iter()
                .any(|(exe, _)| exe.ends_with("herdr.exe")),
            "herdr is never started on Windows"
        );
    }

    #[test]
    fn credential_manager_is_a_presence_boolean() {
        // Jira section 6: git_credential_manager is true only for a native executable on PATH.
        assert!(!qualify(&windows_host(true), &ctx()).git_credential_manager);

        let present = windows_host(true).exe("wbin/git-credential-manager.exe");
        assert!(qualify(&present, &ctx()).git_credential_manager);

        let shim = windows_host(true).file("wbin/git-credential-manager.cmd", "");
        assert!(!qualify(&shim, &ctx()).git_credential_manager);
    }
}
