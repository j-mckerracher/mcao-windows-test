//! Talks to tmux sessions: pane capture, session existence, and verified text submission.
//!
//! Each call runs one `tmux` command with argv and no shell, so a session name is never parsed as
//! shell syntax. By default the server is the one the environment selects.
//! [`TmuxBackend::with_tmux_tmpdir`] pins a private server instead, which keeps tests and labs off
//! the default server.

use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use mcao_core::McaoError;

/// The tmux program, looked up on `PATH`.
const TMUX_PROGRAM: &str = "tmux";

/// The agent prompt glyphs whose bare row is an empty composer, as `bin/fm-composer-lib.sh` lists
/// them.
const AGENT_PROMPT_GLYPHS: [char; 5] = ['❯', '›', '⟩', '→', '❭'];

/// How many trailing non-whitespace characters of the typed text must show, on the cursor row or
/// wrapped onto it, to prove the text is still in the composer.
const PROBE_CHARS: usize = 16;

/// How many rows above the cursor row are read back, so a text that wraps onto the cursor row is
/// still matched.
const WRAP_ROWS_ABOVE: usize = 2;

/// Pauses and Enter budget for [`TmuxBackend::send_text_submit`].
///
/// The values are the shell composer path's defaults (`bin/fm-send.sh`: a 0.3 s settle, three Enter
/// presses, a 0.4 s wait before the composer is read), kept for parity. The Jira item leaves them
/// OPEN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubmitTiming {
    /// Pause between typing the text and the first Enter.
    settle: Duration,
    /// Enter presses allowed before the submit is reported as unconfirmed.
    enter_attempts: u32,
    /// Pause after each Enter, before the composer is read.
    enter_delay: Duration,
}

const SUBMIT_TIMING: SubmitTiming = SubmitTiming {
    settle: Duration::from_millis(300),
    enter_attempts: 3,
    enter_delay: Duration::from_millis(400),
};

/// Reads the tmux server that holds firstmate's sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TmuxBackend {
    tmux_tmpdir: Option<PathBuf>,
}

impl TmuxBackend {
    /// A backend that talks to the tmux server the environment selects.
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend whose server lives under `tmux_tmpdir`, through `TMUX_TMPDIR`.
    ///
    /// `TMUX` is removed from each command's environment. Otherwise an enclosing tmux client
    /// would address the server it runs in, not the private one.
    pub fn with_tmux_tmpdir(tmux_tmpdir: &Path) -> Self {
        Self {
            tmux_tmpdir: Some(tmux_tmpdir.to_path_buf()),
        }
    }

    /// The pane text of `session`, as `tmux capture-pane -p -S -<lines> -t =<session>:` prints it.
    ///
    /// The `=` prefix and trailing `:` make tmux match the session name exactly, so a session
    /// whose name extends `session` is never captured in its place. Invalid UTF-8 in the capture is
    /// replaced with U+FFFD. A session that does not exist, or a server that cannot be reached, is
    /// a backend failure.
    pub fn capture_pane(&self, session: &str, lines: usize) -> Result<String, McaoError> {
        validate_session(session)?;
        let start = format!("-{lines}");
        let target = format!("={session}:");
        let output = self.run(&["capture-pane", "-p", "-S", &start, "-t", &target])?;
        if !output.status.success() {
            return Err(command_failed("capture-pane", session, &output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Whether a session named exactly `session` exists.
    ///
    /// Exit 1 from `has-session` means no such session. That includes a server that is not running,
    /// because such a server has no sessions. Any other failure is a backend failure.
    pub fn session_exists(&self, session: &str) -> Result<bool, McaoError> {
        validate_session(session)?;
        // The `=` prefix makes tmux match the name exactly rather than by prefix.
        let target = format!("={session}");
        let output = self.run(&["has-session", "-t", &target])?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(command_failed("has-session", session, &output)),
        }
    }

    /// Types `text` into `session` once, then presses Enter until the composer confirms the submit.
    ///
    /// The text goes in as literal keys (`send-keys -l --`), so `Enter` or `C-c` in it is typed,
    /// never pressed, and a leading `-` is never read as a tmux option. It is typed exactly once.
    /// Only Enter is repeated, so a retry never types the text again. That is the whole of the
    /// duplicate guarantee. A repeated Enter is not gated on a busy composer or a blocking dialog,
    /// so a message the harness queued can be submitted twice, and a picker the first Enter opened
    /// can be confirmed by a later one.
    ///
    /// After each Enter the composer is read back. The typed text is matched on the cursor row and on
    /// up to two rows above it, so a text that wraps is still found. A bare agent prompt glyph on the
    /// cursor row confirms the submit, and the return value is that Enter press, counting from 1. A
    /// text that still ends on the cursor row sends another Enter, up to three presses in all. Any
    /// other reading stops at once: it proves neither a submit nor a swallowed Enter. Key delivery
    /// alone never counts as a submit.
    ///
    /// This port covers the bare agent-glyph shape and the wrapped text match. The shell classifier's
    /// bordered, left-bar, and separated shapes, footer zone, and busy-queued and turn-started
    /// conversions are not ported. A harness whose cursor is not on its composer, such
    /// as Cursor Agent parking it below the footer, reads as unconfirmed, never as submitted.
    ///
    /// Empty, blank, control-character-bearing, and `;`-terminated text are refused before tmux runs.
    /// tmux drops a trailing unescaped `;` from a literal argument, so such text would reach the
    /// pane altered. The shell path has that drop too.
    ///
    /// # Errors
    ///
    /// [`SubmitError::NotTyped`] when nothing reached the pane: the text was refused, the session is
    /// invalid, or the literal send failed. [`SubmitError::TypedUnconfirmed`] when the text is in the
    /// pane but no submit was confirmed. A caller must read the pane before any resend, and must not
    /// type the text again.
    pub fn send_text_submit(&self, session: &str, text: &str) -> Result<u32, SubmitError> {
        submit_text_with(
            session,
            text,
            SUBMIT_TIMING,
            |args| {
                let output = self.run(args)?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(command_failed("send-keys", session, &output))
                }
            },
            |needle| self.read_composer(session, needle),
        )
    }

    /// Reads the cursor row of `session`'s pane and classifies it against the typed text's `needle`.
    fn read_composer(&self, session: &str, needle: &str) -> Result<Composer, McaoError> {
        let target = format!("={session}:");
        let cursor = self.run(&["display-message", "-p", "-t", &target, "#{cursor_y}"])?;
        if !cursor.status.success() {
            return Err(command_failed("display-message", session, &cursor));
        }
        let Ok(row) = String::from_utf8_lossy(&cursor.stdout)
            .trim()
            .parse::<usize>()
        else {
            return Ok(Composer::Unknown);
        };
        let pane = self.run(&["capture-pane", "-p", "-S", "0", "-E", "-", "-t", &target])?;
        if !pane.status.success() {
            return Err(command_failed("capture-pane", session, &pane));
        }
        let text = String::from_utf8_lossy(&pane.stdout);
        let lines: Vec<&str> = text.lines().collect();
        let Some(&cursor_row) = lines.get(row) else {
            return Ok(Composer::Unknown);
        };
        let above = &lines[row.saturating_sub(WRAP_ROWS_ABOVE)..row];
        Ok(classify_composer(above, cursor_row, needle))
    }

    /// Runs one tmux command with `args` and captures its output.
    fn run(&self, args: &[&str]) -> Result<Output, McaoError> {
        let mut command = Command::new(TMUX_PROGRAM);
        command.args(args);
        if let Some(tmux_tmpdir) = &self.tmux_tmpdir {
            command.env("TMUX_TMPDIR", tmux_tmpdir).env_remove("TMUX");
        }
        command.output().map_err(|error| match error.kind() {
            ErrorKind::NotFound => {
                McaoError::Preflight(format!("{TMUX_PROGRAM} was not found on PATH"))
            }
            _ => McaoError::Io(error),
        })
    }
}

/// Why [`TmuxBackend::send_text_submit`] did not confirm a submit.
#[derive(Debug)]
pub enum SubmitError {
    /// Nothing reached the pane: the text was refused before tmux ran, or the literal send failed.
    /// Sending the same text again cannot duplicate it.
    NotTyped(McaoError),
    /// The text is in the pane, typed once, and no submit was confirmed. The caller must read the
    /// pane before any resend, and must not type the text again.
    TypedUnconfirmed(TypedUnconfirmed),
}

/// A text that is in a session's pane without a confirmed submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedUnconfirmed {
    /// The session the text was typed into.
    pub session: String,
    /// Enter presses sent before the attempt stopped.
    pub enter_presses: u32,
    /// Why the submit stopped, including the stderr of each failed Enter press.
    pub detail: String,
}

impl fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTyped(error) => write!(formatter, "text not typed: {error}"),
            Self::TypedUnconfirmed(unconfirmed) => write!(
                formatter,
                "text typed into session {} but the submit is unconfirmed after {} Enter press(es): {}",
                unconfirmed.session, unconfirmed.enter_presses, unconfirmed.detail
            ),
        }
    }
}

impl std::error::Error for SubmitError {}

/// What the composer row under the cursor shows after an Enter press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Composer {
    /// A bare agent prompt glyph and nothing after it: the typed text has left the composer.
    Empty,
    /// The typed text still ends on the cursor row, so the Enter was not taken.
    Pending,
    /// Anything else. It proves neither a submit nor a swallowed Enter, so nothing is retried.
    Unknown,
}

/// The verified-submit core, with the tmux calls injected so tests can script each one.
///
/// `send_keys` gets the full argv of one `send-keys` call. It runs once for the literal text and
/// then once per Enter press. `read_composer` runs after each Enter press with the typed text's
/// `needle`. Only an Enter press is ever repeated.
fn submit_text_with(
    session: &str,
    text: &str,
    timing: SubmitTiming,
    mut send_keys: impl FnMut(&[&str]) -> Result<(), McaoError>,
    mut read_composer: impl FnMut(&str) -> Result<Composer, McaoError>,
) -> Result<u32, SubmitError> {
    validate_session(session).map_err(SubmitError::NotTyped)?;
    validate_submit_text(text).map_err(SubmitError::NotTyped)?;
    // The `=` prefix and trailing `:` select the session's current pane, as capture_pane does.
    let target = format!("={session}:");
    send_keys(&["send-keys", "-t", &target, "-l", "--", text]).map_err(SubmitError::NotTyped)?;

    // The text is in the pane from here on. Every failure below is TypedUnconfirmed, and nothing
    // types the text again.
    let needle = probe_needle(text);
    let unconfirmed = |enter_presses: u32, detail: String| {
        SubmitError::TypedUnconfirmed(TypedUnconfirmed {
            session: session.to_string(),
            enter_presses,
            detail,
        })
    };
    thread::sleep(timing.settle);
    let mut failed_enters: Vec<String> = Vec::new();
    for press in 1..=timing.enter_attempts {
        if let Err(error) = send_keys(&["send-keys", "-t", &target, "Enter"]) {
            failed_enters.push(format!("Enter {press}: {error}"));
        }
        thread::sleep(timing.enter_delay);
        match read_composer(&needle) {
            Ok(Composer::Empty) => return Ok(press),
            Ok(Composer::Pending) => {}
            Ok(Composer::Unknown) => {
                return Err(unconfirmed(
                    press,
                    with_failed_enters(
                        format!(
                            "the composer is not proven empty after Enter {press}, so no further Enter was sent"
                        ),
                        &failed_enters,
                    ),
                ));
            }
            Err(error) => {
                return Err(unconfirmed(
                    press,
                    with_failed_enters(
                        format!("the composer could not be read after Enter {press}: {error}"),
                        &failed_enters,
                    ),
                ));
            }
        }
    }
    Err(unconfirmed(
        timing.enter_attempts,
        with_failed_enters(
            format!(
                "the composer still shows the text after {} Enter presses (retries exhausted)",
                timing.enter_attempts
            ),
            &failed_enters,
        ),
    ))
}

/// Appends the failed Enter presses' stderr to an unconfirmed detail, so no earlier cause is lost.
fn with_failed_enters(detail: String, failed_enters: &[String]) -> String {
    if failed_enters.is_empty() {
        detail
    } else {
        format!(
            "{detail}; failed Enter presses: {}",
            failed_enters.join("; ")
        )
    }
}

/// Classifies the composer under the cursor from the typed text's `needle` and the rows around it.
///
/// `above` holds the rows just above the cursor row, oldest first, and `cursor_row` is the row the
/// cursor is on. Whitespace is ignored on both sides of the match, because tmux drops trailing
/// spaces from a captured row and a wrapped text can break at one. A match counts only when it ends
/// on the cursor row. A text wholly on a row above is output the harness printed, not the composer,
/// so it never proves the text is still pending. Pending is checked first, so a row that could be
/// both never reads as empty.
fn classify_composer(above: &[&str], cursor_row: &str, needle: &str) -> Composer {
    let mut haystack = String::new();
    for row in above {
        haystack.extend(row.chars().filter(|character| !character.is_whitespace()));
    }
    let cursor_start = haystack.len();
    haystack.extend(
        cursor_row
            .chars()
            .filter(|character| !character.is_whitespace()),
    );
    if !needle.is_empty() && match_ends_after(&haystack, needle, cursor_start) {
        return Composer::Pending;
    }
    let mut characters = cursor_row.trim_matches(char::is_whitespace).chars();
    match (characters.next(), characters.next()) {
        (Some(glyph), None) if AGENT_PROMPT_GLYPHS.contains(&glyph) => Composer::Empty,
        _ => Composer::Unknown,
    }
}

/// Whether `needle` occurs in `haystack` and its last character lies past byte `boundary`.
///
/// Every start is tried, so an overlapping occurrence is never skipped.
fn match_ends_after(haystack: &str, needle: &str, boundary: usize) -> bool {
    haystack
        .char_indices()
        .any(|(start, _)| start + needle.len() > boundary && haystack[start..].starts_with(needle))
}

/// The trailing non-whitespace characters of `text` that identify it on the composer.
///
/// A long text that wraps keeps its tail near the cursor, so the tail is what is matched.
fn probe_needle(text: &str) -> String {
    let characters: Vec<char> = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let skip = characters.len().saturating_sub(PROBE_CHARS);
    characters[skip..].iter().collect()
}

/// Rejects text that tmux would not type as given, or that is blank.
fn validate_submit_text(text: &str) -> Result<(), McaoError> {
    if text.trim_matches(char::is_whitespace).is_empty() {
        return Err(McaoError::Usage("text to submit is empty or blank".into()));
    }
    if let Some(bad) = text.chars().find(|character| character.is_control()) {
        return Err(McaoError::Usage(format!(
            "text to submit contains {bad:?}, a control character, which is not allowed"
        )));
    }
    if text.ends_with(';') {
        return Err(McaoError::Usage(
            "text to submit ends with ';', which tmux drops from the typed text".into(),
        ));
    }
    Ok(())
}

/// Rejects names that cannot be passed to tmux as one literal session name.
///
/// Quotes, control characters, `:`, and `.` are refused at the boundary. `:` and `.` are the
/// separators of tmux's window and pane target syntax, and tmux never stores them in a session
/// name, so a name that contains either one is a target rather than a session.
fn validate_session(session: &str) -> Result<(), McaoError> {
    if session.is_empty() {
        return Err(McaoError::Usage("tmux session name is empty".into()));
    }
    if let Some(bad) = session
        .chars()
        .find(|character| matches!(character, '\'' | '"' | ':' | '.') || character.is_control())
    {
        return Err(McaoError::Usage(format!(
            "tmux session name {session:?} contains {bad:?}, which is not allowed"
        )));
    }
    Ok(())
}

/// The backend failure for a tmux command that exited unsuccessfully.
fn command_failed(subcommand: &str, session: &str, output: &Output) -> McaoError {
    McaoError::Backend(format!(
        "tmux {subcommand} failed for session {session} ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_session_name_is_a_usage_error() {
        let error = TmuxBackend::new().capture_pane("", 5).unwrap_err();
        assert!(matches!(error, McaoError::Usage(_)), "{error:?}");
    }

    #[test]
    fn quoted_or_target_syntax_session_name_is_refused_before_tmux_runs() {
        for name in ["s'1", "s\"1", "s\n1", "s1:0", "s1.0"] {
            let error = TmuxBackend::new().session_exists(name).unwrap_err();
            assert!(matches!(error, McaoError::Usage(_)), "{name:?}: {error:?}");
        }
    }

    /// Timing with no pauses, so scripted tests run at full speed.
    const NO_WAIT: SubmitTiming = SubmitTiming {
        settle: Duration::ZERO,
        enter_attempts: 3,
        enter_delay: Duration::ZERO,
    };

    /// Submits `text` with tmux and the composer scripted.
    ///
    /// Send call `index` (0 is the literal text, then each Enter) fails when `fail_sends` lists it.
    /// The n-th composer read returns `readings[n]`, where `None` is an unreadable composer, and the
    /// last reading repeats. Returns the outcome and every argv tmux was asked to run.
    fn submit_scripted(
        text: &str,
        fail_sends: &[usize],
        readings: &[Option<Composer>],
    ) -> (Result<u32, SubmitError>, Vec<Vec<String>>) {
        let mut calls: Vec<Vec<String>> = Vec::new();
        let mut reads = 0;
        let result = submit_text_with(
            "s1",
            text,
            NO_WAIT,
            |args| {
                let index = calls.len();
                calls.push(args.iter().map(|arg| arg.to_string()).collect());
                if fail_sends.contains(&index) {
                    Err(McaoError::Backend(format!(
                        "scripted send-keys failure {index}"
                    )))
                } else {
                    Ok(())
                }
            },
            |_needle| {
                let reading = readings[reads.min(readings.len() - 1)];
                reads += 1;
                reading.ok_or_else(|| McaoError::Backend("scripted composer read failure".into()))
            },
        );
        (result, calls)
    }

    /// The number of argv calls that typed the literal text, and the number that pressed Enter.
    fn typed_and_enters(calls: &[Vec<String>]) -> (usize, usize) {
        let typed = calls
            .iter()
            .filter(|call| call.contains(&"-l".to_string()))
            .count();
        let enters = calls
            .iter()
            .filter(|call| call.last().map(String::as_str) == Some("Enter"))
            .count();
        (typed, enters)
    }

    #[test]
    fn submit_types_once_and_confirms_on_the_first_empty_composer() {
        let (result, calls) = submit_scripted("hello", &[], &[Some(Composer::Empty)]);

        assert_eq!(result.expect("submit confirmed"), 1);
        assert_eq!(
            calls,
            vec![
                vec!["send-keys", "-t", "=s1:", "-l", "--", "hello"],
                vec!["send-keys", "-t", "=s1:", "Enter"],
            ]
        );
    }

    #[test]
    fn submit_leading_dash_text_stays_literal_after_the_option_terminator() {
        let (_, calls) = submit_scripted("-x Enter C-c", &[], &[Some(Composer::Empty)]);

        assert_eq!(calls[0][3..], ["-l", "--", "-x Enter C-c"]);
    }

    #[test]
    fn submit_presses_enter_again_only_while_the_text_is_still_pending() {
        let (result, calls) = submit_scripted(
            "hello",
            &[],
            &[Some(Composer::Pending), Some(Composer::Empty)],
        );

        assert_eq!(result.expect("second Enter confirms"), 2);
        assert_eq!(typed_and_enters(&calls), (1, 2), "{calls:?}");
    }

    #[test]
    fn submit_exhaustion_is_typed_unconfirmed_and_never_retypes() {
        let (result, calls) = submit_scripted("hello", &[], &[Some(Composer::Pending)]);

        match result.expect_err("the composer never empties") {
            SubmitError::TypedUnconfirmed(unconfirmed) => {
                assert_eq!(unconfirmed.session, "s1");
                assert_eq!(unconfirmed.enter_presses, 3);
                assert!(
                    unconfirmed.detail.contains("retries exhausted"),
                    "{unconfirmed:?}"
                );
            }
            other => panic!("expected TypedUnconfirmed, got {other:?}"),
        }
        assert_eq!(typed_and_enters(&calls), (1, 3), "{calls:?}");
    }

    #[test]
    fn submit_stops_at_once_when_the_composer_is_not_proven_empty() {
        let (result, calls) = submit_scripted("hello", &[], &[Some(Composer::Unknown)]);

        match result.expect_err("nothing proves the submit") {
            SubmitError::TypedUnconfirmed(unconfirmed) => {
                assert_eq!(unconfirmed.enter_presses, 1);
                assert!(
                    unconfirmed.detail.contains("not proven empty"),
                    "{unconfirmed:?}"
                );
            }
            other => panic!("expected TypedUnconfirmed, got {other:?}"),
        }
        assert_eq!(typed_and_enters(&calls), (1, 1), "{calls:?}");
    }

    #[test]
    fn submit_keeps_every_failed_enter_in_the_exhaustion_detail() {
        let (result, calls) = submit_scripted("hello", &[1, 2, 3], &[Some(Composer::Pending)]);

        match result.expect_err("every Enter fails and the text stays") {
            SubmitError::TypedUnconfirmed(unconfirmed) => {
                assert_eq!(unconfirmed.enter_presses, 3);
                for cause in [
                    "scripted send-keys failure 1",
                    "scripted send-keys failure 2",
                    "scripted send-keys failure 3",
                ] {
                    assert!(unconfirmed.detail.contains(cause), "{unconfirmed:?}");
                }
            }
            other => panic!("expected TypedUnconfirmed, got {other:?}"),
        }
        assert_eq!(typed_and_enters(&calls), (1, 3), "{calls:?}");
    }

    #[test]
    fn submit_confirms_by_the_composer_even_when_an_enter_send_failed() {
        let (result, _) = submit_scripted("hello", &[1], &[Some(Composer::Empty)]);

        assert_eq!(result.expect("the composer is empty"), 1);
    }

    #[test]
    fn submit_unreadable_composer_after_typing_is_typed_unconfirmed() {
        let (result, calls) = submit_scripted("hello", &[], &[None]);

        match result.expect_err("the composer cannot be read") {
            SubmitError::TypedUnconfirmed(unconfirmed) => {
                assert_eq!(unconfirmed.enter_presses, 1);
                assert!(
                    unconfirmed.detail.contains("could not be read"),
                    "{unconfirmed:?}"
                );
            }
            other => panic!("expected TypedUnconfirmed, got {other:?}"),
        }
        assert_eq!(typed_and_enters(&calls), (1, 1), "{calls:?}");
    }

    #[test]
    fn submit_failed_literal_send_is_not_typed_and_presses_no_enter() {
        let (result, calls) = submit_scripted("hello", &[0], &[Some(Composer::Empty)]);

        assert!(
            matches!(result, Err(SubmitError::NotTyped(McaoError::Backend(_)))),
            "{result:?}"
        );
        assert_eq!(
            calls.len(),
            1,
            "no Enter after a failed text send: {calls:?}"
        );
    }

    #[test]
    fn submit_refuses_unsafe_text_as_not_typed_before_tmux_runs() {
        for text in [
            "",
            "   ",
            "line one\nline two",
            "line\r",
            "tab\there",
            "esc\u{1b}[A",
            "pending;",
        ] {
            let (result, calls) = submit_scripted(text, &[], &[Some(Composer::Empty)]);

            assert!(
                matches!(result, Err(SubmitError::NotTyped(McaoError::Usage(_)))),
                "{text:?}: {result:?}"
            );
            assert!(calls.is_empty(), "{text:?} reached tmux: {calls:?}");
        }
    }

    #[test]
    fn composer_row_is_empty_only_as_a_bare_agent_glyph() {
        for row in ["❯", "❯\u{a0}", "› ", "⟩", "→", "❭"] {
            assert_eq!(
                classify_composer(&[], row, "hello"),
                Composer::Empty,
                "{row:?}"
            );
        }
        for row in ["$", "❯ ❯", "", "   ", "hello-raw"] {
            assert_eq!(
                classify_composer(&[], row, "zz"),
                Composer::Unknown,
                "{row:?}"
            );
        }
    }

    #[test]
    fn composer_row_still_showing_the_text_is_pending() {
        assert_eq!(
            classify_composer(&[], "❯ hello", "hello"),
            Composer::Pending
        );
        assert_eq!(
            classify_composer(&[], "hello-raw", "hello-raw"),
            Composer::Pending
        );
        // Pending wins over the glyph test, so a text that is itself a glyph never reads as empty.
        assert_eq!(classify_composer(&[], "❯", "❯"), Composer::Pending);
    }

    #[test]
    fn wrapped_text_is_pending_when_its_tail_wraps_onto_the_cursor_row() {
        // An 84-character text in an 80-column pane: the glyph and 78 characters on one row, the
        // last 6 on the cursor row. The 16-character needle spans both rows.
        let above = [
            "❯ aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0123456789",
        ];
        let needle = "0123456789ABCDEF";

        assert_eq!(
            classify_composer(&above, "ABCDEF", needle),
            Composer::Pending
        );
    }

    #[test]
    fn wrapped_text_break_at_a_space_still_matches_ignoring_whitespace() {
        // tmux drops trailing spaces from a captured row, so the wrap point loses its space.
        assert_eq!(
            classify_composer(&["❯ hello wor"], "ld", "helloworld"),
            Composer::Pending
        );
    }

    #[test]
    fn output_printed_above_a_bare_glyph_never_reads_as_pending() {
        // The echo line `GOT[hello]` sits above an empty composer. Its match ends before the cursor
        // row, so the submit is confirmed rather than retried.
        assert_eq!(
            classify_composer(&["GOT[hello]"], "❯", "hello"),
            Composer::Empty
        );
    }

    #[test]
    fn text_wholly_above_the_cursor_row_is_not_pending() {
        assert_eq!(
            classify_composer(&["hello-raw"], "x", "hello-raw"),
            Composer::Unknown
        );
    }

    #[test]
    fn probe_needle_is_the_trailing_non_whitespace_characters() {
        assert_eq!(probe_needle("hello"), "hello");
        assert_eq!(probe_needle("  hello  "), "hello");
        assert_eq!(probe_needle("a b c"), "abc");
        let long = format!("{}XYZ", "a".repeat(40));
        let needle = probe_needle(&long);
        assert_eq!(needle.chars().count(), PROBE_CHARS);
        assert!(needle.ends_with("XYZ"), "{needle:?}");
    }
}
