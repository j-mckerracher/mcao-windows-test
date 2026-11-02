//! Audit PII and PHI redaction for `mcao-core`.
//!
//! Provides redaction of SSNs, email addresses, phone numbers,
//! medical record numbers, and dates of birth without external regex crates.
//!
//! The patterns port the Python `re` originals. Word boundaries (`\b`) use the
//! generated `\w` table `PY_WORD_RANGES` (`audit_redact_pii/py_word_table.rs`), `\d`
//! uses the generated Nd table `PY_DIGIT_RANGES` (`audit_redact_pii/py_digit_table.rs`), and
//! the medical record number's `\s` gap uses [`pystr::is_space`]. Case-insensitive
//! keywords fold as Python's `(?i)` does (see [`py_ignorecase_eq`]). All match
//! CPython 3.14.6 (Unicode 16.0.0) and come from in-tree data, so none depends
//! on the Rust toolchain's Unicode tables. The table contents are pinned by SHA-256,
//! not by shape.

mod py_digit_table;
mod py_word_table;

use crate::audit_redact_tokens::py_ignorecase_eq;
use crate::util::pystr;
use py_digit_table::PY_DIGIT_RANGES;
use py_word_table::PY_WORD_RANGES;

/// Applies the five PII redactions in order; each pattern runs over the output of the previous one.
///
/// The order is SSN, email, phone, medical record number, date of birth, and it
/// is part of the contract: `DOB MRN 123456` loses the number to the medical
/// record rule first, so the date-of-birth rule finds no digit left to match.
///
#[must_use]
pub fn redact_pii(text: &str) -> String {
    let ssn_redacted = redact_pass(text, "[REDACTED:ssn]", try_match_ssn);
    let email_redacted = redact_pass(&ssn_redacted, "[REDACTED:email]", try_match_email);
    let phone_redacted = redact_pass(&email_redacted, "[REDACTED:phone]", try_match_phone);
    let mrn_redacted = redact_pass(
        &phone_redacted,
        "[REDACTED:medical_record_number]",
        try_match_mrn,
    );
    redact_pass(&mrn_redacted, "[REDACTED:date_of_birth]", try_match_dob)
}

fn redact_pass<F>(text: &str, replacement: &str, matcher: F) -> String
where
    F: Fn(&[char], usize) -> Option<usize>,
{
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = matcher(&chars, i) {
            out.push_str(replacement);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Whether `code_point` lies in one of the sorted, disjoint `ranges`.
fn in_ranges(ranges: &[(u32, u32)], code_point: u32) -> bool {
    let first_candidate = ranges.partition_point(|&(_, end)| end < code_point);
    ranges
        .get(first_candidate)
        .is_some_and(|&(start, _)| start <= code_point)
}

/// Python `\w`, the character class that `\b` uses; see `PY_WORD_RANGES`.
fn is_word_char(c: char) -> bool {
    in_ranges(PY_WORD_RANGES, u32::from(c))
}

/// Python `\d` for str patterns, the Unicode decimal class; see `PY_DIGIT_RANGES`.
fn is_py_digit(c: char) -> bool {
    in_ranges(PY_DIGIT_RANGES, u32::from(c))
}

/// Whether index `i` is a `\b` position: exactly one side is a `\w` character.
///
/// Every matcher passes `i <= chars.len()`, so the `i <= chars.len()` test on the left side is
/// always true from `redact_pii`; it stays so that an index past the end is not a boundary rather
/// than a panic. The right-side `i < chars.len()` test does fire, at end of input.
/// `test_word_boundary_index_past_end_is_not_a_boundary` pins the past-end behavior.
fn is_word_boundary(chars: &[char], i: usize) -> bool {
    let prev = if i > 0 && i <= chars.len() {
        is_word_char(chars[i - 1])
    } else {
        false
    };
    let next = if i < chars.len() {
        is_word_char(chars[i])
    } else {
        false
    };
    prev != next
}

fn try_match_ssn(chars: &[char], start: usize) -> Option<usize> {
    if !is_word_boundary(chars, start) {
        return None;
    }
    let end = start.checked_add(11)?;
    if end > chars.len() {
        return None;
    }
    if !chars[start..start + 3].iter().all(|&c| is_py_digit(c)) {
        return None;
    }
    if chars[start + 3] != '-' {
        return None;
    }
    if !chars[start + 4..start + 6].iter().all(|&c| is_py_digit(c)) {
        return None;
    }
    if chars[start + 6] != '-' {
        return None;
    }
    if !chars[start + 7..start + 11].iter().all(|&c| is_py_digit(c)) {
        return None;
    }
    if !is_word_boundary(chars, end) {
        return None;
    }
    Some(end)
}

fn is_email_local(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '%' || c == '+' || c == '-'
}

fn is_email_domain(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '-'
}

fn try_match_email(chars: &[char], start: usize) -> Option<usize> {
    if start >= chars.len() || !is_email_local(chars[start]) {
        return None;
    }
    if start > 0 && is_email_local(chars[start - 1]) {
        return None;
    }
    let mut local_end = start + 1;
    while local_end < chars.len() && is_email_local(chars[local_end]) {
        local_end += 1;
    }
    if local_end >= chars.len() || chars[local_end] != '@' {
        return None;
    }
    let domain_start = local_end + 1;
    let mut domain_end = domain_start;
    while domain_end < chars.len() && is_email_domain(chars[domain_end]) {
        domain_end += 1;
    }
    let d = &chars[domain_start..domain_end];
    if d.is_empty() {
        return None;
    }
    let mut best_k = None;
    if d.len() >= 4 {
        for k in (1..=d.len() - 3).rev() {
            if d[k] == '.' && d[k + 1].is_ascii_alphabetic() && d[k + 2].is_ascii_alphabetic() {
                best_k = Some(k);
                break;
            }
        }
    }
    let k = best_k?;
    let mut tld_end = k + 1;
    while tld_end < d.len() && d[tld_end].is_ascii_alphabetic() {
        tld_end += 1;
    }
    Some(domain_start + tld_end)
}

fn is_phone_sep(c: char) -> bool {
    c == '-' || c == '.' || c == ' '
}

fn try_match_phone(chars: &[char], start: usize) -> Option<usize> {
    let mut cur = start;
    if cur < chars.len() && chars[cur] == '(' {
        cur += 1;
    }
    if !is_word_boundary(chars, cur) {
        return None;
    }
    let end1 = cur.checked_add(3)?;
    if end1 > chars.len() || !chars[cur..end1].iter().all(|&c| is_py_digit(c)) {
        return None;
    }
    cur = end1;
    if cur < chars.len() && chars[cur] == ')' {
        cur += 1;
    }
    if cur >= chars.len() || !is_phone_sep(chars[cur]) {
        return None;
    }
    cur += 1;
    let end2 = cur.checked_add(3)?;
    if end2 > chars.len() || !chars[cur..end2].iter().all(|&c| is_py_digit(c)) {
        return None;
    }
    cur = end2;
    if cur >= chars.len() || !is_phone_sep(chars[cur]) {
        return None;
    }
    cur += 1;
    let end3 = cur.checked_add(4)?;
    if end3 > chars.len() || !chars[cur..end3].iter().all(|&c| is_py_digit(c)) {
        return None;
    }
    cur = end3;
    if !is_word_boundary(chars, cur) {
        return None;
    }
    Some(cur)
}

/// Whether `keyword` (lowercase ASCII) matches at `start` under Python's case-insensitive rules.
fn keyword_at(chars: &[char], start: usize, keyword: &str) -> bool {
    keyword.chars().enumerate().all(|(offset, expected)| {
        chars
            .get(start + offset)
            .is_some_and(|&c| py_ignorecase_eq(c, expected))
    })
}

fn try_match_mrn(chars: &[char], start: usize) -> Option<usize> {
    if !is_word_boundary(chars, start) {
        return None;
    }
    if !keyword_at(chars, start, "mrn") {
        return None;
    }
    let mut cur = start + 3;
    while cur < chars.len()
        && (chars[cur] == ':' || chars[cur] == '#' || pystr::is_space(u32::from(chars[cur])))
    {
        cur += 1;
    }
    let digit_start = cur;
    while cur < chars.len() && is_py_digit(chars[cur]) {
        cur += 1;
    }
    if cur - digit_start >= 5 {
        Some(cur)
    } else {
        None
    }
}

fn try_match_dob(chars: &[char], start: usize) -> Option<usize> {
    if !is_word_boundary(chars, start) {
        return None;
    }
    let kw_len = if keyword_at(chars, start, "dob") {
        3
    } else if keyword_at(chars, start, "date of birth") {
        13
    } else {
        return None;
    };
    let kw_end = start + kw_len;
    if !is_word_boundary(chars, kw_end) {
        return None;
    }
    let available = (chars.len() - kw_end).min(21);
    let window = &chars[kw_end..kw_end + available];
    let limit = match window.iter().position(|&c| c == '\n') {
        Some(pos) => pos,
        None => available,
    };
    let k = window[..limit].iter().rposition(|&c| is_py_digit(c))?;
    Some(kw_end + k + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssn() {
        assert_eq!(redact_pii("123-45-6789"), "[REDACTED:ssn]");
        assert_eq!(redact_pii("a123-45-6789"), "a123-45-6789");
        assert_eq!(redact_pii("123-45-67890"), "123-45-67890");
        assert_eq!(redact_pii("x 123-45-6789."), "x [REDACTED:ssn].");
        assert_eq!(redact_pii("1234-45-6789"), "1234-45-6789");
    }

    #[test]
    fn test_email() {
        assert_eq!(
            redact_pii("contact bob.smith+x@example.co.uk now"),
            "contact [REDACTED:email] now"
        );
        assert_eq!(redact_pii("a@b.c"), "a@b.c");
        assert_eq!(redact_pii("a@b.com."), "[REDACTED:email].");
        assert_eq!(redact_pii("a@b.c0m x@y.zz"), "a@b.c0m [REDACTED:email]");
        assert_eq!(redact_pii("_x@@y.com"), "_x@@y.com");
        assert_eq!(redact_pii("joe@@x.com"), "joe@@x.com");
        assert_eq!(redact_pii("-a@b.org"), "[REDACTED:email]");
        assert_eq!(
            redact_pii("me@host-name.io,you@z.org"),
            "[REDACTED:email],[REDACTED:email]"
        );
        assert_eq!(redact_pii("a@b..com"), "[REDACTED:email]");
        assert_eq!(redact_pii("@x.com"), "@x.com");
        assert_eq!(redact_pii("a b@c.de"), "a [REDACTED:email]");
    }

    #[test]
    fn test_phone() {
        assert_eq!(redact_pii("(555) 123-4567"), "[REDACTED:phone]");
        assert_eq!(redact_pii("555-123-4567"), "[REDACTED:phone]");
        assert_eq!(redact_pii("555.123.4567"), "[REDACTED:phone]");
        assert_eq!(redact_pii("555 123 4567"), "[REDACTED:phone]");
        assert_eq!(redact_pii("(555)-123-4567"), "[REDACTED:phone]");
        assert_eq!(redact_pii("(555 123-4567"), "[REDACTED:phone]");

        assert_eq!(redact_pii("1555-123-4567"), "1555-123-4567");
        assert_eq!(redact_pii("555-123-45678"), "555-123-45678");
        assert_eq!(redact_pii("x555-123-4567"), "x555-123-4567");
        assert_eq!(redact_pii("555-123-4567x"), "555-123-4567x");
        assert_eq!(redact_pii("(555)123-4567"), "(555)123-4567");
    }

    #[test]
    fn test_mrn() {
        assert_eq!(redact_pii("MRN 123456"), "[REDACTED:medical_record_number]");
        assert_eq!(
            redact_pii("mrn:#  12345"),
            "[REDACTED:medical_record_number]"
        );
        assert_eq!(
            redact_pii("Mrn12345678 x"),
            "[REDACTED:medical_record_number] x"
        );

        assert_eq!(redact_pii("MRN1234"), "MRN1234");
        assert_eq!(redact_pii("amrn 12345"), "amrn 12345");
        assert_eq!(redact_pii("MRN: 1234 56789"), "MRN: 1234 56789");
    }

    #[test]
    fn test_dob() {
        assert_eq!(redact_pii("DOB 1990-01-01"), "[REDACTED:date_of_birth]");
        assert_eq!(redact_pii("dob: x1"), "[REDACTED:date_of_birth]");
        assert_eq!(
            redact_pii("date of birth: 12/05/1980 and 1"),
            "[REDACTED:date_of_birth]"
        );
        assert_eq!(
            redact_pii("DOB 12 34 56 78 90 12"),
            "[REDACTED:date_of_birth]"
        );
        assert_eq!(redact_pii("DOB:éé 5"), "[REDACTED:date_of_birth]");

        assert_eq!(
            redact_pii("DOB abcdefghijklmnopqrst1"),
            "DOB abcdefghijklmnopqrst1"
        );
        assert_eq!(
            redact_pii("DOB abcdefghijklmnopqrstu1"),
            "DOB abcdefghijklmnopqrstu1"
        );
        assert_eq!(redact_pii("Date Of Birth\n1"), "Date Of Birth\n1");
        assert_eq!(redact_pii("DOB a\n1"), "DOB a\n1");
        assert_eq!(redact_pii("dob1"), "dob1");
        assert_eq!(redact_pii("adob 1"), "adob 1");
    }

    #[test]
    fn test_ssn_group_and_word_boundary_edges() {
        assert_eq!(redact_pii("123-4a-6789"), "123-4a-6789");
        assert_eq!(redact_pii("é123-45-6789"), "é123-45-6789");
        assert_eq!(redact_pii("_123-45-6789"), "_123-45-6789");
        assert_eq!(redact_pii("123-45-6789_"), "123-45-6789_");
        assert_eq!(redact_pii("123-45-6789é"), "123-45-6789é");
    }

    #[test]
    fn test_word_boundary_matches_python_marks_symbols_and_unassigned() {
        // Python `\w` excludes these code points, so a token next to them is at a
        // boundary. Each expectation was produced by Python 3.14 `re` (Unicode 16.0.0).
        assert_eq!(redact_pii("ⓐ123-45-6789"), "ⓐ[REDACTED:ssn]");
        assert_eq!(
            redact_pii("\u{939}\u{947}123-45-6789"),
            "\u{939}\u{947}[REDACTED:ssn]"
        );
        assert_eq!(
            redact_pii("SSN\u{903}123-45-6789"),
            "SSN\u{903}[REDACTED:ssn]"
        );
        assert_eq!(redact_pii("123-45-6789ⓐ"), "[REDACTED:ssn]ⓐ");
        assert_eq!(redact_pii("\u{88f}123-45-6789"), "\u{88f}[REDACTED:ssn]");
        assert_eq!(redact_pii("123-45-6789\u{88f}"), "[REDACTED:ssn]\u{88f}");
        assert_eq!(redact_pii("DOB\u{345}1"), "[REDACTED:date_of_birth]");
    }

    #[test]
    fn test_word_boundary_keeps_python_word_characters() {
        // Python `\w` includes these, so no boundary sits between them and a token.
        assert_eq!(redact_pii("中123-45-6789"), "中123-45-6789");
        assert_eq!(redact_pii("\u{2160}123-45-6789"), "\u{2160}123-45-6789");
        assert_eq!(redact_pii("\u{966}123-45-6789"), "\u{966}123-45-6789");
        assert_eq!(redact_pii("123-45-6789\u{b2}"), "123-45-6789\u{b2}");
    }

    #[test]
    fn test_word_char_membership_samples() {
        for c in [
            'A', 'z', '0', '_', 'é', '\u{939}', '\u{966}', '\u{2160}', '\u{b2}', '一',
        ] {
            assert!(is_word_char(c), "{c:?} is Python \\w");
        }
        for c in [
            ' ', '-', '!', '\u{a0}', '\u{1c}', '\u{300}', '\u{345}', '\u{903}', '\u{24b6}',
            '\u{88f}',
        ] {
            assert!(!is_word_char(c), "{c:?} is not Python \\w");
        }
    }

    #[test]
    fn test_word_char_python_edges() {
        // Python 3.14.6 `re`: `\w` includes these range ends (Lu, Nd, Nl, No).
        for c in [
            '\u{c0}', '\u{d6}', '\u{d8}', '\u{660}', '\u{669}', '\u{2160}', '\u{966}',
        ] {
            assert!(is_word_char(c), "{c:?} is Python \\w");
        }
        // Python 3.14.6 `re`: not `\w`: multiplication sign, inverted question mark, Arabic
        // percent and decimal separators, a combining mark, an unassigned code point, visarga.
        for c in [
            '\u{d7}', '\u{bf}', '\u{66a}', '\u{66b}', '\u{345}', '\u{88f}', '\u{903}',
        ] {
            assert!(!is_word_char(c), "{c:?} is not Python \\w");
        }
    }

    #[test]
    fn test_word_boundary_next_to_python_edge_code_points() {
        // Expected outputs from Python 3.14.6 `re` with the five patterns.
        assert_eq!(redact_pii("\u{660}123-45-6789"), "\u{660}123-45-6789");
        assert_eq!(redact_pii("\u{66a}123-45-6789"), "\u{66a}[REDACTED:ssn]");
        assert_eq!(redact_pii("\u{d7}123-45-6789"), "\u{d7}[REDACTED:ssn]");
        assert_eq!(redact_pii("\u{c0}123-45-6789"), "\u{c0}123-45-6789");
    }

    #[test]
    fn test_mrn_gap_whitespace_matches_python_space_class() {
        // Python `\s` matches these; U+200B is not whitespace in Python.
        assert_eq!(
            redact_pii("MRN\u{2028}12345"),
            "[REDACTED:medical_record_number]"
        );
        assert_eq!(
            redact_pii("MRN\u{1c}12345"),
            "[REDACTED:medical_record_number]"
        );
        assert_eq!(
            redact_pii("MRN\u{a0}12345"),
            "[REDACTED:medical_record_number]"
        );
        assert_eq!(
            redact_pii("MRN\u{3000}12345"),
            "[REDACTED:medical_record_number]"
        );
        assert_eq!(redact_pii("MRN\u{200b}12345"), "MRN\u{200b}12345");
    }

    #[test]
    fn test_word_boundary_index_past_end_is_not_a_boundary() {
        let chars: Vec<char> = "a1".chars().collect();
        assert!(is_word_boundary(&chars, 0));
        assert!(is_word_boundary(&chars, 2));
        assert!(!is_word_boundary(&chars, 3));
        assert!(!is_word_boundary(&chars, usize::MAX));
    }

    #[test]
    fn test_digit_matches_python_unicode_decimal_digits() {
        // Python `\d` matches Unicode decimal digits, so these shapes are redacted as ASCII ones are.
        assert_eq!(redact_pii("١٢٣-٤٥-٦٧٨٩"), "[REDACTED:ssn]");
        assert_eq!(redact_pii("DOB ١"), "[REDACTED:date_of_birth]");
        assert_eq!(redact_pii("MRN ١٢٣٤٥"), "[REDACTED:medical_record_number]");
        assert_eq!(redact_pii("١٢٣.٥٥٥.١٢٣٤"), "[REDACTED:phone]");
        // Fullwidth digits are Nd too; superscript two (U+00B2) is not a decimal digit.
        assert_eq!(redact_pii("１２３-４５-６７８９"), "[REDACTED:ssn]");
        assert_eq!(redact_pii("123-45-678\u{b2}"), "123-45-678\u{b2}");
    }

    #[test]
    fn test_case_fold_matches_python_dotted_and_dotless_i() {
        // Expected outputs from Python 3.14.6 `re`: `(?i)` folds U+0130 and U+0131 onto `i`.
        assert_eq!(redact_pii("DATE OF BİRTH 1"), "[REDACTED:date_of_birth]");
        assert_eq!(redact_pii("DATE OF BıRTH 1"), "[REDACTED:date_of_birth]");
        assert_eq!(redact_pii("DATE OF BIRTH 1"), "[REDACTED:date_of_birth]");
        assert_eq!(redact_pii("DATE OF BİRTH\n1"), "DATE OF BİRTH\n1");
    }

    #[test]
    fn test_mrn_keyword_case_fold_matches_python() {
        // Expected outputs from Python 3.14.6 `re`: `MRN` folds case-insensitively, `ſ` is not an `n`.
        assert_eq!(redact_pii("mRn 12345"), "[REDACTED:medical_record_number]");
        assert_eq!(redact_pii("MRN\u{17f} 12345"), "MRN\u{17f} 12345");
    }

    #[test]
    fn test_email_edges() {
        assert_eq!(redact_pii("a@.com"), "a@.com");
        assert_eq!(redact_pii("a%b@x.com"), "[REDACTED:email]");
    }

    #[test]
    fn test_mrn_whitespace_classes() {
        // Python `\s` also matches the ASCII separators 0x1c-0x1f and Unicode spaces.
        assert_eq!(
            redact_pii("MRN\u{1f}12345"),
            "[REDACTED:medical_record_number]"
        );
        assert_eq!(
            redact_pii("MRN\u{a0}12345"),
            "[REDACTED:medical_record_number]"
        );
        assert_eq!(
            redact_pii("MRN\u{3000}12345"),
            "[REDACTED:medical_record_number]"
        );
    }

    #[test]
    fn test_dob_window_edge_and_literal_spaces() {
        // A digit exactly 20 characters after the keyword is the last one `.{0,20}` reaches.
        assert_eq!(
            redact_pii("DOB xxxxxxxxxxxxxxxxxxx1"),
            "[REDACTED:date_of_birth]"
        );
        assert_eq!(redact_pii("date\tof birth 1"), "date\tof birth 1");
        assert_eq!(redact_pii("date of\tbirth 1"), "date of\tbirth 1");
        assert_eq!(redact_pii("date  of birth 1"), "date  of birth 1");
    }

    #[test]
    fn test_redaction_order_mrn_before_dob() {
        assert_eq!(
            redact_pii("DOB MRN 123456"),
            "DOB [REDACTED:medical_record_number]"
        );
    }

    #[test]
    fn test_general() {
        assert_eq!(redact_pii(""), "");
        assert_eq!(redact_pii("héllo wörld"), "héllo wörld");
    }

    #[test]
    fn test_combined() {
        let input = "Patient with MRN 123456 born on DOB 1990-01-01 called (555) 123-4567, email user@test.com, ssn 123-45-6789";
        let expected = "Patient with [REDACTED:medical_record_number] born on [REDACTED:date_of_birth] called [REDACTED:phone], email [REDACTED:email], ssn [REDACTED:ssn]";
        assert_eq!(redact_pii(input), expected);
    }
}
