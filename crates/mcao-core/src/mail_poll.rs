//! Mail poll scheduling and cursor parsing for mail polling.
//!
//! Provides cursor and retry set parsing, candidate window bounding,
//! turn alternating budget logic, and durable retry position progression.

use std::collections::HashSet;

use crate::util::pystr;

/// Default wake cap for a poll iteration.
pub const DEFAULT_CAP: usize = 20;

/// Default socket timeout in seconds.
pub const DEFAULT_TIMEOUT_SECS: f64 = 20.0;

/// Errors produced during mail poll parameter parsing.
#[derive(Debug, PartialEq, Eq)]
pub enum PollError {
    /// The specified wake cap was not a valid integer.
    BadCap(String),
}

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PollError::BadCap(raw) => write!(f, "invalid wake cap: {raw}"),
        }
    }
}

impl std::error::Error for PollError {}

/// Parses the socket timeout in seconds.
///
/// Returns 20.0 if raw is `None`, cannot be parsed as `f64`, is non-finite,
/// or is less than or equal to 0.0.
pub fn parse_timeout_secs(raw: Option<&str>) -> f64 {
    let Some(text) = raw else {
        return DEFAULT_TIMEOUT_SECS;
    };
    let trimmed = text.trim_matches(is_strip_ws);
    if trimmed.is_empty() {
        return DEFAULT_TIMEOUT_SECS;
    }
    match trimmed.parse::<f64>() {
        Ok(val) => {
            if val.is_nan() || val.is_infinite() || val <= 0.0 {
                DEFAULT_TIMEOUT_SECS
            } else {
                val
            }
        }
        Err(_) => DEFAULT_TIMEOUT_SECS,
    }
}

/// Parses the poll wake cap.
///
/// If raw is `None` or an empty string, defaults to 20.
/// Otherwise trims whitespace and parses as `i64`.
/// Returns an error on parse failure. Values below 1 default to 20.
pub fn parse_cap(raw: Option<&str>) -> Result<usize, PollError> {
    let s = match raw {
        None => return Ok(DEFAULT_CAP),
        Some("") => return Ok(DEFAULT_CAP),
        Some(s) => s,
    };
    let trimmed = s.trim_matches(is_strip_ws);
    if trimmed.is_empty() {
        return Err(PollError::BadCap(s.to_string()));
    }
    match trimmed.parse::<i64>() {
        Ok(val) => {
            if val < 1 {
                Ok(DEFAULT_CAP)
            } else {
                Ok(val as usize)
            }
        }
        Err(_) => Err(PollError::BadCap(s.to_string())),
    }
}

/// Predicate for Python-compatible whitespace stripping: Python `str.isspace()`, shared with
/// the PII matchers through [`pystr::is_space`].
fn is_strip_ws(c: char) -> bool {
    pystr::is_space(u32::from(c))
}

/// Collapses runs of tabs, carriage returns, and newlines into single spaces,
/// and strips leading and trailing whitespace.
pub fn clean(text: &str) -> String {
    let mut replaced = String::with_capacity(text.len());
    let mut in_run = false;
    for c in text.chars() {
        if c == '\t' || c == '\r' || c == '\n' {
            if !in_run {
                replaced.push(' ');
                in_run = true;
            }
        } else {
            in_run = false;
            replaced.push(c);
        }
    }
    replaced.trim_matches(is_strip_ws).to_string()
}

/// Splits text across universal newlines (`\r\n`, `\r`, or `\n`).
fn universal_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if c == '\r' {
            lines.push(&text[start..idx]);
            if let Some(&(_, '\n')) = chars.peek() {
                chars.next();
            }
            start = chars.peek().map_or(text.len(), |&(next_idx, _)| next_idx);
        } else if c == '\n' {
            lines.push(&text[start..idx]);
            start = chars.peek().map_or(text.len(), |&(next_idx, _)| next_idx);
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Parses a cursor file into the stored mailbox generation and the set of seen UIDs.
pub fn parse_cursor(text: &str) -> (String, HashSet<String>) {
    let mut stored_gen = String::new();
    let mut seen = HashSet::new();
    for line in universal_lines(text) {
        let stripped = line.trim_matches(is_strip_ws);
        if stripped.is_empty() {
            continue;
        }
        if let Some(rest) = stripped.strip_prefix("uidvalidity=") {
            stored_gen = rest.to_string();
        } else {
            seen.insert(stripped.to_string());
        }
    }
    (stored_gen, seen)
}

/// Parses a retry file into an ordered list of unique retry UIDs.
pub fn parse_retry(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    for line in universal_lines(text) {
        let stripped = line.trim_matches(is_strip_ws);
        if stripped.is_empty() {
            continue;
        }
        if seen.insert(stripped.to_string()) {
            order.push(stripped.to_string());
        }
    }
    order
}

/// Parses the durable retry-scan start position clamped to `[0, len)`.
pub fn parse_retry_pos(text: Option<&str>, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let raw = match text {
        None => return 0,
        Some(s) => s.trim_matches(is_strip_ws),
    };
    let val: i64 = if raw.is_empty() {
        0
    } else {
        match raw.parse::<i64>() {
            Ok(v) => v,
            Err(_) => return 0,
        }
    };
    let Ok(len_i64) = i64::try_from(len) else {
        return 0;
    };
    val.rem_euclid(len_i64) as usize
}

/// Returns a bounded slice of the retry order starting at `pos` modulo order length.
pub fn retry_scan_window(order: &[String], pos: usize, window: usize) -> Vec<String> {
    if order.is_empty() {
        return Vec::new();
    }
    let len = order.len();
    let start = pos % len;
    let take_count = len.min(window);
    let mut out = Vec::with_capacity(take_count);
    for item in &order[start..] {
        if out.len() == take_count {
            return out;
        }
        out.push(item.clone());
    }
    for item in &order[..start] {
        if out.len() == take_count {
            return out;
        }
        out.push(item.clone());
    }
    out
}

/// Calculates the next retry start position advancing by `window` items modulo `order_len`.
pub fn next_retry_pos(order_len: usize, window: usize, pos: usize) -> usize {
    if order_len == 0 {
        0
    } else {
        pos.saturating_add(window) % order_len
    }
}

/// Parses the alternating turn flag (0 or 1).
pub fn parse_turn(text: Option<&str>) -> u8 {
    let raw = match text {
        None => return 0,
        Some(s) => s.trim_matches(is_strip_ws),
    };
    let val: i64 = if raw.is_empty() {
        0
    } else {
        match raw.parse::<i64>() {
            Ok(v) => v,
            Err(_) => return 0,
        }
    };
    val.rem_euclid(2) as u8
}

/// Status of a polled message row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Newly surfaced unseen message.
    Ok,
    /// Previously failed message refetched successfully from retry set.
    Retry,
    /// Message whose header could not be fetched.
    Degraded,
}

impl RowStatus {
    /// String representation used in tab-separated poll lines.
    pub fn as_str(self) -> &'static str {
        match self {
            RowStatus::Ok => "ok",
            RowStatus::Retry => "retry",
            RowStatus::Degraded => "degraded",
        }
    }
}

/// A single message row produced by mail polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollRow {
    /// Message UID.
    pub uid: String,
    /// Cleaned Date header value.
    pub date: String,
    /// Cleaned From header value.
    pub from: String,
    /// Cleaned Subject header value.
    pub subject: String,
    /// Row status.
    pub status: RowStatus,
}

impl PollRow {
    /// Formats this row as a tab-separated line without a trailing newline.
    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.uid,
            self.date,
            self.from,
            self.subject,
            self.status.as_str()
        )
    }
}

/// Uncleaned headers retrieved by an IMAP fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHeader {
    /// Raw Date header.
    pub date: String,
    /// Raw Subject header.
    pub subject: String,
    /// Raw From header.
    pub from: String,
}

/// Inputs provided to poll planning.
#[derive(Debug)]
pub struct PollInput<'a> {
    /// Per-poll wake cap (0 defaults to `DEFAULT_CAP`).
    pub cap: usize,
    /// Current mailbox UIDVALIDITY string.
    pub uidvalidity: &'a str,
    /// Mailbox generation recorded in cursor.
    pub stored_generation: &'a str,
    /// Set of message UIDs already surfaced.
    pub seen: &'a HashSet<String>,
    /// Ordered list of retry candidate UIDs.
    pub retry_order: &'a [String],
    /// Durable start position in `retry_order`.
    pub retry_pos: usize,
    /// List of unseen message UIDs returned by mailbox search.
    pub unseen: &'a [String],
    /// Alternating turn flag (0 or 1) for contested single-slot polls.
    pub turn: u8,
}

/// Result of planning a mail poll iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollPlan {
    /// Whether mailbox UIDVALIDITY differed from stored generation.
    pub generation_changed: bool,
    /// Rows to emit for this poll iteration.
    pub rows: Vec<PollRow>,
    /// Updated retry scan position to persist, if any.
    pub retry_pos_write: Option<usize>,
    /// Updated turn flag to persist, if any.
    pub turn_write: Option<u8>,
}

/// Plans and executes a bounded poll pass.
pub fn plan_poll(
    input: &PollInput<'_>,
    fetch: &mut dyn FnMut(&str) -> Option<RawHeader>,
) -> PollPlan {
    let cap = if input.cap == 0 {
        DEFAULT_CAP
    } else {
        input.cap
    };

    let generation_changed =
        input.uidvalidity.is_empty() || input.uidvalidity != input.stored_generation;

    let (new_uids, retry_order, retry_set): (Vec<String>, &[String], HashSet<&str>) =
        if generation_changed {
            (input.unseen.to_vec(), &[], HashSet::new())
        } else {
            let uids = input
                .unseen
                .iter()
                .filter(|u| !input.seen.contains(*u))
                .cloned()
                .collect();
            let set = input.retry_order.iter().map(|s| s.as_str()).collect();
            (uids, input.retry_order, set)
        };

    let window = cap.saturating_mul(4).max(cap.saturating_add(10));
    let new_candidates: Vec<String> = new_uids.into_iter().take(window).collect();
    let retry_window = retry_scan_window(retry_order, input.retry_pos, window);
    let retry_candidates: Vec<String> = retry_window
        .iter()
        .filter(|u| input.seen.contains(*u))
        .cloned()
        .collect();

    let (new_budget, retry_budget, turn_write) =
        if cap == 1 && !new_candidates.is_empty() && !retry_candidates.is_empty() {
            if input.turn == 0 {
                (1, 0, Some(1))
            } else {
                (0, 1, Some(0))
            }
        } else {
            let rb = if !retry_candidates.is_empty() {
                1.max(cap / 4)
            } else {
                0
            };
            (cap.saturating_sub(rb), rb, None)
        };

    let mut rows = Vec::new();
    let mut new_emitted: usize = 0;
    let mut retry_emitted: usize = 0;
    let mut retry_examined: usize = 0;
    let mut retry_idx: i64 = -1;
    let mut first_retry_emitted_index: i64 = -1;

    for u in new_candidates.iter().chain(retry_candidates.iter()) {
        let is_retry = retry_set.contains(u.as_str()) && input.seen.contains(u.as_str());
        if is_retry {
            retry_idx += 1;
            if retry_emitted >= retry_budget {
                continue;
            }
            retry_examined += 1;
        } else if new_emitted >= new_budget {
            continue;
        }

        match fetch(u.as_str()) {
            None => {
                if is_retry {
                    continue;
                }
                rows.push(PollRow {
                    uid: clean(u),
                    date: String::new(),
                    from: "(no header)".to_string(),
                    subject: "unfetchable header - see fm-mail read".to_string(),
                    status: RowStatus::Degraded,
                });
                new_emitted += 1;
            }
            Some(header) => {
                let status = if is_retry {
                    RowStatus::Retry
                } else {
                    RowStatus::Ok
                };
                rows.push(PollRow {
                    uid: clean(u),
                    date: clean(&header.date),
                    from: clean(&header.from),
                    subject: clean(&header.subject),
                    status,
                });
                if is_retry {
                    retry_emitted += 1;
                    if first_retry_emitted_index == -1 {
                        first_retry_emitted_index = retry_idx;
                    }
                } else {
                    new_emitted += 1;
                }
            }
        }
    }

    let order_len = retry_order.len();
    let retry_pos_write = if retry_budget > 0 && !retry_candidates.is_empty() {
        if first_retry_emitted_index > 0 {
            Some(next_retry_pos(
                order_len,
                first_retry_emitted_index as usize,
                input.retry_pos,
            ))
        } else if first_retry_emitted_index < 0 {
            Some(next_retry_pos(
                order_len,
                retry_examined.max(1),
                input.retry_pos,
            ))
        } else {
            None
        }
    } else if !retry_window.is_empty() && retry_candidates.is_empty() {
        Some(next_retry_pos(
            order_len,
            retry_window.len(),
            input.retry_pos,
        ))
    } else {
        None
    };

    PollPlan {
        generation_changed,
        rows,
        retry_pos_write,
        turn_write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_strip_ws_matches_python_isspace() {
        for c in ['\u{1c}', '\u{1f}', '\u{85}', '\u{a0}', '\u{3000}'] {
            assert!(is_strip_ws(c), "{c:?} is Python whitespace");
        }
        for c in ['\u{200b}', '\u{180e}', 'x'] {
            assert!(!is_strip_ws(c), "{c:?} is not Python whitespace");
        }
    }

    #[test]
    fn test_parse_cursor_basic() {
        let (generation, seen) = parse_cursor("uidvalidity=7\n1\n2\r\n\n 3 \n");
        assert_eq!(generation, "7");
        let expected: HashSet<String> = ["1", "2", "3"].into_iter().map(String::from).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn test_parse_cursor_last_uidvalidity_wins() {
        let (generation, seen) = parse_cursor("uidvalidity=1\n1\nuidvalidity=2\n2\n");
        assert_eq!(generation, "2");
        let expected: HashSet<String> = ["1", "2"].into_iter().map(String::from).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn test_parse_cursor_empty() {
        let (generation, seen) = parse_cursor("");
        assert_eq!(generation, "");
        assert!(seen.is_empty());
    }

    #[test]
    fn test_parse_retry() {
        let order = parse_retry("a\nb\na\n\n");
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_parse_retry_pos() {
        assert_eq!(parse_retry_pos(Some("7"), 5), 2);
        assert_eq!(parse_retry_pos(Some("-1"), 5), 4);
        assert_eq!(parse_retry_pos(Some("x"), 5), 0);
        assert_eq!(parse_retry_pos(Some(""), 5), 0);
        assert_eq!(parse_retry_pos(None, 5), 0);
        assert_eq!(parse_retry_pos(Some("7"), 0), 0);
    }

    #[test]
    fn test_retry_scan_window() {
        let order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            retry_scan_window(&order, 1, 2),
            vec!["b".to_string(), "c".to_string()]
        );
        assert_eq!(
            retry_scan_window(&order, 1, 5),
            vec!["b".to_string(), "c".to_string(), "a".to_string()]
        );
        assert_eq!(
            retry_scan_window(&order, 4, 2),
            vec!["b".to_string(), "c".to_string()]
        );
        let empty: Vec<String> = Vec::new();
        assert_eq!(retry_scan_window(&empty, 0, 5), Vec::<String>::new());
    }

    #[test]
    fn test_next_retry_pos() {
        assert_eq!(next_retry_pos(0, 3, 9), 0);
        assert_eq!(next_retry_pos(2, 1, 1), 0);
        assert_eq!(next_retry_pos(30, 11, 25), 6);
    }

    #[test]
    fn test_parse_turn() {
        assert_eq!(parse_turn(Some("3")), 1);
        assert_eq!(parse_turn(Some("-1")), 1);
        assert_eq!(parse_turn(None), 0);
        assert_eq!(parse_turn(Some("z")), 0);
    }

    #[test]
    fn test_clean() {
        assert_eq!(clean("a\t\tb\r\n c "), "a b  c");
        assert_eq!(clean("\u{001c}hello\u{001f}"), "hello");
    }

    #[test]
    fn test_parse_timeout_secs() {
        assert_eq!(parse_timeout_secs(None), 20.0);
        assert_eq!(parse_timeout_secs(Some("5")), 5.0);
        assert_eq!(parse_timeout_secs(Some("0.5")), 0.5);
        assert_eq!(parse_timeout_secs(Some("-1")), 20.0);
        assert_eq!(parse_timeout_secs(Some("abc")), 20.0);
        assert_eq!(parse_timeout_secs(Some("nan")), 20.0);
        assert_eq!(parse_timeout_secs(Some(" 7 ")), 7.0);
    }

    #[test]
    fn test_parse_cap() {
        assert_eq!(parse_cap(None), Ok(20));
        assert_eq!(parse_cap(Some("")), Ok(20));
        assert_eq!(parse_cap(Some("3")), Ok(3));
        assert_eq!(parse_cap(Some("0")), Ok(20));
        assert_eq!(parse_cap(Some("-4")), Ok(20));
        assert_eq!(
            parse_cap(Some("x")),
            Err(PollError::BadCap("x".to_string()))
        );
        assert_eq!(
            parse_cap(Some(" ")),
            Err(PollError::BadCap(" ".to_string()))
        );
    }

    #[test]
    fn test_to_line_and_row_status() {
        assert_eq!(RowStatus::Ok.as_str(), "ok");
        assert_eq!(RowStatus::Retry.as_str(), "retry");
        assert_eq!(RowStatus::Degraded.as_str(), "degraded");

        let row = PollRow {
            uid: "101".to_string(),
            date: "Mon, 1 Jan".to_string(),
            from: "alice@example.com".to_string(),
            subject: "hello".to_string(),
            status: RowStatus::Ok,
        };
        assert_eq!(
            row.to_line(),
            "101\tMon, 1 Jan\talice@example.com\thello\tok"
        );
    }

    #[test]
    fn test_scenario_s1() {
        let seen: HashSet<String> = ["a"].into_iter().map(String::from).collect();
        let retry_order = vec!["a".to_string()];
        let unseen = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let input = PollInput {
            cap: 2,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |_uid: &str| {
            Some(RawHeader {
                date: "d".to_string(),
                subject: "s".to_string(),
                from: "f".to_string(),
            })
        };
        let plan = plan_poll(&input, &mut fetch);
        assert!(!plan.generation_changed);
        assert_eq!(plan.retry_pos_write, None);
        assert_eq!(plan.turn_write, None);
        assert_eq!(
            plan.rows,
            vec![
                PollRow {
                    uid: "b".to_string(),
                    date: "d".to_string(),
                    from: "f".to_string(),
                    subject: "s".to_string(),
                    status: RowStatus::Ok,
                },
                PollRow {
                    uid: "a".to_string(),
                    date: "d".to_string(),
                    from: "f".to_string(),
                    subject: "s".to_string(),
                    status: RowStatus::Retry,
                },
            ]
        );
    }

    #[test]
    fn test_scenario_s2() {
        let seen: HashSet<String> = ["a"].into_iter().map(String::from).collect();
        let retry_order = vec!["a".to_string()];
        let unseen = vec!["a".to_string(), "b".to_string()];
        let input = PollInput {
            cap: 2,
            uidvalidity: "5",
            stored_generation: "4",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |_uid: &str| {
            Some(RawHeader {
                date: "d".to_string(),
                subject: "s".to_string(),
                from: "f".to_string(),
            })
        };
        let plan = plan_poll(&input, &mut fetch);
        assert!(plan.generation_changed);
        assert_eq!(plan.retry_pos_write, None);
        assert_eq!(plan.turn_write, None);
        assert_eq!(
            plan.rows,
            vec![
                PollRow {
                    uid: "a".to_string(),
                    date: "d".to_string(),
                    from: "f".to_string(),
                    subject: "s".to_string(),
                    status: RowStatus::Ok,
                },
                PollRow {
                    uid: "b".to_string(),
                    date: "d".to_string(),
                    from: "f".to_string(),
                    subject: "s".to_string(),
                    status: RowStatus::Ok,
                },
            ]
        );
    }

    #[test]
    fn test_scenario_s3() {
        let seen: HashSet<String> = ["r1", "r2"].into_iter().map(String::from).collect();
        let retry_order = vec!["r1".to_string(), "r2".to_string()];
        let unseen = vec![];
        let input = PollInput {
            cap: 4,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |uid: &str| {
            if uid == "r1" {
                None
            } else {
                Some(RawHeader {
                    date: "d".to_string(),
                    subject: "s".to_string(),
                    from: "f".to_string(),
                })
            }
        };
        let plan = plan_poll(&input, &mut fetch);
        assert!(!plan.generation_changed);
        assert_eq!(plan.retry_pos_write, Some(1));
        assert_eq!(plan.turn_write, None);
        assert_eq!(
            plan.rows,
            vec![PollRow {
                uid: "r2".to_string(),
                date: "d".to_string(),
                from: "f".to_string(),
                subject: "s".to_string(),
                status: RowStatus::Retry,
            }]
        );
    }

    #[test]
    fn test_scenario_s4() {
        let seen: HashSet<String> = ["r1", "r2"].into_iter().map(String::from).collect();
        let retry_order = vec!["r1".to_string(), "r2".to_string()];
        let unseen = vec![];
        let input = PollInput {
            cap: 4,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |_uid: &str| None;
        let plan = plan_poll(&input, &mut fetch);
        assert!(!plan.generation_changed);
        assert_eq!(plan.retry_pos_write, Some(0));
        assert_eq!(plan.turn_write, None);
        assert!(plan.rows.is_empty());
    }

    #[test]
    fn test_scenario_s5() {
        let seen: HashSet<String> = ["r1"].into_iter().map(String::from).collect();
        let retry_order = vec!["r1".to_string()];
        let unseen = vec!["n1".to_string()];

        // Turn 0
        let input0 = PollInput {
            cap: 1,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |_uid: &str| {
            Some(RawHeader {
                date: "d".to_string(),
                subject: "s".to_string(),
                from: "f".to_string(),
            })
        };
        let plan0 = plan_poll(&input0, &mut fetch);
        assert_eq!(plan0.retry_pos_write, None);
        assert_eq!(plan0.turn_write, Some(1));
        assert_eq!(
            plan0.rows,
            vec![PollRow {
                uid: "n1".to_string(),
                date: "d".to_string(),
                from: "f".to_string(),
                subject: "s".to_string(),
                status: RowStatus::Ok,
            }]
        );

        // Turn 1
        let input1 = PollInput {
            cap: 1,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 1,
        };
        let mut fetch = |_uid: &str| {
            Some(RawHeader {
                date: "d".to_string(),
                subject: "s".to_string(),
                from: "f".to_string(),
            })
        };
        let plan1 = plan_poll(&input1, &mut fetch);
        assert_eq!(plan1.retry_pos_write, None);
        assert_eq!(plan1.turn_write, Some(0));
        assert_eq!(
            plan1.rows,
            vec![PollRow {
                uid: "r1".to_string(),
                date: "d".to_string(),
                from: "f".to_string(),
                subject: "s".to_string(),
                status: RowStatus::Retry,
            }]
        );
    }

    #[test]
    fn test_scenario_s6() {
        let seen: HashSet<String> = HashSet::new();
        let retry_order: Vec<String> = (0..30).map(|i| format!("u{i}")).collect();
        let unseen = vec![];
        let input = PollInput {
            cap: 1,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 25,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |_uid: &str| {
            Some(RawHeader {
                date: "d".to_string(),
                subject: "s".to_string(),
                from: "f".to_string(),
            })
        };
        let plan = plan_poll(&input, &mut fetch);
        assert!(plan.rows.is_empty());
        assert_eq!(plan.retry_pos_write, Some(6));
        assert_eq!(plan.turn_write, None);
    }

    #[test]
    fn test_scenario_s7() {
        let seen: HashSet<String> = HashSet::new();
        let retry_order = vec![];
        let unseen = vec!["n1".to_string()];
        let input = PollInput {
            cap: 3,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |_uid: &str| None;
        let plan = plan_poll(&input, &mut fetch);
        assert_eq!(plan.rows.len(), 1);
        let row = &plan.rows[0];
        assert_eq!(row.uid, "n1");
        assert_eq!(row.date, "");
        assert_eq!(row.from, "(no header)");
        assert_eq!(row.subject, "unfetchable header - see fm-mail read");
        assert_eq!(row.status, RowStatus::Degraded);
        assert_eq!(
            row.to_line(),
            "n1\t\t(no header)\tunfetchable header - see fm-mail read\tdegraded"
        );
    }

    #[test]
    fn test_scenario_s8() {
        let seen: HashSet<String> = HashSet::new();
        let retry_order = vec![];
        let unseen = vec!["n1".to_string()];
        let input = PollInput {
            cap: 1,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |_uid: &str| {
            Some(RawHeader {
                date: "d".to_string(),
                subject: "hello\t\nworld".to_string(),
                from: "f".to_string(),
            })
        };
        let plan = plan_poll(&input, &mut fetch);
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(plan.rows[0].subject, "hello world");
    }

    #[test]
    fn test_scenario_s9() {
        let seen: HashSet<String> = HashSet::new();
        let retry_order = vec![];
        let unseen: Vec<String> = (0..21).map(|i| format!("u{i}")).collect();
        let input = PollInput {
            cap: 0,
            uidvalidity: "7",
            stored_generation: "7",
            seen: &seen,
            retry_order: &retry_order,
            retry_pos: 0,
            unseen: &unseen,
            turn: 0,
        };
        let mut fetch = |uid: &str| {
            Some(RawHeader {
                date: "d".to_string(),
                subject: format!("s_{uid}"),
                from: "f".to_string(),
            })
        };
        let plan = plan_poll(&input, &mut fetch);
        assert_eq!(plan.rows.len(), 20);
    }
}
