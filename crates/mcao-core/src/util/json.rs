//! The two `json` behaviours the awaiting-summary programs depend on.
//!
//! `decode_string` is `json.loads` restricted to one string value: strict about control
//! characters and escapes, combines a surrogate pair into one code point, and keeps a lone
//! surrogate. `encode_ascii` is `json.dumps` of one string with `ensure_ascii=True`.

use super::pystr::Text;

const QUOTE: u32 = 0x22;
const BACKSLASH: u32 = 0x5c;

/// Decodes a JSON string the way `json.loads` does. JSON whitespace may follow the closing
/// quote. Returns `None` where `json.loads` raises `ValueError`.
pub fn decode_string(input: &[u32]) -> Option<Text> {
    if input.first() != Some(&QUOTE) {
        return None;
    }
    let mut out = Text::new();
    let mut i = 1;
    loop {
        let c = *input.get(i)?;
        match c {
            QUOTE => {
                i += 1;
                break;
            }
            BACKSLASH => {
                let escape = *input.get(i + 1)?;
                i += 2;
                match escape {
                    0x22 | 0x5c | 0x2f => out.push(escape),
                    0x62 => out.push(0x08),
                    0x66 => out.push(0x0c),
                    0x6e => out.push(0x0a),
                    0x72 => out.push(0x0d),
                    0x74 => out.push(0x09),
                    0x75 => {
                        let mut unit = hex4(input, i)?;
                        i += 4;
                        if (0xd800..=0xdbff).contains(&unit)
                            && input.get(i) == Some(&BACKSLASH)
                            && input.get(i + 1) == Some(&0x75)
                        {
                            let low = hex4(input, i + 2)?;
                            if (0xdc00..=0xdfff).contains(&low) {
                                unit = 0x10000 + (((unit - 0xd800) << 10) | (low - 0xdc00));
                                i += 6;
                            }
                        }
                        out.push(unit);
                    }
                    _ => return None,
                }
            }
            0x00..=0x1f => return None,
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    input[i..]
        .iter()
        .all(|&c| matches!(c, 0x20 | 0x09 | 0x0a | 0x0d))
        .then_some(out)
}

fn hex4(input: &[u32], at: usize) -> Option<u32> {
    input.get(at..at + 4)?.iter().try_fold(0u32, |acc, &c| {
        let digit = char::from_u32(c)?.to_digit(16)?;
        Some((acc << 4) | digit)
    })
}

/// Encodes one string as `json.dumps` does with `ensure_ascii=True`: the short escapes,
/// `\uXXXX` in lowercase hex for everything outside printable ASCII, and surrogate pairs
/// for code points above U+FFFF.
pub fn encode_ascii(s: &[u32]) -> String {
    let mut out = String::from("\"");
    for &c in s {
        match c {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(char::from(c as u8)),
            0x00..=0xffff => out.push_str(&format!("\\u{c:04x}")),
            _ => {
                let offset = c - 0x10000;
                let high = 0xd800 + (offset >> 10);
                let low = 0xdc00 + (offset & 0x3ff);
                out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
            }
        }
    }
    out.push('"');
    out
}
