//! Command-line contract for `mcao herdr lab-viewer` (DPES-864, AC5 and AC7). The session and
//! pidfile checks run on every platform. The pty tests put a fake `herdr` first on PATH, so no
//! real Herdr session is started or touched, and their scratch files live under the target tmp dir.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Child, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

fn mcao() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
}

fn lab_viewer(args: &[&str]) -> Output {
    mcao()
        .args(["herdr", "lab-viewer"])
        .args(args)
        .output()
        .expect("run mcao herdr lab-viewer")
}

/// A fresh scratch directory under the target tmp dir, so no test writes outside the build tree.
#[cfg(unix)]
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("lab-viewer-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// The only arguments the viewer may pass to herdr. Every fake herdr checks them first and exits
/// 64 on a mismatch, so a wrong session or an extra argument fails the test.
#[cfg(unix)]
const EXPECTED_HERDR_ARGS: &str = "--session fm-lab-test";

/// Writes a fake `herdr` script with `body` into `dir/bin` and returns that directory.
#[cfg(unix)]
fn fake_herdr(dir: &Path, body: &str, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = dir.join("bin");
    fs::create_dir_all(&bin).expect("create the fake bin directory");
    let herdr = bin.join("herdr");
    let argv_check = format!(
        "[ \"$*\" = \"{EXPECTED_HERDR_ARGS}\" ] || {{ printf 'unexpected herdr argv: %s\\n' \"$*\" >&2; exit 64; }}"
    );
    fs::write(&herdr, format!("#!/bin/sh\n{argv_check}\n{body}\n")).expect("write the fake herdr");
    fs::set_permissions(&herdr, fs::Permissions::from_mode(mode)).expect("set the fake herdr mode");
    bin
}

#[cfg(unix)]
fn path_with(bin: &Path) -> String {
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Starts `mcao herdr lab-viewer` against a fake herdr in `bin` without waiting for it.
#[cfg(unix)]
fn spawn_viewer(dir: &Path, bin: &Path, session: &str, pidfile: &Path) -> Child {
    mcao()
        .args(["herdr", "lab-viewer", session])
        .arg(pidfile)
        .env("PATH", path_with(bin))
        .env("HERDR_PANE_ID", "pane-1")
        .env("LAB_VIEWER_TEST_DIR", dir)
        .env_remove("TERM")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcao herdr lab-viewer")
}

/// Runs `mcao herdr lab-viewer` against a fake herdr in `bin` and returns the output and the
/// launcher's pid, which is the pid of the mcao process itself.
#[cfg(unix)]
fn run_with_fake_herdr(dir: &Path, bin: &Path, session: &str, pidfile: &Path) -> (Output, u32) {
    let child = spawn_viewer(dir, bin, session, pidfile);
    let launcher = child.id();
    let output = child.wait_with_output().expect("wait for mcao");
    (output, launcher)
}

/// Polls until `path` exists, failing the test after ten seconds.
#[cfg(unix)]
fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn read_pidfile(path: &Path) -> HashMap<String, String> {
    fs::read_to_string(path)
        .expect("read the pidfile")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[test]
fn refuses_sessions_outside_the_lab_namespace() {
    let pidfile = Path::new(env!("CARGO_TARGET_TMPDIR")).join("refused-session.pid");
    let _ = fs::remove_file(&pidfile);
    for session in [
        "default",
        "arbitrary-session",
        "fm-lab-",
        "fm-lab--x",
        "fm-lab-a b",
        "fm-lab-\u{1b}[2J",
    ] {
        let output = lab_viewer(&[session, pidfile.to_str().expect("utf-8 path")]);
        assert_eq!(output.status.code(), Some(2), "session {session:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("refusing session"), "stderr: {stderr}");
        assert!(
            !stderr.contains('\u{1b}'),
            "a control byte reached stderr: {stderr:?}"
        );
    }
    assert!(!pidfile.exists(), "a refused session still wrote a pidfile");
}

#[test]
fn refused_session_uses_python_repr_quoting_and_printability() {
    let pidfile = Path::new(env!("CARGO_TARGET_TMPDIR")).join("python-repr-session.pid");
    let cases = [
        ("fm-lab-a b", "'fm-lab-a b'"),
        ("fm-lab-a'b", "\"fm-lab-a'b\""),
        ("fm-lab-a\"b", "'fm-lab-a\"b'"),
        ("fm-lab-a'b\"", "'fm-lab-a\\'b\"'"),
        ("fm-lab-a\\b", "'fm-lab-a\\\\b'"),
        ("fm-lab-\t", "'fm-lab-\\t'"),
        ("fm-lab-\u{1}", "'fm-lab-\\x01'"),
        ("fm-lab-\u{85}", "'fm-lab-\\x85'"),
        ("fm-lab-\u{a0}", "'fm-lab-\\xa0'"),
        ("fm-lab-\u{200b}", "'fm-lab-\\u200b'"),
        ("fm-lab-\u{e000}", "'fm-lab-\\ue000'"),
        ("fm-lab-\u{378}", "'fm-lab-\\u0378'"),
        ("fm-lab-😀\u{301}", "'fm-lab-😀\u{301}'"),
        ("fm-lab-\u{10ffff}", "'fm-lab-\\U0010ffff'"),
    ];

    for (session, repr) in cases {
        let output = lab_viewer(&[session, pidfile.to_str().expect("utf-8 path")]);
        assert_eq!(output.status.code(), Some(2), "session {session:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("mcao herdr lab-viewer: refusing session {repr}\n"),
            "session {session:?}"
        );
    }
}

#[test]
fn requires_an_absolute_pidfile() {
    let output = lab_viewer(&["fm-lab-test", "relative-viewer.pid"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("pidfile must be an absolute path"),
        "stderr: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn viewer_runs_herdr_with_the_session_and_returns_its_status() {
    let dir = scratch("status");
    let bin = fake_herdr(
        &dir,
        r#"printf '%s\n' "$@" > "$LAB_VIEWER_TEST_DIR/args"
printf '%s' "${HERDR_PANE_ID-unset}" > "$LAB_VIEWER_TEST_DIR/pane"
printf '%s' "$TERM" > "$LAB_VIEWER_TEST_DIR/term"
exit 5"#,
        0o755,
    );
    let pidfile = dir.join("viewer.pid");

    let (output, launcher) = run_with_fake_herdr(&dir, &bin, "fm-lab-test", &pidfile);

    assert_eq!(
        output.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("args")).expect("args"),
        "--session\nfm-lab-test\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("pane")).expect("pane"),
        "unset",
        "HERDR_PANE_ID was not scrubbed"
    );
    assert_eq!(
        fs::read_to_string(dir.join("term")).expect("term"),
        "xterm-256color"
    );

    let record = read_pidfile(&pidfile);
    assert_eq!(record["launcher_pid"], launcher.to_string());
    assert_ne!(record["viewer_pid"], launcher.to_string());
    assert!(!record["launcher_start"].is_empty() && !record["viewer_start"].is_empty());
    assert!(
        !dir.join(format!("viewer.pid.{launcher}.tmp")).exists(),
        "the temporary pidfile was left behind"
    );
}

#[cfg(unix)]
#[test]
fn viewer_killed_by_a_signal_exits_zero() {
    let dir = scratch("signalled");
    let bin = fake_herdr(&dir, r#"kill -TERM $$"#, 0o755);

    let (output, _) = run_with_fake_herdr(&dir, &bin, "fm-lab-test", &dir.join("viewer.pid"));

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn viewer_that_cannot_exec_exits_127_after_recording_its_pid() {
    let dir = scratch("unexecutable");
    let bin = fake_herdr(&dir, "", 0o644);
    let pidfile = dir.join("viewer.pid");

    let (output, launcher) = run_with_fake_herdr(&dir, &bin, "fm-lab-test", &pidfile);

    assert_eq!(output.status.code(), Some(127));
    assert_eq!(read_pidfile(&pidfile)["launcher_pid"], launcher.to_string());
}

#[cfg(unix)]
#[test]
fn unwritable_pidfile_kills_the_viewer_and_exits_3() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let dir = scratch("pidfile-failure");
    let bin = fake_herdr(&dir, "exec sleep 30", 0o755);
    let pidfile = dir.join("missing-dir").join("viewer.pid");

    // The viewer inherits fd 3, the write end of a pipe the test holds. The write end closes only
    // when that exact viewer process exits, so a non-blocking read then returns EOF. The launcher
    // reaps its viewer before it exits, so the check does not depend on timing.
    // std::io::pipe creates close-on-exec descriptors, so parallel tests cannot inherit this one.
    let (mut reader, writer) = std::io::pipe().expect("create the viewer pipe");
    let writer_fd = writer.as_raw_fd();
    let mut command = mcao();
    command
        .args(["herdr", "lab-viewer", "fm-lab-test"])
        .arg(&pidfile)
        .env("PATH", path_with(&bin))
        .env("HERDR_PANE_ID", "pane-1")
        .env_remove("TERM")
        .stdin(Stdio::null());
    // SAFETY: the closure only calls dup2, which is async-signal-safe, before exec.
    unsafe {
        command.pre_exec(move || {
            libc::dup2(writer_fd, 3);
            Ok(())
        });
    }
    let output = command.output().expect("run mcao herdr lab-viewer");
    drop(writer);

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not record process identity"),
        "stderr: {stderr}"
    );
    assert!(!pidfile.exists(), "a failed pidfile write left a pidfile");

    // SAFETY: fcntl only changes the status flags of the read end this test owns. With the
    // writer gone, a non-blocking read returns 0 only once the viewer has closed its copy.
    unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    let mut byte = [0u8; 1];
    let read = reader.read(&mut byte).expect("read the viewer pipe");
    assert_eq!(read, 0, "the viewer survived the failed launch");
}

#[cfg(unix)]
#[test]
fn sigterm_to_the_launcher_is_relayed_to_the_viewer() {
    let dir = scratch("sigterm");
    let bin = fake_herdr(
        &dir,
        r#"trap 'printf relayed > "$LAB_VIEWER_TEST_DIR/term"; exit 9' TERM
: > "$LAB_VIEWER_TEST_DIR/ready"
while :; do sleep 0.05; done"#,
        0o755,
    );
    let pidfile = dir.join("viewer.pid");
    let child = spawn_viewer(&dir, &bin, "fm-lab-test", &pidfile);
    let launcher = child.id();
    // The viewer's trap is set before `ready` appears. The pidfile is renamed into place only
    // after the launcher has armed its relay, so its appearance is the launcher's readiness.
    wait_for_file(&dir.join("ready"));
    wait_for_file(&pidfile);

    // SAFETY: kill only sends a signal to the launcher process this test spawned.
    let rc = unsafe { libc::kill(launcher as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "could not signal the launcher");
    let output = child.wait_with_output().expect("wait for mcao");

    assert_eq!(
        output.status.code(),
        Some(9),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("term")).expect("term marker"),
        "relayed",
        "the viewer did not receive the relayed SIGTERM"
    );
}

#[cfg(unix)]
#[test]
fn viewer_terminal_is_40_rows_by_120_columns() {
    let dir = scratch("pty-size");
    let bin = fake_herdr(&dir, r#"stty size > "$LAB_VIEWER_TEST_DIR/size""#, 0o755);

    let (output, _) = run_with_fake_herdr(&dir, &bin, "fm-lab-test", &dir.join("viewer.pid"));

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("size")).expect("size").trim(),
        "40 120"
    );
}

#[cfg(windows)]
#[test]
fn lab_viewer_is_unsupported_on_windows() {
    let output = lab_viewer(&["fm-lab-test", r"C:\lab\viewer.pid"]);
    assert_eq!(output.status.code(), Some(7));
}
