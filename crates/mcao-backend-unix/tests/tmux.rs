//! Checks [`TmuxBackend`] against a real tmux server. Each test starts its own private server under
//! a fresh `TMUX_TMPDIR`, so no test touches the default server or another test's sessions.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mcao_backend_unix::TmuxBackend;
use mcao_backend_unix::tmux::SubmitError;
use mcao_core::McaoError;

/// Distinguishes the directories of tests that run in the same process.
static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

/// A tmux server with one detached session, killed and removed when dropped.
struct PrivateServer {
    directory: PathBuf,
}

impl PrivateServer {
    /// Starts a session named `session` whose pane runs `shell_command`.
    fn start(session: &str, shell_command: &str) -> Self {
        Self::start_with(session, |_| shell_command.to_string())
    }

    /// Starts a session named `session` whose pane runs the command that `shell_command` builds from
    /// the server's private directory, for a pane that writes its own files there.
    fn start_with(session: &str, shell_command: impl FnOnce(&Path) -> String) -> Self {
        let index = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("mcao-tmux-{}-{index}", std::process::id()));
        fs::create_dir_all(&directory).expect("create the private tmux directory");
        let server = Self { directory };
        let command = shell_command(&server.directory);
        let output = server.tmux(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "80",
            "-y",
            "24",
            &command,
        ]);
        assert!(
            output.status.success(),
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        server
    }

    fn backend(&self) -> TmuxBackend {
        TmuxBackend::with_tmux_tmpdir(&self.directory)
    }

    /// Runs tmux against this server, with the same environment the backend uses.
    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .args(args)
            .env("TMUX_TMPDIR", &self.directory)
            .env_remove("TMUX")
            .output()
            .expect("run tmux; install tmux to run these tests")
    }
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// The pane script: ten numbered lines, then a pause so the session outlives the assertions.
const TEN_LINES: &str = "for i in 1 2 3 4 5 6 7 8 9 10; do echo line$i; done; sleep 60";

/// Waits until `session` shows `marker`, so the capture is taken after the output is printed.
fn wait_for_marker(backend: &TmuxBackend, session: &str, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = backend.capture_pane(session, 40)
            && text.contains(marker)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "session {session} never showed {marker}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn capture_matches_the_bytes_tmux_capture_pane_prints() {
    let server = PrivateServer::start("s1", TEN_LINES);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "line10");

    let direct = server.tmux(&["capture-pane", "-p", "-S", "-5", "-t", "s1"]);
    assert!(direct.status.success(), "direct capture-pane failed");
    let captured = backend
        .capture_pane("s1", 5)
        .expect("capture the session pane");

    assert_eq!(captured.as_bytes(), direct.stdout.as_slice());
    assert!(captured.contains("line10"), "{captured:?}");
}

#[test]
fn capture_of_a_missing_session_is_a_backend_error_naming_the_session() {
    let server = PrivateServer::start("s1", TEN_LINES);

    let error = server
        .backend()
        .capture_pane("missing", 5)
        .expect_err("a missing session has no pane to capture");

    match error {
        McaoError::Backend(message) => {
            assert!(message.contains("session"), "{message}");
            assert!(message.contains("missing"), "{message}");
        }
        other => panic!("expected a backend error, got {other:?}"),
    }
}

#[test]
fn capture_never_matches_a_longer_session_name_by_prefix() {
    let server = PrivateServer::start("s12", TEN_LINES);
    let backend = server.backend();
    wait_for_marker(&backend, "s12", "line10");

    let error = backend
        .capture_pane("s1", 5)
        .expect_err("s1 does not exist, so s12 must not stand in for it");
    assert!(
        matches!(error, McaoError::Backend(ref message) if message.contains("session")),
        "{error:?}"
    );

    let captured = backend
        .capture_pane("s12", 5)
        .expect("the session s12 exists");
    assert!(captured.contains("line10"), "{captured:?}");
}

#[test]
fn capture_with_no_server_is_a_backend_error() {
    let server = PrivateServer::start("s1", TEN_LINES);
    // Stop the server but keep its directory, so the next call finds no server to talk to.
    let _ = server.tmux(&["kill-server"]);

    let error = server
        .backend()
        .capture_pane("s1", 5)
        .expect_err("no server means no pane");

    assert!(matches!(error, McaoError::Backend(_)), "{error:?}");
}

#[test]
fn session_exists_matches_the_name_exactly() {
    let server = PrivateServer::start("s1", TEN_LINES);
    let backend = server.backend();

    assert!(backend.session_exists("s1").expect("probe s1"));
    assert!(
        !backend.session_exists("s").expect("probe s"),
        "a prefix of a session name must not match"
    );
    assert!(!backend.session_exists("missing").expect("probe missing"));
}

#[test]
fn session_exists_is_false_when_no_server_runs() {
    let index = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("mcao-tmux-empty-{}-{index}", std::process::id()));
    fs::create_dir_all(&directory).expect("create an empty tmux directory");
    let backend = TmuxBackend::with_tmux_tmpdir(&directory);

    let exists = backend.session_exists("s1");
    let _ = fs::remove_dir_all(&directory);

    assert!(!exists.expect("no server is a clean negative, not an error"));
}

/// A composer-shaped pane: it draws a prompt glyph, reads one line, prints it inside `GOT[...]`,
/// and draws a bare prompt glyph again. Tmux's canonical echo moves the cursor down on Enter, so
/// the bare glyph lands on the cursor row, which is what the composer read proves empty.
const COMPOSER_READ_ONE_LINE: &str =
    "printf '❯ '; read line; printf 'GOT[%s]\\n❯ ' \"$line\"; sleep 60";

/// A pane that echoes what it is typed and ignores Enter. It is raw-mode, so Enter arrives as a
/// carriage return that `cat` echoes without moving to a new row, and the typed text stays on the
/// cursor row. This is a swallowed Enter.
const ECHOING_COMPOSER_IGNORES_ENTER: &str = "stty raw -echo; printf '❯ '; exec cat";

#[test]
fn submit_types_the_text_literally_and_enter_is_confirmed_by_the_composer() {
    let server = PrivateServer::start("s1", COMPOSER_READ_ONE_LINE);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "❯");

    let attempts = backend
        .send_text_submit("s1", "-x Enter C-c hello")
        .expect("the composer confirms the submit");

    assert_eq!(attempts, 1, "the first Enter is taken");
    // `Enter` and `C-c` are typed as words, and the leading `-` is not read as an option.
    wait_for_marker(&backend, "s1", "GOT[-x Enter C-c hello]");
}

#[test]
fn submit_reports_typed_unconfirmed_and_never_retypes_when_enter_is_ignored() {
    let server = PrivateServer::start("s1", ECHOING_COMPOSER_IGNORES_ENTER);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "❯");

    let error = backend
        .send_text_submit("s1", "hello-raw")
        .expect_err("the composer keeps the text, so nothing confirms the submit");

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.session, "s1");
            assert_eq!(unconfirmed.enter_presses, 3, "{unconfirmed:?}");
            assert!(
                unconfirmed.detail.contains("retries exhausted"),
                "{unconfirmed:?}"
            );
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
    // The text is typed once. A retry of Enter never retypes it, so it appears a single time.
    let pane = backend
        .capture_pane("s1", 40)
        .expect("the session s1 still exists");
    assert_eq!(pane.matches("hello-raw").count(), 1, "{pane:?}");
}

#[test]
fn submit_never_types_into_a_longer_session_name() {
    let server = PrivateServer::start("s12", COMPOSER_READ_ONE_LINE);
    let backend = server.backend();

    let error = backend
        .send_text_submit("s1", "hello")
        .expect_err("s1 does not exist, so s12 must not take the text");
    assert!(
        matches!(
            error,
            SubmitError::NotTyped(McaoError::Backend(ref message))
                if message.contains("session") && message.contains("s1")
        ),
        "{error:?}"
    );

    // Give a mistaken send time to echo into the pane before checking that it did not.
    thread::sleep(Duration::from_millis(300));
    let pane = backend
        .capture_pane("s12", 5)
        .expect("the session s12 still exists");
    assert!(!pane.contains("hello"), "{pane:?}");
}

/// A composer whose text wraps: the pane is 80 columns, the prompt glyph and 68 `a`s fill most of
/// the first row, and the 16-character tail `0123456789ABCDEF` wraps onto the cursor row.
fn wrapped_text() -> String {
    format!("{}0123456789ABCDEF", "a".repeat(68))
}

#[test]
fn submit_reads_back_a_wrapped_text_and_retries_enter_when_it_is_swallowed() {
    let server = PrivateServer::start("s1", ECHOING_COMPOSER_IGNORES_ENTER);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "❯");
    let text = wrapped_text();
    assert!(
        text.chars().count() >= 84,
        "the text must wrap an 80-column pane"
    );

    let error = backend
        .send_text_submit("s1", &text)
        .expect_err("the wrapped text stays in the composer, so no Enter confirms");

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.enter_presses, 3, "{unconfirmed:?}");
            assert!(
                unconfirmed.detail.contains("retries exhausted"),
                "{unconfirmed:?}"
            );
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
    // The text is typed once: the `a`s are not retyped by any retry.
    let pane = backend
        .capture_pane("s1", 40)
        .expect("the session s1 still exists");
    assert_eq!(pane.matches('a').count(), 68, "{pane:?}");
}

#[test]
fn submit_long_single_row_text_with_a_swallowed_enter_retries_three_times() {
    let server = PrivateServer::start("s1", ECHOING_COMPOSER_IGNORES_ENTER);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "❯");
    // The glyph and the text share one 80-column row, so the whole text is on the cursor row.
    let text = "a long single-row text that stays on one line";

    let error = backend
        .send_text_submit("s1", text)
        .expect_err("the text stays in the composer, so no Enter confirms");

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.enter_presses, 3, "{unconfirmed:?}");
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
    let pane = backend
        .capture_pane("s1", 40)
        .expect("the session s1 still exists");
    assert_eq!(pane.matches(text).count(), 1, "{pane:?}");
}

#[test]
fn submit_twenty_character_wrapped_tail_retries_three_times() {
    let server = PrivateServer::start("s1", ECHOING_COMPOSER_IGNORES_ENTER);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "❯");
    // The glyph and 78 characters fill the first row, so the 20-character tail alone wraps onto the
    // cursor row. The 16-character window cannot cover that row, so the whole text must be matched.
    let text = format!("{}0123456789ABCDEFGHIJ", "a".repeat(78));

    let error = backend
        .send_text_submit("s1", &text)
        .expect_err("the wrapped text stays in the composer, so no Enter confirms");

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.enter_presses, 3, "{unconfirmed:?}");
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
}

#[test]
fn submit_confirms_a_wrapped_text_when_the_composer_is_empty_after_enter() {
    let server = PrivateServer::start("s1", COMPOSER_READ_ONE_LINE);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "❯");
    let text = wrapped_text();

    let attempts = backend
        .send_text_submit("s1", &text)
        .expect("the wrapped text leaves the composer after the first Enter");

    assert_eq!(attempts, 1);
    // The printed echo line above the composer also wraps, and it must not read as a pending text.
    wait_for_marker(&backend, "s1", "ABCDEF]");
}

/// A composer stub that prints a busy footer and keeps the text on the cursor row, as a harness
/// does when it has queued a message but not yet cleared the composer. The footer is printed before
/// raw mode, so it stays a normal line.
const BUSY_QUEUED_COMPOSER_KEEPS_TEXT: &str =
    "printf 'queued: working\n❯ '; stty raw -echo; exec cat";

#[test]
fn submit_repeats_enter_while_a_busy_footer_keeps_the_text_on_the_cursor_row() {
    // Documents the narrowed claim: the busy footer is not a guard, so every Enter is sent and the
    // text is never typed again. A busy composer is not proven empty and is reported unconfirmed.
    let server = PrivateServer::start("s1", BUSY_QUEUED_COMPOSER_KEEPS_TEXT);
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "queued: working");

    let error = backend
        .send_text_submit("s1", "hello-busy")
        .expect_err("the busy composer keeps the text, so no Enter confirms");

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.enter_presses, 3, "{unconfirmed:?}");
            assert!(
                unconfirmed.detail.contains("retries exhausted"),
                "{unconfirmed:?}"
            );
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
    let pane = backend
        .capture_pane("s1", 40)
        .expect("the session s1 still exists");
    assert!(pane.contains("queued: working"), "{pane:?}");
    assert_eq!(pane.matches("hello-busy").count(), 1, "{pane:?}");
}

/// A composer that shows a placeholder and never echoes typed text. `stty -echo` keeps the typed
/// `fix` off the screen, so the placeholder row is all the cursor row shows. Each line the pane reads
/// is appended to `enters.log`, so the test counts the Enter presses that reached the pane. A tty
/// that echoes would print `fix` after the placeholder, and that row really would end in the text.
fn placeholder_composer_script(enters_log: &Path) -> String {
    format!(
        "stty -echo; printf '❯ Try \"fix lint errors\"'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; done",
        enters_log.display()
    )
}

#[test]
fn submit_short_text_over_a_placeholder_row_sends_one_enter_and_no_more() {
    let server = PrivateServer::start_with("s1", |directory| {
        placeholder_composer_script(&directory.join("enters.log"))
    });
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "fix lint errors");

    let error = backend
        .send_text_submit("s1", "fix")
        .expect_err("the placeholder is not the typed text, so nothing confirms the submit");

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.enter_presses, 1, "{unconfirmed:?}");
            assert!(
                unconfirmed.detail.contains("no further Enter was sent"),
                "{unconfirmed:?}"
            );
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
    // The Enter the pane reads is logged after its read, so give the log a moment to land.
    thread::sleep(Duration::from_millis(300));
    let enters = fs::read_to_string(server.directory.join("enters.log")).unwrap_or_default();
    assert_eq!(
        enters.lines().count(),
        1,
        "exactly one Enter may reach the pane: {enters:?}"
    );
    assert_eq!(enters.trim_end(), "fix", "{enters:?}");
}

/// A dialog that shows a numbered option ending in the typed word. `stty -echo` keeps the typed
/// `continue` off the screen, so the option row is all the cursor row shows. Each line the pane reads
/// is appended to `enters.log`, so the test counts the Enter presses that reached the pane.
fn dialog_option_script(enters_log: &Path) -> String {
    format!(
        "stty -echo; printf '❯ 1. Yes, continue'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; done",
        enters_log.display()
    )
}

#[test]
fn submit_over_a_dialog_option_row_ending_in_the_text_sends_one_enter_and_no_more() {
    let server = PrivateServer::start_with("s1", |directory| {
        dialog_option_script(&directory.join("enters.log"))
    });
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "1. Yes, continue");

    let error = backend
        .send_text_submit("s1", "continue")
        .expect_err("the option row is not the typed text, so nothing confirms the submit");

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.enter_presses, 1, "{unconfirmed:?}");
            assert!(
                unconfirmed.detail.contains("no further Enter was sent"),
                "{unconfirmed:?}"
            );
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
    // The Enter the pane reads is logged after its read, so give the log a moment to land.
    thread::sleep(Duration::from_millis(300));
    let enters = fs::read_to_string(server.directory.join("enters.log")).unwrap_or_default();
    assert_eq!(
        enters.lines().count(),
        1,
        "exactly one Enter may reach the pane: {enters:?}"
    );
    assert_eq!(enters.trim_end(), "continue", "{enters:?}");
}

/// A pane that prints the typed word with no prompt glyph and never echoes typed text, so the bare
/// row is all the cursor row shows. Each line the pane reads is appended to `enters.log`.
fn bare_row_script(enters_log: &Path) -> String {
    format!(
        "stty -echo; printf 'continue'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; done",
        enters_log.display()
    )
}

#[test]
fn submit_over_a_bare_row_showing_the_text_sends_one_enter_and_no_more() {
    let server = PrivateServer::start_with("s1", |directory| {
        bare_row_script(&directory.join("enters.log"))
    });
    let backend = server.backend();
    wait_for_marker(&backend, "s1", "continue");

    let error = backend.send_text_submit("s1", "continue").expect_err(
        "a printed row with no glyph is not the composer, so nothing confirms the submit",
    );

    match error {
        SubmitError::TypedUnconfirmed(unconfirmed) => {
            assert_eq!(unconfirmed.enter_presses, 1, "{unconfirmed:?}");
            assert!(
                unconfirmed.detail.contains("no further Enter was sent"),
                "{unconfirmed:?}"
            );
        }
        other => panic!("expected TypedUnconfirmed, got {other:?}"),
    }
    // The Enter the pane reads is logged after its read, so give the log a moment to land.
    thread::sleep(Duration::from_millis(300));
    let enters = fs::read_to_string(server.directory.join("enters.log")).unwrap_or_default();
    assert_eq!(
        enters.lines().count(),
        1,
        "exactly one Enter may reach the pane: {enters:?}"
    );
    assert_eq!(enters.trim_end(), "continue", "{enters:?}");
}
