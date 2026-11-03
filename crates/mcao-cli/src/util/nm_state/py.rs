//! CPython 3 behavior the no-mistakes readers reproduce: `str` predicates and splitting,
//! `float.__repr__`, `json.dumps` and `json.loads`, `int()` for decimal text, and
//! `time.gmtime` with `strftime("%Y-%m-%dT%H:%M:%SZ")`.

use std::cmp::Ordering;

/// `str.isspace`: Unicode White_Space plus the information separators U+001C to U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.splitlines()`: breaks on `\n`, `\r`, `\r\n`, `\v`, `\f`, U+001C to U+001E, U+0085,
/// U+2028 and U+2029, and drops a final empty piece.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let break_len = match c {
            '\r' => match chars.peek() {
                Some(&(_, '\n')) => {
                    chars.next();
                    2
                }
                _ => 1,
            },
            '\n' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => c.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..index]);
        start = index + break_len;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `json.dumps(text)` with the given `ensure_ascii`. Control characters use the short escapes
/// `\b \f \n \r \t` where json defines them and lowercase `\u00xx` otherwise; with
/// `ensure_ascii`, every code point from U+007F up is escaped, astral ones as surrogate pairs.
pub fn json_string(text: &str, ensure_ascii: bool) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => push_unicode_escape(&mut out, c as u32),
            c if ensure_ascii && (c as u32) >= 0x7f => {
                let code = c as u32;
                if code > 0xffff {
                    let offset = code - 0x1_0000;
                    push_unicode_escape(&mut out, 0xd800 + (offset >> 10));
                    push_unicode_escape(&mut out, 0xdc00 + (offset & 0x3ff));
                } else {
                    push_unicode_escape(&mut out, code);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn push_unicode_escape(out: &mut String, unit: u32) {
    out.push_str(&format!("\\u{unit:04x}"));
}

/// `float.__repr__` as `json.dumps` writes a float: the shortest round-trip digits, fixed
/// notation when the decimal exponent is in (-4, 16], otherwise `d.ddde+XX`.
pub fn float_repr(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("`{:e}` of a finite nonzero float has an exponent");
    let exponent: i32 = exponent
        .parse()
        .expect("`{:e}` exponent is a decimal integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let point = exponent + 1;
    if -4 < point && point <= 16 {
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            let (whole, fraction) = digits.split_at(point as usize);
            format!("{whole}.{fraction}")
        };
        format!("{sign}{body}")
    } else {
        let (first, rest) = digits.split_at(1);
        let mantissa = if rest.is_empty() {
            first.to_owned()
        } else {
            format!("{first}.{rest}")
        };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{exp_sign}{:02}", exponent.unsigned_abs())
    }
}

/// `json.dumps` of a float: `NaN`, `Infinity` and `-Infinity` for non-finite values.
pub fn json_float(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        float_repr(value)
    }
}

/// `int(text)` for the decimal forms the readers receive: optional surrounding whitespace, an
/// optional sign, and digits with single underscores between them. Non-ASCII decimal digits
/// are not accepted. A value outside `i64` is `None`, since every caller turns it into an error.
pub fn parse_int(text: &str) -> Option<i64> {
    let body = strip(text);
    let (negative, digits) = match body.as_bytes().first() {
        Some(b'-') => (true, &body[1..]),
        Some(b'+') => (false, &body[1..]),
        _ => (false, body),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut value: i128 = 0;
    for byte in digits.bytes() {
        match byte {
            b'_' => continue,
            b'0'..=b'9' => {
                value = value * 10 + i128::from(byte - b'0');
                if value > i128::from(i64::MAX) + 1 {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let value = if negative { -value } else { value };
    i64::try_from(value).ok()
}

/// `time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(seconds))`. Years below 10000 are zero-padded
/// to four characters, sign included, as macOS `strftime` prints them.
pub fn utc_text(seconds: i64) -> Result<String, GmtimeError> {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    if i32::try_from(year - 1900).is_err() {
        return Err(GmtimeError::YearOutOfRange);
    }
    let (hour, minute, second) = (
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60,
    );
    let year_text = if year < 10_000 {
        format!("{year:04}")
    } else {
        year.to_string()
    };
    Ok(format!(
        "{year_text}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

/// `time.gmtime` for a float timestamp: floored, and an error when it is not representable as
/// a C `time_t`.
pub fn utc_text_from_float(seconds: f64) -> Result<String, GmtimeError> {
    let floored = seconds.floor();
    if !floored.is_finite()
        || !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&floored)
    {
        return Err(GmtimeError::TimestampOutOfRange);
    }
    utc_text(floored as i64)
}

/// Failures of `time.gmtime` as Python raises them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmtimeError {
    /// `OverflowError`: the timestamp does not fit a C `time_t`.
    TimestampOutOfRange,
    /// `OSError` (`EOVERFLOW`): the year does not fit the C `int` field of `struct tm`.
    YearOutOfRange,
}

/// Howard Hinnant's `civil_from_days`: the proleptic Gregorian date of a day count from 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `a == b` for two Python numbers, exact across `int` and `float`.
pub fn compare_numbers(a: NumberRef, b: NumberRef) -> Ordering {
    match (a, b) {
        (NumberRef::Int(x), NumberRef::Int(y)) => x.cmp(&y),
        (NumberRef::Float(x), NumberRef::Float(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        (NumberRef::Int(x), NumberRef::Float(y)) => compare_int_float(x, y),
        (NumberRef::Float(x), NumberRef::Int(y)) => compare_int_float(y, x).reverse(),
    }
}

/// A Python `int` or `float` operand for [`compare_numbers`].
#[derive(Clone, Copy)]
pub enum NumberRef {
    Int(i128),
    Float(f64),
}

fn compare_int_float(int: i128, float: f64) -> Ordering {
    const TWO_POW_127: f64 = 1.701_411_834_604_692_3e38;
    if float >= TWO_POW_127 {
        return Ordering::Less;
    }
    if float < -TWO_POW_127 {
        return Ordering::Greater;
    }
    let whole = float.trunc();
    match int.cmp(&(whole as i128)) {
        Ordering::Equal => {
            let fraction = float - whole;
            if fraction > 0.0 {
                Ordering::Less
            } else if fraction < 0.0 {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        other => other,
    }
}

/// A JSON value as CPython's `json.loads` yields it, with only what the readers compare.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    /// `true`, `false`, numbers, `NaN` and `Infinity` are never compared.
    Other,
    /// A string as UTF-16 code units, so a lone surrogate stays representable as CPython keeps it.
    Str(Vec<u16>),
    Array,
    Object(Vec<(Vec<u16>, Json)>),
}

/// Whether a decoded JSON string is exactly `text`.
pub fn units_eq(units: &[u16], text: &str) -> bool {
    units.iter().copied().eq(text.encode_utf16())
}

/// Why `json.loads` refused a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonError {
    /// `ValueError`: malformed text, or an integer past CPython's 4300-digit limit.
    Malformed,
    /// `RecursionError`: nesting past the decoder's depth limit, which is not a `ValueError`.
    TooDeep,
}

/// Nesting depth at which the decoder gives up. CPython's real limit depends on the
/// caller's stack depth, so this constant is an approximation of it.
const MAX_DEPTH: usize = 1000;
/// CPython 3.14's default `sys.int_info.default_max_str_digits`.
const MAX_INT_DIGITS: usize = 4300;

/// `json.loads(text)` with CPython's default options.
pub fn json_loads(text: &str) -> Result<Json, JsonError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        at: 0,
    };
    parser.skip_space();
    let value = parser.value(0)?;
    parser.skip_space();
    if parser.at != parser.bytes.len() {
        return Err(JsonError::Malformed);
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, literal: &[u8]) -> bool {
        if self.bytes[self.at..].starts_with(literal) {
            self.at += literal.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        match self.bytes.get(self.at) {
            Some(b'"') => self.string().map(Json::Str),
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'n') if self.eat(b"null") => Ok(Json::Null),
            Some(b't') if self.eat(b"true") => Ok(Json::Other),
            Some(b'f') if self.eat(b"false") => Ok(Json::Other),
            Some(b'N') if self.eat(b"NaN") => Ok(Json::Other),
            Some(b'I') if self.eat(b"Infinity") => Ok(Json::Other),
            Some(b'-') if self.eat(b"-Infinity") => Ok(Json::Other),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(JsonError::Malformed),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(JsonError::TooDeep);
        }
        self.at += 1;
        let mut fields = Vec::new();
        self.skip_space();
        if self.eat(b"}") {
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_space();
            if self.bytes.get(self.at) != Some(&b'"') {
                return Err(JsonError::Malformed);
            }
            let key = self.string()?;
            self.skip_space();
            if !self.eat(b":") {
                return Err(JsonError::Malformed);
            }
            self.skip_space();
            let value = self.value(depth)?;
            fields.push((key, value));
            self.skip_space();
            if self.eat(b"}") {
                return Ok(Json::Object(fields));
            }
            if !self.eat(b",") {
                return Err(JsonError::Malformed);
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(JsonError::TooDeep);
        }
        self.at += 1;
        self.skip_space();
        if self.eat(b"]") {
            return Ok(Json::Array);
        }
        loop {
            self.skip_space();
            self.value(depth)?;
            self.skip_space();
            if self.eat(b"]") {
                return Ok(Json::Array);
            }
            if !self.eat(b",") {
                return Err(JsonError::Malformed);
            }
        }
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.at;
        self.eat(b"-");
        match self.bytes.get(self.at) {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(JsonError::Malformed),
        }
        let integer_end = self.at;
        if self.bytes.get(self.at) == Some(&b'.')
            && self.bytes.get(self.at + 1).is_some_and(u8::is_ascii_digit)
        {
            self.at += 1;
            self.digits();
            self.exponent();
        } else if self.exponent() {
            // A fraction-free exponent makes the value a float.
        } else if integer_end - start > MAX_INT_DIGITS + usize::from(self.bytes[start] == b'-') {
            return Err(JsonError::Malformed);
        }
        Ok(Json::Other)
    }

    fn digits(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
    }

    fn exponent(&mut self) -> bool {
        let start = self.at;
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.digits();
                return true;
            }
            self.at = start;
        }
        false
    }

    /// Decodes one string literal into UTF-16 units. A `\u` escape is taken as one unit, so a
    /// high surrogate followed by a low one is the pair CPython's scanner also yields.
    fn string(&mut self) -> Result<Vec<u16>, JsonError> {
        self.at += 1;
        let mut out: Vec<u16> = Vec::new();
        loop {
            let Some(&byte) = self.bytes.get(self.at) else {
                return Err(JsonError::Malformed);
            };
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escape = *self.bytes.get(self.at).ok_or(JsonError::Malformed)?;
                    self.at += 1;
                    let unit = match escape {
                        b'"' => u16::from(b'"'),
                        b'\\' => u16::from(b'\\'),
                        b'/' => u16::from(b'/'),
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'n' => 0x0a,
                        b'r' => 0x0d,
                        b't' => 0x09,
                        b'u' => self.hex4()?,
                        _ => return Err(JsonError::Malformed),
                    };
                    out.push(unit);
                }
                0x00..=0x1f => return Err(JsonError::Malformed),
                _ => {
                    let rest = &self.text[self.at..];
                    let c = rest.chars().next().ok_or(JsonError::Malformed)?;
                    let mut units = [0u16; 2];
                    out.extend_from_slice(c.encode_utf16(&mut units));
                    self.at += c.len_utf8();
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, JsonError> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or(JsonError::Malformed)?;
        let text = std::str::from_utf8(digits).map_err(|_| JsonError::Malformed)?;
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(JsonError::Malformed);
        }
        self.at += 4;
        u16::from_str_radix(text, 16).map_err(|_| JsonError::Malformed)
    }
}
