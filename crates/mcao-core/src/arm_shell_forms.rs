//! Shell invocation shapes, variable references, and helper functions for arm command policy.

/// Extracts the base name component of a path-like string.
///
/// Returns the last non-empty segment delimited by `/`, or the input itself
/// if no non-empty segments exist.
pub fn basename(value: &str) -> &str {
    match value.split('/').rfind(|s| !s.is_empty()) {
        Some(last) => last,
        None => value,
    }
}

/// Checks whether a string represents a variable assignment (`^[A-Za-z_][A-Za-z0-9_]*=`).
pub fn is_assignment(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    for c in chars {
        if c == '=' {
            return true;
        }
        if !c.is_ascii_alphanumeric() && c != '_' {
            return false;
        }
    }
    false
}

/// Helper function to check if a byte is a valid start of a variable name (`[A-Za-z_]`).
fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Helper function to check if a byte is a valid continuation of a variable name (`[A-Za-z0-9_]`).
fn is_name_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Checks whether a string references any variable names in the given list.
///
/// Returns `false` if `names` is empty. Scans `value` for `${NAME}` and `$NAME`
/// references, matching `NAME` against `names`.
pub fn word_references_any(value: &str, names: &[&str]) -> bool {
    if names.is_empty() {
        return false;
    }
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                let name_start = i + 2;
                if name_start < bytes.len() && is_name_start(bytes[name_start]) {
                    let mut end = name_start + 1;
                    while end < bytes.len() && is_name_continue(bytes[end]) {
                        end += 1;
                    }
                    if end < bytes.len() && bytes[end] == b'}' {
                        if std::str::from_utf8(&bytes[name_start..end])
                            .is_ok_and(|name| names.contains(&name))
                        {
                            return true;
                        }
                        i = end + 1;
                        continue;
                    }
                }
            } else if i + 1 < bytes.len() && is_name_start(bytes[i + 1]) {
                let name_start = i + 1;
                let mut end = name_start + 1;
                while end < bytes.len() && is_name_continue(bytes[end]) {
                    end += 1;
                }
                if std::str::from_utf8(&bytes[name_start..end])
                    .is_ok_and(|name| names.contains(&name))
                {
                    return true;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    false
}

/// A lexed shell word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellWord {
    /// The string content of the word after quote stripping.
    pub value: String,
    /// Whether the word consists entirely of literal characters without expansions.
    pub literal: bool,
    /// Whether the word contains command or process substitutions.
    pub has_subs: bool,
}

/// The execution shape of a shell invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// Shell executes a command string passed via `-c`.
    Command,
    /// Shell executes a script file from disk.
    Script,
    /// Shell reads commands from standard input.
    Stdin,
}

/// Result of classifying a shell invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInvocation {
    /// Shape of the shell execution.
    pub kind: ShellKind,
    /// Word index of the payload (`-c` command string or script file path), if any.
    pub payload: Option<usize>,
    /// Word index of the script operand when reading from stdin, if any.
    pub operand: Option<usize>,
}

/// Helper function to check if an argument matches `^-[A-Za-z]*c[A-Za-z]*$`.
fn is_c_option(s: &str) -> bool {
    let mut chars = s.chars();
    if chars.next() != Some('-') {
        return false;
    }
    let mut has_c = false;
    for ch in chars {
        if !ch.is_ascii_alphabetic() {
            return false;
        }
        if ch == 'c' {
            has_c = true;
        }
    }
    has_c
}

/// Helper function to check if an argument matches `^-[A-Za-z]*s[A-Za-z]*$`.
fn is_s_option(s: &str) -> bool {
    let mut chars = s.chars();
    if chars.next() != Some('-') {
        return false;
    }
    let mut has_s = false;
    for ch in chars {
        if !ch.is_ascii_alphabetic() {
            return false;
        }
        if ch == 's' {
            has_s = true;
        }
    }
    has_s
}

/// Classifies a shell invocation into its execution shape and argument indices.
///
/// Returns `None` if `command_index` is out of bounds or the command basename
/// is not `sh`, `bash`, or `zsh`.
pub fn shell_invocation(words: &[ShellWord], command_index: usize) -> Option<ShellInvocation> {
    let cmd = words.get(command_index)?;
    let name = basename(&cmd.value);
    if name != "sh" && name != "bash" && name != "zsh" {
        return None;
    }

    let mut reads_stdin = false;
    let mut options_ended = false;
    let mut i = command_index + 1;

    while i < words.len() {
        let v = &words[i].value;

        // (1) ^-[A-Za-z]*c[A-Za-z]*$
        if is_c_option(v) {
            let mut p = i + 1;
            if words.get(p).map(|w| w.value.as_str()) == Some("--") {
                p += 1;
            }
            let payload = if p < words.len() { Some(p) } else { None };
            return Some(ShellInvocation {
                kind: ShellKind::Command,
                payload,
                operand: None,
            });
        }

        // (2) -O or +O: skip the next word too (i += 2 in total), continue
        if v == "-O" || v == "+O" {
            i += 2;
            continue;
        }

        // (3) if !options_ended and matches ^-[A-Za-z]*s[A-Za-z]*$: reads_stdin = true (then fall through)
        if !options_ended && is_s_option(v) {
            reads_stdin = true;
        }

        // (4) v == "--": if reads_stdin return Stdin with operand = index i+1 if it exists else None;
        //     else options_ended = true (fall through)
        if v == "--" {
            if reads_stdin {
                let operand = if i + 1 < words.len() {
                    Some(i + 1)
                } else {
                    None
                };
                return Some(ShellInvocation {
                    kind: ShellKind::Stdin,
                    payload: None,
                    operand,
                });
            }
            options_ended = true;
        }

        // (5) if v == "--" or v starts with '-' or '+': continue
        if v == "--" || v.starts_with('-') || v.starts_with('+') {
            i += 1;
            continue;
        }

        // (6) if reads_stdin return Stdin with operand = Some(i); else return Script with payload = Some(i)
        if reads_stdin {
            return Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: Some(i),
            });
        } else {
            return Some(ShellInvocation {
                kind: ShellKind::Script,
                payload: Some(i),
                operand: None,
            });
        }
    }

    // After the loop return Stdin (payload and operand None)
    Some(ShellInvocation {
        kind: ShellKind::Stdin,
        payload: None,
        operand: None,
    })
}

/// Extracts the payload of an `eval` command invocation.
///
/// Returns `None` if `command_index` is out of bounds, the command basename is not
/// `eval`, there are no arguments following `eval`, or any argument has `!literal || has_subs`.
pub fn eval_payload(words: &[ShellWord], command_index: usize) -> Option<String> {
    let cmd = words.get(command_index)?;
    if basename(&cmd.value) != "eval" {
        return None;
    }
    let rest = words.get(command_index + 1..)?;
    if rest.is_empty() || rest.iter().any(|w| !w.literal || w.has_subs) {
        return None;
    }
    let values: Vec<&str> = rest.iter().map(|w| w.value.as_str()).collect();
    Some(values.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(value: &str) -> ShellWord {
        ShellWord {
            value: value.to_string(),
            literal: true,
            has_subs: false,
        }
    }

    fn words(values: &[&str]) -> Vec<ShellWord> {
        values.iter().map(|v| word(v)).collect()
    }

    #[test]
    fn test_shell_invocation() {
        // bash -c ls Command 2 -
        assert_eq!(
            shell_invocation(&words(&["bash", "-c", "ls"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(2),
                operand: None,
            })
        );
        // sh -lc ls Command 2 -
        assert_eq!(
            shell_invocation(&words(&["sh", "-lc", "ls"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(2),
                operand: None,
            })
        );
        // bash -c -- ls Command 3 -
        assert_eq!(
            shell_invocation(&words(&["bash", "-c", "--", "ls"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(3),
                operand: None,
            })
        );
        // bash -c Command none -
        assert_eq!(
            shell_invocation(&words(&["bash", "-c"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: None,
                operand: None,
            })
        );
        // bash Stdin
        assert_eq!(
            shell_invocation(&words(&["bash"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: None,
            })
        );
        // bash x.sh Script 1
        assert_eq!(
            shell_invocation(&words(&["bash", "x.sh"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Script,
                payload: Some(1),
                operand: None,
            })
        );
        // bash -s Stdin
        assert_eq!(
            shell_invocation(&words(&["bash", "-s"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: None,
            })
        );
        // bash -s x Stdin operand 2
        assert_eq!(
            shell_invocation(&words(&["bash", "-s", "x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: Some(2),
            })
        );
        // bash -s -- -c x Stdin operand 3
        assert_eq!(
            shell_invocation(&words(&["bash", "-s", "--", "-c", "x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: Some(3),
            })
        );
        // bash -- -c x Command 3
        assert_eq!(
            shell_invocation(&words(&["bash", "--", "-c", "x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(3),
                operand: None,
            })
        );
        // bash -O extglob x.sh Script 3
        assert_eq!(
            shell_invocation(&words(&["bash", "-O", "extglob", "x.sh"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Script,
                payload: Some(3),
                operand: None,
            })
        );
        // bash +O extglob -c ls Command 4
        assert_eq!(
            shell_invocation(&words(&["bash", "+O", "extglob", "-c", "ls"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(4),
                operand: None,
            })
        );
        // bash -x Stdin
        assert_eq!(
            shell_invocation(&words(&["bash", "-x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: None,
            })
        );
        // bash -- x Script 2
        assert_eq!(
            shell_invocation(&words(&["bash", "--", "x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Script,
                payload: Some(2),
                operand: None,
            })
        );
        // bash -s -c x Command 3
        assert_eq!(
            shell_invocation(&words(&["bash", "-s", "-c", "x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(3),
                operand: None,
            })
        );
        // bash --norc x Script 2
        assert_eq!(
            shell_invocation(&words(&["bash", "--norc", "x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Script,
                payload: Some(2),
                operand: None,
            })
        );
        // bash -- -s Stdin
        assert_eq!(
            shell_invocation(&words(&["bash", "--", "-s"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: None,
            })
        );
        // zsh -ec a Command 2
        assert_eq!(
            shell_invocation(&words(&["zsh", "-ec", "a"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(2),
                operand: None,
            })
        );
        // /bin/sh -c a Command 2
        assert_eq!(
            shell_invocation(&words(&["/bin/sh", "-c", "a"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(2),
                operand: None,
            })
        );
        // ls None
        assert_eq!(shell_invocation(&words(&["ls"]), 0), None);
        // bash -C x Script 2
        assert_eq!(
            shell_invocation(&words(&["bash", "-C", "x"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Script,
                payload: Some(2),
                operand: None,
            })
        );
        // bash -cx y Command 2
        assert_eq!(
            shell_invocation(&words(&["bash", "-cx", "y"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(2),
                operand: None,
            })
        );
        // bash -sx y Stdin operand 2
        assert_eq!(
            shell_invocation(&words(&["bash", "-sx", "y"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: Some(2),
            })
        );
        // bash -s -- Stdin, no operand
        assert_eq!(
            shell_invocation(&words(&["bash", "-s", "--"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: None,
            })
        );
        // bash -x -- -c y Command 4
        assert_eq!(
            shell_invocation(&words(&["bash", "-x", "--", "-c", "y"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Command,
                payload: Some(4),
                operand: None,
            })
        );
        // bash -O Stdin
        assert_eq!(
            shell_invocation(&words(&["bash", "-O"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: None,
            })
        );
        // bash -s -O x y Stdin operand 4
        assert_eq!(
            shell_invocation(&words(&["bash", "-s", "-O", "x", "y"]), 0),
            Some(ShellInvocation {
                kind: ShellKind::Stdin,
                payload: None,
                operand: Some(4),
            })
        );
        // shell_invocation(&[], 0) is None
        assert_eq!(shell_invocation(&[], 0), None);
        // shell_invocation(&words, 5) for 2 words is None
        assert_eq!(shell_invocation(&words(&["bash", "x"]), 5), None);
    }

    #[test]
    fn test_eval_payload() {
        // eval ls -l -> ls -l
        assert_eq!(
            eval_payload(&words(&["eval", "ls", "-l"]), 0),
            Some("ls -l".to_string())
        );
        // eval None
        assert_eq!(eval_payload(&words(&["eval"]), 0), None);
        // eval $x (word $x with literal=false) None
        assert_eq!(
            eval_payload(
                &[
                    word("eval"),
                    ShellWord {
                        value: "$x".to_string(),
                        literal: false,
                        has_subs: false,
                    },
                ],
                0
            ),
            None
        );
        // eval a "b c" -> a b c
        assert_eq!(
            eval_payload(&words(&["eval", "a", "b c"]), 0),
            Some("a b c".to_string())
        );
        // eval $(x) (has_subs) None
        assert_eq!(
            eval_payload(
                &[
                    word("eval"),
                    ShellWord {
                        value: "$(x)".to_string(),
                        literal: true,
                        has_subs: true,
                    },
                ],
                0
            ),
            None
        );
        // /bin/eval a -> a
        assert_eq!(
            eval_payload(&words(&["/bin/eval", "a"]), 0),
            Some("a".to_string())
        );
        // ls eval a (ci 0) None
        assert_eq!(eval_payload(&words(&["ls", "eval", "a"]), 0), None);
    }

    #[test]
    fn test_word_references_any() {
        let names = ["A", "B"];
        // $A true
        assert!(word_references_any("$A", &names));
        // ${A} true
        assert!(word_references_any("${A}", &names));
        // $AB false
        assert!(!word_references_any("$AB", &names));
        // ${A false
        assert!(!word_references_any("${A", &names));
        // $C$B true
        assert!(word_references_any("$C$B", &names));
        // $1A false
        assert!(!word_references_any("$1A", &names));
        // ${B}x true
        assert!(word_references_any("${B}x", &names));
        // $$A true
        assert!(word_references_any("$$A", &names));
        // ${ A} false
        assert!(!word_references_any("${ A}", &names));
        // x false
        assert!(!word_references_any("x", &names));
        // empty value false
        assert!(!word_references_any("", &names));
        // with names []: $A false
        assert!(!word_references_any("$A", &[]));
    }

    #[test]
    fn test_basename() {
        assert_eq!(basename("/usr/bin/sudo"), "sudo");
        assert_eq!(basename("a//"), "a");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename(""), "");
        assert_eq!(basename("abc"), "abc");
    }

    #[test]
    fn test_is_assignment() {
        assert!(is_assignment("A=1"));
        assert!(is_assignment("_x="));
        assert!(is_assignment("a1=b"));
        assert!(!is_assignment("1A=x"));
        assert!(!is_assignment("=x"));
        assert!(!is_assignment("A"));
        assert!(!is_assignment("A-B=1"));
    }
}
