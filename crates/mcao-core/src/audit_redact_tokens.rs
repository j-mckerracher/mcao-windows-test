//! Credential and token redaction for audit text.
//!
//! Port of the first eight `REDACTIONS` entries of `bin/fm-audit-sanitize.py` (private key, JWT,
//! GitHub tokens, AWS key, Google API key, Slack token, bearer token, credential assignment)
//! without a regex crate. The PII half and the length truncation live in other modules; the
//! sanitizer glue applies them after this one.
//!
//! Every pattern scans left to right over `char`s, replaces the leftmost match with
//! `[REDACTED:<label>]` and continues after the match end. The JWT pattern also skips the rest of
//! a base64url run after a miss at an `eyJ` start, which keeps its scan linear (see
//! `resume_after_jwt_miss`). The AWS and Google key patterns count only as many body chars as
//! their regex needs, so repeated key prefixes stay linear too. Case folding is ASCII only, except
//! in the credential keywords, which fold as Python's case-insensitive match does (see
//! `py_ignorecase_eq`).

/// A matcher returns the exclusive end index of a non-empty match starting at the given index.
type Matcher = fn(&[char], usize) -> Option<usize>;

/// Returns the index where the scan resumes after `matcher` finds no match at a start index.
/// The result must be greater than that index.
type Resume = fn(&[char], usize) -> usize;

/// The patterns in application order; each runs over the output of the previous one.
const PATTERNS: [(&str, Matcher, Resume); 9] = [
    ("private_key", match_private_key, next_start),
    ("jwt", match_jwt, resume_after_jwt_miss),
    ("github_token", match_github_prefixed, next_start),
    ("github_token", match_github_pat, next_start),
    ("aws_access_key", match_aws_key, next_start),
    ("google_api_key", match_google_key, next_start),
    ("slack_token", match_slack_token, next_start),
    ("bearer_token", match_bearer, next_start),
    (
        "credential_assignment",
        match_credential_assignment,
        next_start,
    ),
];

/// Applies the eight token redactions in order; each pattern runs over the output of the previous one.
///
/// The nine entries in the table are the eight redaction kinds, because the GitHub token has two
/// spellings that share one label. The function is total: it never panics on any input.
pub fn redact_tokens(text: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    for (label, matcher, resume) in PATTERNS {
        chars = replace_all(&chars, label, matcher, resume);
    }
    chars.into_iter().collect()
}

/// Replaces every non-overlapping match found by `matcher` with `[REDACTED:<label>]`.
///
/// After a miss, the scan continues at `resume`; chars it skips are copied unchanged.
fn replace_all(chars: &[char], label: &str, matcher: Matcher, resume: Resume) -> Vec<char> {
    let replacement = format!("[REDACTED:{label}]");
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        match matcher(chars, i) {
            Some(end) if end > i => {
                out.extend(replacement.chars());
                i = end;
            }
            _ => {
                let next = resume(chars, i);
                out.extend_from_slice(&chars[i..next]);
                i = next;
            }
        }
    }
    out
}

/// The default resume point: the next char.
fn next_start(_chars: &[char], start: usize) -> usize {
    start + 1
}

/// Python `\s` as decided for this port: Unicode whitespace plus the four ASCII separators.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `[A-Za-z0-9]`.
fn is_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// `[A-Za-z0-9_-]`.
fn is_b64u(c: char) -> bool {
    is_alnum(c) || c == '_' || c == '-'
}

/// Length of the maximal run of chars satisfying `pred` that starts at `start`.
fn run_len(chars: &[char], start: usize, pred: fn(char) -> bool) -> usize {
    run_len_capped(chars, start, usize::MAX, pred)
}

/// Like `run_len`, but counts at most `cap` chars. A matcher that needs only `cap` chars uses this
/// so that a long run is not rescanned from every candidate start.
fn run_len_capped(chars: &[char], start: usize, cap: usize, pred: fn(char) -> bool) -> usize {
    chars.get(start..).map_or(0, |rest| {
        rest.iter().take(cap).take_while(|&&c| pred(c)).count()
    })
}

/// Index after `lit` when it matches exactly at `start`.
fn match_lit(chars: &[char], start: usize, lit: &str) -> Option<usize> {
    let mut at = start;
    for expected in lit.chars() {
        if *chars.get(at)? != expected {
            return None;
        }
        at += 1;
    }
    Some(at)
}

/// Index after `lit` when it matches at `start` under Python's case-insensitive rules.
fn match_lit_ci(chars: &[char], start: usize, lit: &str) -> Option<usize> {
    let mut at = start;
    for expected in lit.chars() {
        if !py_ignorecase_eq(*chars.get(at)?, expected) {
            return None;
        }
        at += 1;
    }
    Some(at)
}

/// Python `re` IGNORECASE equality of one char with a lowercase ASCII pattern char.
///
/// Python folds through simple case mappings, so besides the ASCII case pair these non-ASCII chars
/// match: U+0130 and U+0131 match `i`, U+212A (KELVIN SIGN) matches `k`, and U+017F (LONG S)
/// matches `s`. Those are the only non-ASCII chars that match an ASCII letter in CPython 3.9, 3.12,
/// 3.13 and 3.14.
pub(crate) fn py_ignorecase_eq(c: char, expected: char) -> bool {
    c.eq_ignore_ascii_case(&expected)
        || matches!(
            (expected.to_ascii_lowercase(), c),
            ('i', '\u{130}' | '\u{131}') | ('k', '\u{212a}') | ('s', '\u{17f}')
        )
}

/// Index after exactly five hyphens at `start`.
///
/// The count is built from a constant so no five-hyphen marker appears in the source.
fn match_dashes(chars: &[char], start: usize) -> Option<usize> {
    const DASH_COUNT: usize = 5;
    let end = start.checked_add(DASH_COUNT)?;
    chars
        .get(start..end)?
        .iter()
        .all(|&c| c == '-')
        .then_some(end)
}

/// After a `BEGIN ` or `END ` marker: the maximal `[A-Z ]` run must end with `PRIVATE KEY` and be
/// followed by five hyphens. Returns the index after those hyphens.
fn match_key_label_tail(chars: &[char], start: usize) -> Option<usize> {
    const KEY_WORDS: &str = "PRIVATE KEY";
    let run_end = start + run_len(chars, start, |c| c.is_ascii_uppercase() || c == ' ');
    let tail_start = run_end.checked_sub(KEY_WORDS.chars().count())?;
    if tail_start < start || match_lit(chars, tail_start, KEY_WORDS)? != run_end {
        return None;
    }
    match_dashes(chars, run_end)
}

/// Index after a full `BEGIN`/`END` marker for a private key at `start`.
fn match_key_marker(chars: &[char], start: usize, word: &str) -> Option<usize> {
    let after_dashes = match_dashes(chars, start)?;
    let after_word = match_lit(chars, after_dashes, word)?;
    match_key_label_tail(chars, after_word)
}

/// A private key block: header, then everything up to the earliest footer or the end of the text.
fn match_private_key(chars: &[char], start: usize) -> Option<usize> {
    let header_end = match_key_marker(chars, start, "BEGIN ")?;
    let footer_end = (header_end..chars.len()).find_map(|at| match_key_marker(chars, at, "END "));
    Some(footer_end.unwrap_or(chars.len()))
}

/// A JWT: the base64url header prefix, two dot-separated segments of at least five chars, and a
/// signature segment that may be empty.
fn match_jwt(chars: &[char], start: usize) -> Option<usize> {
    let after_prefix = match_lit(chars, start, "ey")?;
    let after_prefix = match_lit(chars, after_prefix, "J")?;
    let first = run_len(chars, after_prefix, is_b64u);
    if first < 5 {
        return None;
    }
    let dot = match_lit(chars, after_prefix + first, ".")?;
    let second = run_len(chars, dot, is_b64u);
    if second < 5 {
        return None;
    }
    let dot = match_lit(chars, dot + second, ".")?;
    Some(dot + run_len(chars, dot, is_b64u))
}

/// Resume point after a JWT miss. After an `eyJ` start, it skips to the end of the base64url run,
/// so long runs of `eyJ` stay linear.
///
/// No match is lost: every start `s'` in the run `[s, e)` shares the end `e`. The dot at `e`, the
/// second segment, and the signature after it do not depend on `s'`, so a miss at `s` means every
/// later start misses too. The first segment `[s'+3, e)` only shrinks as `s'` grows, so a short
/// first segment at `s` also misses for every later start.
fn resume_after_jwt_miss(chars: &[char], start: usize) -> usize {
    if match_lit(chars, start, "eyJ").is_some() {
        start + run_len(chars, start, is_b64u)
    } else {
        start + 1
    }
}

/// A GitHub token with a `gh<p|o|u|s|r>_` prefix and at least 20 alphanumerics.
fn match_github_prefixed(chars: &[char], start: usize) -> Option<usize> {
    let after_gh = match_lit(chars, start, "gh")?;
    let kind = *chars.get(after_gh)?;
    if !"pousr".contains(kind) {
        return None;
    }
    let body = match_lit(chars, after_gh + 1, "_")?;
    let len = run_len(chars, body, is_alnum);
    (len >= 20).then_some(body + len)
}

/// A fine-grained GitHub personal access token: prefix plus at least 20 of `[A-Za-z0-9_]`.
fn match_github_pat(chars: &[char], start: usize) -> Option<usize> {
    let body = match_lit(chars, start, "github_pat_")?;
    let len = run_len(chars, body, |c| is_alnum(c) || c == '_');
    (len >= 20).then_some(body + len)
}

/// An AWS access key id: `AKIA` plus exactly 16 of `[0-9A-Z]`; any extra chars stay in the text.
fn match_aws_key(chars: &[char], start: usize) -> Option<usize> {
    const BODY_LEN: usize = 16;
    let body = match_lit(chars, start, "AKIA")?;
    let len = run_len_capped(chars, body, BODY_LEN, |c| {
        c.is_ascii_digit() || c.is_ascii_uppercase()
    });
    (len >= BODY_LEN).then_some(body + BODY_LEN)
}

/// A Google API key: `AIza` plus exactly 35 of `[0-9A-Za-z_-]`; any extra chars stay in the text.
fn match_google_key(chars: &[char], start: usize) -> Option<usize> {
    const BODY_LEN: usize = 35;
    let body = match_lit(chars, start, "AIza")?;
    let len = run_len_capped(chars, body, BODY_LEN, is_b64u);
    (len >= BODY_LEN).then_some(body + BODY_LEN)
}

/// A Slack token: `x`, `ox`, one of `a b p r s`, a hyphen and at least 10 of `[A-Za-z0-9-]`.
///
/// The prefix is matched in two parts so no token-shaped literal appears in the source.
fn match_slack_token(chars: &[char], start: usize) -> Option<usize> {
    let after_x = match_lit(chars, start, "x")?;
    let after_ox = match_lit(chars, after_x, "ox")?;
    let kind = *chars.get(after_ox)?;
    if !"abprs".contains(kind) {
        return None;
    }
    let body = match_lit(chars, after_ox + 1, "-")?;
    let len = run_len(chars, body, |c| is_alnum(c) || c == '-');
    (len >= 10).then_some(body + len)
}

/// A bearer credential: the exact-case word, whitespace, then at least 16 token chars.
fn match_bearer(chars: &[char], start: usize) -> Option<usize> {
    let after_word = match_lit(chars, start, "Bearer")?;
    let spaces = run_len(chars, after_word, is_py_space);
    if spaces == 0 {
        return None;
    }
    let body = after_word + spaces;
    let len = run_len(chars, body, |c| is_alnum(c) || "._~+/=-".contains(c));
    (len >= 16).then_some(body + len)
}

/// End of a credential keyword at `start`, matched case-insensitively as Python matches it.
///
/// The keywords are `password`, `passwd`, `secret`, `token`, `api` with an optional `_` or `-`
/// before `key`, and the `x-awopt-key` header name.
fn match_credential_keyword(chars: &[char], start: usize) -> Option<usize> {
    const KEYWORDS: [&str; 5] = ["password", "passwd", "secret", "token", "x-awopt-key"];
    if let Some(end) = KEYWORDS
        .iter()
        .find_map(|word| match_lit_ci(chars, start, word))
    {
        return Some(end);
    }
    let after_api = match_lit_ci(chars, start, "api")?;
    let key_start = match chars.get(after_api) {
        Some('_' | '-') => after_api + 1,
        _ => after_api,
    };
    match_lit_ci(chars, key_start, "key")
}

/// A credential assignment: keyword, optional whitespace, `:` or `=`, optional whitespace, then at
/// least one non-whitespace char (maximal). There is no word boundary before the keyword.
fn match_credential_assignment(chars: &[char], start: usize) -> Option<usize> {
    let after_keyword = match_credential_keyword(chars, start)?;
    let sep = after_keyword + run_len(chars, after_keyword, is_py_space);
    if !matches!(chars.get(sep), Some(':' | '=')) {
        return None;
    }
    let value = sep + 1 + run_len(chars, sep + 1, is_py_space);
    let len = run_len(chars, value, |c| !is_py_space(c));
    (len > 0).then_some(value + len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const MARKER_OPEN: &str = "[REDACTED:";

    fn dashes() -> String {
        "-".repeat(5)
    }

    fn jwt_prefix() -> String {
        format!("ey{}", "J")
    }

    fn slack_prefix() -> String {
        format!("x{}", "ox")
    }

    fn label(name: &str) -> String {
        format!("{MARKER_OPEN}{name}]")
    }

    fn assert_redacts(input: &str, expected: &str) {
        assert_eq!(redact_tokens(input), expected, "input: {input:?}");
    }

    fn assert_unchanged(input: &str) {
        assert_redacts(input, input);
    }

    #[test]
    fn private_key_with_footer_ends_at_footer() {
        let d = dashes();
        let input = format!("{d}BEGIN RSA PRIVATE KEY{d}\nabc\n{d}END RSA PRIVATE KEY{d} tail");
        assert_redacts(&input, &format!("{} tail", label("private_key")));
    }

    #[test]
    fn private_key_without_footer_runs_to_end() {
        let d = dashes();
        let input = format!("x {d}BEGIN PRIVATE KEY{d}abc no end");
        assert_redacts(&input, &format!("x {}", label("private_key")));
    }

    #[test]
    fn private_key_header_requirements() {
        let d = dashes();
        assert_unchanged(&format!("{d}BEGIN CERTIFICATE{d}a"));
        assert_unchanged(&format!(
            "{d}BEGIN rsa PRIVATE KEY{d}a{d}END PRIVATE KEY{d}"
        ));
    }

    #[test]
    fn private_key_stops_at_earliest_footer() {
        let d = dashes();
        let input =
            format!("{d}BEGIN A  B PRIVATE KEY{d}x{d}END EC PRIVATE KEY{d}y{d}END PRIVATE KEY{d}");
        let expected = format!("{}y{d}END PRIVATE KEY{d}", label("private_key"));
        assert_redacts(&input, &expected);
    }

    #[test]
    fn private_key_footer_may_start_at_header_end() {
        let d = dashes();
        let input = format!("{d}BEGIN PRIVATE KEY{d}{d}END PRIVATE KEY{d}z");
        assert_redacts(&input, &format!("{}z", label("private_key")));
    }

    #[test]
    fn jwt_requires_two_long_segments() {
        let j = jwt_prefix();
        assert_redacts(
            &format!("{j}abcde.fghij.klm x"),
            &format!("{} x", label("jwt")),
        );
        assert_unchanged(&format!("{j}abcd.fghij.k"));
        assert_redacts(&format!("{j}abcde.fghij."), &label("jwt"));
        assert_redacts(
            &format!("a {j}hbGciOi.{j}zdWIiOi.SflKxwRJSM"),
            &format!("a {}", label("jwt")),
        );
    }

    #[test]
    fn github_prefixed_tokens() {
        let gh = label("github_token");
        assert_redacts(
            &format!("ghp_{}Z more", "a".repeat(20)),
            &format!("{gh} more"),
        );
        assert_unchanged(&format!("ghp_{}", "a".repeat(19)));
        assert_redacts(&format!("gho_{}", "B".repeat(25)), &gh);
        assert_unchanged(&format!("ghx_{}", "a".repeat(20)));
    }

    #[test]
    fn github_fine_grained_tokens() {
        assert_redacts(
            &format!("github_pat_{}", "a_1".repeat(7)),
            &label("github_token"),
        );
        assert_unchanged("github_pat_short");
    }

    #[test]
    fn aws_key_takes_exactly_sixteen() {
        let expected = format!("{}X", label("aws_access_key"));
        assert_redacts(&format!("AKIA{}X", "A".repeat(16)), &expected);
        assert_unchanged(&format!("AKIA{}", "a".repeat(16)));
        assert_unchanged(&format!("AKIA{}", "1".repeat(15)));
    }

    #[test]
    fn google_key_takes_exactly_thirty_five() {
        let expected = format!("{}zz", label("google_api_key"));
        assert_redacts(&format!("AIza{}zz", "a".repeat(35)), &expected);
        assert_unchanged(&format!("AIza{}", "a".repeat(34)));
    }

    #[test]
    fn slack_tokens() {
        let s = slack_prefix();
        let slack = label("slack_token");
        assert_redacts(&format!("{s}b-1234567890"), &slack);
        assert_unchanged(&format!("{s}z-1234567890"));
        assert_unchanged(&format!("{s}a-123456789"));
        assert_redacts(&format!("{s}p-{}", "a-".repeat(10)), &slack);
    }

    #[test]
    fn bearer_tokens() {
        let bearer = label("bearer_token");
        assert_redacts("Bearer abcdefghijklmnop", &bearer);
        assert_redacts("Bearer  \t abcdefghijklmnopqrstuv.x", &bearer);
        assert_unchanged("Bearer abcdefghijklmno");
        assert_unchanged("bearer abcdefghijklmnop");
        assert_redacts("Bearer\u{a0}abcdefghijklmnop", &bearer);
    }

    #[test]
    fn credential_assignment_matches() {
        let cred = label("credential_assignment");
        assert_redacts("password=hunter2 ok", &format!("{cred} ok"));
        assert_redacts("PassWord : x y", &format!("{cred} y"));
        assert_redacts("passwd:\n\nz", &cred);
        assert_redacts("x-awopt-key=abc", &cred);
        assert_redacts("mytoken=zz", &format!("my{cred}"));
        assert_redacts("password = = x", &format!("{cred} x"));
        assert_redacts("TOKEN=a;b rest", &format!("{cred} rest"));
        assert_redacts("Password=é", &cred);
        assert_redacts("secret\t=\tv", &cred);
    }

    #[test]
    fn credential_assignment_api_key_spellings() {
        let cred = label("credential_assignment");
        assert_redacts(
            "api_key=1 apikey=2 api-key=3 API_KEY: 4",
            &format!("{cred} {cred} {cred} {cred}"),
        );
    }

    #[test]
    fn credential_assignment_needs_a_value() {
        assert_unchanged("secret= ");
        assert_unchanged("token:");
        assert_unchanged("tokens=1");
    }

    #[test]
    fn credential_keywords_fold_non_ascii_like_python() {
        let cred = label("credential_assignment");
        // The 16 keyword positions where Python's case-insensitive match also accepts a non-ASCII
        // char: `i` in `api` (U+0130, U+0131), `s` in password, passwd and secret (U+017F), and `k`
        // in token, api_key, api-key, apikey and x-awopt-key (U+212A).
        let folded = [
            "ap\u{130}_key=hunter2",
            "ap\u{131}_key=hunter2",
            "ap\u{130}-key=hunter2",
            "ap\u{131}-key=hunter2",
            "ap\u{130}key=hunter2",
            "ap\u{131}key=hunter2",
            "pa\u{17f}sword=hunter2",
            "pas\u{17f}word=hunter2",
            "pa\u{17f}swd=hunter2",
            "pas\u{17f}wd=hunter2",
            "\u{17f}ecret=hunter2",
            "to\u{212a}en=hunter2",
            "api_\u{212a}ey=hunter2",
            "api-\u{212a}ey=hunter2",
            "api\u{212a}ey=hunter2",
            "x-awopt-\u{212a}ey=hunter2",
        ];
        for input in folded {
            assert_redacts(input, &cred);
        }
        assert_redacts("API\u{212a}EY=hunter2", &cred);
        assert_redacts("PA\u{17f}SWORD=hunter2", &cred);
        assert_redacts("AP\u{130}_KEY=hunter2", &cred);
    }

    #[test]
    fn credential_keywords_do_not_fold_other_look_alikes() {
        // Python does not match these: Cyrillic dze for `s`, dotless i for `o` in token, a bare
        // Kelvin sign with no `api` prefix, and Cyrillic ie for `e`.
        assert_unchanged("pa\u{455}sword=hunter2");
        assert_unchanged("t\u{131}ken=hunter2");
        assert_unchanged("\u{212a}ey=hunter2");
        assert_unchanged("api_k\u{435}y=hunter2");
    }

    #[test]
    fn patterns_apply_in_order() {
        let input = format!("ghp_{} password=x", "a".repeat(20));
        let expected = format!(
            "{} {}",
            label("github_token"),
            label("credential_assignment")
        );
        assert_redacts(&input, &expected);
        assert_redacts("", "");
    }

    #[test]
    fn multibyte_text_does_not_panic() {
        assert_unchanged("héllo wörld \u{1f600} ey");
        assert_unchanged("token");
        assert_unchanged("Bearer");
    }

    /// The scan before the JWT resume rule: every start index is tried, and a miss advances by one.
    fn replace_all_every_start(chars: &[char], label: &str, matcher: Matcher) -> Vec<char> {
        let replacement = format!("[REDACTED:{label}]");
        let mut out = Vec::with_capacity(chars.len());
        let mut i = 0;
        while i < chars.len() {
            match matcher(chars, i) {
                Some(end) if end > i => {
                    out.extend(replacement.chars());
                    i = end;
                }
                _ => {
                    out.push(chars[i]);
                    i += 1;
                }
            }
        }
        out
    }

    fn redact_tokens_every_start(text: &str) -> String {
        let mut chars: Vec<char> = text.chars().collect();
        for (label, matcher, _) in PATTERNS {
            chars = replace_all_every_start(&chars, label, matcher);
        }
        chars.into_iter().collect()
    }

    struct XorShift(u64);

    impl XorShift {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next_u64() % n as u64).unwrap_or(0)
        }
    }

    fn mixed_fragments() -> Vec<String> {
        let s = |x: &str| x.to_string();
        vec![
            jwt_prefix(),
            s("ey"),
            s("J"),
            s("e"),
            s("y"),
            s("a"),
            s("Z"),
            s("0"),
            s("_"),
            s("-"),
            s("."),
            s(" "),
            s("\n"),
            s("é"),
            s("\u{17f}"),
            s("\u{212a}"),
            s("gh"),
            s("p"),
            dashes(),
            s("BEGIN "),
            s("END "),
            s("PRIVATE KEY"),
            s("AKIA"),
            slack_prefix(),
            s("Bearer "),
            s("password"),
            s("="),
            s("*"),
        ]
    }

    fn jwt_fragments() -> Vec<String> {
        let s = |x: &str| x.to_string();
        vec![
            jwt_prefix(),
            jwt_prefix(),
            s("ey"),
            s("J"),
            s("a"),
            s("b"),
            s("Z"),
            s("0"),
            s("_"),
            s("-"),
            s("."),
            s(" "),
        ]
    }

    #[test]
    fn jwt_resume_matches_every_start_scan() {
        let mixed = mixed_fragments();
        let jwt = jwt_fragments();
        let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
        for case in 0..6000 {
            let fragments = if case % 3 == 0 { &mixed } else { &jwt };
            let len = if case % 2 == 0 { 40 } else { 400 };
            let count = rng.below(len);
            let text: String = (0..count)
                .map(|_| fragments[rng.below(fragments.len())].as_str())
                .collect();
            assert_eq!(
                redact_tokens(&text),
                redact_tokens_every_start(&text),
                "input: {text:?}"
            );
        }
    }

    #[test]
    fn jwt_resume_keeps_matches_inside_eyj_runs() {
        let j = jwt_prefix();
        assert_redacts(&format!("{}abcde.fghij.k", j.repeat(3)), &label("jwt"));
        assert_redacts(&format!("{j}abcde{j}abcde.fghij."), &label("jwt"));
        assert_unchanged(&format!("{j}abc.{j}abcde.fghij"));
    }

    #[test]
    fn jwt_scan_stays_linear_on_one_mib_input() {
        const MIB: usize = 1 << 20;
        let inputs = [
            jwt_prefix().repeat(MIB / 3),
            format!("{}{}", jwt_prefix(), "a".repeat(MIB - 3)),
        ];
        for input in inputs {
            let started = Instant::now();
            let out = redact_tokens(&input);
            let elapsed = started.elapsed();
            assert!(out == input, "adversarial JWT input must stay unchanged");
            assert!(
                elapsed < Duration::from_secs(2),
                "1 MiB JWT scan took {elapsed:?}"
            );
        }
    }

    /// Asserts that `redact_tokens` returns `expected` for `input` within the scan budget. The
    /// budget is about 10× the debug-build time of the linear scan on these inputs.
    fn assert_scan_within_budget(input: &str, expected: &str, name: &str) {
        let started = Instant::now();
        let out = redact_tokens(input);
        let elapsed = started.elapsed();
        assert!(out == expected, "{name} redaction output changed");
        assert!(
            elapsed < Duration::from_secs(2),
            "256 KiB {name} scan took {elapsed:?}"
        );
    }

    #[test]
    fn aws_scan_stays_linear_on_repeated_prefix() {
        const LEN: usize = 256 << 10;
        let keys = LEN / 20;
        assert_scan_within_budget(
            &"AKIA".repeat(5 * keys),
            &label("aws_access_key").repeat(keys),
            "AKIA*N",
        );
    }

    #[test]
    fn google_scan_stays_linear_on_repeated_prefix() {
        const LEN: usize = 256 << 10;
        let keys = LEN / 40;
        assert_scan_within_budget(
            &"AIza".repeat(10 * keys),
            &format!("{}a", label("google_api_key")).repeat(keys),
            "AIza*N",
        );
    }

    #[test]
    fn aws_key_length_run_and_adjacency_parity() {
        let key = label("aws_access_key");
        assert_redacts("AKIAIOSFODNN7EXAMPLE", &key);
        assert_redacts("AKIAIOSFODNN7EXAMPLE9Z", &format!("{key}9Z"));
        assert_redacts(&"AKIA".repeat(6), &format!("{key}AKIA"));
        assert_unchanged("AKIAIOSFODNN7EXAMPL-x");
        assert_redacts(&"AKIAIOSFODNN7EXAMPLE".repeat(2), &format!("{key}{key}"));
        assert_redacts(
            &format!("AKIAIOSFODNN7EXAMPLEAIza{}", "a_B-9".repeat(7)),
            &format!("{key}{}", label("google_api_key")),
        );
    }

    #[test]
    fn google_key_length_run_and_adjacency_parity() {
        let key = label("google_api_key");
        let body = "a_B-9".repeat(7);
        assert_redacts(&format!("AIza{body}"), &key);
        assert_redacts(&format!("AIza{body}Zz"), &format!("{key}Zz"));
        assert_unchanged(&format!("AIza{}a_B-", "a_B-9".repeat(6)));
        assert_redacts(&format!("AIza{body}.tail"), &format!("{key}.tail"));
        assert_unchanged(&format!("AIza{}.", "a_B-9".repeat(4)));
        assert_redacts(&format!("AIza{body}AIza{body}"), &format!("{key}{key}"));
    }
}
