//! JSON reader and writer that reproduce the behavior of Python's `json` module for the
//! assignment-profile record: the same acceptance, the same refusal messages, and the same
//! canonical bytes from `json.dumps(indent=2, sort_keys=True, ensure_ascii=False)`.

use std::fmt::Write as _;

/// A parsed JSON value. Integers keep their decimal text, so no digits are lost, and object
/// members keep document order, because the schema walk reports the first error it meets.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Json {
    Null,
    Bool(bool),
    Int(String),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(super) fn get(&self, key: &str) -> Option<&Json> {
        self.as_object()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(super) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub(super) fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(members) => Some(members),
            _ => None,
        }
    }

    /// Python truthiness, used where the schema walk tests a keyword's value.
    pub(super) fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Int(s) => s != "0",
            Json::Float(f) => *f != 0.0,
            Json::Str(s) => !s.is_empty(),
            Json::Array(items) => !items.is_empty(),
            Json::Object(members) => !members.is_empty(),
        }
    }
}

/// A failure while reading JSON text. `Syntax` carries the message Python's decoder puts in
/// `JSONDecodeError.msg`; `Refuse` carries a refusal raised by the strict hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ParseError {
    Syntax(&'static str),
    Refuse(String),
}

/// Parses `text` the way `json.loads` with the strict hooks does.
pub(super) fn parse(text: &str) -> Result<Json, ParseError> {
    if text.starts_with('\u{feff}') {
        return Err(ParseError::Syntax(
            "Unexpected UTF-8 BOM (decode using utf-8-sig)",
        ));
    }
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(ParseError::Syntax("Extra data"));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn starts_with(&self, literal: &str) -> bool {
        self.bytes[self.pos..].starts_with(literal.as_bytes())
    }

    fn value(&mut self) -> Result<Json, ParseError> {
        match self.peek() {
            Some(b'"') => self.string().map(Json::Str),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'n') if self.starts_with("null") => {
                self.pos += 4;
                Ok(Json::Null)
            }
            Some(b't') if self.starts_with("true") => {
                self.pos += 4;
                Ok(Json::Bool(true))
            }
            Some(b'f') if self.starts_with("false") => {
                self.pos += 5;
                Ok(Json::Bool(false))
            }
            Some(b'N') if self.starts_with("NaN") => Err(constant("NaN")),
            Some(b'I') if self.starts_with("Infinity") => Err(constant("Infinity")),
            Some(b'-') if self.starts_with("-Infinity") => Err(constant("-Infinity")),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(ParseError::Syntax("Expecting value")),
        }
    }

    fn number(&mut self) -> Result<Json, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(ParseError::Syntax("Expecting value")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') && self.digit_at(self.pos + 1) {
            self.pos += 1;
            self.digits();
            is_float = true;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mut look = self.pos + 1;
            if matches!(self.bytes.get(look), Some(b'+' | b'-')) {
                look += 1;
            }
            if self.digit_at(look) {
                self.pos = look;
                self.digits();
                is_float = true;
            }
        }
        let literal = &self.text[start..self.pos];
        if is_float {
            // Overflowing literals parse to infinity, as Python's float() does.
            literal
                .parse::<f64>()
                .map(Json::Float)
                .map_err(|_| ParseError::Syntax("Expecting value"))
        } else if literal == "-0" {
            Ok(Json::Int("0".to_string()))
        } else {
            Ok(Json::Int(literal.to_string()))
        }
    }

    fn digit_at(&self, index: usize) -> bool {
        matches!(self.bytes.get(index), Some(b'0'..=b'9'))
    }

    fn digits(&mut self) {
        while self.digit_at(self.pos) {
            self.pos += 1;
        }
    }

    fn object(&mut self) -> Result<Json, ParseError> {
        self.pos += 1;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(Vec::new()));
        }
        let mut members: Vec<(String, Json)> = Vec::new();
        loop {
            if self.peek() != Some(b'"') {
                return Err(ParseError::Syntax(
                    "Expecting property name enclosed in double quotes",
                ));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(ParseError::Syntax("Expecting ':' delimiter"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value()?;
            members.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                }
                _ => return Err(ParseError::Syntax("Expecting ',' delimiter")),
            }
        }
        // Python raises for a duplicate only once the whole object has been read.
        for (index, (key, _)) in members.iter().enumerate() {
            if members[..index].iter().any(|(earlier, _)| earlier == key) {
                return Err(ParseError::Refuse(format!(
                    "duplicate key '{key}' in the record"
                )));
            }
        }
        Ok(Json::Object(members))
    }

    fn array(&mut self) -> Result<Json, ParseError> {
        self.pos += 1;
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(Vec::new()));
        }
        let mut items = Vec::new();
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(b']') {
                        return Err(ParseError::Syntax(
                            "Illegal trailing comma before end of array",
                        ));
                    }
                }
                _ => return Err(ParseError::Syntax("Expecting ',' delimiter")),
            }
        }
    }

    fn string(&mut self) -> Result<String, ParseError> {
        let unterminated = ParseError::Syntax("Unterminated string starting at");
        self.pos += 1;
        let mut out = String::new();
        let mut chunk_start = self.pos;
        loop {
            let Some(byte) = self.peek() else {
                return Err(unterminated);
            };
            match byte {
                b'"' => {
                    out.push_str(&self.text[chunk_start..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    out.push_str(&self.text[chunk_start..self.pos]);
                    self.pos += 1;
                    let Some(escape) = self.peek() else {
                        return Err(unterminated);
                    };
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(ParseError::Syntax("Invalid \\escape")),
                    }
                    chunk_start = self.pos;
                }
                0..=0x1f => return Err(ParseError::Syntax("Invalid control character at")),
                _ => self.pos += 1,
            }
        }
    }

    /// Reads the four hex digits after `\u`, and the low half of a surrogate pair when present.
    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let unit = self
            .hex4()
            .ok_or(ParseError::Syntax("Invalid \\uXXXX escape"))?;
        let code = match unit {
            0xd800..=0xdbff => {
                let low = if self.bytes[self.pos..].starts_with(b"\\u") {
                    self.pos += 2;
                    self.hex4()
                        .ok_or(ParseError::Syntax("Invalid \\uXXXX escape"))?
                } else {
                    0
                };
                if !(0xdc00..=0xdfff).contains(&low) {
                    return Err(lone_surrogate());
                }
                0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00)
            }
            0xdc00..=0xdfff => return Err(lone_surrogate()),
            _ => u32::from(unit),
        };
        char::from_u32(code).ok_or_else(lone_surrogate)
    }

    fn hex4(&mut self) -> Option<u16> {
        let digits = self.bytes.get(self.pos..self.pos + 4)?;
        let text = std::str::from_utf8(digits).ok()?;
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.pos += 4;
        u16::from_str_radix(text, 16).ok()
    }
}

fn constant(name: &str) -> ParseError {
    ParseError::Refuse(format!("non-standard JSON constant '{name}' in the record"))
}

/// Python keeps lone surrogates; a Rust string cannot hold one, so this core refuses them.
fn lone_surrogate() -> ParseError {
    ParseError::Refuse(
        "the record contains a lone surrogate escape, which this core cannot represent".to_string(),
    )
}

/// Output style. `indent: None` gives Python's compact separators.
#[derive(Debug, Clone, Copy)]
pub(super) struct Style {
    pub(super) indent: Option<usize>,
    pub(super) ascii: bool,
}

/// The canonical record style: `json.dumps(indent=2, sort_keys=True, ensure_ascii=False)`.
pub(super) const CANONICAL: Style = Style {
    indent: Some(2),
    ascii: false,
};

/// Python's `json.dumps(value, sort_keys=True)` defaults, used for messages and comparisons.
pub(super) const COMPACT: Style = Style {
    indent: None,
    ascii: true,
};

pub(super) fn dump(value: &Json, style: Style) -> String {
    let mut out = String::new();
    write_value(&mut out, value, style, 0);
    out
}

fn write_value(out: &mut String, value: &Json, style: Style, depth: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(digits) => out.push_str(digits),
        Json::Float(f) => out.push_str(&py_float(*f)),
        Json::Str(s) => write_str(out, s, style.ascii),
        Json::Array(items) => {
            let parts: Vec<&Json> = items.iter().collect();
            write_container(out, ('[', ']'), &parts, style, depth, |out, item, depth| {
                write_value(out, item, style, depth)
            });
        }
        Json::Object(members) => {
            let mut sorted: Vec<&(String, Json)> = members.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            write_container(
                out,
                ('{', '}'),
                &sorted,
                style,
                depth,
                |out, member, depth| {
                    write_str(out, &member.0, style.ascii);
                    out.push_str(": ");
                    write_value(out, &member.1, style, depth);
                },
            );
        }
    }
}

fn write_container<T>(
    out: &mut String,
    (open, close): (char, char),
    items: &[T],
    style: Style,
    depth: usize,
    mut write_item: impl FnMut(&mut String, &T, usize),
) {
    out.push(open);
    if items.is_empty() {
        out.push(close);
        return;
    }
    match style.indent {
        Some(width) => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                push_spaces(out, width * (depth + 1));
                write_item(out, item, depth + 1);
            }
            out.push('\n');
            push_spaces(out, width * depth);
        }
        None => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_item(out, item, depth + 1);
            }
        }
    }
    out.push(close);
}

fn push_spaces(out: &mut String, columns: usize) {
    out.extend(std::iter::repeat_n(' ', columns));
}

fn write_str(out: &mut String, s: &str, ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ if ascii => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            _ if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `repr(float)`: shortest round-trip digits, fixed notation for exponents in [-4, 16).
pub(super) fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let scientific = format!("{f:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if mantissa.starts_with('-') { "-" } else { "" };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    if (-4..16).contains(&exponent) {
        let body = if exponent >= 0 {
            let int_len = exponent as usize + 1;
            let padded = format!("{digits:0<int_len$}");
            let (int_part, frac_part) = padded.split_at(int_len);
            if frac_part.is_empty() {
                format!("{int_part}.0")
            } else {
                format!("{int_part}.{frac_part}")
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        };
        format!("{sign}{body}")
    } else {
        let (first, rest) = digits.split_at(1);
        let mantissa = if rest.is_empty() {
            first.to_string()
        } else {
            format!("{first}.{rest}")
        };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{exp_sign}{:02}", exponent.abs())
    }
}
