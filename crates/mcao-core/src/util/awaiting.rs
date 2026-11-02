//! The two awaiting-summary parsers that `bin/fm-awaiting-captain.sh` runs as inline Python.
//!
//! `hold_ids` is the reference's rows program: it reads `tasks-axi list` output and prints the
//! id of each owner hold (`hold_kind` value `captain`) that is not done. `hold_object` is the
//! show program: it reads one `tasks-axi show` record and prints one JSON object describing an
//! open owner hold.
//!
//! Each function returns the bytes, stderr and exit status the reference process produces.
//! A Python crash is reduced to its final traceback line, which is the part a reader acts on.
//! The saved fixtures keep the full traceback, and their tests compare that last line.

use super::base64;
use super::json;
use super::pystr::{self, LF, Text};

const QUOTE: u32 = 0x22;
const BACKSLASH: u32 = 0x5c;
const COMMA: u32 = 0x2c;
const SPACE: u32 = 0x20;
const TAB: u32 = 0x09;
const HOLD_PREFIX: &str = "fm-hold-v1:";
const SUMMARY_MAX: usize = 160;
const NOT_RECOGNISED: &str = "fm-awaiting-captain: backlog list output was not recognised";

/// What the reference process would have written and returned.
#[derive(Debug, PartialEq, Eq)]
pub struct Run {
    /// Bytes written to stdout, including any written before a crash.
    pub stdout: Vec<u8>,
    /// Empty on success. After a crash, the reference's final traceback line with its newline.
    pub stderr: String,
    /// 0 on success, 1 after a crash, 4 when the rows input is not recognised.
    pub exit: u8,
}

impl Run {
    fn ok(stdout: Vec<u8>) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit: 0,
        }
    }

    fn crash(stdout: Vec<u8>, line: &str) -> Self {
        Self {
            stdout,
            stderr: format!("{line}\n"),
            exit: 1,
        }
    }
}

/// The reference's rows program: prints the id of every owner hold that is not done.
pub fn hold_ids(input: &[u8]) -> Run {
    let text = match decode_input(input) {
        Ok(text) => text,
        Err(line) => return Run::crash(Vec::new(), &line),
    };
    let mut header: Option<Vec<Text>> = None;
    let mut rows: Vec<Vec<Text>> = Vec::new();
    for line in pystr::split(&text, LF) {
        if header.is_none() {
            header = parse_header(line);
            continue;
        }
        if !pystr::starts_with_ascii(line, "  ") {
            break;
        }
        rows.push(split_row(&line[2..]));
    }
    let Some(header) = header else {
        let counted_zero = pystr::split(&text, LF).any(|line| pystr::eq_ascii(line, "count: 0"));
        return if counted_zero {
            Run::ok(Vec::new())
        } else {
            Run {
                stdout: Vec::new(),
                stderr: format!("{NOT_RECOGNISED}\n"),
                exit: 4,
            }
        };
    };

    let mut stdout = Vec::new();
    for values in &rows {
        let is_hold =
            cell(&header, values, "hold_kind").is_some_and(|v| pystr::eq_ascii(v, "captain"));
        let is_done = cell(&header, values, "state").is_some_and(|v| pystr::eq_ascii(v, "done"));
        if !is_hold || is_done {
            continue;
        }
        let id = cell(&header, values, "id").unwrap_or(&[]);
        match print_line(id) {
            Ok(bytes) => stdout.extend(bytes),
            Err(line) => return Run::crash(stdout, &line),
        }
    }
    Run::ok(stdout)
}

/// The reference's show program: prints one JSON object for an open owner hold, or nothing.
pub fn hold_object(input: &[u8]) -> Run {
    let text = match decode_input(input) {
        Ok(text) => text,
        Err(line) => return Run::crash(Vec::new(), &line),
    };
    let fields: Vec<(&[u32], &[u32])> = pystr::split(&text, LF).filter_map(field_line).collect();
    let raw_field = |name: &str| {
        fields
            .iter()
            .find(|(field, _)| pystr::eq_ascii(field, name))
            .map(|(_, raw)| *raw)
    };
    let value = |name: &str| -> Option<Text> {
        let raw = raw_field(name)?;
        let decoded = if raw.first() == Some(&QUOTE) {
            json::decode_string(raw)
        } else {
            None
        };
        Some(decoded.unwrap_or_else(|| raw.to_vec()))
    };

    if !is(value("hold_kind").as_deref(), "captain") || is(value("state").as_deref(), "done") {
        return Run::ok(Vec::new());
    }
    let body = value("body").unwrap_or_default();
    let class = hold_class(&body);
    if is(class.as_deref(), "dependency-wait") || is(class.as_deref(), "answered-bookkeeping") {
        return Run::ok(Vec::new());
    }

    let mut reason = clean(value("hold_reason")).unwrap_or_default();
    if pystr::starts_with_ascii(&reason, HOLD_PREFIX) {
        reason = base64::decode(&reason[HOLD_PREFIX.len()..])
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map_or_else(Text::new, |decoded| pystr::from_str(&decoded));
    }

    let title = value("title").unwrap_or_default();
    let source = if reason.is_empty() { &title } else { &reason };
    let mut summary = join_spaced(&pystr::split_whitespace(source));
    if summary.is_empty() {
        summary = value("id").unwrap_or_default();
    }
    if summary.len() > SUMMARY_MAX {
        summary = pystr::rstrip(&summary[..SUMMARY_MAX - 3]).to_vec();
        summary.extend(pystr::from_str("..."));
    }

    let body_first = pystr::split(&body, LF).next().unwrap_or(&[]);
    let hold_set = match hold_set_date(body_first) {
        Some(date) => date.to_vec(),
        None => non_empty(value("created")).unwrap_or_else(|| pystr::from_str("unknown")),
    };
    let mut raised_at = hold_set.clone();
    if !raised_at.contains(&u32::from(b'T')) {
        raised_at.extend(pystr::from_str("T00:00:00Z"));
    }

    let id = value("id");
    let hold_until = clean(value("hold_until"));
    let line = format!(
        "{{\"id\": {}, \"summary\": {}, \"hold_set\": {}, \"raised_at\": {}, \"hold_until\": {}}}\n",
        json_or_null(id.as_deref()),
        json::encode_ascii(&summary),
        json::encode_ascii(&hold_set),
        json::encode_ascii(&raised_at),
        json_or_null(hold_until.as_deref()),
    );
    Run::ok(line.into_bytes())
}

/// Python's strict UTF-8 read of stdin. On failure, the decoder's final traceback line.
fn decode_input(input: &[u8]) -> Result<Text, String> {
    std::str::from_utf8(input)
        .map(pystr::from_str)
        .map_err(|err| decode_error_line(input, &err))
}

fn decode_error_line(input: &[u8], err: &std::str::Utf8Error) -> String {
    let start = err.valid_up_to();
    let lead = input[start];
    let (end, reason) = match err.error_len() {
        Some(len) => {
            let reason = if matches!(lead, 0x80..=0xc1 | 0xf5..=0xff) {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            (start + len, reason)
        }
        None => (input.len(), "unexpected end of data"),
    };
    let span = if end - start == 1 {
        format!("byte 0x{lead:02x} in position {start}")
    } else {
        format!("bytes in position {start}-{}", end - 1)
    };
    format!("UnicodeDecodeError: 'utf-8' codec can't decode {span}: {reason}")
}

/// Python's `print(text)` writing strict UTF-8: the bytes and a newline, or the encoder's
/// final line when `text` holds a surrogate that JSON produced.
fn print_line(text: &[u32]) -> Result<Vec<u8>, String> {
    if let Some(start) = text.iter().position(|&c| is_surrogate(c)) {
        let run = text[start..]
            .iter()
            .take_while(|&&c| is_surrogate(c))
            .count();
        let span = if run == 1 {
            format!("character '\\u{:04x}' in position {start}", text[start])
        } else {
            format!("characters in position {start}-{}", start + run - 1)
        };
        return Err(format!(
            "UnicodeEncodeError: 'utf-8' codec can't encode {span}: surrogates not allowed"
        ));
    }
    let mut bytes: Vec<u8> = text
        .iter()
        .filter_map(|&c| char::from_u32(c))
        .collect::<String>()
        .into_bytes();
    bytes.push(b'\n');
    Ok(bytes)
}

fn is_surrogate(c: u32) -> bool {
    (0xd800..=0xdfff).contains(&c)
}

fn is_ascii_digit(c: u32) -> bool {
    (0x30..=0x39).contains(&c)
}

fn is_name_char(c: u32) -> bool {
    matches!(c, 0x61..=0x7a | 0x5f)
}

/// The header of `tasks[N]{a,b,c}:`, as its column names, or `None` for any other line.
fn parse_header(line: &[u32]) -> Option<Vec<Text>> {
    let rest = pystr::strip_ascii(line, "tasks[")?;
    let digits = rest.iter().take_while(|&&c| is_ascii_digit(c)).count();
    if digits == 0 {
        return None;
    }
    let rest = pystr::strip_ascii(&rest[digits..], "]{")?;
    let close = rest.iter().position(|&c| c == u32::from(b'}'))?;
    if !pystr::eq_ascii(&rest[close + 1..], ":") {
        return None;
    }
    Some(
        pystr::split(&rest[..close], COMMA)
            .map(<[u32]>::to_vec)
            .collect(),
    )
}

/// Splits one data row the way the reference's `split_row` does, quirks included. An unquoted
/// field runs to the next comma. A quoted field is decoded as JSON, or kept as its raw inner
/// text when that fails. After either, one character is skipped without checking it.
fn split_row(s: &[u32]) -> Vec<Text> {
    let mut out = Vec::new();
    let n = s.len();
    let mut i = 0;
    loop {
        if i < n && s[i] == QUOTE {
            let mut j = i + 1;
            while j < n && s[j] != QUOTE {
                j += if s[j] == BACKSLASH { 2 } else { 1 };
            }
            let token = &s[i..(j + 1).min(n)];
            out.push(json::decode_string(token).unwrap_or_else(|| inner(token).to_vec()));
            i = j + 1;
        } else {
            let j = s[i..]
                .iter()
                .position(|&c| c == COMMA)
                .map_or(n, |offset| i + offset);
            out.push(s[i..j].to_vec());
            i = j;
        }
        if i >= n {
            return out;
        }
        i += 1;
    }
}

/// Python's `token[1:-1]`.
fn inner(token: &[u32]) -> &[u32] {
    if token.len() >= 2 {
        &token[1..token.len() - 1]
    } else {
        &[]
    }
}

/// `dict(zip(header, values)).get(key)`: the last matching column wins, and columns beyond the
/// shorter of the two lists are absent.
fn cell<'a>(header: &[Text], values: &'a [Text], key: &str) -> Option<&'a [u32]> {
    header
        .iter()
        .zip(values)
        .rev()
        .find(|(name, _)| pystr::eq_ascii(name.as_slice(), key))
        .map(|(_, value)| value.as_slice())
}

/// `^  ([a-z_]+): (.*)$`: one `tasks-axi show` field line, as (name, raw value).
fn field_line(line: &[u32]) -> Option<(&[u32], &[u32])> {
    let rest = pystr::strip_ascii(line, "  ")?;
    let name_len = rest.iter().take_while(|&&c| is_name_char(c)).count();
    let raw = pystr::strip_ascii(&rest[name_len..], ": ")?;
    (name_len > 0).then_some((&rest[..name_len], raw))
}

/// The first `Captain hold class:` value in the header run at the top of a body. The run ends
/// at the first non-blank line that is not a `Captain hold set|origin|class:` line.
fn hold_class(body: &[u32]) -> Option<Text> {
    let mut class = None;
    for line in pystr::split(body, LF) {
        if pystr::is_blank(line) {
            continue;
        }
        if !is_hold_header(line) {
            break;
        }
        if class.is_none() {
            class = class_value(line).map(<[u32]>::to_vec);
        }
    }
    class
}

fn is_hold_header(line: &[u32]) -> bool {
    [
        "Captain hold set:",
        "Captain hold origin:",
        "Captain hold class:",
    ]
    .iter()
    .any(|prefix| pystr::starts_with_ascii(line, prefix))
}

/// `^Captain hold class:[ \t]*([^ \t]+)$`
fn class_value(line: &[u32]) -> Option<&[u32]> {
    let rest = pystr::strip_ascii(line, "Captain hold class:")?;
    let skipped = rest.iter().take_while(|&&c| c == SPACE || c == TAB).count();
    let value = &rest[skipped..];
    (!value.is_empty() && !value.iter().any(|&c| c == SPACE || c == TAB)).then_some(value)
}

/// `^Captain hold set: (DATE(T TIME Z)?)$`, where a digit is `d`.
fn hold_set_date(line: &[u32]) -> Option<&[u32]> {
    let date = pystr::strip_ascii(line, "Captain hold set: ")?;
    (matches_shape(date, "dddd-dd-dd") || matches_shape(date, "dddd-dd-ddTdd:dd:ddZ"))
        .then_some(date)
}

fn matches_shape(text: &[u32], shape: &str) -> bool {
    text.len() == shape.len()
        && text.iter().zip(shape.bytes()).all(|(&c, p)| {
            if p == b'd' {
                is_ascii_digit(c)
            } else {
                c == u32::from(p)
            }
        })
}

/// The reference's `clean`: absent, empty, `-` and `none` all mean no value.
fn clean(value: Option<Text>) -> Option<Text> {
    value.filter(|v| !(v.is_empty() || pystr::eq_ascii(v, "-") || pystr::eq_ascii(v, "none")))
}

fn non_empty(value: Option<Text>) -> Option<Text> {
    value.filter(|v| !v.is_empty())
}

/// Python's `value == expected` for an optional decoded value, where `None` never matches.
fn is(value: Option<&[u32]>, expected: &str) -> bool {
    value.is_some_and(|v| pystr::eq_ascii(v, expected))
}

fn join_spaced(words: &[&[u32]]) -> Text {
    let mut out = Text::new();
    for (index, word) in words.iter().enumerate() {
        if index > 0 {
            out.push(SPACE);
        }
        out.extend_from_slice(word);
    }
    out
}

fn json_or_null(value: Option<&[u32]>) -> String {
    value.map_or_else(|| "null".to_owned(), json::encode_ascii)
}
