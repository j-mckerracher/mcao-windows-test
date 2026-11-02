//! Golden cases for `mcao hook subagent-pretool` (DPES-894 slice 2), converted from the behavior
//! tests in `tests/fm-subagent-pretool-check.test.sh`. Each case runs the built binary against a
//! scratch firstmate home, a scratch secondmate or task worktree, or a non-firstmate repo.
//!
//! The tests need `git` on `PATH`, because primary-home identity is read from git. They do not
//! need `jq`, which the shell tests use only to read their own output.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const DELEGATION_TOOLS: &[&str] = &[
    "Task",
    "Agent",
    "Workflow",
    "RemoteTrigger",
    "Monitor",
    "ScheduleWakeup",
    "SendMessage",
    "EnterWorktree",
    "ExitWorktree",
    "CronCreate",
    "CronDelete",
];

const FUTURE_TOOLS: &[&str] = &[
    "SubagentCreate",
    "SpawnWorker",
    "DelegateTask",
    "AgentPool",
    "WorkflowRun",
    "ScheduleJob",
    "CronSchedule",
    "CreateWorktree",
    "DispatchAgent",
    "TaskHandoff",
    "RemoteExec",
    "BackgroundAgent",
];

const PRESERVED_TOOLS: &[&str] = &[
    "Bash",
    "Edit",
    "Read",
    "Write",
    "Skill",
    "ToolSearch",
    "WebFetch",
    "WebSearch",
    "NotebookEdit",
    "ReportFindings",
    "DesignSync",
    "PushNotification",
];

const OBSERVE_OR_STOP: &[&str] = &[
    "TaskOutput",
    "TaskStop",
    "TaskGet",
    "TaskList",
    "CronList",
    "BashOutput",
    "KillShell",
];

const PLAN_ONLY: &[&str] = &["TaskCreate", "TaskUpdate"];

const PLAN_ONLY_NEAR_MISSES: &[&str] = &[
    "TaskCreateAgent",
    "TaskCreateWorktree",
    "TaskUpdateAgent",
    "RemoteTaskCreate",
    "Task",
    "TaskCreator",
];

const MCP_TOOLS: &[&str] = &[
    "mcp__linear__list_issues",
    "mcp__tracker__create_task",
    "mcp__acme__spawn_agent",
    "mcp__slack__slack_send_message",
];

const SCOUT_ROUTE: &str = "first classify the work under the AGENTS.md intake contract: work already classified as a scout goes to bin/fm-scout.sh \"<question>\" [project], while authorized ship work and its bounded research go to bin/fm-brief.sh then bin/fm-spawn.sh";
const BRIEF_ONLY_ROUTE: &str = "first classify the work under the AGENTS.md intake contract, then use bin/fm-brief.sh followed by bin/fm-spawn.sh for dispatched work";

/// A scratch directory under cargo's per-target temporary directory, removed before reuse.
fn scratch(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "hook-subagent-pretool-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create scratch directory");
    path
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} in {}", dir.display());
}

/// A firstmate primary: a plain git checkout with `AGENTS.md`, `bin/`, and `state/`.
fn primary(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name).join("primary");
    fs::create_dir_all(root.join("bin")).expect("bin");
    fs::create_dir_all(root.join("state")).expect("state");
    fs::write(root.join("AGENTS.md"), "# fixture\n").expect("AGENTS.md");
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "fixture"]);
    git(&root, &["config", "user.email", "fixture@example.test"]);
    git(&root, &["add", "AGENTS.md"]);
    git(&root, &["commit", "-qm", "fixture"]);
    let state = root.join("state");
    (root, state)
}

fn guard_without_harness(root: &Path, state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .args(["hook", "subagent-pretool"])
        .env("FM_ROOT_OVERRIDE", root)
        .env("FM_HOME", root)
        .env("FM_STATE_OVERRIDE", state)
        .env_remove("FM_ALLOW_SUBAGENT");
    command
}

fn guard(harness: &str, root: &Path, state: &Path) -> Command {
    let mut command = guard_without_harness(root, state);
    command.args(["--harness", harness]);
    command
}

fn run_tool(root: &Path, state: &Path, tool: &str) -> Output {
    guard("claude", root, state)
        .args(["--tool", tool])
        .output()
        .expect("mcao runs")
}

fn run_tool_with_env(root: &Path, state: &Path, tool: &str, key: &str, value: &str) -> Output {
    guard("claude", root, state)
        .args(["--tool", tool])
        .env(key, value)
        .output()
        .expect("mcao runs")
}

fn run_stdin(harness: &str, root: &Path, state: &Path, payload: &str) -> Output {
    run_stdin_bytes(harness, root, state, payload.as_bytes())
}

fn run_stdin_bytes(harness: &str, root: &Path, state: &Path, payload: &[u8]) -> Output {
    let mut child = guard(harness, root, state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("mcao spawns");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(payload)
        .expect("payload written");
    child.wait_with_output().expect("mcao finishes")
}

fn assert_allow(output: &Output, label: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} must allow: {output:?}"
    );
    assert!(output.stdout.is_empty(), "{label} allow wrote stdout");
    assert!(output.stderr.is_empty(), "{label} allow wrote stderr");
}

/// Claude's deny: exit 2, empty stdout, and the Claude permission object on stderr.
fn assert_claude_deny(output: &Output, tool: &str) {
    assert_eq!(output.status.code(), Some(2), "{tool} must deny");
    assert!(
        output.stdout.is_empty(),
        "{tool} deny wrote stdout, which Claude ignores"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("deny stderr is JSON");
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
    let message = body["systemMessage"].as_str().expect("systemMessage");
    assert!(message.starts_with("[subagent-dispatch]"), "{message}");
    assert!(
        message.contains(&format!("blocked tool: {tool}")),
        "{message}"
    );
}

#[test]
fn denies_every_known_delegation_tool() {
    let (root, state) = primary("known");
    for tool in DELEGATION_TOOLS {
        assert_claude_deny(&run_tool(&root, &state, tool), tool);
    }
}

#[test]
fn denies_delegation_shaped_tools_no_list_knows_yet() {
    let (root, state) = primary("future");
    for tool in FUTURE_TOOLS {
        assert_claude_deny(&run_tool(&root, &state, tool), tool);
    }
}

#[test]
fn allows_ordinary_and_observe_or_stop_tools() {
    let (root, state) = primary("preserved");
    for tool in PRESERVED_TOOLS.iter().chain(OBSERVE_OR_STOP) {
        assert_allow(&run_tool(&root, &state, tool), tool);
    }
}

#[test]
fn allows_session_local_todo_tools() {
    let (root, state) = primary("plan-only");
    for tool in PLAN_ONLY {
        assert_allow(&run_tool(&root, &state, tool), tool);
    }
}

#[test]
fn plan_only_exclusion_is_exact_name() {
    let (root, state) = primary("near-miss");
    for tool in PLAN_ONLY_NEAR_MISSES {
        assert_claude_deny(&run_tool(&root, &state, tool), tool);
    }
}

#[test]
fn never_classifies_mcp_tools() {
    let (root, state) = primary("mcp");
    for tool in MCP_TOOLS {
        assert_allow(&run_tool(&root, &state, tool), tool);
    }
}

#[test]
fn deny_message_reserves_scout_route_for_classified_work() {
    let (root, state) = primary("message");
    fs::write(
        root.join("bin").join("fm-scout.sh"),
        "#!/usr/bin/env bash\n",
    )
    .expect("scout");
    let with_scout = run_tool(&root, &state, "Agent");
    let message = serde_json::from_slice::<serde_json::Value>(&with_scout.stderr)
        .expect("deny stderr is JSON")["systemMessage"]
        .as_str()
        .expect("systemMessage")
        .to_owned();
    assert!(message.contains(SCOUT_ROUTE), "{message}");
    assert!(
        !message.contains("investigation or diagnosis goes to bin/fm-scout.sh"),
        "{message}"
    );

    fs::remove_file(root.join("bin").join("fm-scout.sh")).expect("remove scout");
    let without_scout = run_tool(&root, &state, "Agent");
    let message = serde_json::from_slice::<serde_json::Value>(&without_scout.stderr)
        .expect("deny stderr is JSON")["systemMessage"]
        .as_str()
        .expect("systemMessage")
        .to_owned();
    assert!(message.contains(BRIEF_ONLY_ROUTE), "{message}");
    assert_eq!(without_scout.status.code(), Some(2));
}

#[test]
fn escape_hatch_releases_only_the_exact_opt_in_value() {
    let (root, state) = primary("escape");
    assert_allow(
        &run_tool_with_env(&root, &state, "Agent", "FM_ALLOW_SUBAGENT", "1"),
        "escape hatch set",
    );
    assert_claude_deny(&run_tool(&root, &state, "Agent"), "Agent");
    for value in ["", "0", "yes", "true", "11"] {
        let output = run_tool_with_env(&root, &state, "Agent", "FM_ALLOW_SUBAGENT", value);
        assert_eq!(
            output.status.code(),
            Some(2),
            "FM_ALLOW_SUBAGENT='{value}' must not release the guard"
        );
    }
}

#[test]
fn task_worktree_and_non_firstmate_repo_are_inert() {
    let (root, _state) = primary("inert");
    let base = root.parent().expect("scratch base").to_path_buf();
    let child = base.join("child");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fixture-child",
            child.to_str().expect("utf8"),
        ],
    );
    fs::create_dir_all(child.join("bin")).expect("child bin");
    fs::create_dir_all(child.join("state")).expect("child state");
    fs::write(child.join("AGENTS.md"), "# fixture\n").expect("child AGENTS.md");
    assert_allow(
        &run_tool(&child, &child.join("state"), "Agent"),
        "crewmate task worktree",
    );

    let plain = base.join("plain");
    fs::create_dir_all(plain.join("bin")).expect("plain bin");
    fs::create_dir_all(plain.join("state")).expect("plain state");
    git(&base, &["init", "-q", plain.to_str().expect("utf8")]);
    assert_allow(
        &run_tool(&plain, &plain.join("state"), "Agent"),
        "non-firstmate repo",
    );
}

#[test]
fn marked_secondmate_worktree_is_in_scope_but_a_markerless_line_is_not() {
    let (root, _state) = primary("secondmate");
    let base = root.parent().expect("scratch base").to_path_buf();
    let second = base.join("second");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fixture-second",
            second.to_str().expect("utf8"),
        ],
    );
    fs::create_dir_all(second.join("bin")).expect("second bin");
    fs::create_dir_all(second.join("state")).expect("second state");
    fs::write(second.join("AGENTS.md"), "# fixture\n").expect("second AGENTS.md");

    // A marker needs its first line to end in a newline; without one the home is not secondmate.
    fs::write(second.join(".fm-secondmate-home"), "sm-fixture").expect("marker");
    assert_allow(
        &run_tool(&second, &second.join("state"), "Agent"),
        "marker without a trailing newline",
    );

    fs::write(second.join(".fm-secondmate-home"), "sm-fixture\n").expect("marker");
    assert_claude_deny(&run_tool(&second, &second.join("state"), "Agent"), "Agent");
}

/// The working directory is never the root: a primary checkout with no explicit root is inert.
#[test]
fn the_working_directory_is_never_the_root() {
    let (root, _state) = primary("cwd-only");
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args([
            "hook",
            "subagent-pretool",
            "--harness",
            "claude",
            "--tool",
            "Agent",
        ])
        .current_dir(&root)
        .env_remove("FM_ROOT_OVERRIDE")
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .env_remove("FM_ALLOW_SUBAGENT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .output()
        .expect("mcao runs");
    assert_allow(&output, "a primary working directory with no explicit root");
}

/// A working directory outside any repository. The parent of the scratch directory is a git
/// ceiling, so git cannot find the checkout that holds the build tree.
fn outside_any_repository(name: &str) -> PathBuf {
    scratch(name)
}

#[test]
fn an_explicit_root_flag_guards_from_outside_any_repository() {
    let (root, _state) = primary("outside-flag");
    let outside = outside_any_repository("outside-flag-cwd");
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args([
            "hook",
            "subagent-pretool",
            "--harness",
            "claude",
            "--tool",
            "Agent",
            "--root",
        ])
        .arg(&root)
        .current_dir(&outside)
        .env("GIT_CEILING_DIRECTORIES", outside.parent().expect("parent"))
        .env_remove("FM_ROOT_OVERRIDE")
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .env_remove("FM_ALLOW_SUBAGENT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .output()
        .expect("mcao runs");
    assert_claude_deny(&output, "Agent");
}

#[test]
fn claude_project_dir_guards_from_outside_any_repository() {
    let (root, _state) = primary("outside-env");
    let outside = outside_any_repository("outside-env-cwd");
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args([
            "hook",
            "subagent-pretool",
            "--harness",
            "claude",
            "--tool",
            "Agent",
        ])
        .current_dir(&outside)
        .env("GIT_CEILING_DIRECTORIES", outside.parent().expect("parent"))
        .env("CLAUDE_PROJECT_DIR", &root)
        .env_remove("FM_ROOT_OVERRIDE")
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .env_remove("FM_ALLOW_SUBAGENT")
        .output()
        .expect("mcao runs");
    assert_claude_deny(&output, "Agent");
}

#[test]
fn stdin_transports_and_output_shapes() {
    let (root, state) = primary("stdin");
    assert_claude_deny_stdin(&run_stdin(
        "claude",
        &root,
        &state,
        r#"{"tool_name":"Agent","tool_input":{"prompt":"go"}}"#,
    ));

    let grok = run_stdin("grok", &root, &state, r#"{"toolName":"Agent"}"#);
    assert_eq!(grok.status.code(), Some(2), "Grok-shaped stdin must deny");
    let decision: serde_json::Value =
        serde_json::from_slice(&grok.stdout).expect("default deny writes a Grok object");
    assert_eq!(decision["decision"], "deny");
    assert!(
        decision["reason"]
            .as_str()
            .expect("reason")
            .starts_with("[subagent-dispatch]")
    );

    assert_allow(
        &run_stdin(
            "claude",
            &root,
            &state,
            r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        ),
        "Bash through stdin",
    );
}

/// Stdin is read as bytes, so a stray non-UTF-8 byte in an unrelated field must not turn a
/// delegation-shaped name into an allow.
#[test]
fn stdin_with_a_non_utf8_byte_still_denies() {
    let (root, state) = primary("stdin-non-utf8");
    let mut payload = br#"{"tool_name":"Agent","x":""#.to_vec();
    payload.push(0xff);
    payload.extend_from_slice(br#""}"#);
    assert_claude_deny(&run_stdin_bytes("claude", &root, &state, &payload), "Agent");
}

fn assert_claude_deny_stdin(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "Claude-shaped stdin must deny"
    );
    assert!(output.stdout.is_empty(), "Claude deny wrote stdout");
}

#[test]
fn malformed_stdin_fails_closed_and_empty_stdin_allows() {
    let (root, state) = primary("malformed");
    assert_claude_deny_stdin(&run_stdin("claude", &root, &state, "{not-json"));
    for payload in ["", "   ", "{}", r#"{"tool_name":null}"#, r#""Agent""#] {
        let output = run_stdin("claude", &root, &state, payload);
        assert_allow(&output, &format!("payload {payload:?}"));
    }
}

/// `jq` reads every top-level value, so a delegation object still denies after a non-object value.
#[test]
fn a_non_object_before_a_delegation_object_denies() {
    let (root, state) = primary("stdin-non-object-first");
    assert_claude_deny_stdin(&run_stdin(
        "claude",
        &root,
        &state,
        r#""x"{"tool_name":"Agent"}"#,
    ));
}

/// `jq` exits on the last value, so a non-object last value makes the shell allow the call even
/// when an earlier object names a delegation tool.
#[test]
fn a_delegation_object_before_a_non_object_allows() {
    let (root, state) = primary("stdin-non-object-last");
    assert_allow(
        &run_stdin("claude", &root, &state, r#"{"tool_name":"Agent"}"x""#),
        "Agent followed by a string",
    );
}

#[test]
fn a_leading_byte_order_mark_is_stripped_before_the_name_is_read() {
    let (root, state) = primary("stdin-bom");
    assert_claude_deny_stdin(&run_stdin(
        "claude",
        &root,
        &state,
        "\u{feff}{\"tool_name\":\"Agent\"}",
    ));
    assert_allow(
        &run_stdin("claude", &root, &state, "\u{feff}{\"tool_name\":\"Bash\"}"),
        "Bash behind a byte-order mark",
    );
}

/// One golden case per input that `jq` accepts and the port could not read before this fix.
/// Each names a delegation-shaped tool, which the shell denies.
#[test]
fn stdin_lone_low_surrogate_escape_denies() {
    let (root, state) = primary("stdin-surrogate");
    assert_claude_deny_stdin(&run_stdin(
        "claude",
        &root,
        &state,
        r#"{"tool_name":"Agent","x":"\udc00"}"#,
    ));
}

#[test]
fn stdin_out_of_range_number_denies() {
    let (root, state) = primary("stdin-number");
    assert_claude_deny_stdin(&run_stdin(
        "claude",
        &root,
        &state,
        r#"{"tool_name":"Agent","x":1e1000}"#,
    ));
}

#[test]
fn stdin_raw_nul_after_the_object_denies() {
    let (root, state) = primary("stdin-nul-after");
    assert_claude_deny_stdin(&run_stdin_bytes(
        "claude",
        &root,
        &state,
        b"{\"tool_name\":\"Agent\"}\0",
    ));
}

#[test]
fn stdin_raw_nul_inside_the_name_denies() {
    let (root, state) = primary("stdin-nul-inside");
    assert_claude_deny_stdin(&run_stdin_bytes(
        "claude",
        &root,
        &state,
        b"{\"tool_name\":\"Ag\0ent\"}",
    ));
}

#[test]
fn stdin_nul_stripping_does_not_allow_a_delegation_name() {
    let (root, state) = primary("stdin-nul-bash");
    assert_claude_deny_stdin(&run_stdin_bytes(
        "claude",
        &root,
        &state,
        b"{\"tool_name\":\"Agent\"}\0\0",
    ));
    assert_allow(
        &run_stdin_bytes("claude", &root, &state, b"{\"tool_name\":\"Bash\"}\0"),
        "Bash with a trailing NUL",
    );
}

#[test]
fn stdin_deep_nesting_in_an_unrelated_field_denies() {
    let (root, state) = primary("stdin-depth");
    let payload = format!(
        r#"{{"tool_name":"Agent","x":{}{}}}"#,
        "[".repeat(200),
        "]".repeat(200)
    );
    assert_claude_deny_stdin(&run_stdin("claude", &root, &state, &payload));
}

#[test]
fn stdin_object_valued_tool_name_denies() {
    let (root, state) = primary("stdin-object-name");
    assert_claude_deny_stdin(&run_stdin(
        "claude",
        &root,
        &state,
        r#"{"tool_name":{"a":"age","":"nt"}}"#,
    ));
}

#[test]
fn an_unknown_harness_is_a_usage_error() {
    let (root, state) = primary("usage");
    let output = guard("unknown", &root, &state)
        .args(["--tool", "Agent"])
        .output()
        .expect("mcao runs");
    assert_eq!(output.status.code(), Some(2));
}

/// Without `--harness`, the output is the shell's default: Grok stdout on deny, nothing on allow.
#[test]
fn without_a_harness_the_output_matches_the_shells_default() {
    let (root, state) = primary("default-mode");
    assert_allow(
        &guard_without_harness(&root, &state)
            .args(["--tool", "Bash"])
            .output()
            .expect("mcao runs"),
        "Bash without --harness",
    );

    let output = guard_without_harness(&root, &state)
        .args(["--tool", "Agent"])
        .output()
        .expect("mcao runs");
    assert_eq!(output.status.code(), Some(2), "Agent must deny");
    let decision: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("default deny writes a Grok object");
    assert_eq!(decision["decision"], "deny");
    let body: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("deny stderr is JSON");
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn a_repeated_tool_flag_keeps_the_last_value() {
    let (root, state) = primary("repeat-tool");
    assert_allow(
        &guard_without_harness(&root, &state)
            .args(["--tool", "Agent", "--tool", "Read"])
            .output()
            .expect("mcao runs"),
        "Agent then Read",
    );
    assert_claude_deny(
        &guard("claude", &root, &state)
            .args(["--tool", "Read", "--tool", "Agent"])
            .output()
            .expect("mcao runs"),
        "Agent",
    );
}

/// `.claude/settings.json` passes the shell's `--claude`, so the port must accept that spelling.
#[test]
fn the_shells_claude_flag_is_accepted() {
    let (root, state) = primary("claude-flag");
    assert_allow(
        &guard_without_harness(&root, &state)
            .args(["--claude", "--tool", "Bash"])
            .output()
            .expect("mcao runs"),
        "Bash with --claude",
    );
    assert_claude_deny(
        &guard_without_harness(&root, &state)
            .args(["--claude", "--tool", "Agent"])
            .output()
            .expect("mcao runs"),
        "Agent",
    );
}

/// The shell accepts a repeated `--claude`, so the port must not turn the repeat into a usage error.
#[test]
fn a_repeated_claude_flag_is_accepted() {
    let (root, state) = primary("repeat-claude");
    assert_allow(
        &guard_without_harness(&root, &state)
            .args(["--claude", "--tool", "Bash", "--claude"])
            .output()
            .expect("mcao runs"),
        "Bash with a repeated --claude",
    );
    assert_claude_deny(
        &guard_without_harness(&root, &state)
            .args(["--claude", "--claude", "--tool", "Agent"])
            .output()
            .expect("mcao runs"),
        "Agent",
    );
}

#[test]
fn a_repeated_harness_flag_keeps_the_last_value() {
    let (root, state) = primary("repeat-harness");
    let output = guard_without_harness(&root, &state)
        .args([
            "--harness",
            "grok",
            "--harness",
            "claude",
            "--tool",
            "Agent",
        ])
        .output()
        .expect("mcao runs");
    assert_claude_deny(&output, "Agent");
}

/// The shell's result for `$'\xffAgent'` depends on the locale: it allows only with
/// `LANG=en_US.UTF-8` and `LC_ALL` unset, and denies under `LC_ALL=C`, `LANG=C`, and no locale.
/// The port classifies the ASCII letters after the byte under every locale, so it denies all three
/// inputs below. That is the decided contract.
#[cfg(unix)]
#[test]
fn a_non_utf8_tool_name_is_classified_by_its_ascii_letters() {
    use std::os::unix::ffi::OsStrExt;
    let (root, state) = primary("non-utf8");
    assert_allow(
        &guard_without_harness(&root, &state)
            .arg("--tool")
            .arg(std::ffi::OsStr::from_bytes(b"Bash\xff"))
            .output()
            .expect("mcao runs"),
        "Bash with a stray byte",
    );
    let output = guard_without_harness(&root, &state)
        .arg("--tool")
        .arg(std::ffi::OsStr::from_bytes(b"Agent\xff"))
        .output()
        .expect("mcao runs");
    assert_eq!(
        output.status.code(),
        Some(2),
        "Agent with a stray byte must deny"
    );
    let output = guard_without_harness(&root, &state)
        .arg("--tool")
        .arg(std::ffi::OsStr::from_bytes(b"\xffAgent"))
        .output()
        .expect("mcao runs");
    assert_eq!(
        output.status.code(),
        Some(2),
        "a stray byte before Agent must deny: the port classifies every ASCII letter"
    );
}

/// Claude's deny for input the port could not read: exit 2, empty stdout, and the Claude object on
/// stderr. Its message names the unreadable input, not a tool.
#[cfg(unix)]
fn assert_claude_unreadable_deny(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "unreadable stdin must deny: {output:?}"
    );
    assert!(output.stdout.is_empty(), "Claude deny wrote stdout");
    let body: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("deny stderr is JSON");
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
    let message = body["systemMessage"].as_str().expect("systemMessage");
    assert!(
        message.contains("could not be read as a tool name"),
        "{message}"
    );
}

/// A directory opens, but every read fails with EISDIR, so the payload cannot be read at all.
#[cfg(unix)]
#[test]
fn a_directory_as_stdin_denies_inside_a_primary_home() {
    let (root, state) = primary("stdin-directory");
    let directory = fs::File::open(&root).expect("directory opens");
    let output = guard("claude", &root, &state)
        .stdin(Stdio::from(directory))
        .output()
        .expect("mcao runs");
    assert_claude_unreadable_deny(&output);
}
