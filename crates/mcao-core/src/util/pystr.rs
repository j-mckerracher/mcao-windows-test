//! Python `str` semantics over code points.
//!
//! A Python `str` can hold a lone surrogate, which a JSON `\uD800` escape produces. Rust's
//! `String` cannot, so text here is a slice of `u32` code points. Input bytes are decoded
//! strictly as UTF-8 by the caller, so only JSON-decoded values can carry surrogates.

/// A Python `str` as code points. May contain lone surrogates.
pub type Text = Vec<u32>;

/// The line feed code point, the only separator `str.split("\n")` needs here.
pub const LF: u32 = 0x0a;

/// Python `str.isspace()` for one code point. The set was checked exhaustively against CPython
/// 3.14.6 `str.isspace()` over all code points; `test_is_space_is_python_whitespace_over_every_scalar`
/// pins the transcribed set.
pub fn is_space(c: u32) -> bool {
    matches!(
        c,
        0x09..=0x0d
            | 0x1c..=0x20
            | 0x85
            | 0xa0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
    )
}

/// The code points of a Rust string, as a `Text`.
pub fn from_str(s: &str) -> Text {
    s.chars().map(u32::from).collect()
}

/// True when `t` is exactly the ASCII string `s`.
pub fn eq_ascii(t: &[u32], s: &str) -> bool {
    t.len() == s.len() && t.iter().zip(s.bytes()).all(|(&a, b)| a == u32::from(b))
}

/// True when `t` begins with the ASCII string `prefix`.
pub fn starts_with_ascii(t: &[u32], prefix: &str) -> bool {
    strip_ascii(t, prefix).is_some()
}

/// `t` without the leading ASCII `prefix`, when `t` begins with it.
pub fn strip_ascii<'a>(t: &'a [u32], prefix: &str) -> Option<&'a [u32]> {
    let head = t.get(..prefix.len())?;
    eq_ascii(head, prefix).then(|| &t[prefix.len()..])
}

/// Python `str.split(sep)` for a one-code-point separator. Keeps empty fields, so an
/// empty input yields one empty field.
pub fn split(t: &[u32], sep: u32) -> impl Iterator<Item = &[u32]> {
    t.split(move |&c| c == sep)
}

/// Python `str.split()` with no arguments: runs of whitespace separate, empties are dropped.
pub fn split_whitespace(t: &[u32]) -> Vec<&[u32]> {
    t.split(|&c| is_space(c))
        .filter(|part| !part.is_empty())
        .collect()
}

/// Python `str.strip() == ""`.
pub fn is_blank(t: &[u32]) -> bool {
    t.iter().all(|&c| is_space(c))
}

/// Python `str.rstrip()`.
pub fn rstrip(t: &[u32]) -> &[u32] {
    let end = t
        .iter()
        .rposition(|&c| !is_space(c))
        .map_or(0, |last| last + 1);
    &t[..end]
}

#[cfg(test)]
mod tests {
    use super::is_space;

    #[test]
    fn test_is_space_is_python_whitespace_over_every_scalar() {
        // Python `\s` and `str.isspace()` over all code points: these 29 code points (CPython 3.14.6).
        let python_space = |cp: u32| {
            matches!(
                cp,
                0x09..=0x0d
                    | 0x1c..=0x20
                    | 0x85
                    | 0xa0
                    | 0x1680
                    | 0x2000..=0x200a
                    | 0x2028
                    | 0x2029
                    | 0x202f
                    | 0x205f
                    | 0x3000
            )
        };
        let mut count = 0_u32;
        for cp in 0..=0x0010_ffff_u32 {
            assert_eq!(is_space(cp), python_space(cp), "U+{cp:04X}");
            // Rust `White_Space` plus U+001C..=U+001F is the predicate the matchers used before.
            if let Some(c) = char::from_u32(cp) {
                assert_eq!(
                    is_space(cp),
                    c.is_whitespace() || (0x1c..=0x1f).contains(&cp)
                );
            }
            count += u32::from(is_space(cp));
        }
        assert_eq!(count, 29);
    }
}
