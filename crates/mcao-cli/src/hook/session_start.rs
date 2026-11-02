//! `mcao hook session-start`: the session-open source router.
//!
//! Wires the decisions in `mcao_core::hook_source` to the behavior of
//! `bin/fm-sessionstart-run.sh`, which stays the authority. Every wiring requirement in
//! `docs/hook-source.md` is either met here or listed as a divergence in that document.
//!
//! Steps the shell runs that this port does not perform itself, such as the full digest in
//! `fm-session-start.sh`, are executed as the same child scripts the shell calls. The port does
//! not reimplement them.
//!
//! Divergences that are forced by the port, not by the shell:
//! - The firstmate root is `FM_ROOT_OVERRIDE`. The running binary's path does not prove which
//!   checkout owns its scripts, even when it is under a Cargo target directory. Without an explicit
//!   override, the port reports the missing root on stderr and stands down. The working directory
//!   and the build machine's path are never consulted.
//! - The session-lock owner check ports `fm_session_lock_owned_by_self`, reading process facts with
//!   `ps` and `kill`. Those tools exist on unix only. On Windows the ancestry walk finds nothing,
//!   so no lock is ever self-owned and `clear` or `compact` run the full digest. The `\\?\` paths
//!   that `canonicalize` returns there are not normalized for the Cursor path check.
//! - The foreign-host predicate is jq-free, but it runs only when `jq` is on `PATH`, as the
//!   shell's predicate does. Without `jq` the payload is not foreign.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::Args;
use mcao_core::hook_source::{
    self, Completion, CompletionFacts, SessionStartArgs, SourceRoute, stand_down_exit_code,
};

use crate::home_guard::hook_home_refused;

/// Arguments for `mcao hook session-start`. They follow the shell's own grammar: `--source <name>`,
/// `--source=<name>`, and `--pi-prerequisite`. Any other argument is ignored, as in the shell,
/// including `--help` and `-h`, so clap's help flag is disabled for this subcommand.
#[derive(Debug, Args)]
#[command(disable_help_flag = true)]
pub struct SessionStartHookArgs {
    #[arg(
        value_name = "ARGS",
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    args: Vec<OsString>,
}

/// Runs the session-start router. The exit code is 0 unless a stand-down under
/// `--pi-prerequisite` or a child's own exit code (nudge only) says otherwise. A refused OneDrive
/// home stands down before any state is created, so it takes the same stand-down code.
pub fn run(args: &SessionStartHookArgs) -> ExitCode {
    let (mut source, pi_prerequisite) = parse_args_os(&args.args);

    let Some(root) = firstmate_root() else {
        return stand_down(pi_prerequisite);
    };
    if is_gate_agent(&root) {
        return stand_down(pi_prerequisite);
    }
    let state = state_dir(&root);
    if hook_home_refused() {
        return stand_down(pi_prerequisite);
    }
    if let Err(error) = ensure_state_dir(&root, &state) {
        eprintln!(
            "fm-sessionstart-run: startup could not create the state directory {}: {}",
            state.display(),
            strip_os_error(&error)
        );
        return stand_down(pi_prerequisite);
    }
    if !(primary_root_matches(&root) && state.is_dir()) {
        return stand_down(pi_prerequisite);
    }

    if source.is_empty() && !io::stdin().is_terminal() {
        let mut raw = Vec::new();
        // The shell keeps whatever `cat` produced before a read error, so a partial read stands.
        let _ = io::stdin().read_to_end(&mut raw);
        let payload = hook_source::normalize_command_substitution(&raw);
        if jq_on_path() && hook_source::payload_is_foreign_host(&payload) {
            return ExitCode::SUCCESS;
        }
        source = os_from_bytes(&hook_source::extract_source_bytes(&payload));
    }

    // Lossy text is used only to route. Invalid UTF-8 never equals a named route, so the lossy
    // form cannot change a decision. The child receives `source` byte for byte, as the shell does.
    let routed = source.to_string_lossy().into_owned();
    let completion = completion_state(&state, &routed);
    match hook_source::route(&routed, completion) {
        SourceRoute::Nudge => run_child(&root, "fm-sessionstart-nudge.sh", &[]),
        SourceRoute::Reemit { .. } => {
            let args = [
                OsString::from("--reemit"),
                OsString::from("--source"),
                source,
            ];
            let _ = run_child(&root, "fm-session-start.sh", &args);
            ExitCode::SUCCESS
        }
        SourceRoute::Full { .. } => {
            let args = [OsString::from("--source"), source];
            let _ = run_child(&root, "fm-session-start.sh", &args);
            ExitCode::SUCCESS
        }
        _ => ExitCode::SUCCESS,
    }
}

/// Parses the shell's argument grammar on `OsString`s, so a non-UTF-8 `--source` value keeps its
/// bytes. It mirrors `hook_source::parse_args`, which takes `&str` and so cannot.
fn parse_args_os(args: &[OsString]) -> (OsString, bool) {
    let mut source = OsString::new();
    let mut pi_prerequisite = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg.as_os_str() == OsStr::new("--source") {
            // A bare trailing `--source` leaves the source empty, as the shell does.
            source = iter.next().cloned().unwrap_or_default();
        } else if let Some(value) = strip_os_prefix(arg, "--source=") {
            source = value;
        } else if arg.as_os_str() == OsStr::new("--pi-prerequisite") {
            pi_prerequisite = true;
        }
    }
    (source, pi_prerequisite)
}

/// Strips an ASCII `prefix` from an argument without a lossy conversion.
#[cfg(unix)]
fn strip_os_prefix(arg: &OsStr, prefix: &str) -> Option<OsString> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    arg.as_bytes()
        .strip_prefix(prefix.as_bytes())
        .map(|rest| OsString::from_vec(rest.to_vec()))
}

#[cfg(windows)]
fn strip_os_prefix(arg: &OsStr, prefix: &str) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let wide: Vec<u16> = arg.encode_wide().collect();
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    wide.strip_prefix(prefix.as_slice())
        .map(OsString::from_wide)
}

#[cfg(not(any(unix, windows)))]
fn strip_os_prefix(arg: &OsStr, prefix: &str) -> Option<OsString> {
    arg.to_str()?.strip_prefix(prefix).map(OsString::from)
}

fn stand_down(pi_prerequisite: bool) -> ExitCode {
    let parsed = SessionStartArgs {
        pi_prerequisite,
        ..SessionStartArgs::default()
    };
    // `stand_down_exit_code` returns 0 or 3, so the cast is exact.
    ExitCode::from(stand_down_exit_code(&parsed) as u8)
}

/// Creates the state directory when the root is a primary checkout, as `mkdir -p` does in the shell.
fn ensure_state_dir(root: &Path, state: &Path) -> io::Result<()> {
    if !state.is_dir() && primary_root_matches(root) {
        fs::create_dir_all(state)?;
    }
    Ok(())
}

/// Converts source bytes to an argument without a UTF-8 round trip where the platform allows it.
#[cfg(unix)]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Runs `bin/<script>` as the shell does, with inherited stdio. The nudge route returns the
/// child's exit code, as `exec` does; the digest routes ignore it, as `|| true` does.
fn run_child(root: &Path, script: &str, args: &[OsString]) -> ExitCode {
    let path = root.join("bin").join(script);
    match Command::new(&path).args(args).status() {
        Ok(status) => {
            let code = status.code().unwrap_or(1).clamp(0, 255);
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Err(error) => {
            eprintln!("{}: {}", path.display(), strip_os_error(&error));
            ExitCode::from(127)
        }
    }
}

/// Returns the firstmate root only when explicitly selected with `FM_ROOT_OVERRIDE`.
fn firstmate_root() -> Option<PathBuf> {
    if let Some(root) = non_empty_env("FM_ROOT_OVERRIDE") {
        return Some(PathBuf::from(root));
    }
    eprintln!("fm-sessionstart-run: no firstmate root: set FM_ROOT_OVERRIDE");
    None
}

fn state_dir(root: &Path) -> PathBuf {
    if let Some(state) = non_empty_env("FM_STATE_OVERRIDE") {
        return PathBuf::from(state);
    }
    let home = non_empty_env("FM_HOME")
        .or_else(|| non_empty_env("FM_ROOT_OVERRIDE"))
        .unwrap_or_else(|| root.as_os_str().to_owned());
    PathBuf::from(home).join("state")
}

/// Mirrors `command -v jq` in `fm_hook_payload_is_foreign_host`: the foreign-host check runs only
/// when `jq` resolves on `PATH`.
fn jq_on_path() -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| is_executable_file(&dir.join(JQ_NAME)))
}

#[cfg(unix)]
const JQ_NAME: &str = "jq";
#[cfg(not(unix))]
const JQ_NAME: &str = "jq.exe";

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

fn non_empty_env(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

/// Mirrors `fm_is_gate_agent`: a no-mistakes gate context, marked by the env var or by a
/// git common dir under `.no-mistakes/repos/*.git`.
fn is_gate_agent(root: &Path) -> bool {
    if env::var("FM_GATE_REFUSE_BYPASS").is_ok_and(|value| value == "1") {
        return false;
    }
    if env::var_os("NO_MISTAKES_GATE").is_some() {
        return true;
    }
    let Some(common) = git_output(root, "--git-common-dir") else {
        return false;
    };
    let Ok(common) = fs::canonicalize(root.join(common)) else {
        return false;
    };
    let text = common.to_string_lossy();
    text.contains("/.no-mistakes/repos/") && text.ends_with(".git")
}

/// Mirrors `fm_primary_root_matches`: a plain checkout or a marked secondmate home, with
/// `AGENTS.md` and `bin/`.
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

/// Mirrors `fm_root_is_secondmate_home`: the marker's first line, with whitespace removed, must be
/// non-empty and made of `A-Za-z0-9._-`. The line must end in a newline, as `read` requires.
fn is_secondmate_home(root: &Path) -> bool {
    let marker = root.join(".fm-secondmate-home");
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
        .filter(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | 0x0b | 0x0c))
        .collect();
    !id.is_empty()
        && id
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Gathers the lock and completion facts the shell reads, then asks the core whether session start
/// completed. Only `clear` and `compact` consult it, so other sources skip the reads.
fn completion_state(state: &Path, source: &str) -> Completion {
    if !matches!(source, "clear" | "compact") {
        return Completion::Absent;
    }
    let lock = state.join(".lock");
    let completion = state.join(".session-start-complete");
    let lock_meta = fs::symlink_metadata(&lock).ok();
    let completion_meta = fs::symlink_metadata(&completion).ok();
    let lock_text = read_normalized(&lock);
    let completion_text = read_normalized(&completion);
    let lock_is_regular_file = lock_meta.as_ref().is_some_and(|m| m.is_file());
    let lock_owned_by_self = lock_is_regular_file
        && lock_text
            .as_deref()
            .is_some_and(|text| session_lock_owned_by_self(state, text));
    let facts = CompletionFacts {
        lock_is_regular_file,
        completion_is_regular_file: completion_meta.as_ref().is_some_and(|m| m.is_file()),
        lock_owned_by_self,
        lock_text: lock_text.as_deref(),
        completion_text: completion_text.as_deref(),
    };
    if hook_source::session_start_completed(&facts) {
        Completion::Recorded
    } else {
        Completion::Absent
    }
}

/// Reads a file as command substitution does, then returns UTF-8 text or `None` on failure.
fn read_normalized(path: &Path) -> Option<String> {
    let raw = fs::read(path).ok()?;
    let normalized = hook_source::normalize_command_substitution(&raw);
    String::from_utf8(normalized).ok()
}

/// Mirrors `fm_session_lock_owned_by_self`. The recorded pid must be a harness ancestor of this
/// process. Failing that, the lock must have been recorded by this same trusted Claude session, and
/// its pid must still be a live harness.
fn session_lock_owned_by_self(state: &Path, lock_text: &str) -> bool {
    let Some(lock_pid) = parse_pid(lock_text) else {
        return false;
    };
    let Some(ancestry) = harness_ancestry() else {
        return false;
    };
    // The shell compares the recorded text with each ancestor pid as a string, so `0123` is not `123`.
    if ancestry
        .iter()
        .any(|entry| entry.pid.to_string() == lock_text)
    {
        return true;
    }
    same_session(state, &ancestry) && harness_pid_alive(lock_pid)
}

/// A process in this process's harness ancestry. `claude` marks a Claude Code process.
#[derive(Debug, Clone, Copy)]
struct HarnessProcess {
    pid: u32,
    claude: bool,
}

/// Walks up to 16 ancestors and returns the contiguous harness run, innermost first, as
/// `fm_harness_ancestry_pids` does. A Claude run extends through its nested workers; any other
/// harness ends the run at its first match.
fn harness_ancestry() -> Option<Vec<HarnessProcess>> {
    let mut pid = std::process::id();
    let mut found = Vec::new();
    let mut extending = false;
    for _ in 0..16 {
        let Some(comm) = ps_field(pid, "comm=") else {
            break;
        };
        let args = ps_field(pid, "args=").unwrap_or_default();
        if let Some(claude) = harness_process(&comm, &args) {
            found.push(HarnessProcess { pid, claude });
            if !claude {
                break;
            }
            extending = true;
        } else if extending {
            break;
        }
        let Some(parent) =
            ps_field(pid, "ppid=").and_then(|text| parse_pid(&text.replace(' ', "")))
        else {
            break;
        };
        if parent == 0 {
            break;
        }
        pid = parent;
    }
    (!found.is_empty()).then_some(found)
}

/// Harness names matched as substrings of a command name or argument text. `pi`, `pi-signed`,
/// `omp`, and `copilot` match exactly, because a substring match would claim `ompd` or
/// `copilot-language-server`.
const HARNESS_SUBSTRINGS: [&str; 5] = ["claude", "codex", "opencode", "grok", "kimi"];
const HARNESS_EXACT: [&str; 4] = ["pi", "pi-signed", "omp", "copilot"];
/// Harness executable names matched as whole path components, in the shell's order.
const HARNESS_NAMES: [&str; 9] = [
    "claude",
    "codex",
    "opencode",
    "grok",
    "kimi",
    "pi-signed",
    "pi",
    "omp",
    "copilot",
];

/// Returns `Some(is_claude)` when a command name and argument string describe a verified harness,
/// using the shell's evidence and order: the basename, an exact harness path component in the
/// command or argv0, a node or python interpreter running a harness, then Cursor's identity.
fn harness_process(comm: &str, args: &str) -> Option<bool> {
    let base = basename(comm);
    if harness_name_matches(base) {
        return Some(base.contains("claude"));
    }
    let argv0 = args.split(' ').next().unwrap_or_default();
    if let Some(name) = harness_path_name(comm).or_else(|| harness_path_name(argv0)) {
        return Some(name == "claude");
    }
    if (comm.contains("node") || comm.contains("python")) && harness_name_matches(args) {
        return Some(args.contains("claude"));
    }
    cursor_process_matches(comm, argv0).then_some(false)
}

fn harness_name_matches(text: &str) -> bool {
    HARNESS_SUBSTRINGS.iter().any(|name| text.contains(name)) || HARNESS_EXACT.contains(&text)
}

fn harness_path_name(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return None;
    }
    let wrapped = format!("/{path}/");
    HARNESS_NAMES
        .into_iter()
        .find(|name| wrapped.contains(&format!("/{name}/")))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Mirrors `fm_cursor_process_matches`, which checks names and install paths only and runs no
/// probe. A Cursor process is named `cursor-agent`, or is a generic interpreter or legacy `agent`
/// whose argv0 or command path is a cursor-agent install.
fn cursor_process_matches(comm: &str, argv0: &str) -> bool {
    if comm.is_empty() && argv0.is_empty() {
        return false;
    }
    let argv0 = if argv0.is_empty() { comm } else { argv0 };
    let base = basename(comm);
    let base = base.strip_prefix('-').unwrap_or(base);
    if base == "cursor-agent" {
        return true;
    }
    if is_cursor_interpreter_name(base) {
        return cursor_argv0_matches(argv0) || cursor_path_matches(comm);
    }
    comm.contains('/') && cursor_path_matches(comm)
}

fn is_cursor_interpreter_name(base: &str) -> bool {
    if matches!(base, "agent" | "MainThread") {
        return true;
    }
    ["node", "python"].iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with('-')
                || rest.starts_with(|c: char| c.is_ascii_digit())
        })
    })
}

fn cursor_argv0_matches(argv0: &str) -> bool {
    match argv0 {
        "" | "MainThread" => false,
        "cursor-agent" => true,
        _ => cursor_path_matches(argv0),
    }
}

/// Mirrors `fm_cursor_path_is_cursor`: the resolved name is `cursor-agent`, or the path lies under a
/// `cursor-agent/versions/<version>/` install. A path that cannot be resolved is judged as written.
fn cursor_path_matches(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let canonical = canonical.to_string_lossy();
    if basename(&canonical) == "cursor-agent" {
        return true;
    }
    const VERSIONS: &str = "/cursor-agent/versions/";
    canonical
        .find(VERSIONS)
        .is_some_and(|at| canonical[at + VERSIONS.len()..].contains('/'))
}

/// Mirrors `fm_session_lock_same_session`: the trusted Claude session id equals the id recorded
/// beside the lock.
fn same_session(state: &Path, ancestry: &[HarnessProcess]) -> bool {
    let Some(trusted) = trusted_session_id(ancestry) else {
        return false;
    };
    recorded_session_id(state).is_some_and(|recorded| recorded == trusted)
}

/// Mirrors `fm_session_lock_trusted_session_id`. The Claude id is trusted only when `CLAUDE_PID`
/// names a Claude process in this ancestry, so an id left in a hand-started harness environment is
/// ignored.
fn trusted_session_id(ancestry: &[HarnessProcess]) -> Option<String> {
    let id = env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty())?;
    if id.contains(['\n', '\r']) {
        return None;
    }
    let claude_pid = parse_pid(&env::var("CLAUDE_PID").ok()?)?;
    ancestry
        .iter()
        .find(|entry| entry.pid == claude_pid)
        .filter(|entry| entry.claude)
        .map(|_| id)
}

/// Mirrors `fm_session_lock_recorded_session_id`: the first line of `state/.lock-session`, unless
/// the file is missing, a symlink, empty, or carries a carriage return.
fn recorded_session_id(state: &Path) -> Option<String> {
    let path = state.join(".lock-session");
    if !fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let raw = fs::read(&path).ok()?;
    let first = raw.split(|&byte| byte == b'\n').next()?;
    let line = String::from_utf8(first.to_vec()).ok()?;
    (!line.is_empty() && !line.contains('\r')).then_some(line)
}

/// Mirrors `fm_harness_pid_alive`: the pid answers `kill -0` and still looks like a harness.
fn harness_pid_alive(pid: u32) -> bool {
    let signalable = Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    signalable
        && ps_field(pid, "comm=").is_some_and(|comm| {
            let args = ps_field(pid, "args=").unwrap_or_default();
            harness_process(&comm, &args).is_some()
        })
}

/// Parses a pid as the shell's digit guard does: non-empty ASCII digits that fit a `u32`.
fn parse_pid(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Reads one `ps -o <field>` value for `pid`, without its trailing newlines, as command
/// substitution does.
fn ps_field(pid: u32, field: &str) -> Option<String> {
    let pid_text = pid.to_string();
    let output = Command::new("ps")
        .args(["-o", field, "-p", pid_text.as_str()])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.trim_end_matches('\n').to_owned())
}

/// Runs `git -C <dir> rev-parse <arg>` and returns trimmed stdout, or `None` on failure.
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

/// Removes Rust's `(os error N)` suffix, so the text matches the shell's `strerror` text.
fn strip_os_error(error: &io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error") {
        Some(index) => text[..index].to_owned(),
        None => text,
    }
}
