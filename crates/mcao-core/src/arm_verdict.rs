//! Pure watcher-arm pretool verdict: protected-script identity and the decision order of
//! `bin/fm-arm-command-policy.mjs`.
//!
//! The shell lexer and recursive command analysis are not part of this module. A caller supplies
//! the summarized [`Analysis`] and receives an [`ArmDecision`]. Codes and reason text match the
//! reference policy's `REASONS` table verbatim.

/// Kind of protected watcher script named by a command word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedKind {
    /// Not a protected watcher script.
    None,
    /// `bin/fm-watch-arm.sh`, the supported way to arm the watcher.
    Arm,
    /// `bin/fm-watch-checkpoint.sh`, the supported checkpoint run.
    Checkpoint,
    /// `bin/fm-watch.sh`, which must not run directly.
    Watch,
}

/// Protected scripts in match order: repository-relative path and its kind.
const PROTECTED_SCRIPTS: [(&str, ProtectedKind); 3] = [
    ("bin/fm-watch-arm.sh", ProtectedKind::Arm),
    ("bin/fm-watch-checkpoint.sh", ProtectedKind::Checkpoint),
    ("bin/fm-watch.sh", ProtectedKind::Watch),
];

/// Normalizes a POSIX path with the rules of Node's `path.posix.normalize`.
///
/// An empty path yields `.`. Empty and `.` segments are dropped. A `..` segment pops the last kept
/// segment unless that segment is itself `..`; otherwise it is kept for relative paths and dropped
/// for absolute paths. A trailing `/` is preserved, and an absolute path keeps its leading `/`.
pub fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut kept: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                let pops = matches!(kept.last(), Some(last) if *last != "..");
                if pops {
                    kept.pop();
                } else if !absolute {
                    kept.push("..");
                }
            }
            other => kept.push(other),
        }
    }
    let joined = kept.join("/");
    if joined.is_empty() {
        let empty = if absolute {
            "/"
        } else if trailing {
            "./"
        } else {
            "."
        };
        return empty.to_string();
    }
    let mut out = String::with_capacity(joined.len() + 2);
    if absolute {
        out.push('/');
    }
    out.push_str(&joined);
    if trailing {
        out.push('/');
    }
    out
}

/// Classifies `value` as a protected watcher script for the checkout at `root`.
///
/// Both `value` and the joined `root` path are normalized with [`posix_normalize`], then compared
/// case-folded, because the default macOS volume is case-insensitive and `FM-WATCH-ARM.SH` reaches
/// the same file as `fm-watch-arm.sh`. A value matches a script when it equals the relative path,
/// equals that path under `root`, or ends with `/` followed by the relative path. An empty `root`
/// joins to the relative path alone. A bare script name with no directory part also matches, because
/// PATH resolution reaches the watcher by that name.
pub fn protected_identity(value: &str, root: &str) -> ProtectedKind {
    let normalized = posix_normalize(value).to_lowercase();
    for (relative, kind) in PROTECTED_SCRIPTS {
        let script = relative.to_lowercase();
        let rooted = if root.is_empty() {
            script.clone()
        } else {
            format!("{root}/{script}")
        };
        let joined = posix_normalize(&rooted).to_lowercase();
        let suffix = format!("/{script}");
        let bare = script
            .rsplit_once('/')
            .map_or(script.as_str(), |(_, name)| name);
        if normalized == script
            || normalized == joined
            || normalized.ends_with(suffix.as_str())
            || normalized == bare
        {
            return kind;
        }
    }
    ProtectedKind::None
}

/// One command node of a summarized shell program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// Protected-script kind of the node's command, or [`ProtectedKind::None`].
    pub protected_kind: ProtectedKind,
    /// Whether the node runs a protected watcher nested inside another construct.
    pub nested_protected: bool,
    /// Whether the node carries shell redirection.
    pub redirection: bool,
    /// Whether the node carries command or process substitution.
    pub substitution: bool,
    /// Whether every token of the node is a plain word with no substitutions.
    pub ordinary_words_only: bool,
    /// Count of leading variable assignments before the command word.
    pub prefix_assignments: usize,
    /// Command wrapper names preceding the command word, outermost first.
    pub wrappers: Vec<String>,
    /// Values of the node's command-position words, index 0 first.
    pub words: Vec<String>,
}

/// Summarized result of analyzing one shell command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    /// Whether analysis hit unsupported or malformed shell syntax.
    pub error: bool,
    /// Whether any protected watcher script was found, directly or nested.
    pub protected_found: bool,
    /// Whether a protected watcher script was found nested inside another construct.
    pub nested_protected: bool,
    /// Whether the command is a broad process kill targeting the watcher.
    pub broad_kill: bool,
    /// Separators between top-level nodes, in source order.
    pub separators: Vec<String>,
    /// Top-level command nodes, in source order.
    pub nodes: Vec<NodeInfo>,
}

/// Pretool verdict for a watcher-arm command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmDecision {
    /// The command may run.
    Allow,
    /// The command is refused.
    Deny {
        /// Stable REASONS key, such as `watcher-pipeline`.
        code: &'static str,
        /// Verbatim explanation for `code`.
        reason: &'static str,
    },
}

/// Deny outcomes, each mapping to one REASONS key and its verbatim explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenyCode {
    BroadWatcherKill,
    UnclassifiableProtectedCommand,
    WatcherDirect,
    WatcherNested,
    WatcherBackground,
    WatcherPipeline,
    WatcherRedirection,
    WatcherBundled,
}

impl DenyCode {
    /// Returns the REASONS key for this outcome.
    fn code(self) -> &'static str {
        match self {
            Self::BroadWatcherKill => "broad-watcher-kill",
            Self::UnclassifiableProtectedCommand => "unclassifiable-protected-command",
            Self::WatcherDirect => "watcher-direct",
            Self::WatcherNested => "watcher-nested",
            Self::WatcherBackground => "watcher-background",
            Self::WatcherPipeline => "watcher-pipeline",
            Self::WatcherRedirection => "watcher-redirection",
            Self::WatcherBundled => "watcher-bundled",
        }
    }

    /// Returns the verbatim REASONS text for this outcome.
    fn reason(self) -> &'static str {
        match self {
            Self::BroadWatcherKill => {
                "a broad process kill targeting the firstmate watcher is forbidden"
            }
            Self::UnclassifiableProtectedCommand => {
                "unsupported or malformed shell syntax contains a protected watcher command"
            }
            Self::WatcherDirect => {
                "bin/fm-watch.sh must not be run directly; arm the watcher with \
                 bin/fm-watch-arm.sh or run bin/fm-watch-checkpoint.sh instead"
            }
            Self::WatcherNested => {
                "a protected watcher command must not run through a wrapper, substitution, \
                 or compound command"
            }
            Self::WatcherBackground => {
                "a protected watcher command cannot run in an asynchronous shell list or \
                 through nohup/disown"
            }
            Self::WatcherPipeline => {
                "a protected watcher command must not participate in a pipeline"
            }
            Self::WatcherRedirection => {
                "a protected watcher command must not use shell redirection"
            }
            Self::WatcherBundled => {
                "a protected watcher command must be the sole final command after approved \
                 setup nodes"
            }
        }
    }
}

/// Builds the deny verdict for `outcome`.
fn deny(outcome: DenyCode) -> ArmDecision {
    ArmDecision::Deny {
        code: outcome.code(),
        reason: outcome.reason(),
    }
}

/// Returns the final component of `value`, ignoring empty components, or `value` itself.
fn basename(value: &str) -> &str {
    value
        .split('/')
        .rfind(|part| !part.is_empty())
        .unwrap_or(value)
}

/// Reports whether `value` has the form `NAME=...` with a shell-identifier `NAME`.
fn is_assignment(value: &str) -> bool {
    match value.split_once('=') {
        Some((name, _)) => {
            let mut chars = name.chars();
            matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
                && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Reports whether `node` is an approved setup node: `cd X` or `export NAME=...`.
fn is_setup_node(node: &NodeInfo) -> bool {
    if !node.ordinary_words_only || node.prefix_assignments != 0 || !node.wrappers.is_empty() {
        return false;
    }
    match node.words.as_slice() {
        [command, _] if command.as_str() == "cd" => true,
        [command, assignment] if command.as_str() == "export" => is_assignment(assignment),
        _ => false,
    }
}

/// Reports whether the final node may be the protected command of a blessed program.
fn final_protected_allowed(node: &NodeInfo) -> bool {
    if !matches!(
        node.protected_kind,
        ProtectedKind::Arm | ProtectedKind::Checkpoint
    ) || node.redirection
        || node.substitution
        || !node.ordinary_words_only
        || node.prefix_assignments != 0
    {
        return false;
    }
    match node.wrappers.as_slice() {
        [] => true,
        [wrapper] => wrapper.as_str() == "exec",
        _ => false,
    }
}

/// Reports whether the program is approved setup nodes followed by one final protected command.
fn blessed(analysis: &Analysis) -> bool {
    let Some((last, setup)) = analysis.nodes.split_last() else {
        return false;
    };
    if analysis
        .separators
        .iter()
        .any(|sep| !matches!(sep.as_str(), ";" | "newline" | "&&"))
    {
        return false;
    }
    final_protected_allowed(last)
        && setup.iter().all(|node| {
            node.protected_kind == ProtectedKind::None
                && !node.nested_protected
                && is_setup_node(node)
        })
}

/// Decides whether a summarized watcher-arm command may run.
///
/// Checks run in this order and the first hit wins: broad kill, unclassifiable protected command,
/// no protected command, direct `bin/fm-watch.sh`, nested protected command, background or
/// `nohup`/`disown`, pipeline, redirection, substitution, blessed program, prefix assignment or
/// non-`exec` wrapper, and finally bundled. A protected command or nested run counts when the
/// summary or any node reports it, as in the reference policy's `decide`.
pub fn decide(analysis: &Analysis) -> ArmDecision {
    let nested_protected =
        analysis.nested_protected || analysis.nodes.iter().any(|node| node.nested_protected);
    let protected_found = analysis.protected_found
        || nested_protected
        || analysis
            .nodes
            .iter()
            .any(|node| node.protected_kind != ProtectedKind::None);
    if analysis.broad_kill {
        return deny(DenyCode::BroadWatcherKill);
    }
    if analysis.error && protected_found {
        return deny(DenyCode::UnclassifiableProtectedCommand);
    }
    if !protected_found {
        return ArmDecision::Allow;
    }
    let separator = |wanted: &str| analysis.separators.iter().any(|sep| sep == wanted);
    if analysis
        .nodes
        .iter()
        .any(|node| node.protected_kind == ProtectedKind::Watch)
    {
        return deny(DenyCode::WatcherDirect);
    }
    if nested_protected {
        return deny(DenyCode::WatcherNested);
    }
    let backgrounded = analysis.nodes.iter().any(|node| {
        node.wrappers.iter().any(|wrapper| wrapper == "nohup")
            || basename(node.words.first().map_or("", |word| word.as_str())) == "disown"
    });
    if separator("&") || backgrounded {
        return deny(DenyCode::WatcherBackground);
    }
    if separator("|") || separator("|&") {
        return deny(DenyCode::WatcherPipeline);
    }
    if analysis.nodes.iter().any(|node| node.redirection) {
        return deny(DenyCode::WatcherRedirection);
    }
    if analysis.nodes.iter().any(|node| node.substitution) {
        return deny(DenyCode::WatcherNested);
    }
    if blessed(analysis) {
        return ArmDecision::Allow;
    }
    let wrapped_or_assigned = analysis.nodes.iter().any(|node| {
        node.prefix_assignments > 0 || node.wrappers.iter().any(|wrapper| wrapper != "exec")
    });
    if wrapped_or_assigned {
        return deny(DenyCode::WatcherNested);
    }
    deny(DenyCode::WatcherBundled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: ProtectedKind, words: &[&str]) -> NodeInfo {
        NodeInfo {
            protected_kind: kind,
            nested_protected: false,
            redirection: false,
            substitution: false,
            ordinary_words_only: true,
            prefix_assignments: 0,
            wrappers: Vec::new(),
            words: words.iter().map(|word| (*word).to_string()).collect(),
        }
    }

    fn tweak(mut node: NodeInfo, edit: fn(&mut NodeInfo)) -> NodeInfo {
        edit(&mut node);
        node
    }

    fn wrapped(mut node: NodeInfo, wrappers: &[&str]) -> NodeInfo {
        node.wrappers = wrappers
            .iter()
            .map(|wrapper| (*wrapper).to_string())
            .collect();
        node
    }

    fn analysis(nodes: Vec<NodeInfo>, separators: &[&str]) -> Analysis {
        Analysis {
            error: false,
            protected_found: true,
            nested_protected: false,
            broad_kill: false,
            separators: separators.iter().map(|sep| (*sep).to_string()).collect(),
            nodes,
        }
    }

    fn arm() -> NodeInfo {
        node(ProtectedKind::Arm, &["bin/fm-watch-arm.sh"])
    }

    fn cd(target: &str) -> NodeInfo {
        node(ProtectedKind::None, &["cd", target])
    }

    fn deny_of(code: &'static str, reason: &'static str) -> ArmDecision {
        ArmDecision::Deny { code, reason }
    }

    #[test]
    fn posix_normalize_matches_node_vectors() {
        let cases = [
            ("", "."),
            ("/", "/"),
            ("//", "/"),
            ("a", "a"),
            ("./a", "a"),
            ("../a", "../a"),
            ("a/..", "."),
            ("a/../..", ".."),
            ("/..", "/"),
            ("/a/../..", "/"),
            ("a//b/./c/", "a/b/c/"),
            ("./", "./"),
            ("a/b/../../..", ".."),
            ("../..", "../.."),
            ("///a", "/a"),
            ("...", "..."),
            ("a/...//b/.", "a/.../b"),
        ];
        for (input, expected) in cases {
            assert_eq!(posix_normalize(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn protected_identity_matches_node_vectors() {
        let cases = [
            ("bin/fm-watch-arm.sh", ProtectedKind::Arm),
            ("./bin/fm-watch-arm.sh", ProtectedKind::Arm),
            ("/r/bin/fm-watch-checkpoint.sh", ProtectedKind::Checkpoint),
            ("/r/bin/../bin/fm-watch.sh", ProtectedKind::Watch),
            ("/other/bin/fm-watch.sh", ProtectedKind::Watch),
            ("bin//fm-watch-arm.sh", ProtectedKind::Arm),
            ("bin/fm-watch-arm.sh/", ProtectedKind::None),
            ("/r/x/bin/fm-watch-arm.sh", ProtectedKind::Arm),
            ("fm-watch-arm.sh", ProtectedKind::Arm),
            ("", ProtectedKind::None),
            ("a/../bin/fm-watch.sh", ProtectedKind::Watch),
            ("bin/fm-watch-arm.shx", ProtectedKind::None),
            ("//r//bin/fm-watch-arm.sh", ProtectedKind::Arm),
            ("BIN/FM-WATCH-ARM.SH", ProtectedKind::Arm),
            ("/R/BIN/Fm-Watch.sh", ProtectedKind::Watch),
            ("/r/bin/FM-WATCH-CHECKPOINT.SH", ProtectedKind::Checkpoint),
            ("bin/FM-WATCH-ARM.SHX", ProtectedKind::None),
        ];
        for (input, expected) in cases {
            assert_eq!(protected_identity(input, "/r"), expected, "input {input:?}");
        }
    }

    #[test]
    fn protected_identity_with_empty_root() {
        assert_eq!(
            protected_identity("bin/fm-watch.sh", ""),
            ProtectedKind::Watch
        );
        assert_eq!(
            protected_identity("/bin/fm-watch.sh", ""),
            ProtectedKind::Watch
        );
    }

    #[test]
    fn bare_script_names_are_protected_only_without_a_directory() {
        let cases = [
            ("fm-watch-arm.sh", ProtectedKind::Arm),
            ("./fm-watch-arm.sh", ProtectedKind::Arm),
            ("fm-watch-checkpoint.sh", ProtectedKind::Checkpoint),
            ("fm-watch.sh", ProtectedKind::Watch),
            ("./fm-watch.sh", ProtectedKind::Watch),
            ("fm-watch-arm.sh/", ProtectedKind::None),
            ("fm-watch-arm.shx", ProtectedKind::None),
            ("fm-watch", ProtectedKind::None),
            ("dir/fm-watch-arm.sh", ProtectedKind::None),
            ("../fm-watch-arm.sh", ProtectedKind::None),
            ("/elsewhere/fm-watch-arm.sh", ProtectedKind::None),
            ("FM-WATCH-ARM.SH", ProtectedKind::Arm),
            ("Fm-Watch.sh", ProtectedKind::Watch),
        ];
        for (input, expected) in cases {
            assert_eq!(protected_identity(input, "/r"), expected, "input {input:?}");
            assert_eq!(
                protected_identity(input, ""),
                expected,
                "input {input:?} with empty root"
            );
        }
    }

    /// One summarized analysis per shape a watcher script word can take in a command.
    fn script_shapes(script: &str) -> Vec<Analysis> {
        let script_node = node(protected_identity(script, "/r"), &[script]);
        let cat = node(ProtectedKind::None, &["cat"]);
        vec![
            analysis(vec![script_node.clone()], &[]),
            analysis(vec![script_node.clone(), cat.clone()], &["|"]),
            analysis(vec![script_node.clone()], &["&"]),
            analysis(
                vec![tweak(script_node.clone(), |n| n.prefix_assignments = 1)],
                &[],
            ),
            analysis(
                vec![tweak(script_node.clone(), |n| n.redirection = true)],
                &[],
            ),
            analysis(vec![cd("x"), script_node.clone()], &["&&"]),
            analysis(vec![script_node.clone(), cat.clone()], &["&&"]),
            analysis(vec![wrapped(script_node.clone(), &["nohup"])], &[]),
            analysis(
                vec![
                    node(ProtectedKind::None, &["export", "PATH=/s/bin:$PATH"]),
                    script_node,
                    cat,
                ],
                &[";", "|"],
            ),
        ]
    }

    #[test]
    fn bare_script_names_decide_like_bin_paths() {
        let decide_all = |script: &str| -> Vec<ArmDecision> {
            script_shapes(script).iter().map(decide).collect()
        };
        let bare = decide_all("fm-watch-arm.sh");
        assert_eq!(bare, decide_all("bin/fm-watch-arm.sh"));
        assert_eq!(
            bare,
            vec![
                ArmDecision::Allow,
                deny_of("watcher-pipeline", DenyCode::WatcherPipeline.reason()),
                deny_of("watcher-background", DenyCode::WatcherBackground.reason()),
                deny_of("watcher-nested", DenyCode::WatcherNested.reason()),
                deny_of("watcher-redirection", DenyCode::WatcherRedirection.reason()),
                ArmDecision::Allow,
                deny_of("watcher-bundled", DenyCode::WatcherBundled.reason()),
                deny_of("watcher-background", DenyCode::WatcherBackground.reason()),
                deny_of("watcher-pipeline", DenyCode::WatcherPipeline.reason()),
            ]
        );

        let direct = deny_of("watcher-direct", DenyCode::WatcherDirect.reason());
        assert_eq!(decide_all("fm-watch.sh"), vec![direct.clone(); 9]);
        assert_eq!(decide_all("bin/fm-watch.sh"), vec![direct.clone(); 9]);
        assert_eq!(decide_all("FM-WATCH.SH"), vec![direct; 9]);
        assert_eq!(decide_all("FM-WATCH-ARM.SH"), bare);
    }

    #[test]
    fn nested_flags_on_nodes_decide_like_the_summary() {
        let mut nested_node = node(ProtectedKind::None, &["bash", "-c", "bin/fm-watch-arm.sh"]);
        nested_node.nested_protected = true;
        let mut summary = analysis(vec![nested_node.clone()], &[]);
        summary.nested_protected = false;
        assert_eq!(
            decide(&summary),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );

        let mut unflagged = analysis(vec![nested_node], &[]);
        unflagged.protected_found = false;
        unflagged.nested_protected = false;
        assert_eq!(
            decide(&unflagged),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );

        let nested_setup = tweak(cd("x"), |n| n.nested_protected = true);
        assert_eq!(
            decide(&analysis(vec![nested_setup, arm()], &["&&"])),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );
    }

    #[test]
    fn protected_nodes_count_when_summary_omits_them() {
        let mut allowed = analysis(vec![arm()], &[]);
        allowed.protected_found = false;
        assert_eq!(decide(&allowed), ArmDecision::Allow);

        let mut watch = analysis(vec![node(ProtectedKind::Watch, &["bin/fm-watch.sh"])], &[]);
        watch.protected_found = false;
        assert_eq!(
            decide(&watch),
            deny_of("watcher-direct", DenyCode::WatcherDirect.reason())
        );
    }

    #[test]
    fn unprotected_or_error_without_protected_allows() {
        let mut broad = analysis(Vec::new(), &[]);
        broad.protected_found = false;
        broad.broad_kill = true;
        assert_eq!(
            decide(&broad),
            deny_of("broad-watcher-kill", DenyCode::BroadWatcherKill.reason())
        );

        let mut error = analysis(Vec::new(), &[]);
        error.error = true;
        error.protected_found = false;
        assert_eq!(decide(&error), ArmDecision::Allow);

        let mut plain = analysis(Vec::new(), &[]);
        plain.protected_found = false;
        assert_eq!(decide(&plain), ArmDecision::Allow);
    }

    #[test]
    fn error_with_protected_is_unclassifiable() {
        let mut malformed = analysis(vec![arm()], &[]);
        malformed.error = true;
        assert_eq!(
            decide(&malformed),
            deny_of(
                "unclassifiable-protected-command",
                DenyCode::UnclassifiableProtectedCommand.reason()
            )
        );
    }

    #[test]
    fn direct_watch_is_denied_even_when_nested() {
        let watch = node(ProtectedKind::Watch, &["bin/fm-watch.sh"]);
        assert_eq!(
            decide(&analysis(vec![watch.clone()], &[])),
            deny_of("watcher-direct", DenyCode::WatcherDirect.reason())
        );
        let mut nested = analysis(vec![watch], &[]);
        nested.nested_protected = true;
        assert_eq!(
            decide(&nested),
            deny_of("watcher-direct", DenyCode::WatcherDirect.reason())
        );
    }

    #[test]
    fn nested_protected_is_denied() {
        let mut nested = analysis(Vec::new(), &[]);
        nested.nested_protected = true;
        assert_eq!(
            decide(&nested),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );
    }

    #[test]
    fn background_forms_are_denied() {
        let ampersand = analysis(vec![arm()], &["&"]);
        assert_eq!(
            decide(&ampersand),
            deny_of("watcher-background", DenyCode::WatcherBackground.reason())
        );

        let nohup = analysis(vec![wrapped(arm(), &["nohup"])], &[]);
        assert_eq!(
            decide(&nohup),
            deny_of("watcher-background", DenyCode::WatcherBackground.reason())
        );

        let disowned = analysis(
            vec![node(
                ProtectedKind::Arm,
                &["/bin/disown", "bin/fm-watch-arm.sh"],
            )],
            &[],
        );
        assert_eq!(
            decide(&disowned),
            deny_of("watcher-background", DenyCode::WatcherBackground.reason())
        );

        let ampersand_pipe = analysis(vec![arm()], &["&", "|"]);
        assert_eq!(
            decide(&ampersand_pipe),
            deny_of("watcher-background", DenyCode::WatcherBackground.reason())
        );
    }

    #[test]
    fn pipeline_forms_are_denied() {
        for sep in ["|", "|&"] {
            assert_eq!(
                decide(&analysis(vec![arm()], &[sep])),
                deny_of("watcher-pipeline", DenyCode::WatcherPipeline.reason()),
                "separator {sep:?}"
            );
        }
    }

    #[test]
    fn redirection_and_substitution_are_denied() {
        let redirected = tweak(arm(), |n| n.redirection = true);
        assert_eq!(
            decide(&analysis(vec![redirected], &[])),
            deny_of("watcher-redirection", DenyCode::WatcherRedirection.reason())
        );

        let substituted = tweak(arm(), |n| n.substitution = true);
        assert_eq!(
            decide(&analysis(vec![substituted], &[])),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );

        let both = tweak(arm(), |n| {
            n.redirection = true;
            n.substitution = true;
        });
        assert_eq!(
            decide(&analysis(vec![both], &[])),
            deny_of("watcher-redirection", DenyCode::WatcherRedirection.reason())
        );
    }

    #[test]
    fn approved_programs_are_allowed() {
        assert_eq!(decide(&analysis(vec![arm()], &[])), ArmDecision::Allow);
        assert_eq!(
            decide(&analysis(vec![cd("x"), arm()], &["&&"])),
            ArmDecision::Allow
        );
        assert_eq!(
            decide(&analysis(
                vec![node(ProtectedKind::None, &["export", "A=1"]), arm()],
                &[";"]
            )),
            ArmDecision::Allow
        );
        assert_eq!(
            decide(&analysis(vec![cd("x"), arm()], &["newline"])),
            ArmDecision::Allow
        );
        assert_eq!(
            decide(&analysis(
                vec![node(
                    ProtectedKind::Checkpoint,
                    &["bin/fm-watch-checkpoint.sh"]
                )],
                &[]
            )),
            ArmDecision::Allow
        );
        assert_eq!(
            decide(&analysis(vec![wrapped(arm(), &["exec"])], &[])),
            ArmDecision::Allow
        );
    }

    #[test]
    fn bundled_programs_are_denied() {
        let bundled_cases = [
            analysis(vec![arm(), cd("x")], &["&&"]),
            analysis(vec![cd("x"), arm()], &["||"]),
            analysis(vec![arm(), arm()], &["&&"]),
            analysis(
                vec![tweak(cd("x"), |n| n.ordinary_words_only = false), arm()],
                &["&&"],
            ),
            analysis(
                vec![node(ProtectedKind::None, &["cd", "a", "b"]), arm()],
                &["&&"],
            ),
            analysis(
                vec![node(ProtectedKind::None, &["export", "A"]), arm()],
                &["&&"],
            ),
            analysis(vec![cd("x"), wrapped(arm(), &["exec", "exec"])], &["&&"]),
            analysis(Vec::new(), &[]),
        ];
        for case in bundled_cases {
            assert_eq!(
                decide(&case),
                deny_of("watcher-bundled", DenyCode::WatcherBundled.reason()),
                "case {case:?}"
            );
        }
    }

    #[test]
    fn nested_wrappers_and_prefixes_are_denied() {
        let prefixed = tweak(arm(), |n| n.prefix_assignments = 1);
        assert_eq!(
            decide(&analysis(vec![prefixed], &[])),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );
        assert_eq!(
            decide(&analysis(vec![wrapped(arm(), &["sudo"])], &[])),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );
        assert_eq!(
            decide(&analysis(vec![wrapped(arm(), &["env"])], &[])),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );
        let prefixed_cd = tweak(cd("x"), |n| n.prefix_assignments = 1);
        assert_eq!(
            decide(&analysis(vec![prefixed_cd, arm()], &["&&"])),
            deny_of("watcher-nested", DenyCode::WatcherNested.reason())
        );
    }

    #[test]
    fn every_code_carries_its_verbatim_reason() {
        let table = [
            (
                DenyCode::BroadWatcherKill,
                "broad-watcher-kill",
                "a broad process kill targeting the firstmate watcher is forbidden",
            ),
            (
                DenyCode::UnclassifiableProtectedCommand,
                "unclassifiable-protected-command",
                "unsupported or malformed shell syntax contains a protected watcher command",
            ),
            (
                DenyCode::WatcherDirect,
                "watcher-direct",
                "bin/fm-watch.sh must not be run directly; arm the watcher with \
                 bin/fm-watch-arm.sh or run bin/fm-watch-checkpoint.sh instead",
            ),
            (
                DenyCode::WatcherNested,
                "watcher-nested",
                "a protected watcher command must not run through a wrapper, substitution, \
                 or compound command",
            ),
            (
                DenyCode::WatcherBackground,
                "watcher-background",
                "a protected watcher command cannot run in an asynchronous shell list or \
                 through nohup/disown",
            ),
            (
                DenyCode::WatcherPipeline,
                "watcher-pipeline",
                "a protected watcher command must not participate in a pipeline",
            ),
            (
                DenyCode::WatcherRedirection,
                "watcher-redirection",
                "a protected watcher command must not use shell redirection",
            ),
            (
                DenyCode::WatcherBundled,
                "watcher-bundled",
                "a protected watcher command must be the sole final command after approved \
                 setup nodes",
            ),
        ];
        for (outcome, code, reason) in table {
            assert_eq!(outcome.code(), code);
            assert_eq!(outcome.reason(), reason);
            assert_eq!(deny(outcome), deny_of(code, reason));
        }
    }
}
