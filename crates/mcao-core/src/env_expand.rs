//! Percent-variable expander for Windows-style strings (DPES-892).
//!
//! Expands `%VAR%` references using a provided lookup function or the process
//! environment. Supports `%%` as an escape sequence for a literal `%`.

use std::error::Error;
use std::fmt;

/// An error that occurred during percent-variable expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpandError {
    /// A variable was referenced but not defined in the lookup table.
    Unset(String),
    /// A percent sign was not closed by a matching percent sign.
    Unterminated,
    /// The input string was empty.
    Empty,
}

impl fmt::Display for ExpandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unset(name) => write!(f, "variable {name} is not set"),
            Self::Unterminated => write!(f, "unterminated variable reference"),
            Self::Empty => write!(f, "input is empty"),
        }
    }
}

impl Error for ExpandError {}

/// Expands `%VAR%` references in `input` using the provided `lookup` closure.
///
/// # Rules
///
/// - `%NAME%` is replaced by the value returned by `lookup("NAME")`.
/// - `%%` yields a literal `%`.
/// - A single unmatched `%` produces [`ExpandError::Unterminated`].
/// - `%%`-free text passes through unchanged.
/// - If `input` is empty, returns [`ExpandError::Empty`].
/// - If a variable is unset, returns [`ExpandError::Unset`].
/// - Values are inserted literally and are not re-scanned for further `%`.
pub fn expand_with(
    input: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, ExpandError> {
    if input.is_empty() {
        return Err(ExpandError::Empty);
    }

    let mut out = String::with_capacity(input.len());
    let mut remaining = input;

    while !remaining.is_empty() {
        if let Some(pos) = remaining.find('%') {
            out.push_str(&remaining[..pos]);

            let after_pct = &remaining[pos + 1..];
            if after_pct.is_empty() {
                return Err(ExpandError::Unterminated);
            }

            if let Some(rest) = after_pct.strip_prefix('%') {
                out.push('%');
                remaining = rest;
            } else if let Some(close_pos) = after_pct.find('%') {
                let var_name = &after_pct[..close_pos];
                match lookup(var_name) {
                    Some(val) => {
                        out.push_str(&val);
                        remaining = &after_pct[close_pos + 1..];
                    }
                    None => {
                        return Err(ExpandError::Unset(var_name.to_string()));
                    }
                }
            } else {
                return Err(ExpandError::Unterminated);
            }
        } else {
            out.push_str(remaining);
            break;
        }
    }

    Ok(out)
}

/// Expands `%VAR%` references in `input` using environment variables.
///
/// Non-Unicode environment variable values are treated as unset.
pub fn expand(input: &str) -> Result<String, ExpandError> {
    expand_with(input, &|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_unchanged() {
        let lookup = |_name: &str| -> Option<String> { None };
        assert_eq!(
            expand_with("hello world", &lookup),
            Ok("hello world".to_string())
        );
        assert_eq!(
            expand_with("C:\\Program Files\\mcao\\home", &lookup),
            Ok("C:\\Program Files\\mcao\\home".to_string())
        );
        assert_eq!(
            expand_with("no_vars_here", &lookup),
            Ok("no_vars_here".to_string())
        );
    }

    #[test]
    fn one_variable() {
        let lookup = |name: &str| match name {
            "LOCALAPPDATA" => Some("C:\\Users\\alice\\AppData\\Local".to_string()),
            "USER" => Some("alice".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_with("%LOCALAPPDATA%\\mcao\\home", &lookup),
            Ok("C:\\Users\\alice\\AppData\\Local\\mcao\\home".to_string())
        );
        assert_eq!(expand_with("%USER%", &lookup), Ok("alice".to_string()));
        assert_eq!(
            expand_with("hello %USER%", &lookup),
            Ok("hello alice".to_string())
        );
    }

    #[test]
    fn two_variables() {
        let lookup = |name: &str| match name {
            "FIRST" => Some("foo".to_string()),
            "SECOND" => Some("bar".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_with("%FIRST% and %SECOND%", &lookup),
            Ok("foo and bar".to_string())
        );
        assert_eq!(
            expand_with("start/%FIRST%/%SECOND%/end", &lookup),
            Ok("start/foo/bar/end".to_string())
        );
    }

    #[test]
    fn percent_literal() {
        let lookup = |_name: &str| -> Option<String> { None };
        assert_eq!(expand_with("%%", &lookup), Ok("%".to_string()));
        assert_eq!(expand_with("%%%%", &lookup), Ok("%%".to_string()));
        assert_eq!(
            expand_with("100%% complete", &lookup),
            Ok("100% complete".to_string())
        );
        assert_eq!(
            expand_with("%%leading", &lookup),
            Ok("%leading".to_string())
        );
        assert_eq!(
            expand_with("trailing%%", &lookup),
            Ok("trailing%".to_string())
        );
    }

    #[test]
    fn unterminated() {
        let lookup = |name: &str| match name {
            "VAR" => Some("val".to_string()),
            _ => None,
        };
        assert_eq!(expand_with("%", &lookup), Err(ExpandError::Unterminated));
        assert_eq!(expand_with("%VAR", &lookup), Err(ExpandError::Unterminated));
        assert_eq!(expand_with("VAR%", &lookup), Err(ExpandError::Unterminated));
        assert_eq!(
            expand_with("prefix %VAR", &lookup),
            Err(ExpandError::Unterminated)
        );
        assert_eq!(expand_with("%%%", &lookup), Err(ExpandError::Unterminated));
        assert_eq!(
            expand_with("%VAR%%", &lookup),
            Err(ExpandError::Unterminated)
        );
        assert_eq!(
            expand_with("foo%bar", &lookup),
            Err(ExpandError::Unterminated)
        );
    }

    #[test]
    fn unset() {
        let lookup = |name: &str| match name {
            "SET" => Some("val".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_with("%NOT_SET%", &lookup),
            Err(ExpandError::Unset("NOT_SET".to_string()))
        );
        assert_eq!(
            expand_with("%MixedCase%", &lookup),
            Err(ExpandError::Unset("MixedCase".to_string()))
        );
        assert_eq!(
            expand_with("prefix %MISSING% suffix", &lookup),
            Err(ExpandError::Unset("MISSING".to_string()))
        );
    }

    #[test]
    fn value_containing_percent_not_re_expanded() {
        let lookup = |name: &str| match name {
            "VAR" => Some("%OTHER%".to_string()),
            "OTHER" => Some("nested".to_string()),
            "PCT" => Some("100%%".to_string()),
            _ => None,
        };
        assert_eq!(expand_with("%VAR%", &lookup), Ok("%OTHER%".to_string()));
        assert_eq!(expand_with("%PCT%", &lookup), Ok("100%%".to_string()));
    }

    #[test]
    fn adjacent_variables() {
        let lookup = |name: &str| match name {
            "A" => Some("hello".to_string()),
            "B" => Some("world".to_string()),
            _ => None,
        };
        assert_eq!(expand_with("%A%%B%", &lookup), Ok("helloworld".to_string()));
    }

    #[test]
    fn empty_input() {
        let lookup = |_name: &str| -> Option<String> { None };
        assert_eq!(expand_with("", &lookup), Err(ExpandError::Empty));
        assert_eq!(expand(""), Err(ExpandError::Empty));
    }

    #[test]
    fn display_text() {
        assert_eq!(
            format!("{}", ExpandError::Unset("FOO".to_string())),
            "variable FOO is not set"
        );
        assert_eq!(
            format!("{}", ExpandError::Unterminated),
            "unterminated variable reference"
        );
        assert_eq!(format!("{}", ExpandError::Empty), "input is empty");
    }

    #[test]
    fn expand_env_fallback() {
        assert_eq!(
            expand("plain text without variables"),
            Ok("plain text without variables".to_string())
        );
    }
}
