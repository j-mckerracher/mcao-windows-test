//! Semantic policy for the cd-guard: detects persistent directory changes.

/// One top-level command node after lexing and command-position analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdNode {
    /// Values of the command node's words.
    pub words: Vec<String>,
    /// Command word index in [`words`](Self::words).
    pub index: usize,
    /// Count of leading variable assignments before the command word.
    pub prefix_assignments: usize,
    /// Command wrapper names preceding the command word.
    pub wrappers: Vec<String>,
}

/// Policy decision for whether a command is permitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdDecision {
    /// The command is allowed to execute.
    Allow,
    /// The command was denied by policy.
    Deny {
        /// Stable machine-readable error code.
        code: &'static str,
        /// Human-readable explanation.
        reason: &'static str,
    },
}

/// Error code emitted when a persistent directory change is denied.
pub const CODE_PERSISTENT_CD: &str = "persistent-cd";

/// Explanation emitted when a persistent directory change is denied.
pub const REASON_PERSISTENT_CD: &str = "a persistent top-level directory change in the primary firstmate checkout is blocked; it would move the shell out of the home so a later firstmate-owned command runs inside a project clone. Reach the target without moving the shell - use git -C <dir> or an absolute path on the command itself - or scope the cd to a subshell like (cd <dir> && ...).";

fn is_pipe(separator: &str) -> bool {
    separator == "|" || separator == "|&"
}

fn node_persists(separators: &[String], index: usize) -> bool {
    if separators
        .get(index)
        .is_some_and(|sep| sep == "&" || is_pipe(sep))
    {
        return false;
    }
    if index > 0 && separators.get(index - 1).is_some_and(|sep| is_pipe(sep)) {
        return false;
    }
    true
}

fn has_path_qualified_command_prefix(prefix: &[String]) -> bool {
    prefix
        .iter()
        .any(|word| word.contains('/') && word.rsplit('/').next() == Some("command"))
}

fn is_command_query_flag(word: &str) -> bool {
    let mut chars = word.chars();
    if chars.next() != Some('-') {
        return false;
    }
    for c in chars {
        if c == '-' {
            return false;
        }
        if c == 'v' || c == 'V' {
            return true;
        }
    }
    false
}

fn has_command_query_prefix(prefix: &[String]) -> bool {
    let mut command_prefix = false;
    for word in prefix {
        if word == "command" {
            command_prefix = true;
            continue;
        }
        if command_prefix && is_command_query_flag(word) {
            return true;
        }
    }
    false
}

/// Decides whether a shell command persistently changes the primary working directory.
pub fn decide(lex_error: bool, nodes: &[CdNode], separators: &[String]) -> CdDecision {
    if lex_error {
        return CdDecision::Allow;
    }

    for (i, node) in nodes.iter().enumerate() {
        if !node_persists(separators, i) {
            continue;
        }

        let word_count = node.words.len();
        let start = node.prefix_assignments.min(word_count);
        let end = node.index.min(word_count);
        let prefix = if start < end {
            &node.words[start..end]
        } else {
            &[][..]
        };

        if has_path_qualified_command_prefix(prefix) {
            continue;
        }
        if has_command_query_prefix(prefix) {
            continue;
        }

        let mut j = node.index;
        let mut command = node.words.get(j).map(|s| s.as_str());
        while let Some("builtin" | "command") = command {
            j += 1;
            command = node.words.get(j).map(|s| s.as_str());
        }

        let Some(command) = command else {
            continue;
        };

        if !matches!(command, "cd" | "pushd" | "popd") {
            continue;
        }

        if node.wrappers.iter().any(|w| {
            matches!(
                w.as_str(),
                "env" | "sudo" | "nohup" | "timeout" | "gtimeout" | "exec"
            )
        }) {
            continue;
        }

        return CdDecision::Deny {
            code: CODE_PERSISTENT_CD,
            reason: REASON_PERSISTENT_CD,
        };
    }

    CdDecision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(words: &[&str], index: usize, prefix_assignments: usize, wrappers: &[&str]) -> CdNode {
        CdNode {
            words: words.iter().map(|s| (*s).to_string()).collect(),
            index,
            prefix_assignments,
            wrappers: wrappers.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn seps(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn assert_allow(lex_error: bool, nodes: &[CdNode], separators: &[String]) {
        assert_eq!(decide(lex_error, nodes, separators), CdDecision::Allow);
    }

    fn assert_deny(lex_error: bool, nodes: &[CdNode], separators: &[String]) {
        assert_eq!(
            decide(lex_error, nodes, separators),
            CdDecision::Deny {
                code: CODE_PERSISTENT_CD,
                reason: REASON_PERSISTENT_CD,
            }
        );
    }

    #[test]
    fn test_reason_and_code_constants() {
        assert_eq!(CODE_PERSISTENT_CD, "persistent-cd");
        assert!(REASON_PERSISTENT_CD.starts_with("a persistent top-level directory change"));
        assert!(REASON_PERSISTENT_CD.ends_with("(cd <dir> && ...)."));
    }

    #[test]
    fn test_cd_alone_and_chains() {
        // cd x: N([cd,x],0,0,[]) seps [] -> D
        assert_deny(false, &[node(&["cd", "x"], 0, 0, &[])], &[]);

        // cd x && ls: N([cd,x],0,0,[]) N([ls],0,0,[]) seps [&&] -> D
        assert_deny(
            false,
            &[node(&["cd", "x"], 0, 0, &[]), node(&["ls"], 0, 0, &[])],
            &seps(&["&&"]),
        );

        // ls; cd x: [ls] then [cd,x] seps [;] -> D
        assert_deny(
            false,
            &[node(&["ls"], 0, 0, &[]), node(&["cd", "x"], 0, 0, &[])],
            &seps(&[";"]),
        );

        // ls &\ncd x: seps [&] -> D (second node)
        assert_deny(
            false,
            &[node(&["ls"], 0, 0, &[]), node(&["cd", "x"], 0, 0, &[])],
            &seps(&["&"]),
        );

        // cd x & : seps [&] one node -> A
        assert_allow(false, &[node(&["cd", "x"], 0, 0, &[])], &seps(&["&"]));

        // cd x | cat: seps [|] -> A
        assert_allow(
            false,
            &[node(&["cd", "x"], 0, 0, &[]), node(&["cat"], 0, 0, &[])],
            &seps(&["|"]),
        );

        // cat | cd x: seps [|] -> A
        assert_allow(
            false,
            &[node(&["cat"], 0, 0, &[]), node(&["cd", "x"], 0, 0, &[])],
            &seps(&["|"]),
        );

        // a | b; cd x: three nodes seps [|,;] -> D
        assert_deny(
            false,
            &[
                node(&["a"], 0, 0, &[]),
                node(&["b"], 0, 0, &[]),
                node(&["cd", "x"], 0, 0, &[]),
            ],
            &seps(&["|", ";"]),
        );

        // cd x |& cat: seps [|&] -> A
        assert_allow(
            false,
            &[node(&["cd", "x"], 0, 0, &[]), node(&["cat"], 0, 0, &[])],
            &seps(&["|&"]),
        );

        // ls | cd x ; cd y: seps [|,;] -> D
        assert_deny(
            false,
            &[
                node(&["ls"], 0, 0, &[]),
                node(&["cd", "x"], 0, 0, &[]),
                node(&["cd", "y"], 0, 0, &[]),
            ],
            &seps(&["|", ";"]),
        );

        // a & b ; cd z: seps [&,;] -> D
        assert_deny(
            false,
            &[
                node(&["a"], 0, 0, &[]),
                node(&["b"], 0, 0, &[]),
                node(&["cd", "z"], 0, 0, &[]),
            ],
            &seps(&["&", ";"]),
        );

        // cd x\ncd y: seps [newline] -> D
        assert_deny(
            false,
            &[node(&["cd", "x"], 0, 0, &[]), node(&["cd", "y"], 0, 0, &[])],
            &seps(&["\n"]),
        );
    }

    #[test]
    fn test_groups() {
        // Groups: (cd x) and { cd x; }: N([],0,0,[]) -> A
        assert_allow(false, &[node(&[], 0, 0, &[])], &[]);
    }

    #[test]
    fn test_wrappers() {
        // sudo cd x: N([sudo,cd,x],1,0,[sudo]) -> A
        assert_allow(false, &[node(&["sudo", "cd", "x"], 1, 0, &["sudo"])], &[]);

        // env cd x
        assert_allow(false, &[node(&["env", "cd", "x"], 1, 0, &["env"])], &[]);

        // nohup cd x
        assert_allow(false, &[node(&["nohup", "cd", "x"], 1, 0, &["nohup"])], &[]);

        // exec cd x
        assert_allow(false, &[node(&["exec", "cd", "x"], 1, 0, &["exec"])], &[]);

        // timeout 5 cd x (N([timeout,5,cd,x],2,0,[timeout])) -> A
        assert_allow(
            false,
            &[node(&["timeout", "5", "cd", "x"], 2, 0, &["timeout"])],
            &[],
        );

        // gtimeout cd x
        assert_allow(
            false,
            &[node(&["gtimeout", "cd", "x"], 1, 0, &["gtimeout"])],
            &[],
        );

        // command cd x: N([command,cd,x],1,0,[command]) -> D
        assert_deny(
            false,
            &[node(&["command", "cd", "x"], 1, 0, &["command"])],
            &[],
        );

        // command builtin cd x: N([command,builtin,cd,x],1,0,[command]) -> D
        assert_deny(
            false,
            &[node(&["command", "builtin", "cd", "x"], 1, 0, &["command"])],
            &[],
        );

        // builtin cd x: N([builtin,cd,x],0,0,[]) -> D
        assert_deny(false, &[node(&["builtin", "cd", "x"], 0, 0, &[])], &[]);

        // builtin command cd x: N([builtin,command,cd,x],0,0,[]) -> D
        assert_deny(
            false,
            &[node(&["builtin", "command", "cd", "x"], 0, 0, &[])],
            &[],
        );

        // command -p cd x: N([command,-p,cd,x],2,0,[command]) -> D
        assert_deny(
            false,
            &[node(&["command", "-p", "cd", "x"], 2, 0, &["command"])],
            &[],
        );

        // x=1 command cd x: N([x=1,command,cd,x],2,1,[command]) -> D
        assert_deny(
            false,
            &[node(&["x=1", "command", "cd", "x"], 2, 1, &["command"])],
            &[],
        );

        // FOO=1 cd x: N([FOO=1,cd,x],1,1,[]) -> D
        assert_deny(false, &[node(&["FOO=1", "cd", "x"], 1, 1, &[])], &[]);
    }

    #[test]
    fn test_query_forms() {
        // command -v cd: N([command,-v,cd],2,0,[command]) -> A
        assert_allow(
            false,
            &[node(&["command", "-v", "cd"], 2, 0, &["command"])],
            &[],
        );

        // command -V cd -> A
        assert_allow(
            false,
            &[node(&["command", "-V", "cd"], 2, 0, &["command"])],
            &[],
        );

        // /usr/bin/command cd x: N([/usr/bin/command,cd,x],1,0,[command]) -> A
        assert_allow(
            false,
            &[node(&["/usr/bin/command", "cd", "x"], 1, 0, &["command"])],
            &[],
        );

        // command: N([command],1,0,[command]) -> A (no command word)
        assert_allow(false, &[node(&["command"], 1, 0, &["command"])], &[]);

        // command -x cd: N([command,-x,cd],1,0,[command]) -> A
        assert_allow(
            false,
            &[node(&["command", "-x", "cd"], 1, 0, &["command"])],
            &[],
        );
    }

    #[test]
    fn test_other_commands() {
        // pushd /tmp -> D
        assert_deny(false, &[node(&["pushd", "/tmp"], 0, 0, &[])], &[]);

        // popd -> D
        assert_deny(false, &[node(&["popd"], 0, 0, &[])], &[]);

        // cd -> D
        assert_deny(false, &[node(&["cd"], 0, 0, &[])], &[]);

        // cd 'a b'; ls (N([cd,a b],0,0,[])) -> D
        assert_deny(
            false,
            &[node(&["cd", "a b"], 0, 0, &[]), node(&["ls"], 0, 0, &[])],
            &seps(&[";"]),
        );

        // echo cd -> A
        assert_allow(false, &[node(&["echo", "cd"], 0, 0, &[])], &[]);

        // git -C x status -> A
        assert_allow(
            false,
            &[node(&["git", "-C", "x", "status"], 0, 0, &[])],
            &[],
        );

        // echo $(cd x): N([echo,""],0,0,[]) -> A
        assert_allow(false, &[node(&["echo", ""], 0, 0, &[])], &[]);
    }

    #[test]
    fn test_edge_cases() {
        // decide(true, ...) (lex error, e.g. echo "unclosed) -> A even with a cd node
        assert_allow(true, &[node(&["cd", "x"], 0, 0, &[])], &[]);

        // Empty nodes -> A
        assert_allow(false, &[], &[]);

        // prefix_assignments greater than index does not panic
        assert_deny(false, &[node(&["cd", "x"], 0, 5, &[])], &[]);

        // An index beyond words.len() does not panic (A)
        assert_allow(false, &[node(&["cd", "x"], 10, 0, &[])], &[]);

        // A separators slice shorter than nodes does not panic
        assert_deny(
            false,
            &[node(&["ls"], 0, 0, &[]), node(&["cd", "x"], 0, 0, &[])],
            &[],
        );
    }

    #[test]
    fn test_command_query_flag_matching() {
        assert!(is_command_query_flag("-v"));
        assert!(is_command_query_flag("-V"));
        assert!(is_command_query_flag("-pv"));
        assert!(is_command_query_flag("-vfoo"));
        assert!(!is_command_query_flag("-p"));
        assert!(!is_command_query_flag("--version"));
        assert!(!is_command_query_flag("-foo-v"));
        assert!(!is_command_query_flag("-"));
        assert!(!is_command_query_flag("v"));
        assert!(!is_command_query_flag(""));
    }
}
