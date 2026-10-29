//! Terminal text and window sizing shared by every terminal backend.
//!
//! [`normalize_capture`] turns the raw bytes a ConPTY or pty child printed into the
//! text a caller reads: UTF-8 with LF line ends and no terminal control sequences.
//! [`resolve_window_size`] picks the size a terminal is opened at. Both are pure, so
//! they are tested on every platform.

/// The size used when neither the environment nor the console names one, as
/// `(columns, rows)`.
pub const DEFAULT_WINDOW: (u16, u16) = (120, 40);

/// The smallest size a terminal is opened at, as `(columns, rows)`.
pub const MIN_WINDOW: (u16, u16) = (80, 24);

/// The largest dimension a ConPTY accepts. Its coordinates are signed 16-bit values.
const MAX_DIMENSION: u16 = i16::MAX as u16;

const ESC: char = '\u{1b}';
const BEL: char = '\u{7}';
const CAN: char = '\u{18}';
const SUB: char = '\u{1a}';
const C1_CSI: char = '\u{9b}';
const C1_OSC: char = '\u{9d}';
const C1_STRING_STARTS: [char; 4] = ['\u{90}', '\u{98}', '\u{9e}', '\u{9f}'];

/// The most characters one control string may hold before it is abandoned. A string that
/// never reaches its terminator would otherwise swallow the rest of the capture.
const MAX_STRING_BODY_CHARS: usize = 4096;

/// Where the normalizer is inside an escape sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Plain text.
    Ground,
    /// Just after an ESC.
    Escape,
    /// Inside a CSI sequence, up to its final byte.
    Csi,
    /// Inside an escape sequence with intermediate bytes, up to its final byte.
    Intermediate,
    /// Inside an OSC sequence. BEL or ST ends it.
    Osc,
    /// Inside a DCS, SOS, PM or APC string. Only ST ends it.
    StringBody,
    /// An ESC seen inside an OSC or string. A backslash ends the string.
    StringEscape,
}

/// Converts captured terminal bytes to the text a caller reads.
///
/// Invalid UTF-8 is replaced. CRLF and a lone CR both become LF. Escape sequences
/// are removed: CSI, OSC and string sequences whole, and two-byte escapes with their
/// intermediate bytes. CAN and SUB cancel a sequence in progress, as a VT parser does.
/// A line feed or tab inside an unfinished escape sequence ends that sequence and is
/// kept, so the character after it is text and not the sequence's final byte. A control
/// string that is not terminated within [`MAX_STRING_BODY_CHARS`] characters is abandoned
/// there, and the characters after that point are kept as text. Other control characters
/// are dropped.
///
/// The result is the output stream with its control sequences removed. It is not a
/// rendered screen: cursor movement and overwrites are not applied, so a progress bar's
/// redraws stay as separate text.
pub fn normalize_capture(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut state = State::Ground;
    let mut string_chars = 0_usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && state == State::Ground {
            // CRLF is one line end: the LF that follows emits it.
            if chars.peek() != Some(&'\n') {
                out.push('\n');
            }
            continue;
        }
        state = step(state, c, &mut out);
        string_chars = if is_control_string(state) {
            string_chars + 1
        } else {
            0
        };
        if string_chars > MAX_STRING_BODY_CHARS {
            state = State::Ground;
            string_chars = 0;
        }
    }
    out
}

/// Reports whether `state` is inside a control string, whose length is capped.
fn is_control_string(state: State) -> bool {
    matches!(state, State::Osc | State::StringBody | State::StringEscape)
}

/// Advances the normalizer by one character, appending any kept text to `out`.
fn step(state: State, c: char, out: &mut String) -> State {
    if state != State::Ground && matches!(c, CAN | SUB) {
        return State::Ground;
    }
    match state {
        State::Ground => match c {
            '\n' | '\t' => {
                out.push(c);
                State::Ground
            }
            ESC => State::Escape,
            C1_CSI => State::Csi,
            C1_OSC => State::Osc,
            c if C1_STRING_STARTS.contains(&c) => State::StringBody,
            c if is_dropped(c) => State::Ground,
            c => {
                out.push(c);
                State::Ground
            }
        },
        State::Escape => match c {
            ESC => State::Escape,
            '[' => State::Csi,
            ']' => State::Osc,
            'P' | 'X' | '^' | '_' => State::StringBody,
            ' '..='/' => State::Intermediate,
            '0'..='~' => State::Ground,
            // A line end cuts the unfinished sequence short. The next character is text.
            '\n' | '\t' => {
                out.push(c);
                State::Ground
            }
            _ => State::Ground,
        },
        State::Csi => match c {
            ESC => State::Escape,
            '\n' | '\t' => {
                out.push(c);
                State::Ground
            }
            '@'..='~' => State::Ground,
            _ => State::Csi,
        },
        State::Intermediate => match c {
            ESC => State::Escape,
            '\n' | '\t' => {
                out.push(c);
                State::Ground
            }
            '0'..='~' => State::Ground,
            _ => State::Intermediate,
        },
        State::Osc => match c {
            BEL => State::Ground,
            ESC => State::StringEscape,
            _ => State::Osc,
        },
        State::StringBody => match c {
            ESC => State::StringEscape,
            _ => State::StringBody,
        },
        State::StringEscape => match c {
            '\\' => State::Ground,
            // An ESC that does not start a string terminator begins a new sequence.
            _ => step(State::Escape, c, out),
        },
    }
}

/// Reports whether `c` is a control character that is not kept as text.
fn is_dropped(c: char) -> bool {
    c < ' ' || c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c)
}

/// Picks the terminal size as `(columns, rows)`.
///
/// `env` is the value of the `SIZE` variable in the form `COLSxROWS`. It wins when it
/// parses. Otherwise `buffer`, the size of the console buffer, is used. Otherwise the
/// size is [`DEFAULT_WINDOW`]. The result is clamped to at least [`MIN_WINDOW`] and at
/// most the largest dimension a ConPTY accepts.
pub fn resolve_window_size(env: Option<&str>, buffer: Option<(u16, u16)>) -> (u16, u16) {
    env.and_then(parse_size)
        .or(buffer)
        .map_or(DEFAULT_WINDOW, clamp_window)
}

/// Parses `COLSxROWS`, accepting either `x` or `X` as the separator.
fn parse_size(text: &str) -> Option<(u16, u16)> {
    let (columns, rows) = text.trim().split_once(['x', 'X'])?;
    Some((columns.trim().parse().ok()?, rows.trim().parse().ok()?))
}

fn clamp_window((columns, rows): (u16, u16)) -> (u16, u16) {
    (
        columns.clamp(MIN_WINDOW.0, MAX_DIMENSION),
        rows.clamp(MIN_WINDOW.1, MAX_DIMENSION),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_text_passes_through_unchanged() {
        assert_eq!(normalize_capture("héllo ✓\n".as_bytes()), "héllo ✓\n");
    }

    #[test]
    fn crlf_and_lone_cr_become_lf() {
        assert_eq!(normalize_capture(b"a\r\nb\rc\r\n"), "a\nb\nc\n");
        assert_eq!(normalize_capture(b"a\r\r\nb"), "a\n\nb");
    }

    #[test]
    fn csi_colour_sequences_are_folded_away() {
        assert_eq!(normalize_capture(b"\x1b[32mgreen\x1b[0m"), "green");
        assert_eq!(normalize_capture(b"\x1b[1;31;4mX\x1b[?25l"), "X");
    }

    #[test]
    fn osc_sequences_end_at_bel_or_st() {
        assert_eq!(normalize_capture(b"\x1b]0;title\x07ok"), "ok");
        assert_eq!(normalize_capture(b"\x1b]8;;http://x\x1b\\link"), "link");
    }

    #[test]
    fn string_sequences_end_only_at_st() {
        assert_eq!(normalize_capture(b"\x1bPdata\x07still\x1b\\x"), "x");
    }

    #[test]
    fn two_byte_and_charset_escapes_are_removed() {
        assert_eq!(normalize_capture(b"\x1b7saved\x1b8"), "saved");
        assert_eq!(normalize_capture(b"\x1b(Bplain"), "plain");
    }

    #[test]
    fn a_new_escape_inside_a_string_starts_a_fresh_sequence() {
        assert_eq!(normalize_capture(b"\x1b]0;t\x1b[31mred"), "red");
    }

    #[test]
    fn a_line_end_inside_an_unfinished_escape_does_not_eat_the_next_character() {
        // Each letter after the line end is the first character of the next line, not
        // the final byte of the sequence that the line end cut short.
        assert_eq!(normalize_capture(b"\x1b\nvisible\n"), "\nvisible\n");
        assert_eq!(normalize_capture(b"\x1b[\nvisible\n"), "\nvisible\n");
        assert_eq!(normalize_capture(b"\x1b(\tvisible\n"), "\tvisible\n");
    }

    #[test]
    fn controls_are_dropped_but_tabs_and_newlines_stay() {
        assert_eq!(normalize_capture(b"a\x07\x08b\tc\n"), "ab\tc\n");
        assert_eq!(normalize_capture("x\u{9b}31my".as_bytes()), "xy");
    }

    #[test]
    fn can_and_sub_cancel_a_sequence_in_progress() {
        assert_eq!(normalize_capture(b"\x1b]0;title\x18ok"), "ok");
        assert_eq!(normalize_capture(b"\x1b[31\x1amixed"), "mixed");
        assert_eq!(normalize_capture(b"\x1bPdata\x18text"), "text");
        assert_eq!(normalize_capture(b"a\x18\x1ab"), "ab");
    }

    #[test]
    fn a_control_string_within_the_cap_is_removed_whole() {
        let body = "x".repeat(MAX_STRING_BODY_CHARS - 2);
        assert_eq!(
            normalize_capture(format!("\x1b]{body}\x07ok").as_bytes()),
            "ok"
        );
    }

    #[test]
    fn an_unterminated_control_string_is_abandoned_at_the_cap() {
        let body = "x".repeat(MAX_STRING_BODY_CHARS * 2);
        assert_eq!(
            normalize_capture(format!("\x1b]{body}\x07ok").as_bytes()),
            format!("{}ok", "x".repeat(MAX_STRING_BODY_CHARS))
        );
    }

    #[test]
    fn invalid_utf8_is_replaced_not_refused() {
        assert_eq!(normalize_capture(b"ok\xffend"), "ok\u{fffd}end");
    }

    #[test]
    fn size_from_the_environment_wins_when_it_parses() {
        assert_eq!(
            resolve_window_size(Some("100x30"), Some((150, 50))),
            (100, 30)
        );
        assert_eq!(resolve_window_size(Some(" 132X43 "), None), (132, 43));
    }

    #[test]
    fn an_unreadable_environment_size_falls_back_to_the_buffer() {
        assert_eq!(
            resolve_window_size(Some("wide"), Some((150, 50))),
            (150, 50)
        );
        assert_eq!(resolve_window_size(Some("100"), Some((150, 50))), (150, 50));
    }

    #[test]
    fn the_default_applies_when_nothing_names_a_size() {
        assert_eq!(resolve_window_size(None, None), DEFAULT_WINDOW);
        assert_eq!(DEFAULT_WINDOW, (120, 40));
    }

    #[test]
    fn sizes_below_the_minimum_are_raised_to_it() {
        assert_eq!(resolve_window_size(Some("10x5"), None), MIN_WINDOW);
        assert_eq!(resolve_window_size(None, Some((79, 100))), (80, 100));
    }

    #[test]
    fn sizes_above_the_conpty_limit_are_lowered_to_it() {
        assert_eq!(
            resolve_window_size(Some("65535x65535"), None),
            (MAX_DIMENSION, MAX_DIMENSION)
        );
    }
}
