//! Shell program splitting and command-position analysis.
//!
//! Operates on already-lexed shell tokens: [`split_program`] cuts a token
//! stream into nodes at separators, and [`command_position`] finds the word
//! that actually runs once leading assignments and wrapper commands (`sudo`,
//! `env`, `timeout`, ...) are skipped.
//!
//! Not a security boundary. This port matches the live reference policy, and
//! both share known gaps: wrappers outside the option tables hide a protected
//! command, macOS `env -P <dir>` is misread, and `env` options after an
//! assignment are misread. The `known_gap_*` tests pin these, and they are
//! tracked in DPES-963 and DPES-966. No security gate may use this module until
//! both this port and `bin/fm-arm-command-policy.mjs` are fixed and
//! independently reviewed.

// Canonical helpers live in `arm_shell_forms`.
use crate::arm_shell_forms::{basename, is_assignment};

/// Kind of a shell substitution found inside a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstitutionKind {
    /// Command substitution, `$(...)` or backticks.
    Command,
    /// Process substitution, `<(...)` or `>(...)`.
    Process,
}

/// A substitution embedded in a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    /// Which kind of substitution this is.
    pub kind: SubstitutionKind,
    /// Source text inside the substitution.
    pub content: String,
}

/// A shell word token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// Text of the word.
    pub value: String,
    /// Whether the word is a plain literal with no expansion.
    pub literal: bool,
    /// Substitutions embedded in the word.
    pub subs: Vec<Substitution>,
    /// Whether any part of the word was quoted.
    pub quoted: bool,
    /// Whether the word contains an unquoted expansion.
    pub unquoted_expansion: bool,
}

/// A redirection token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redir {
    /// Redirection operator text, such as `>` or `>&`.
    pub value: String,
    /// Whether the target is part of the operator, so no word follows it.
    pub inline_target: bool,
    /// File descriptor the redirection applies to.
    pub fd: u32,
    /// Here-document body, when the redirection carries one.
    pub heredoc: Option<String>,
}

/// Kind of a grouping construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// Subshell, `( ... )`.
    Subshell,
    /// Brace group, `{ ...; }`.
    Brace,
}

/// A grouping token holding the source text inside the group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Which kind of group this is.
    pub kind: GroupKind,
    /// Source text inside the group.
    pub content: String,
}

/// One lexed shell token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Control operator or separator, such as `&&`, `;` or `newline`.
    Op(String),
    /// Redirection.
    Redir(Redir),
    /// Subshell or brace group.
    Group(Group),
    /// Ordinary word.
    Word(Word),
}

/// A token stream cut into nodes at separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Tokens of each node, in source order.
    pub nodes: Vec<Vec<Token>>,
    /// Separators between nodes, in source order.
    pub separators: Vec<String>,
}

/// Splits `tokens` into nodes at [`Token::Op`] separators.
///
/// Empty nodes are not produced. A leading separator is kept in
/// [`Program::separators`] unless it is `newline`, and trailing `newline`
/// separators are dropped.
pub fn split_program(tokens: &[Token]) -> Program {
    let mut nodes: Vec<Vec<Token>> = Vec::new();
    let mut separators: Vec<String> = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    for token in tokens {
        match token {
            Token::Op(value) => {
                if !current.is_empty() {
                    nodes.push(std::mem::take(&mut current));
                    separators.push(value.clone());
                } else if value != "newline" {
                    separators.push(value.clone());
                }
            }
            other => current.push(other.clone()),
        }
    }
    if !current.is_empty() {
        nodes.push(current);
    }
    while separators.len() >= nodes.len() && separators.last().is_some_and(|last| last == "newline")
    {
        separators.pop();
    }
    Program { nodes, separators }
}

/// Result of finding the command word of one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPosition {
    /// Words of the node, with redirections and their targets removed.
    pub words: Vec<Word>,
    /// Index into `words` of the word that runs, after skipping assignments
    /// and wrappers. It equals `words.len()` when no command word remains.
    pub index: usize,
    /// Wrapper command names skipped, in order.
    pub wrappers: Vec<String>,
    /// Number of leading variable assignments.
    pub prefix_assignments: usize,
    /// Whether a wrapper option could not be interpreted, so `index` may be
    /// wrong.
    pub unresolved_wrapper_option: bool,
    /// Command strings passed through `env -S` or `env --split-string`.
    pub wrapper_payloads: Vec<String>,
}

impl CommandPosition {
    /// Returns the word at [`CommandPosition::index`], if there is one.
    pub fn command(&self) -> Option<&Word> {
        self.words.get(self.index)
    }
}

/// Option tables of one wrapper command.
struct WrapperOptions {
    short_no_arg: &'static str,
    short_takes_arg: &'static str,
    long_no_arg: &'static [&'static str],
    long_takes_arg: &'static [&'static str],
}

const EMPTY_OPTIONS: WrapperOptions = WrapperOptions {
    short_no_arg: "",
    short_takes_arg: "",
    long_no_arg: &[],
    long_takes_arg: &[],
};

/// Returns the option tables for wrapper `owner`.
fn wrapper_options(owner: &str) -> WrapperOptions {
    match owner {
        "command" => WrapperOptions {
            short_no_arg: "pvV",
            long_no_arg: &["help", "version"],
            ..EMPTY_OPTIONS
        },
        "env" => WrapperOptions {
            short_no_arg: "0iPv",
            short_takes_arg: "aCSu",
            long_no_arg: &["ignore-environment", "null", "help", "version"],
            long_takes_arg: &[
                "argv0",
                "block-signal",
                "chdir",
                "default-signal",
                "ignore-signal",
                "split-string",
                "unset",
            ],
        },
        "exec" => WrapperOptions {
            short_no_arg: "cl",
            short_takes_arg: "a",
            ..EMPTY_OPTIONS
        },
        "nohup" => WrapperOptions {
            long_no_arg: &["help", "version"],
            ..EMPTY_OPTIONS
        },
        "sudo" => WrapperOptions {
            short_no_arg: "ABbEeHiKklNnPSsvV",
            short_takes_arg: "CDghprRtTuU",
            long_no_arg: &[
                "askpass",
                "background",
                "bell",
                "edit",
                "help",
                "login",
                "non-interactive",
                "preserve-env",
                "preserve-groups",
                "remove-timestamp",
                "reset-timestamp",
                "set-home",
                "shell",
                "stdin",
                "validate",
                "version",
            ],
            long_takes_arg: &[
                "chdir",
                "chroot",
                "close-from",
                "command-timeout",
                "group",
                "host",
                "other-user",
                "prompt",
                "role",
                "type",
                "user",
            ],
        },
        "timeout" => WrapperOptions {
            short_no_arg: "fpv",
            short_takes_arg: "ks",
            long_no_arg: &[
                "foreground",
                "preserve-status",
                "verbose",
                "help",
                "version",
            ],
            long_takes_arg: &["kill-after", "signal"],
        },
        _ => EMPTY_OPTIONS,
    }
}

/// Outcome of skipping the options of one wrapper command.
struct ConsumedOptions {
    index: usize,
    unresolved: bool,
    payloads: Vec<String>,
}

/// Skips the options of wrapper `name` starting at `words[start]`.
fn consume_wrapper_options(name: &str, words: &[Word], start: usize) -> ConsumedOptions {
    let owner = if name == "gtimeout" { "timeout" } else { name };
    let options = wrapper_options(owner);
    let is_env = name == "env";
    let mut payloads: Vec<String> = Vec::new();
    let mut next = start;
    let done = |index: usize, unresolved: bool, payloads: Vec<String>| ConsumedOptions {
        index,
        unresolved,
        payloads,
    };
    while let Some(word) = words.get(next) {
        let value = word.value.as_str();
        if value == "--" {
            return done(next + 1, false, payloads);
        }
        if !value.starts_with('-') || value == "-" {
            return done(next, false, payloads);
        }
        if let Some(long) = value.strip_prefix("--") {
            let (opt, inline_arg) = match long.split_once('=') {
                Some((opt, arg)) => (opt, Some(arg)),
                None => (long, None),
            };
            if options.long_no_arg.contains(&opt) {
                next += 1;
                continue;
            }
            if !options.long_takes_arg.contains(&opt) {
                return done(next, true, payloads);
            }
            let split_string = is_env && opt == "split-string";
            if let Some(arg) = inline_arg {
                if split_string {
                    payloads.push(arg.to_string());
                }
                next += 1;
            } else {
                let Some(arg_word) = words.get(next + 1) else {
                    return done(next, true, payloads);
                };
                if split_string {
                    payloads.push(arg_word.value.clone());
                }
                next += 2;
            }
            continue;
        }
        let mut cluster_consumed = false;
        for (offset, o) in value.char_indices().skip(1) {
            if options.short_no_arg.contains(o) {
                continue;
            }
            if !options.short_takes_arg.contains(o) {
                return done(next, true, payloads);
            }
            let rest = &value[offset + o.len_utf8()..];
            if rest.is_empty() {
                let Some(arg_word) = words.get(next + 1) else {
                    return done(next, true, payloads);
                };
                if is_env && o == 'S' {
                    payloads.push(arg_word.value.clone());
                }
                next += 2;
            } else {
                if is_env && o == 'S' {
                    payloads.push(rest.to_string());
                }
                next += 1;
            }
            cluster_consumed = true;
            break;
        }
        if !cluster_consumed {
            next += 1;
        }
    }
    done(next, false, payloads)
}

/// Collects the words of a node, dropping redirections and their targets.
fn words_in_node(tokens: &[Token]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut skip = false;
    for token in tokens {
        match token {
            Token::Redir(redir) => skip = !redir.inline_target,
            Token::Word(word) => {
                if skip {
                    skip = false;
                } else {
                    words.push(word.clone());
                }
            }
            Token::Op(_) | Token::Group(_) => {}
        }
    }
    words
}

/// Finds the command word of one node, skipping leading assignments and
/// wrapper commands such as `sudo`, `env`, `timeout`, `command`, `exec` and
/// `nohup` together with their options.
///
/// When a wrapper option cannot be interpreted,
/// [`CommandPosition::unresolved_wrapper_option`] is set and `index` points
/// at the word where interpretation stopped.
pub fn command_position(tokens: &[Token]) -> CommandPosition {
    let words = words_in_node(tokens);
    let mut index = words
        .iter()
        .take_while(|word| is_assignment(&word.value))
        .count();
    let prefix_assignments = index;
    let mut wrappers: Vec<String> = Vec::new();
    let mut wrapper_payloads: Vec<String> = Vec::new();
    let mut unresolved = false;
    while let Some(word) = words.get(index) {
        let name = basename(&word.value);
        match name {
            "exec" | "command" | "sudo" | "nohup" | "env" => {
                wrappers.push(name.to_string());
                let consumed = consume_wrapper_options(name, &words, index + 1);
                unresolved |= consumed.unresolved;
                wrapper_payloads.extend(consumed.payloads);
                index = consumed.index;
                if name == "env" {
                    while words
                        .get(index)
                        .is_some_and(|w| w.value.starts_with('-') || is_assignment(&w.value))
                    {
                        index += 1;
                    }
                }
            }
            "timeout" | "gtimeout" => {
                wrappers.push(name.to_string());
                let consumed = consume_wrapper_options(name, &words, index + 1);
                unresolved |= consumed.unresolved;
                index = consumed.index;
                if words.get(index).is_none() {
                    unresolved = true;
                    break;
                }
                index += 1;
                if words.get(index).is_none() {
                    unresolved = true;
                    break;
                }
            }
            _ => break,
        }
    }
    CommandPosition {
        words,
        index,
        wrappers,
        prefix_assignments,
        unresolved_wrapper_option: unresolved,
        wrapper_payloads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(value: &str) -> Word {
        Word {
            value: value.to_string(),
            literal: true,
            subs: Vec::new(),
            quoted: false,
            unquoted_expansion: false,
        }
    }

    fn op(value: &str) -> Token {
        Token::Op(value.to_string())
    }

    fn word(value: &str) -> Token {
        Token::Word(w(value))
    }

    fn redir(value: &str, fd: u32, inline_target: bool) -> Token {
        Token::Redir(Redir {
            value: value.to_string(),
            inline_target,
            fd,
            heredoc: None,
        })
    }

    /// Builds word tokens from plain strings.
    fn words(values: &[&str]) -> Vec<Token> {
        values.iter().map(|v| word(v)).collect()
    }

    /// Builds a stream where `&&`, `;`, `|`, `&`, `;;` and `newline` become
    /// operators and everything else a word.
    fn stream(values: &[&str]) -> Vec<Token> {
        values
            .iter()
            .map(|v| match *v {
                "&&" | ";" | "|" | "&" | ";;" | "newline" => op(v),
                _ => word(v),
            })
            .collect()
    }

    fn split(values: &[&str]) -> (Vec<String>, usize) {
        let program = split_program(&stream(values));
        (program.separators, program.nodes.len())
    }

    fn seps(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn split_simple_and_empty() {
        assert_eq!(split(&["ls"]), (seps(&[]), 1));
        assert_eq!(split(&[]), (seps(&[]), 0));
    }

    #[test]
    fn split_at_operators() {
        assert_eq!(split(&["cd", "/tmp", "&&", "ls"]), (seps(&["&&"]), 2));
        assert_eq!(
            split(&["echo", "hi", "|", "cat", ";", "ls", "&"]),
            (seps(&["|", ";", "&"]), 3)
        );
    }

    #[test]
    fn split_newline_handling() {
        assert_eq!(split(&["ls", "newline", "newline"]), (seps(&[]), 1));
        assert_eq!(split(&["a", "&&", "newline", "b"]), (seps(&["&&"]), 2));
        assert_eq!(
            split(&["a", ";", "newline", ";", "b"]),
            (seps(&[";", ";"]), 2)
        );
        assert_eq!(split(&["newline", "ls"]), (seps(&[]), 1));
        assert_eq!(split(&[";", "ls"]), (seps(&[";"]), 1));
        assert_eq!(split(&["echo", "a", ";;", "b"]), (seps(&[";;"]), 2));
        assert_eq!(split(&["a", "newline", "b"]), (seps(&["newline"]), 2));
    }

    #[test]
    fn split_keeps_node_tokens() {
        let program = split_program(&stream(&["a", "b", "&&", "c"]));
        assert_eq!(program.nodes, vec![words(&["a", "b"]), words(&["c"])]);
    }

    fn word_values(tokens: &[Token]) -> Vec<String> {
        command_position(tokens)
            .words
            .into_iter()
            .map(|w| w.value)
            .collect()
    }

    #[test]
    fn words_in_node_drops_redirect_targets() {
        let tokens = vec![word("ls"), redir(">", 1, false), word("out"), word("x")];
        assert_eq!(word_values(&tokens), seps(&["ls", "x"]));
        let tokens = vec![word("ls"), redir(">&", 2, true), word("x")];
        assert_eq!(word_values(&tokens), seps(&["ls", "x"]));
        let group = Token::Group(Group {
            kind: GroupKind::Subshell,
            content: "x".to_string(),
        });
        // A group is ignored and does not reset the pending target skip.
        let tokens = vec![redir(">&", 2, true), group.clone(), word("a")];
        assert_eq!(word_values(&tokens), seps(&["a"]));
        let tokens = vec![redir(">", 1, false), group, word("a")];
        assert_eq!(word_values(&tokens), seps(&[]));
    }

    /// Every wrapper-table entry as the reference lists it. Kept as a literal on
    /// purpose: a test that read the tables from the code would not notice a
    /// removed entry.
    const EXPECTED_ENTRIES: &[(&str, &str, bool)] = &[
        ("command", "-p", false),
        ("command", "-v", false),
        ("command", "-V", false),
        ("env", "-0", false),
        ("env", "-i", false),
        ("env", "-P", false),
        ("env", "-v", false),
        ("env", "-a", true),
        ("env", "-C", true),
        ("env", "-S", true),
        ("env", "-u", true),
        ("exec", "-c", false),
        ("exec", "-l", false),
        ("exec", "-a", true),
        ("sudo", "-A", false),
        ("sudo", "-B", false),
        ("sudo", "-b", false),
        ("sudo", "-E", false),
        ("sudo", "-e", false),
        ("sudo", "-H", false),
        ("sudo", "-i", false),
        ("sudo", "-K", false),
        ("sudo", "-k", false),
        ("sudo", "-l", false),
        ("sudo", "-N", false),
        ("sudo", "-n", false),
        ("sudo", "-P", false),
        ("sudo", "-S", false),
        ("sudo", "-s", false),
        ("sudo", "-v", false),
        ("sudo", "-V", false),
        ("sudo", "-C", true),
        ("sudo", "-D", true),
        ("sudo", "-g", true),
        ("sudo", "-h", true),
        ("sudo", "-p", true),
        ("sudo", "-r", true),
        ("sudo", "-R", true),
        ("sudo", "-t", true),
        ("sudo", "-T", true),
        ("sudo", "-u", true),
        ("sudo", "-U", true),
        ("timeout", "-f", false),
        ("timeout", "-p", false),
        ("timeout", "-v", false),
        ("timeout", "-k", true),
        ("timeout", "-s", true),
        ("command", "--help", false),
        ("command", "--version", false),
        ("env", "--ignore-environment", false),
        ("env", "--null", false),
        ("env", "--help", false),
        ("env", "--version", false),
        ("env", "--argv0", true),
        ("env", "--block-signal", true),
        ("env", "--chdir", true),
        ("env", "--default-signal", true),
        ("env", "--ignore-signal", true),
        ("env", "--split-string", true),
        ("env", "--unset", true),
        ("nohup", "--help", false),
        ("nohup", "--version", false),
        ("sudo", "--askpass", false),
        ("sudo", "--background", false),
        ("sudo", "--bell", false),
        ("sudo", "--edit", false),
        ("sudo", "--help", false),
        ("sudo", "--login", false),
        ("sudo", "--non-interactive", false),
        ("sudo", "--preserve-env", false),
        ("sudo", "--preserve-groups", false),
        ("sudo", "--remove-timestamp", false),
        ("sudo", "--reset-timestamp", false),
        ("sudo", "--set-home", false),
        ("sudo", "--shell", false),
        ("sudo", "--stdin", false),
        ("sudo", "--validate", false),
        ("sudo", "--version", false),
        ("sudo", "--chdir", true),
        ("sudo", "--chroot", true),
        ("sudo", "--close-from", true),
        ("sudo", "--command-timeout", true),
        ("sudo", "--group", true),
        ("sudo", "--host", true),
        ("sudo", "--other-user", true),
        ("sudo", "--prompt", true),
        ("sudo", "--role", true),
        ("sudo", "--type", true),
        ("sudo", "--user", true),
        ("timeout", "--foreground", false),
        ("timeout", "--preserve-status", false),
        ("timeout", "--verbose", false),
        ("timeout", "--help", false),
        ("timeout", "--version", false),
        ("timeout", "--kill-after", true),
        ("timeout", "--signal", true),
    ];

    /// Each expected entry must be consumed as documented: a flag alone, or a
    /// flag with its argument, in separate or inline form for long options. A
    /// removed or mistyped table entry leaves an option unresolved or moves the
    /// command index, which this test catches.
    #[test]
    fn wrapper_table_entries_are_consumed() {
        for &(owner, option, takes_arg) in EXPECTED_ENTRIES {
            let tail: &[&str] = if owner == "timeout" {
                &["5", "ls"]
            } else {
                &["ls"]
            };
            if !takes_arg {
                assert_consumed(owner, vec![option.to_string()], tail);
            } else if option.starts_with("--") {
                assert_consumed(owner, vec![option.to_string(), "ARG".to_string()], tail);
                assert_consumed(owner, vec![format!("{option}=ARG")], tail);
            } else {
                assert_consumed(owner, vec![option.to_string(), "ARG".to_string()], tail);
            }
        }
    }

    fn assert_consumed(owner: &str, options: Vec<String>, tail: &[&str]) {
        let mut values: Vec<String> = vec![owner.to_string()];
        values.extend(options);
        values.extend(tail.iter().map(|word| word.to_string()));
        let line = values.join(" ");
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let got = command_position(&words(&refs));
        assert!(!got.unresolved_wrapper_option, "unresolved for `{line}`");
        assert_eq!(got.index, values.len() - 1, "index for `{line}`");
        assert_eq!(
            got.command().map(|c| c.value.as_str()),
            Some("ls"),
            "command for `{line}`"
        );
    }

    fn position(line: &str) -> CommandPosition {
        let values: Vec<&str> = line.split_whitespace().collect();
        command_position(&words(&values))
    }

    /// One position case: input words, expected index, expected command
    /// word, and whether a wrapper option stays unresolved.
    type Case = (&'static str, usize, Option<&'static str>, bool);

    fn check_cases(cases: &[Case]) {
        for &(line, index, command, unresolved) in cases {
            let got = position(line);
            assert_eq!(got.index, index, "index for `{line}`");
            assert_eq!(
                got.command().map(|c| c.value.as_str()),
                command,
                "command for `{line}`"
            );
            assert_eq!(
                got.unresolved_wrapper_option, unresolved,
                "unresolved for `{line}`"
            );
        }
    }

    #[test]
    fn position_plain_and_assignments() {
        check_cases(&[
            ("cd /tmp", 0, Some("cd"), false),
            ("FOO=1", 1, None, false),
            ("a=1 b=2 c", 2, Some("c"), false),
            ("echo hi", 0, Some("echo"), false),
            ("", 0, None, false),
        ]);
        assert_eq!(position("a=1 b=2 c").prefix_assignments, 2);
        assert_eq!(position("FOO=1").prefix_assignments, 1);
        let empty = position("");
        assert!(empty.words.is_empty() && empty.wrappers.is_empty());
        assert!(empty.wrapper_payloads.is_empty());
    }

    #[test]
    fn position_wrapper_names() {
        let got = position("FOO=1 sudo -u root env X=2 cd x");
        assert_eq!((got.index, got.prefix_assignments), (6, 1));
        assert_eq!(got.wrappers, seps(&["sudo", "env"]));
        assert!(got.wrapper_payloads.is_empty());
        let got = position("timeout -k 5 10 cd x");
        assert_eq!(got.index, 4);
        assert_eq!(got.wrappers, seps(&["timeout"]));
        assert_eq!(position("gtimeout 5 ls").wrappers, seps(&["gtimeout"]));
        assert_eq!(position("/usr/bin/sudo ls").wrappers, seps(&["sudo"]));
        assert_eq!(
            position("nohup nohup ls").wrappers,
            seps(&["nohup", "nohup"])
        );
        assert_eq!(position("command -v cd").wrappers, seps(&["command"]));
        let got = position("sudo env timeout 5 command cd d");
        assert_eq!(got.wrappers, seps(&["sudo", "env", "timeout", "command"]));
    }

    #[test]
    fn position_env_split_string_payloads() {
        let tokens = vec![word("env"), word("-S"), word("cd x"), word("ls")];
        let got = command_position(&tokens);
        assert_eq!(got.index, 3);
        assert_eq!(got.wrappers, seps(&["env"]));
        assert_eq!(got.wrapper_payloads, seps(&["cd x"]));
        for (line, index, payload) in [
            ("env --split-string=a x", 2, "a"),
            ("env --split-string a x", 3, "a"),
            ("env -Sfoo x", 2, "foo"),
        ] {
            let got = position(line);
            assert_eq!(got.index, index, "index for `{line}`");
            assert_eq!(
                got.wrapper_payloads,
                seps(&[payload]),
                "payload for `{line}`"
            );
        }
    }

    #[test]
    fn position_timeout_does_not_collect_payloads() {
        assert!(position("timeout -s KILL 5 ls").wrapper_payloads.is_empty());
    }

    #[test]
    fn position_env_cases() {
        check_cases(&[
            ("env --unknown x", 2, Some("x"), true),
            ("env -- cd", 2, Some("cd"), false),
            ("env -", 2, None, false),
            ("env -u", 2, None, true),
            ("env -0 ls", 2, Some("ls"), false),
            ("env -a x -C d ls", 5, Some("ls"), false),
            ("env A=1 B=2", 3, None, false),
            ("env --unset=x ls", 2, Some("ls"), false),
            ("env --unset x ls", 3, Some("ls"), false),
        ]);
    }

    #[test]
    fn position_sudo_cases() {
        check_cases(&[
            ("sudo -u", 1, Some("-u"), true),
            ("sudo -- ls", 2, Some("ls"), false),
            ("sudo -nu root ls", 3, Some("ls"), false),
            ("sudo -uroot ls", 2, Some("ls"), false),
            ("sudo -Z ls", 1, Some("-Z"), true),
            ("sudo --user root ls", 3, Some("ls"), false),
            ("sudo --user", 1, Some("--user"), true),
            ("sudo --user=root ls", 2, Some("ls"), false),
            ("sudo --bogus ls", 1, Some("--bogus"), true),
            ("sudo -- -u", 2, Some("-u"), false),
            ("sudo -", 1, Some("-"), false),
            ("sudo -V ls", 2, Some("ls"), false),
        ]);
    }

    #[test]
    fn position_timeout_cases() {
        check_cases(&[
            ("timeout 5", 2, None, true),
            ("timeout -k", 2, None, true),
            ("timeout -v", 2, None, true),
            ("timeout -- 5 ls", 3, Some("ls"), false),
            ("gtimeout 5 ls", 2, Some("ls"), false),
            ("gtimeout -s KILL 5 ls", 4, Some("ls"), false),
            ("timeout --kill-after=3 5 cd", 3, Some("cd"), false),
            ("timeout --foreground 5 x", 3, Some("x"), false),
        ]);
    }

    #[test]
    fn position_other_wrapper_cases() {
        check_cases(&[
            ("command -v cd", 2, Some("cd"), false),
            ("command -p -- cd x", 3, Some("cd"), false),
            ("command -x cd", 1, Some("-x"), true),
            ("nohup -x ls", 1, Some("-x"), true),
            ("nohup nohup ls", 2, Some("ls"), false),
            ("exec -a name ls", 3, Some("ls"), false),
            ("exec -c ls", 2, Some("ls"), false),
            ("exec -l ls", 2, Some("ls"), false),
            ("/usr/bin/sudo ls", 1, Some("ls"), false),
            ("sudo env timeout 5 command cd d", 5, Some("cd"), false),
        ]);
    }

    /// Known gap, DPES-963 and DPES-966. Wrappers outside the tables (`nice`,
    /// `xargs`, `setsid`, `ionice`, `stdbuf`, `caffeinate`, `doas`, `chroot`,
    /// `builtin`) are reported as the command, so a protected command after
    /// them is never inspected. The live reference behaves the same way. This
    /// test pins that behavior; a fix must change it on purpose.
    #[test]
    fn known_gap_unlisted_wrapper_hides_later_command() {
        for wrapper in [
            "nice",
            "xargs",
            "setsid",
            "ionice",
            "stdbuf",
            "caffeinate",
            "doas",
            "chroot",
            "builtin",
        ] {
            let got = position(&format!("{wrapper} bin/fm-watch.sh"));
            assert_eq!(got.index, 0, "index for `{wrapper}`");
            assert_eq!(
                got.command().map(|c| c.value.as_str()),
                Some(wrapper),
                "command for `{wrapper}`"
            );
            assert!(!got.unresolved_wrapper_option, "unresolved for `{wrapper}`");
        }
    }

    /// Known gap, DPES-966. macOS `env -P <utilpath>` takes an argument, but
    /// the table lists `P` as a flag, so the path is reported as the command.
    /// Verified on macOS: `env -P /tmp /bin/echo` consumes `/tmp`.
    #[test]
    fn known_gap_env_p_takes_argument() {
        check_cases(&[("env -P /tmp bin/fm-watch.sh", 2, Some("/tmp"), false)]);
    }

    /// Known gap, DPES-966. macOS `env` stops option parsing at the first
    /// assignment, so `-u` and `-x` after one are not options. The port skips
    /// them and reports the next word. Verified on macOS: `env A=1 -u B` and
    /// `env A=1 -x B=2` both fail with `No such file or directory`. The
    /// reported command is never run, so this is over-reporting, not a bypass.
    #[test]
    fn known_gap_env_option_after_assignment() {
        check_cases(&[
            ("env -i A=1 -u B cmd", 4, Some("B"), false),
            ("env A=1 -x B=2 cmd", 4, Some("cmd"), false),
        ]);
    }
}
