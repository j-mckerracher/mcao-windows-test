//! Golden parity for `mcao hook session-start` (DPES-893), against `bin/fm-sessionstart-run.sh`.
//!
//! Each case builds a scratch firstmate root under the cargo target tmp dir. The root holds copies of
//! the shell's own run script and helper libraries from `bin/`, plus stub children that record their
//! argv, so neither side starts a real digest. The shell and `mcao` run against the same root and the
//! same stdin, and their exit code, stdout, and stderr must match.
//!
//! Prerequisites:
//! - `bash`, `git`, and `ps` on `PATH`, as the shell needs them. Unix-only cases also need `kill`.
//! - `jq` on `PATH` for the foreign-host case only. The shell's foreign-host predicate calls `jq`,
//!   and without it the payload is not foreign, so that case prints `SKIP` and returns. The
//!   missing-`jq` case builds its own `PATH` without `jq` and runs on every host.
//!
//! Unix-only: Windows has no runtime parity for this hook, because the session-lock ancestry walk
//! needs `ps` and `kill`. `docs/hook-source.md` records that no Windows runtime test exists.

#![cfg(unix)]

#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SHELL_LIBS: &[&str] = &[
    "fm-sessionstart-run.sh",
    "fm-gate-refuse-lib.sh",
    "fm-primary-scope-lib.sh",
    "fm-session-lock-lib.sh",
    "fm-cursor-lib.sh",
    "fm-hook-host-lib.sh",
];

const STUBS: &[&str] = &["fm-session-start.sh", "fm-sessionstart-nudge.sh"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_session_start")
}

/// Builds a scratch primary firstmate root. `git_init` controls whether it is a plain checkout.
fn scratch_root(name: &str, git_init: bool) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hook-session-start-{name}"));
    let _ = fs::remove_dir_all(&root);
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("create scratch bin");
    for lib in SHELL_LIBS {
        fs::copy(repo_root().join("bin").join(lib), bin.join(lib)).expect("copy shell lib");
    }
    for stub in STUBS {
        fs::copy(fixtures().join(stub), bin.join(stub)).expect("copy stub child");
    }
    fs::write(root.join("AGENTS.md"), "# scratch\n").expect("write AGENTS.md");
    fs::create_dir_all(root.join("state")).expect("create state dir");
    if git_init {
        run_git(&root, &["init", "-q"]);
    }
    root
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// Runs one invocation of the shell or of `mcao`, with the same environment and stdin.
fn invoke(program: Command, root: &Path, stdin: &[u8]) -> Output {
    let mut command = program;
    command
        .env("FM_ROOT_OVERRIDE", root)
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .env_remove("NO_MISTAKES_GATE")
        .env_remove("FM_GATE_REFUSE_BYPASS")
        .env_remove("CLAUDE_PID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn session-start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("wait session-start")
}

fn shell(root: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut command = Command::new("bash");
    command
        .arg(root.join("bin/fm-sessionstart-run.sh"))
        .args(args);
    invoke(command, root, stdin)
}

fn mcao(root: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command.args(["hook", "session-start"]).args(args);
    invoke(command, root, stdin)
}

/// Asserts that the shell and `mcao` agree on exit code, stdout, and stderr.
fn assert_parity(name: &str, args: &[&str], stdin: impl AsRef<[u8]>, root: &Path) -> Output {
    let expected = shell(root, args, stdin.as_ref());
    let actual = mcao(root, args, stdin.as_ref());
    assert_same(name, &expected, &actual);
    actual
}

fn assert_same(name: &str, expected: &Output, actual: &Output) {
    assert_eq!(
        actual.status.code(),
        expected.status.code(),
        "{name}: exit code"
    );
    assert_eq!(actual.stdout, expected.stdout, "{name}: stdout");
    assert_eq!(actual.stderr, expected.stderr, "{name}: stderr");
}

/// Runs `command` under a fake harness: a shell whose argv[0] is a Claude Code install path, so the
/// ancestry check sees a harness-shaped process. The shell writes its own pid as the lock and the
/// completion record, using the given printf formats, so the lock owner is an ancestor of the child.
#[cfg(unix)]
fn under_harness(
    root: &Path,
    command: &[OsString],
    stdin: &[u8],
    lock_fmt: &str,
    completion_fmt: &str,
) -> Output {
    use std::os::unix::process::CommandExt;
    let script = format!(
        "printf '{lock_fmt}' \"$$\" > \"$1/state/.lock\"; \
         printf '{completion_fmt}' \"$$\" > \"$1/state/.session-start-complete\"; \
         shift; \"$@\"; exit $?"
    );
    let mut program = Command::new("bash");
    program
        .arg0(root.join("claude").join("fake-harness"))
        .arg("-c")
        .arg(script)
        .arg("fake-harness")
        .arg(root)
        .args(command);
    invoke(program, root, stdin)
}

/// Runs the shell and `mcao` under the same fake harness owner and asserts parity. The lock and
/// completion formats control the trailing newlines the owner writes.
#[cfg(unix)]
fn assert_harness_parity(
    name: &str,
    source_args: &[&str],
    stdin: &str,
    lock_fmt: &str,
    completion_fmt: &str,
    root: &Path,
) -> Output {
    let (shell_cmd, mcao_cmd) = command_pair(root, source_args);
    let expected = under_harness(root, &shell_cmd, stdin.as_bytes(), lock_fmt, completion_fmt);
    let actual = under_harness(root, &mcao_cmd, stdin.as_bytes(), lock_fmt, completion_fmt);
    assert_same(name, &expected, &actual);
    actual
}

/// The argv of the shell and of `mcao` for one source argument list.
#[cfg(unix)]
fn command_pair(root: &Path, source_args: &[&str]) -> (Vec<OsString>, Vec<OsString>) {
    let mut shell_cmd = vec![
        OsString::from("bash"),
        root.join("bin/fm-sessionstart-run.sh").into_os_string(),
    ];
    shell_cmd.extend(source_args.iter().map(OsString::from));
    let mut mcao_cmd = vec![
        OsString::from(env!("CARGO_BIN_EXE_mcao")),
        OsString::from("hook"),
        OsString::from("session-start"),
    ];
    mcao_cmd.extend(source_args.iter().map(OsString::from));
    (shell_cmd, mcao_cmd)
}

#[test]
fn resume_routes_to_the_nudge_with_the_same_exit() {
    let root = scratch_root("resume", true);
    let out = assert_parity("resume", &["--source", "resume"], "", &root);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

#[test]
fn startup_argument_runs_the_full_digest() {
    let root = scratch_root("startup-arg", true);
    let out = assert_parity("startup argument", &["--source=startup"], "", &root);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [startup]\n"
    );
}

#[test]
fn payload_source_is_read_from_stdin() {
    let root = scratch_root("payload", true);
    let out = assert_parity(
        "payload source",
        &[],
        r#"{"session_id":"s","source":"startup"}"#,
        &root,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [startup]\n"
    );
}

#[test]
fn clear_without_a_completion_record_runs_the_full_digest() {
    let root = scratch_root("clear-absent", true);
    let out = assert_parity("clear, not completed", &["--source", "clear"], "", &root);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

#[test]
fn foreign_host_payload_stands_down_silently() {
    if !host_has_jq() {
        eprintln!("SKIP foreign_host_payload_stands_down_silently: jq is not on PATH");
        return;
    }
    let root = scratch_root("foreign", true);
    let out = assert_parity(
        "foreign host",
        &[],
        r#"{"cursor_version":"1.0","source":"clear"}"#,
        &root,
    );
    assert!(
        out.stdout.is_empty(),
        "a foreign host must not run the digest"
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn malformed_payload_falls_through_to_the_full_run_with_an_empty_source() {
    let root = scratch_root("malformed", true);
    let out = assert_parity("malformed payload", &[], "{not json", &root);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "FULL [--source] []\n");
}

#[test]
fn non_primary_root_stands_down_with_the_pi_exit_code() {
    let root = scratch_root("not-primary", false);
    let out = assert_parity(
        "non-primary under --pi-prerequisite",
        &["--source", "startup", "--pi-prerequisite"],
        "",
        &root,
    );
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn state_directory_creation_failure_reports_and_stands_down() {
    let root = scratch_root("mkdir-fail", true);
    fs::remove_dir_all(root.join("state")).expect("remove state dir");
    fs::write(root.join("state"), "not a directory\n").expect("block state path");
    let out = assert_parity(
        "state directory blocked",
        &["--source", "startup", "--pi-prerequisite"],
        "",
        &root,
    );
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("could not create the state directory"));
}

#[test]
fn clear_owned_by_a_process_outside_the_harness_ancestry_runs_the_full_digest() {
    let root = scratch_root("clear-foreign-owner", true);
    let pid = std::process::id().to_string();
    fs::write(root.join("state/.lock"), format!("{pid}\n")).expect("write lock");
    fs::write(
        root.join("state/.session-start-complete"),
        format!("{pid}\n"),
    )
    .expect("write completion");
    let out = assert_parity("clear, test owner", &["--source", "clear"], "", &root);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

#[cfg(unix)]
#[test]
fn clear_with_a_completion_owned_by_a_harness_ancestor_reemits() {
    let root = scratch_root("clear-harness-owner", true);
    let out = assert_harness_parity(
        "clear, harness owner",
        &["--source", "clear"],
        "",
        r"%s\n",
        r"%s\n",
        &root,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--reemit] [--source] [clear]\n"
    );
}

#[cfg(unix)]
#[test]
fn compact_with_a_completion_owned_by_a_harness_ancestor_reemits() {
    let root = scratch_root("compact-harness-owner", true);
    let out = assert_harness_parity(
        "compact, harness owner",
        &["--source", "compact"],
        "",
        r"%s\n",
        r"%s\n",
        &root,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--reemit] [--source] [compact]\n"
    );
}

#[cfg(unix)]
#[test]
fn trailing_newlines_in_lock_and_completion_are_normalized_before_comparison() {
    let root = scratch_root("clear-newlines", true);
    let out = assert_harness_parity(
        "clear, extra lock newlines",
        &["--source", "clear"],
        "",
        r"%s\n\n",
        "%s",
        &root,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--reemit] [--source] [clear]\n"
    );
}

#[test]
fn explicit_empty_source_reads_the_payload() {
    let root = scratch_root("empty-source", true);
    let out = assert_parity(
        "explicit empty source",
        &["--source", ""],
        r#"{"source":"resume"}"#,
        &root,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

#[test]
fn bare_trailing_source_reads_the_payload() {
    let root = scratch_root("bare-source", true);
    let out = assert_parity(
        "bare trailing source",
        &["--source"],
        r#"{"source":"clear"}"#,
        &root,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

#[test]
fn payload_with_invalid_utf8_before_the_source_key_still_extracts_the_source() {
    let root = scratch_root("payload-invalid-utf8", true);
    let out = assert_parity(
        "invalid UTF-8 payload",
        &[],
        b"{\"note\":\"\xff\xfe\",\"source\":\"resume\"}",
        &root,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

#[cfg(unix)]
#[test]
fn non_utf8_source_argument_reaches_the_child_byte_for_byte() {
    use std::os::unix::ffi::OsStringExt;
    let root = scratch_root("argv-non-utf8", true);
    let raw = OsString::from_vec(vec![b's', 0xff, b'x']);
    let mut shell_cmd = Command::new("bash");
    shell_cmd
        .arg(root.join("bin/fm-sessionstart-run.sh"))
        .arg("--source")
        .arg(&raw);
    let expected = invoke(shell_cmd, &root, b"");
    let mut mcao_cmd = Command::new(env!("CARGO_BIN_EXE_mcao"));
    mcao_cmd
        .args(["hook", "session-start", "--source"])
        .arg(&raw);
    let actual = invoke(mcao_cmd, &root, b"");
    assert_same("non-UTF-8 --source", &expected, &actual);
    let mut want = b"FULL [--source] [".to_vec();
    want.extend_from_slice(&[b's', 0xff, b'x']);
    want.extend_from_slice(b"]\n");
    assert_eq!(actual.stdout, want);
}

#[test]
fn help_flag_is_ignored_as_in_the_shell() {
    let root = scratch_root("help-long", true);
    let out = assert_parity(
        "--help with resume payload",
        &["--help"],
        r#"{"source":"resume"}"#,
        &root,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

#[test]
fn short_help_flag_is_ignored_as_in_the_shell() {
    let root = scratch_root("help-short", true);
    let out = assert_parity(
        "-h with resume payload",
        &["-h"],
        r#"{"source":"resume"}"#,
        &root,
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

#[test]
fn help_flag_with_no_payload_runs_the_full_digest_with_an_empty_source() {
    let root = scratch_root("help-empty", true);
    let out = assert_parity("-h with empty stdin", &["-h"], "", &root);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "FULL [--source] []\n");
}

#[test]
fn reload_routes_to_the_nudge() {
    let root = scratch_root("reload", true);
    let out = assert_parity("reload", &["--source", "reload"], "", &root);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

#[test]
fn fork_routes_to_the_nudge() {
    let root = scratch_root("fork", true);
    let out = assert_parity("fork", &["--source", "fork"], "", &root);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

/// True when `jq` runs from `PATH`, as the shell's foreign-host predicate needs.
fn host_has_jq() -> bool {
    Command::new("jq")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Builds a directory of symlinks to every executable on the host `PATH` except `jq`, so the
/// shell's `command -v jq` fails as it does on a host without `jq`.
#[cfg(unix)]
fn path_without_jq(root: &Path) -> PathBuf {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = root.join("no-jq-bin");
    fs::create_dir_all(&dir).expect("create no-jq bin");
    let host = std::env::var_os("PATH").unwrap_or_default();
    for host_dir in std::env::split_paths(&host) {
        let Ok(entries) = fs::read_dir(&host_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == "jq" {
                continue;
            }
            let link = dir.join(&name);
            if link.symlink_metadata().is_ok() {
                continue;
            }
            let target = entry.path();
            let executable = fs::metadata(&target)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0);
            if executable {
                symlink(&target, &link).expect("link host tool");
            }
        }
    }
    assert!(!dir.join("jq").exists(), "the no-jq PATH must not hold jq");
    dir
}

/// Runs the shell and `mcao` with `PATH` set to `path`, and asserts parity.
#[cfg(unix)]
fn assert_parity_on_path(
    name: &str,
    source_args: &[&str],
    stdin: &str,
    root: &Path,
    path: &Path,
) -> Output {
    let mut shell_cmd = Command::new("bash");
    shell_cmd
        .arg(root.join("bin/fm-sessionstart-run.sh"))
        .args(source_args)
        .env("PATH", path);
    let expected = invoke(shell_cmd, root, stdin.as_bytes());
    let mut mcao_cmd = Command::new(env!("CARGO_BIN_EXE_mcao"));
    mcao_cmd
        .args(["hook", "session-start"])
        .args(source_args)
        .env("PATH", path);
    let actual = invoke(mcao_cmd, root, stdin.as_bytes());
    assert_same(name, &expected, &actual);
    actual
}

#[cfg(unix)]
#[test]
fn foreign_host_payload_without_jq_is_not_foreign_and_the_run_continues() {
    let root = scratch_root("foreign-no-jq", true);
    let path = path_without_jq(&root);
    let out = assert_parity_on_path(
        "foreign host, no jq on PATH",
        &[],
        r#"{"cursor_version":"1.0","source":"clear"}"#,
        &root,
        &path,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

/// Runs the shell and `mcao` under a fake harness whose exported Claude identity is `session_id`.
/// The fake harness is the trusted Claude process, so the session id is accepted for ownership.
#[cfg(unix)]
fn under_claude_session(
    root: &Path,
    command: &[OsString],
    stdin: &[u8],
    session_id: &str,
) -> Output {
    use std::os::unix::process::CommandExt;
    let mut program = Command::new("bash");
    program
        .arg0(root.join("claude").join("fake-harness"))
        .arg("-c")
        .arg("export CLAUDE_PID=$$ CLAUDE_CODE_SESSION_ID=\"$1\"; shift; \"$@\"; exit $?")
        .arg("fake-harness")
        .arg(session_id)
        .args(command);
    invoke(program, root, stdin)
}

/// Runs the shell and `mcao` under the same Claude session and asserts parity.
#[cfg(unix)]
fn assert_session_parity(
    name: &str,
    source_args: &[&str],
    stdin: &str,
    root: &Path,
    session_id: &str,
) -> Output {
    let (shell_cmd, mcao_cmd) = command_pair(root, source_args);
    let expected = under_claude_session(root, &shell_cmd, stdin.as_bytes(), session_id);
    let actual = under_claude_session(root, &mcao_cmd, stdin.as_bytes(), session_id);
    assert_same(name, &expected, &actual);
    actual
}

/// A live process whose command name and argv0 look like a Claude Code install, and which is not
/// an ancestor of the test run. The session lock may name it. It is killed on drop.
#[cfg(unix)]
struct LiveOwner(std::process::Child);

#[cfg(unix)]
impl LiveOwner {
    fn start(root: &Path) -> Self {
        use std::os::unix::process::CommandExt;
        let child = Command::new("bash")
            .arg0(root.join("claude").join("fake-owner"))
            .arg("-c")
            .arg("read -r _")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn live owner");
        Self(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

#[cfg(unix)]
impl Drop for LiveOwner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A pid that belonged to a process which has already exited.
#[cfg(unix)]
fn dead_pid() -> u32 {
    let mut child = Command::new("bash")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn short-lived process");
    let pid = child.id();
    child.wait().expect("wait short-lived process");
    pid
}

/// Writes a lock and a completion record naming `pid`, and the session sidecar when given.
#[cfg(unix)]
fn record_owner(root: &Path, pid: u32, session_sidecar: Option<&str>) {
    fs::write(root.join("state/.lock"), format!("{pid}\n")).expect("write lock");
    fs::write(
        root.join("state/.session-start-complete"),
        format!("{pid}\n"),
    )
    .expect("write completion");
    if let Some(id) = session_sidecar {
        fs::write(root.join("state/.lock-session"), format!("{id}\n")).expect("write sidecar");
    }
}

#[cfg(unix)]
#[test]
fn clear_with_a_live_owner_and_a_matching_session_sidecar_reemits() {
    let root = scratch_root("session-same", true);
    let owner = LiveOwner::start(&root);
    record_owner(&root, owner.pid(), Some("session-a"));
    let out = assert_session_parity(
        "clear, live owner, same session",
        &["--source", "clear"],
        "",
        &root,
        "session-a",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--reemit] [--source] [clear]\n"
    );
}

#[cfg(unix)]
#[test]
fn clear_with_a_live_owner_and_a_different_session_id_runs_full() {
    let root = scratch_root("session-other-id", true);
    let owner = LiveOwner::start(&root);
    record_owner(&root, owner.pid(), Some("session-b"));
    let out = assert_session_parity(
        "clear, live owner, other session",
        &["--source", "clear"],
        "",
        &root,
        "session-a",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

#[cfg(unix)]
#[test]
fn clear_with_a_live_owner_and_no_session_sidecar_runs_full() {
    let root = scratch_root("session-no-sidecar", true);
    let owner = LiveOwner::start(&root);
    record_owner(&root, owner.pid(), None);
    let out = assert_session_parity(
        "clear, live owner, no sidecar",
        &["--source", "clear"],
        "",
        &root,
        "session-a",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

#[cfg(unix)]
#[test]
fn clear_with_a_dead_owner_and_a_matching_session_sidecar_runs_full() {
    let root = scratch_root("session-dead-owner", true);
    record_owner(&root, dead_pid(), Some("session-a"));
    let out = assert_session_parity(
        "clear, dead owner, same session",
        &["--source", "clear"],
        "",
        &root,
        "session-a",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

#[cfg(unix)]
#[test]
fn clear_with_a_symlinked_lock_runs_full() {
    let root = scratch_root("clear-symlinked-lock", true);
    fs::write(root.join("state/lock-target"), "").expect("write lock target");
    std::os::unix::fs::symlink(root.join("state/lock-target"), root.join("state/.lock"))
        .expect("symlink lock");
    let out = assert_harness_parity(
        "clear, symlinked lock",
        &["--source", "clear"],
        "",
        r"%s\n",
        r"%s\n",
        &root,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}

/// Copies the built `mcao` into a checkout's conventional Cargo target path.
#[cfg(unix)]
fn install_binary_in_checkout(root: &Path) -> PathBuf {
    let dir = root.join("target").join("debug");
    fs::create_dir_all(&dir).expect("create cargo output dir");
    let exe = dir.join("mcao");
    fs::copy(env!("CARGO_BIN_EXE_mcao"), &exe).expect("copy mcao into the checkout");
    exe
}

/// Runs `program` from `cwd` with no root overrides, `--source resume`, and no stdin.
#[cfg(unix)]
fn run_resume_from(program: &Path, cwd: &Path, override_root: Option<&Path>) -> Output {
    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .args(["hook", "session-start", "--source", "resume"])
        .env_remove("FM_ROOT_OVERRIDE")
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .stdin(Stdio::null());
    if let Some(root) = override_root {
        command.env("FM_ROOT_OVERRIDE", root);
    }
    command.output().expect("run mcao")
}

/// A primary-shaped repository whose nudge prints a marker, to detect any script it supplies.
#[cfg(unix)]
fn unrelated_repository(name: &str) -> PathBuf {
    let unrelated = scratch_root(name, true);
    fs::write(
        unrelated.join("bin/fm-sessionstart-nudge.sh"),
        "#!/usr/bin/env bash\nprintf 'UNRELATED NUDGE\\n'\n",
    )
    .expect("write unrelated nudge");
    unrelated
}

/// A Cargo target path does not identify a script root; the explicit override does.
#[cfg(unix)]
#[test]
fn binary_in_checkout_requires_explicit_override() {
    let checkout = scratch_root("binary-checkout", true);
    let exe = install_binary_in_checkout(&checkout);
    let unrelated = unrelated_repository("unrelated-cwd");
    let out = run_resume_from(&exe, &unrelated, None);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no firstmate root: set FM_ROOT_OVERRIDE"),
        "stderr: {:?}",
        out.stderr
    );

    let out = run_resume_from(&exe, &unrelated, Some(&checkout));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);
}

/// A copied binary in another checkout's Cargo target directory must not run its scripts.
#[cfg(unix)]
#[test]
fn copied_binary_in_unrelated_cargo_target_runs_no_foreign_script() {
    let foreign = scratch_root("foreign-checkout", true);
    fs::write(
        foreign.join("bin/fm-sessionstart-nudge.sh"),
        "#!/usr/bin/env bash\nprintf 'FOREIGN-CHECKOUT-SCRIPT\\n'\n",
    )
    .expect("write foreign nudge");
    let exe = install_binary_in_checkout(&foreign);
    let out = run_resume_from(&exe, &foreign, None);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no firstmate root: set FM_ROOT_OVERRIDE"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("FOREIGN-CHECKOUT-SCRIPT"));
}

/// A copy without an override runs no script, even when the working directory is a checkout with
/// its own nudge and the original build checkout still exists.
#[cfg(unix)]
#[test]
fn copied_binary_outside_its_checkout_runs_no_script() {
    let unrelated = unrelated_repository("unrelated-for-copy");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("relocated-binary");
    fs::create_dir_all(&dir).expect("create relocated dir");
    let copy = dir.join("mcao");
    fs::copy(env!("CARGO_BIN_EXE_mcao"), &copy).expect("copy mcao out of the checkout");
    let out = run_resume_from(&copy, &unrelated, None);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no firstmate root: set FM_ROOT_OVERRIDE"),
        "stderr: {stderr}"
    );
}

/// `FM_ROOT_OVERRIDE` still selects the root for a copied binary, as it does for every caller.
#[cfg(unix)]
#[test]
fn copied_binary_with_an_override_uses_the_override_root() {
    let override_root = scratch_root("override-root", true);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("relocated-override");
    fs::create_dir_all(&dir).expect("create relocated dir");
    let copy = dir.join("mcao");
    fs::copy(env!("CARGO_BIN_EXE_mcao"), &copy).expect("copy mcao out of the checkout");
    let out = run_resume_from(
        &copy,
        &unrelated_repository("cwd-for-override"),
        Some(&override_root),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

/// A symlink does not establish a script root; the explicit override remains usable.
#[cfg(unix)]
#[test]
fn symlink_to_a_checkout_binary_requires_explicit_override() {
    let checkout = scratch_root("symlink-checkout", true);
    let exe = install_binary_in_checkout(&checkout);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("symlink-entry");
    fs::create_dir_all(&dir).expect("create symlink dir");
    let link = dir.join("mcao");
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&exe, &link).expect("symlink mcao");
    let cwd = unrelated_repository("cwd-for-symlink");
    let out = run_resume_from(&link, &cwd, None);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no firstmate root: set FM_ROOT_OVERRIDE"),
        "stderr: {:?}",
        out.stderr
    );

    let out = run_resume_from(&link, &cwd, Some(&checkout));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "NUDGE\n");
}

/// Runs `mcao` under a fake harness that records `{prefix}{its own pid}` as the lock and the
/// completion, so the owner comparison meets a harness ancestor with a zero-padded pid.
#[cfg(unix)]
fn under_claude_session_recording_padded_owner(
    root: &Path,
    command: &[OsString],
    stdin: &[u8],
    prefix: &str,
) -> Output {
    use std::os::unix::process::CommandExt;
    let mut program = Command::new("bash");
    program
        .arg0(root.join("claude").join("fake-harness"))
        .arg("-c")
        .arg(
            "export CLAUDE_PID=$$ CLAUDE_CODE_SESSION_ID=session-a; \
             printf '%s%s\\n' \"$1\" \"$$\" > \"$2\"; cp \"$2\" \"$3\"; shift 3; \"$@\"; exit $?",
        )
        .arg("fake-harness")
        .arg(prefix)
        .arg(root.join("state/.lock"))
        .arg(root.join("state/.session-start-complete"))
        .args(command);
    invoke(program, root, stdin)
}

/// `0123` names the same process as `123` only to a numeric parser. The shell compares the texts,
/// so a padded owner is not the harness ancestor and the clear runs the full digest.
#[cfg(unix)]
#[test]
fn clear_with_a_zero_padded_owner_pid_is_not_the_harness_ancestor() {
    let root = scratch_root("session-padded-pid", true);
    let (shell_cmd, mcao_cmd) = command_pair(&root, &["--source", "clear"]);
    let expected = under_claude_session_recording_padded_owner(&root, &shell_cmd, b"", "0");
    let actual = under_claude_session_recording_padded_owner(&root, &mcao_cmd, b"", "0");
    assert_same("clear, zero-padded owner pid", &expected, &actual);
    assert_eq!(
        String::from_utf8_lossy(&actual.stdout),
        "FULL [--source] [clear]\n"
    );
}

/// The shell's same-session branch accepts a padded owner: `kill` and `ps` read `0<pid>` as `<pid>`.
/// With a matching session sidecar and a live harness ancestor, both sides re-emit.
#[cfg(unix)]
#[test]
fn clear_with_a_zero_padded_owner_in_the_same_session_reemits_as_in_the_shell() {
    let root = scratch_root("session-padded-same", true);
    fs::write(root.join("state/.lock-session"), "session-a\n").expect("write sidecar");
    let (shell_cmd, mcao_cmd) = command_pair(&root, &["--source", "clear"]);
    let expected = under_claude_session_recording_padded_owner(&root, &shell_cmd, b"", "0");
    let actual = under_claude_session_recording_padded_owner(&root, &mcao_cmd, b"", "0");
    assert_same("clear, padded owner, same session", &expected, &actual);
    assert_eq!(
        String::from_utf8_lossy(&actual.stdout),
        "FULL [--reemit] [--source] [clear]\n"
    );
}

#[cfg(unix)]
#[test]
fn clear_with_a_symlinked_completion_runs_full() {
    let root = scratch_root("clear-symlinked-completion", true);
    fs::write(root.join("state/completion-target"), "").expect("write completion target");
    std::os::unix::fs::symlink(
        root.join("state/completion-target"),
        root.join("state/.session-start-complete"),
    )
    .expect("symlink completion");
    let out = assert_harness_parity(
        "clear, symlinked completion",
        &["--source", "clear"],
        "",
        r"%s\n",
        r"%s\n",
        &root,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "FULL [--source] [clear]\n"
    );
}
