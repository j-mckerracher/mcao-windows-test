//! DPES-962 AC1: the tmux attach target from `unix_attach_argv` must not prefix-match a sibling.
//!
//! Runs only against a private tmux socket with fake sessions. It never touches the user's tmux
//! server, a real worker session, or a watcher. Skips with a message where tmux is not installed.

#![cfg(unix)]

use std::process::{Command, Output};
use std::thread::sleep;
use std::time::{Duration, Instant};

use mcao_core::state::BackendToken;
use mcao_core::steer::{AttachMode, unix_attach_argv};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// A tmux server on a socket private to this test; killed on drop.
struct PrivateTmux {
    socket: String,
}

impl PrivateTmux {
    fn new(tag: &str) -> Self {
        Self {
            socket: format!("mcao-dpes962-{tag}-{}", std::process::id()),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("tmux runs")
    }

    fn start_fake_session(&self, name: &str) {
        let out = self.run(&["new-session", "-d", "-s", name, "sleep 300"]);
        assert!(out.status.success(), "fake session {name} starts");
    }

    /// Reports whether `-t <target>` names an existing session, the rule attach-session uses.
    fn session_resolves(&self, target: &str) -> bool {
        self.run(&["has-session", "-t", target]).status.success()
    }

    /// Names the session that `-t <target>` resolves to, read through its current window.
    fn resolved_session_name(&self, target: &str) -> String {
        let window = format!("{target}:");
        let out = self.run(&["display-message", "-p", "-t", &window, "#{session_name}"]);
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

/// Returns the value that follows `-t` in `argv`.
fn attach_target(argv: &[String]) -> &str {
    let index = argv
        .iter()
        .position(|arg| arg == "-t")
        .expect("tmux argv has -t");
    &argv[index + 1]
}

#[test]
fn exact_target_does_not_prefix_match_a_sibling_session() {
    if !tmux_available() {
        eprintln!("skipped: tmux is not installed; DPES-962 AC1 sibling probe not run");
        return;
    }
    let tmux = PrivateTmux::new("sibling");
    // Only a sibling exists; the requested session is absent.
    tmux.start_fake_session("fm-dpes-880");

    // Control: the bare name the old argv emitted prefix-matches the sibling.
    assert!(tmux.session_resolves("fm-dpes-88"));
    assert_eq!(tmux.resolved_session_name("fm-dpes-88"), "fm-dpes-880");

    for mode in [AttachMode::Writable, AttachMode::ReadOnly] {
        let argv = unix_attach_argv(BackendToken::Tmux, "dpes-88", mode).expect("tmux argv");
        assert_eq!(attach_target(&argv), "=fm-dpes-88");
        assert!(
            !tmux.session_resolves(attach_target(&argv)),
            "exact target must not resolve to the sibling for {mode:?}"
        );
    }
}

#[test]
fn exact_target_resolves_the_requested_session_when_present() {
    if !tmux_available() {
        eprintln!("skipped: tmux is not installed; DPES-962 AC1 exact probe not run");
        return;
    }
    let tmux = PrivateTmux::new("exact");
    tmux.start_fake_session("fm-dpes-880");
    tmux.start_fake_session("fm-dpes-88");

    for mode in [AttachMode::Writable, AttachMode::ReadOnly] {
        let argv = unix_attach_argv(BackendToken::Tmux, "dpes-88", mode).expect("tmux argv");
        assert_eq!(
            tmux.resolved_session_name(attach_target(&argv)),
            "fm-dpes-88"
        );
    }
}

/// DPES-979 AC4: a real `tmux attach-session` argv run in a private terminal.
///
/// The attach client runs as the command of a pane on an outer private server, so it has its own
/// pseudo-terminal. The inner server (the fake session) uses a private `TMUX_TMPDIR`, and every
/// tmux call drops an inherited `TMUX`, so no attach is refused as nested and no user server is
/// touched. Both servers ignore user config (`-f /dev/null`) and are killed on drop.
struct AttachEnv {
    dir: std::path::PathBuf,
    outer: String,
}

impl AttachEnv {
    fn new(tag: &str) -> Self {
        // Every instance gets a fresh, cleared directory, so no socket or file leaks between tests.
        // Short names keep the Unix socket path under the platform length limit. The base comes
        // from `temp_dir()`, which on macOS is a long `$TMPDIR`; with the short name the inner
        // socket path stays under sun_path's 104 bytes.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let pid = std::process::id();
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mcao-979-{pid}-{n}-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("private tmux dir");
        Self {
            dir,
            outer: format!("o{pid}{n}{tag}"),
        }
    }

    /// Runs tmux on the inner server, the default socket under the private `TMUX_TMPDIR`.
    fn inner(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .env_remove("TMUX")
            .env("TMUX_TMPDIR", &self.dir)
            .args(["-f", "/dev/null"])
            .args(args)
            .output()
            .expect("tmux runs")
    }

    /// Runs tmux on the outer private socket, which hosts the attach client's terminal.
    fn outer(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .env_remove("TMUX")
            .env("TMUX_TMPDIR", &self.dir)
            .args(["-f", "/dev/null", "-L", &self.outer])
            .args(args)
            .output()
            .expect("tmux runs")
    }

    /// The pane command: the argv from `unix_attach_argv`, run with the inner server's socket.
    ///
    /// Every word is single-quoted: the pane runs under the user's shell, and zsh expands a bare
    /// `=name` word as a command path (`EQUALS`), which would stop the exact `=fm-<id>` target.
    fn attach_command(&self, mode: AttachMode) -> String {
        let argv = unix_attach_argv(BackendToken::Tmux, "dpes-88", mode).expect("tmux argv");
        // `env` takes the assignment as an argument, so a quoted `NAME=value` still applies.
        let words = ["env", "-u", "TMUX"]
            .map(str::to_owned)
            .into_iter()
            .chain(std::iter::once(format!(
                "TMUX_TMPDIR={}",
                self.dir.display()
            )))
            .chain(argv);
        words
            .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Waits for one attach client on the inner server and returns its `client_readonly` flag and
    /// the session it attached to.
    fn wait_for_client(&self) -> (String, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let out = self.inner(&["list-clients", "-F", "#{client_readonly} #{client_session}"]);
            let line = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if let Some((flag, session)) = line.split_once(' ') {
                return (flag.to_owned(), session.to_owned());
            }
            assert!(
                Instant::now() < deadline,
                "attach client never appeared; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            sleep(Duration::from_millis(100));
        }
    }
}

impl AttachEnv {
    /// Waits until the inner server has no attach client left, which follows the client's detach.
    fn wait_for_detach(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // With no server left, `list-clients` prints nothing, which also means no client.
            let out = self.inner(&["list-clients"]);
            if out.stdout.is_empty() {
                return;
            }
            assert!(Instant::now() < deadline, "attach client never detached");
            sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for AttachEnv {
    fn drop(&mut self) {
        let _ = self.outer(&["kill-server"]);
        let _ = self.inner(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Reports whether `path` holds `needle` within `timeout`.
fn file_holds_within(path: &std::path::Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if std::fs::read_to_string(path).is_ok_and(|text| text.contains(needle)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(100));
    }
}

#[test]
fn attach_session_readonly_flag_and_input_forwarding_match_mode() {
    if !tmux_available() {
        eprintln!("skipped: tmux is not installed; DPES-979 AC4 attach probe not run");
        return;
    }
    for mode in [AttachMode::Writable, AttachMode::ReadOnly] {
        let tag = if mode == AttachMode::ReadOnly {
            "r"
        } else {
            "w"
        };
        let env = AttachEnv::new(tag);
        let out_file = env.dir.join("input.out");
        // The fake session copies whatever input reaches its pane into `out_file`.
        let fake = env.inner(&[
            "new-session",
            "-d",
            "-s",
            "fm-dpes-88",
            &format!("cat > {}", out_file.display()),
        ]);
        assert!(fake.status.success(), "fake session starts for {mode:?}");

        let host = env.outer(&[
            "new-session",
            "-d",
            "-s",
            "host",
            "-P",
            "-F",
            "#{pane_id}",
            &env.attach_command(mode),
        ]);
        assert!(host.status.success(), "outer host pane starts for {mode:?}");
        let pane = String::from_utf8_lossy(&host.stdout).trim().to_owned();

        let (readonly, session) = env.wait_for_client();
        assert_eq!(
            session, "fm-dpes-88",
            "attach reached the exact session for {mode:?}"
        );
        let expected_flag = if mode == AttachMode::ReadOnly {
            "1"
        } else {
            "0"
        };
        assert_eq!(readonly, expected_flag, "client_readonly for {mode:?}");

        // Keys typed into the attach client's terminal, then its detach prefix (C-b d). The client
        // sends its keys in order, so the server has handled every key before it sees the detach.
        env.outer(&["send-keys", "-t", &pane, "-l", "mcao-keys"]);
        env.outer(&["send-keys", "-t", &pane, "Enter"]);
        env.outer(&["send-keys", "-t", &pane, "C-b"]);
        env.outer(&["send-keys", "-t", &pane, "d"]);
        env.wait_for_detach();
        match mode {
            // The fake session's `cat` writes asynchronously, so both legs allow a bounded wait for
            // a write. A writable client must produce one. A read-only client must not: the wait is
            // about one second, so a late forwarded key would still land in that window. The
            // `client_readonly` assertion above is the primary guard for read-only.
            AttachMode::Writable => assert!(
                file_holds_within(&out_file, "mcao-keys", Duration::from_secs(5)),
                "writable attach forwards input"
            ),
            AttachMode::ReadOnly => assert!(
                !file_holds_within(&out_file, "mcao-keys", Duration::from_secs(1)),
                "read-only attach must not forward input"
            ),
        }
    }
}
