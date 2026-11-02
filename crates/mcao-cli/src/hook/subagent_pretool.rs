//! `mcao hook subagent-pretool`: the PreToolUse guard against delegation outside the fleet.
//! The guard is advisory and non-sandbox; see `docs/subagent-guard.md#advisory-status`.
//!
//! Ports `bin/fm-subagent-pretool-check.sh`, the behavioral authority. A tool name is
//! delegation-shaped when its normalized form contains a stem from [`DELEGATION_STEMS`]. In a
//! genuine firstmate primary home such a name is denied: exit 2, a Claude-shaped deny object on
//! stderr, and a Grok-shaped object on stdout unless Claude mode is on. Claude mode comes from
//! `--claude` or `--harness claude`, because Claude requires stdout to stay empty on deny. With
//! neither, stdout gets the Grok object, as the shell's default does. Every other outcome exits 0
//! with no output.
//!
//! Divergences from the shell, each forced by the port itself:
//! - The firstmate root is `--root`, else `FM_ROOT_OVERRIDE`, else `CLAUDE_PROJECT_DIR`. The port
//!   never reads the working directory, so a call with no explicit root is inert. The shell derives
//!   its root from its script location, which a binary does not have. The registration must pass
//!   one of these inputs before the hook is wired into any harness.
//! - `--harness` is port-only. The shell rejects it with exit 2, while the port accepts it.
//! - Tool names are parsed in-process, so the shell's missing-`jq` fail-open branch does not
//!   exist.
//! - Fail closed on stdin the port cannot read as a tool name. The shell allows any payload that
//!   `jq` rejects, but the port denies a payload that is not a strict stream of JSON values, such
//!   as `{not-json`, or a byte-order mark after the first byte. In probes with jq 1.8.1, it accepted
//!   `1e1000` and nested arrays at depths 128, 129, and 256; the Rust hook rejected those inputs.
//!   With a delegation-shaped tool name, the shell denied the `1e1000` payload and payloads with
//!   nested arrays at those tested depths. A lone high surrogate escape (`\ud800`) in a key or value
//!   was rejected by jq 1.8.1, so the shell allowed that payload; jq accepted a lone low surrogate
//!   (`\udc00`) in either position, so the shell denied a delegation-shaped payload containing it.
//!   The Rust hook denied both escapes in keys and values. These jq outcomes are version-qualified
//!   probe results, not a cross-version guarantee. Raw NUL bytes are dropped first, as bash's
//!   `$(cat)` drops them.
//! - A leading UTF-8 byte-order mark is stripped, as `jq` strips it. Only the first byte is checked.
//! - Only the last top-level value decides a non-object outcome. `jq` reads every top-level value
//!   and keeps the names of the objects before it, but its exit status comes from the last value. A
//!   last value that is neither an object nor null fails the run, and the shell then allows the call.
//!   So `"x"{"tool_name":"Agent"}` denies, as the shell does, and `{"tool_name":"Agent"}"x"` allows.
//! - Stdin that fails with an error other than EBADF, such as a directory, denies. In a Darwin
//!   ARM64 probe with bash 5.3.20 and jq 1.8.1, the full shell hook allowed an empty non-blocking
//!   pipe (whose read returned EAGAIN); the Rust hook denied it. Rust allowed closed stdin as an
//!   empty payload, while the full shell hook did not terminate within one second on closed stdin
//!   in that probe. This is a version- and platform-specific observation, not a general Bash
//!   closed-stdin guarantee.
//! - A non-string `tool_name`, or `toolName` when `tool_name` is null or false, denies. The shell
//!   classifies the text `jq` prints for an object or array, which the port does not reproduce.
//!   The port denies every non-string name without classifying it.
//! - Output JSON comes from serde_json, which escapes control characters that the shell emits
//!   raw, such as `\t` and `\x01` in a tool name. The deny objects on stderr and stdout differ
//!   from the shell's only in that escaping. Classification keeps only `a-z` and `0-9`, so the
//!   escaped bytes never change the exit code.
//! - A non-UTF-8 `--tool` value is converted lossily, so the port classifies every ASCII letter
//!   and digit in it. In the same Darwin ARM64/bash 5.3.20 probe, the shell allowed `$'\xffAgent'`
//!   with `LANG=en_US.UTF-8` and `LC_ALL` unset; it denied with `LANG=C`, `LC_ALL=C`,
//!   `LC_ALL=en_US.UTF-8`, and with both variables unset. The Rust hook denied in all five
//!   environments. These are observed locale-specific outcomes, not a cross-platform guarantee.
//! - Stdin is read as bytes and decoded lossily. An invalid byte inside a JSON string does not
//!   change the tool name. Outside a string it is a parse failure, which fails closed.
//!
//! Usage errors match the shell's exit codes: an unknown flag, a missing `--tool` value, and an
//! unknown `--harness` value exit 2 in both. `--help` exits 0 in both, with different text.
//!
//! DPES-894 AC2 (an unknown stem exits 2) is not implemented. The shipped contract classifies a
//! tool name by shape and fails open for every name it does not match, as
//! `tests/fm-subagent-pretool-check.test.sh` pins. Denying unknown names would block `Bash` and
//! `Read`.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::{Args, ValueEnum};
use serde_json::{Value, json};

/// Lowercase substrings that make a normalized tool name delegation-shaped. The first match is
/// the one the deny message names.
const DELEGATION_STEMS: &[&str] = &[
    "agent",
    "subagent",
    "task",
    "workflow",
    "cron",
    "schedul",
    "worktree",
    "delegate",
    "spawn",
    "dispatch",
    "handoff",
    "sendmessage",
    "remote",
    "monitor",
];

/// Exact normalized names that match a stem but only observe or stop work that already exists.
const OBSERVE_ONLY_TOOLS: &[&str] = &[
    "taskoutput",
    "taskstop",
    "taskget",
    "tasklist",
    "cronlist",
    "bashoutput",
    "killshell",
];

/// Exact normalized names that write only the session-local todo list, which has no executor.
const PLAN_ONLY_TOOLS: &[&str] = &["taskcreate", "taskupdate"];

const SCOUT_ROUTE: &str = "first classify the work under the AGENTS.md intake contract: work already classified as a scout goes to bin/fm-scout.sh \"<question>\" [project], while authorized ship work and its bounded research go to bin/fm-brief.sh then bin/fm-spawn.sh";
const BRIEF_ONLY_ROUTE: &str = "first classify the work under the AGENTS.md intake contract, then use bin/fm-brief.sh followed by bin/fm-spawn.sh for dispatched work";
const SECONDMATE_MARKER: &str = ".fm-secondmate-home";

/// The harness that delivered the PreToolUse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    Claude,
    Codex,
    Grok,
    Opencode,
    Pi,
}

/// Flags for `mcao hook subagent-pretool`.
#[derive(Debug, Args)]
pub struct SubagentPretoolArgs {
    /// Harness that delivered the event. Only `claude` changes the output, as `--claude` does.
    /// Without it, the output is the shell's default: a Grok-shaped object on stdout. A repeated
    /// flag keeps the last value.
    #[arg(long, value_enum, overrides_with = "harness")]
    harness: Option<Harness>,
    /// Claude mode under the shell's own spelling, the form `.claude/settings.json` passes. A
    /// repeated flag is accepted, as in the shell.
    #[arg(long, overrides_with = "claude")]
    claude: bool,
    /// Tool name to classify. A repeated flag keeps the last value, as in the shell. Without it,
    /// the name is read from a PreToolUse payload on stdin.
    #[arg(
        long,
        value_name = "TOOL",
        allow_hyphen_values = true,
        overrides_with = "tool"
    )]
    tool: Option<OsString>,
    /// Firstmate root to guard. Without it, `FM_ROOT_OVERRIDE` and then `CLAUDE_PROJECT_DIR` are
    /// read. The working directory is never used. A repeated flag keeps the last value.
    #[arg(long, value_name = "DIR", overrides_with = "root")]
    root: Option<PathBuf>,
}

/// Why a call is blocked: a delegation-shaped tool name, or a payload that cannot be read as one.
enum Block<'a> {
    Delegation { tool: &'a str, stem: &'static str },
    Unreadable,
}

/// Runs the guard. Exit 2 denies; every other outcome exits 0. Input that cannot be read as a tool
/// name denies inside a primary home.
pub fn run(args: &SubagentPretoolArgs) -> ExitCode {
    let payload = match &args.tool {
        Some(tool) => PayloadTool::Name(tool.to_string_lossy().into_owned()),
        None => tool_from_stdin(),
    };
    let block = match &payload {
        PayloadTool::Name(tool) => match delegation_stem(tool) {
            Some(stem) => Block::Delegation { tool, stem },
            None => return ExitCode::SUCCESS,
        },
        PayloadTool::Unreadable => Block::Unreadable,
    };
    if env::var_os("FM_ALLOW_SUBAGENT").is_some_and(|value| value == "1") {
        return ExitCode::SUCCESS;
    }
    let Some(root) = firstmate_root(args.root.as_deref()) else {
        return ExitCode::SUCCESS;
    };
    let state = state_dir(&root);
    if !primary_scope_matches(&root, &state) {
        return ExitCode::SUCCESS;
    }
    let claude = args.claude || args.harness == Some(Harness::Claude);
    deny(&block, &root, claude)
}

/// Returns the stem a tool name is delegation-shaped on, or `None` when the name is allowed.
#[must_use]
pub fn delegation_stem(tool: &str) -> Option<&'static str> {
    if tool.starts_with("mcp__") {
        return None;
    }
    let normalized = normalize(tool);
    if OBSERVE_ONLY_TOOLS.contains(&normalized.as_str())
        || PLAN_ONLY_TOOLS.contains(&normalized.as_str())
    {
        return None;
    }
    DELEGATION_STEMS
        .iter()
        .copied()
        .find(|stem| normalized.contains(*stem))
}

/// Lowercases ASCII letters and keeps only `a-z` and `0-9`, as the shell's `tr` pipeline does.
fn normalize(tool: &str) -> String {
    tool.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .filter(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        .map(char::from)
        .collect()
}

/// Reads the tool name from a PreToolUse payload on stdin. Stdin that fails to read is
/// [`PayloadTool::Unreadable`], which denies inside a primary home; an empty read is not an error
/// and allows. Bytes are read as the shell reads them: `$(cat)` drops NUL bytes, and invalid UTF-8
/// is replaced rather than rejected, so a stray byte inside a string cannot turn a delegation-shaped
/// name into an allow.
fn tool_from_stdin() -> PayloadTool {
    let mut payload = Vec::new();
    if io::stdin().read_to_end(&mut payload).is_err() {
        return PayloadTool::Unreadable;
    }
    payload.retain(|&byte| byte != 0);
    tool_from_payload(&String::from_utf8_lossy(&payload))
}

/// What a hook payload says about the tool being called.
#[derive(Debug, PartialEq, Eq)]
pub enum PayloadTool {
    /// The name `jq -r` prints, with its lines joined. Empty when the payload names no tool.
    Name(String),
    /// Input the port cannot read as a plain tool name.
    Unreadable,
}

/// Extracts the tool name the shell reads with `jq -r '(.tool_name // .toolName // empty)'`.
///
/// A leading UTF-8 byte-order mark is stripped, as `jq` strips it. Every top-level value is read,
/// and each object contributes its name. `null` contributes nothing. A name that is not a string
/// is [`PayloadTool::Unreadable`]. Only the last value decides a non-object outcome: `jq` exits
/// nonzero when the last value is neither an object nor null, and the shell then allows the call
/// whatever the earlier values named, so this returns an empty name. A payload that is not a strict
/// JSON stream is [`PayloadTool::Unreadable`].
#[must_use]
pub fn tool_from_payload(payload: &str) -> PayloadTool {
    let payload = payload.strip_prefix('\u{feff}').unwrap_or(payload);
    let mut names = Vec::new();
    let mut unreadable = false;
    let mut last_is_non_object = false;
    for value in serde_json::Deserializer::from_str(payload).into_iter::<Value>() {
        let Ok(value) = value else {
            return PayloadTool::Unreadable;
        };
        last_is_non_object = false;
        match value {
            Value::Null => {}
            Value::Object(fields) => {
                let name = fields
                    .get("tool_name")
                    .filter(|value| is_truthy(value))
                    .or_else(|| fields.get("toolName").filter(|value| is_truthy(value)));
                match name {
                    None => {}
                    Some(Value::String(text)) => names.push(text.clone()),
                    Some(_) => unreadable = true,
                }
            }
            _ => last_is_non_object = true,
        }
    }
    if last_is_non_object {
        PayloadTool::Name(String::new())
    } else if unreadable {
        PayloadTool::Unreadable
    } else {
        PayloadTool::Name(names.join("\n").trim_end_matches('\n').to_owned())
    }
}

/// Matches jq's `//` falsiness: only `null` and `false` fall through to the next alternative.
fn is_truthy(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false))
}

/// Returns the firstmate root from an explicit input: `--root`, then `FM_ROOT_OVERRIDE`, then
/// `CLAUDE_PROJECT_DIR`. The working directory is never consulted.
fn firstmate_root(explicit: Option<&Path>) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| non_empty_env("FM_ROOT_OVERRIDE").map(PathBuf::from))
        .or_else(|| non_empty_env("CLAUDE_PROJECT_DIR").map(PathBuf::from))
}

/// Returns the state directory: `FM_STATE_OVERRIDE`, else `state` under `FM_HOME`, which
/// defaults to `FM_ROOT_OVERRIDE` and then to the firstmate root.
fn state_dir(root: &Path) -> PathBuf {
    if let Some(state) = non_empty_env("FM_STATE_OVERRIDE") {
        return PathBuf::from(state);
    }
    let home = non_empty_env("FM_HOME")
        .or_else(|| non_empty_env("FM_ROOT_OVERRIDE"))
        .unwrap_or_else(|| root.as_os_str().to_owned());
    PathBuf::from(home).join("state")
}

fn non_empty_env(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

/// True when the state directory exists beside a genuine primary root.
fn primary_scope_matches(root: &Path, state: &Path) -> bool {
    primary_root_matches(root) && state.is_dir()
}

/// True for a marked secondmate home, or a plain checkout whose git dir is its common dir. A
/// linked task worktree is neither, so it stays out of scope.
fn primary_root_matches(root: &Path) -> bool {
    if !is_secondmate_home(root) {
        let (Some(git_dir), Some(common_dir)) = (
            git_output(root, "--git-dir"),
            git_output(root, "--git-common-dir"),
        ) else {
            return false;
        };
        if git_dir != common_dir {
            return false;
        }
    }
    root.join("AGENTS.md").is_file() && root.join("bin").is_dir()
}

/// True when `root` holds a valid `.fm-secondmate-home` marker. The shell's `read` fails at end
/// of file without a newline, so the marker's first line must end in one.
fn is_secondmate_home(root: &Path) -> bool {
    let marker = root.join(SECONDMATE_MARKER);
    if fs::symlink_metadata(&marker).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    if !marker.is_file() {
        return false;
    }
    let Ok(bytes) = fs::read(&marker) else {
        return false;
    };
    let Some(line_end) = bytes.iter().position(|&byte| byte == b'\n') else {
        return false;
    };
    let id: Vec<u8> = bytes[..line_end]
        .iter()
        .copied()
        .filter(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
        .collect();
    !id.is_empty()
        && id
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Runs `git -C <dir> rev-parse <arg>` and returns its trimmed stdout, or `None` on failure.
fn git_output(dir: &Path, arg: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", arg])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim_end_matches('\n').to_owned())
}

/// Emits the deny decision and returns exit 2. The Claude object goes to stderr; the Grok object
/// goes to stdout unless `claude` is set.
fn deny(block: &Block<'_>, root: &Path, claude: bool) -> ExitCode {
    let route = if root.join("bin").join("fm-scout.sh").is_file() {
        SCOUT_ROUTE
    } else {
        BRIEF_ONLY_ROUTE
    };
    let reason = match block {
        Block::Delegation { tool, stem } => format!(
            "[subagent-dispatch] the firstmate primary dispatches through the fleet, not the harness's own delegation tools: work started that way has no durable fleet record, leaves every firstmate guard inert, and dies with this session. Instead, {route} (blocked tool: {tool}, delegation-shaped on \"{stem}\"). Launch the session with FM_ALLOW_SUBAGENT=1 for a deliberate exception."
        ),
        Block::Unreadable => format!(
            "[subagent-dispatch] the hook input could not be read as a tool name, so the firstmate primary blocks the call. Instead, {route}. Launch the session with FM_ALLOW_SUBAGENT=1 for a deliberate exception."
        ),
    }
    // The shell's json_escape turns each newline in the reason, even one inside a tool name, into a space.
    .replace('\n', " ");
    let claude_object = json!({
        "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"},
        "systemMessage": reason,
    });
    let _ = writeln!(io::stderr(), "{claude_object}");
    if !claude {
        let grok_object = json!({"decision": "deny", "reason": reason});
        let _ = writeln!(io::stdout(), "{grok_object}");
    }
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keeps_only_lowercase_alphanumerics() {
        assert_eq!(normalize("Task Output"), "taskoutput");
        assert_eq!(normalize("TASK_OUTPUT"), "taskoutput");
        assert_eq!(normalize("Ünïcode9"), "ncode9");
    }

    #[test]
    fn delegation_shape_follows_the_shell_exceptions() {
        for tool in [
            "Task",
            "Agent",
            "Workflow",
            "ScheduleWakeup",
            "EnterWorktree",
            "SendMessage",
        ] {
            assert!(
                delegation_stem(tool).is_some(),
                "{tool} must be delegation-shaped"
            );
        }
        for tool in [
            "TaskOutput",
            "TaskStop",
            "TaskGet",
            "TaskList",
            "CronList",
            "BashOutput",
            "KillShell",
        ] {
            assert_eq!(delegation_stem(tool), None, "{tool} observes or stops work");
        }
        for tool in ["TaskCreate", "TaskUpdate"] {
            assert_eq!(
                delegation_stem(tool),
                None,
                "{tool} writes only the todo list"
            );
        }
        for tool in [
            "TaskCreateAgent",
            "TaskCreateWorktree",
            "TaskUpdateAgent",
            "RemoteTaskCreate",
            "TaskCreator",
        ] {
            assert!(
                delegation_stem(tool).is_some(),
                "{tool} must not match a plan-only name"
            );
        }
        for tool in [
            "Bash",
            "Edit",
            "Read",
            "WebSearch",
            "Skill",
            "ToolSearch",
            "unknown",
        ] {
            assert_eq!(delegation_stem(tool), None, "{tool} is ordinary");
        }
        assert_eq!(delegation_stem("mcp__tracker__create_task"), None);
        assert_eq!(delegation_stem(""), None);
    }

    #[test]
    fn first_stem_in_shell_order_is_the_one_named() {
        assert_eq!(delegation_stem("SubagentCreate"), Some("agent"));
        assert_eq!(delegation_stem("RemoteExec"), Some("remote"));
    }

    fn named(tool: &str) -> PayloadTool {
        PayloadTool::Name(tool.to_owned())
    }

    #[test]
    fn payload_name_follows_jq_alternatives() {
        assert_eq!(
            tool_from_payload(r#"{"tool_name":"Agent"}"#),
            named("Agent")
        );
        assert_eq!(tool_from_payload(r#"{"toolName":"Agent"}"#), named("Agent"));
        assert_eq!(
            tool_from_payload(r#"{"tool_name":false,"toolName":"Agent"}"#),
            named("Agent")
        );
        assert_eq!(
            tool_from_payload(r#"{"tool_name":null,"toolName":"Agent"}"#),
            named("Agent")
        );
        assert_eq!(
            tool_from_payload(r#"{"tool_name":"","toolName":"Agent"}"#),
            named("")
        );
        assert_eq!(tool_from_payload(r#"{"tool_name":null}"#), named(""));
        assert_eq!(tool_from_payload("{}"), named(""));
        assert_eq!(tool_from_payload("null"), named(""));
    }

    #[test]
    fn non_string_names_are_unreadable() {
        for payload in [
            r#"{"tool_name":["agent"]}"#,
            r#"{"tool_name":{"a":"age","":"nt"}}"#,
            r#"{"tool_name":5}"#,
            r#"{"tool_name":false,"toolName":{}}"#,
        ] {
            assert_eq!(
                tool_from_payload(payload),
                PayloadTool::Unreadable,
                "{payload}"
            );
        }
    }

    #[test]
    fn payloads_that_are_not_strict_json_streams_are_unreadable() {
        for payload in [
            "{not-json",
            r#"{"tool_name":"Agent"} trailing"#,
            r#"{"tool_name":"Agent"} {"#,
            r#"{"tool_name":"\udc00"}"#,
            r#"{"tool_name":"Agent","x":1e1000}"#,
        ] {
            assert_eq!(
                tool_from_payload(payload),
                PayloadTool::Unreadable,
                "{payload}"
            );
        }
    }

    #[test]
    fn values_that_are_not_objects_contribute_no_name() {
        for payload in [
            "",
            "   ",
            r#""Agent""#,
            "[1]",
            "42",
            r#"{"tool_name":"Agent"} 42"#,
        ] {
            assert_eq!(tool_from_payload(payload), named(""), "{payload:?}");
        }
    }

    #[test]
    fn a_stream_of_objects_contributes_each_name() {
        assert_eq!(
            tool_from_payload(r#"{"tool_name":"Read"}{"tool_name":"Agent"}"#),
            named("Read\nAgent")
        );
    }
}
