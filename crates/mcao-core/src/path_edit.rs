//! Windows `PATH` environment variable string manipulation.
//!
//! Pure string logic for checking, adding, and removing directories in a Windows-style
//! semicolon-separated `PATH` value without registry access or environment expansion.
//! The module is platform-neutral on purpose, so the logic is testable on any host.
//!
//! A directory argument is usable only if it contains no `;` and is not empty after
//! normalization (see [`contains_entry`]). Every function treats an unusable directory as a
//! no-op, so `add_entry` stays idempotent and `remove_entry` can always undo it.

/// Normalizes a path entry for comparison.
///
/// Strips surrounding double quotes and at most one trailing slash (`\` or `/`).
fn normalize_entry(mut entry: &str) -> &str {
    if entry.len() >= 2 && entry.starts_with('"') && entry.ends_with('"') {
        entry = &entry[1..entry.len() - 1];
    }
    if entry.ends_with('\\') || entry.ends_with('/') {
        entry = &entry[..entry.len() - 1];
    }
    entry
}

/// Checks whether `dir` can be matched against PATH entries.
///
/// A directory containing `;` can never equal a single split entry, and one that is empty
/// after normalization (for example `\`, `/`, or two quote characters) never matches any
/// entry, so neither can be added or removed consistently.
fn is_usable_dir(dir: &str) -> bool {
    !dir.contains(';') && !normalize_entry(dir).is_empty()
}

/// Checks whether a single PATH segment matches a usable target directory.
fn entry_matches(entry: &str, dir: &str) -> bool {
    let norm_entry = normalize_entry(entry);
    !norm_entry.is_empty() && norm_entry.eq_ignore_ascii_case(normalize_entry(dir))
}

/// Checks whether `path` contains `dir`.
///
/// Comparison is ASCII case-insensitive, ignores one trailing `\` or `/` on either side,
/// and ignores surrounding double quotes on an entry. Variable references such as `%VAR%`
/// are compared as literal text and never expanded. A `dir` that is empty after
/// normalization or contains `;` matches nothing and returns `false`.
pub fn contains_entry(path: &str, dir: &str) -> bool {
    if !is_usable_dir(dir) || path.is_empty() {
        return false;
    }
    path.split(';').any(|entry| entry_matches(entry, dir))
}

/// Adds `dir` to `path` if not already present.
///
/// This operation is idempotent for every usable `dir`. If `dir` is already present in
/// `path`, the original string is returned unchanged. Otherwise, `dir` is appended to the end
/// separated by `;`. If `path` is empty, `dir` alone is returned without a leading `;`. A
/// `dir` that is empty after normalization or contains `;` is never added and returns the
/// original `path`. Empty segments produced by `;;` or a leading or trailing `;` are always
/// preserved.
pub fn add_entry(path: &str, dir: &str) -> String {
    if !is_usable_dir(dir) || contains_entry(path, dir) {
        return path.to_string();
    }
    if path.is_empty() {
        return dir.to_string();
    }
    if path.ends_with(';') {
        format!("{path}{dir}")
    } else {
        format!("{path};{dir}")
    }
}

/// Removes every matching occurrence of `dir` from `path`.
///
/// Keeps all other entries in their original order and exact spelling. If `dir` is not
/// present, is empty after normalization, or contains `;`, the original string is returned
/// unchanged. Once a match is removed, every remaining segment, including empty segments
/// produced by `;;`, stays in place, except that an empty string is returned when only empty
/// segments remain (for example removing the last entry from `C:\x;` or `;;C:\x`).
pub fn remove_entry(path: &str, dir: &str) -> String {
    if !contains_entry(path, dir) {
        return path.to_string();
    }
    let kept: Vec<&str> = path
        .split(';')
        .filter(|&entry| !entry_matches(entry, dir))
        .collect();

    if kept.iter().all(|s| s.is_empty()) {
        return String::new();
    }

    kept.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_to_empty() {
        assert_eq!(add_entry("", r"C:\mcao\bin"), r"C:\mcao\bin");
        assert!(!add_entry("", r"C:\mcao\bin").starts_with(';'));
    }

    #[test]
    fn add_to_existing() {
        assert_eq!(
            add_entry(r"C:\Windows;C:\Tools", r"C:\mcao\bin"),
            r"C:\Windows;C:\Tools;C:\mcao\bin"
        );
        assert_eq!(
            add_entry(r"C:\Windows;", r"C:\mcao\bin"),
            r"C:\Windows;C:\mcao\bin"
        );
    }

    #[test]
    fn add_twice() {
        let once = add_entry(r"C:\Windows", r"C:\mcao\bin");
        let twice = add_entry(&once, r"C:\mcao\bin");
        assert_eq!(once, twice);
        assert_eq!(twice, r"C:\Windows;C:\mcao\bin");
    }

    #[test]
    fn case_insensitive_duplicate() {
        assert!(contains_entry(r"C:\Windows;C:\MCAO\bin", r"c:\mcao\bin"));
        assert_eq!(
            add_entry(r"C:\Windows;C:\MCAO\bin", r"c:\mcao\bin"),
            r"C:\Windows;C:\MCAO\bin"
        );
    }

    #[test]
    fn trailing_backslash_duplicate() {
        assert!(contains_entry(r"C:\Windows;C:\mcao\bin\", r"C:\mcao\bin"));
        assert!(contains_entry(r"C:\Windows;C:\mcao\bin", r"C:\mcao\bin\"));
        assert!(contains_entry(r"C:\Windows;C:\mcao\bin/", r"C:\mcao\bin"));
        assert_eq!(
            add_entry(r"C:\Windows;C:\mcao\bin\", r"C:\mcao\bin"),
            r"C:\Windows;C:\mcao\bin\"
        );
        assert_eq!(
            add_entry(r"C:\Windows;C:\mcao\bin", r"C:\mcao\bin\"),
            r"C:\Windows;C:\mcao\bin"
        );
    }

    #[test]
    fn quoted_duplicate() {
        assert!(contains_entry(
            r#"C:\Windows;"C:\mcao\bin""#,
            r"C:\mcao\bin"
        ));
        assert!(contains_entry(
            r"C:\Windows;C:\mcao\bin",
            r#""C:\mcao\bin""#
        ));
        assert!(contains_entry(
            r#"C:\Windows;"C:\mcao\bin\""#,
            r"C:\mcao\bin"
        ));
        assert_eq!(
            add_entry(r#"C:\Windows;"C:\mcao\bin""#, r"C:\mcao\bin"),
            r#"C:\Windows;"C:\mcao\bin""#
        );
        assert_eq!(
            add_entry(r"C:\Windows;C:\mcao\bin", r#""C:\mcao\bin""#),
            r"C:\Windows;C:\mcao\bin"
        );
    }

    #[test]
    fn remove_present() {
        assert_eq!(
            remove_entry(r"C:\Windows;C:\mcao\bin;C:\Tools", r"C:\mcao\bin"),
            r"C:\Windows;C:\Tools"
        );
    }

    #[test]
    fn remove_absent() {
        assert_eq!(
            remove_entry(r"C:\Windows;C:\Tools", r"C:\mcao\bin"),
            r"C:\Windows;C:\Tools"
        );
    }

    #[test]
    fn remove_all_duplicates() {
        let path = r#"C:\mcao\bin;C:\Windows;c:\MCAO\bin;C:\Tools;"C:\mcao\bin\";C:\Extra"#;
        assert_eq!(
            remove_entry(path, r"C:\mcao\bin"),
            r"C:\Windows;C:\Tools;C:\Extra"
        );
    }

    #[test]
    fn remove_last_entry_gives_empty() {
        assert_eq!(remove_entry(r"C:\mcao\bin", r"C:\mcao\bin"), "");
        assert_eq!(remove_entry(r"C:\mcao\bin\", r"C:\mcao\bin"), "");
        assert_eq!(remove_entry(r#""C:\mcao\bin""#, r"C:\mcao\bin"), "");
        assert_eq!(remove_entry(r"C:\mcao\bin;c:\MCAO\bin", r"C:\mcao\bin"), "");
        assert_eq!(remove_entry(r";C:\mcao\bin", r"C:\mcao\bin"), "");
        assert_eq!(remove_entry(r"C:\mcao\bin;", r"C:\mcao\bin"), "");
    }

    #[test]
    fn userprofile_bin_entries_untouched() {
        assert!(!contains_entry(
            r"%USERPROFILE%\bin;C:\Tools",
            r"C:\Users\alice\bin"
        ));
        assert_eq!(
            remove_entry(r"%USERPROFILE%\bin;C:\mcao\bin", r"C:\Users\alice\bin"),
            r"%USERPROFILE%\bin;C:\mcao\bin"
        );
        assert!(contains_entry(
            r"%USERPROFILE%\bin;C:\Tools",
            r"%USERPROFILE%\bin"
        ));
        assert!(contains_entry(
            r"%USERPROFILE%\bin;C:\Tools",
            r"%userprofile%\bin"
        ));
        assert_eq!(
            remove_entry(r"%USERPROFILE%\bin;C:\mcao\bin", r"%USERPROFILE%\bin"),
            r"C:\mcao\bin"
        );
    }

    #[test]
    fn semicolons_preserved() {
        assert_eq!(
            add_entry(r"C:\Windows;;C:\Tools", r"C:\mcao\bin"),
            r"C:\Windows;;C:\Tools;C:\mcao\bin"
        );
        assert_eq!(
            add_entry(r"C:\Windows;;C:\mcao\bin", r"C:\mcao\bin"),
            r"C:\Windows;;C:\mcao\bin"
        );
        assert_eq!(
            remove_entry(r"C:\Windows;;C:\mcao\bin;C:\Tools", r"C:\mcao\bin"),
            r"C:\Windows;;C:\Tools"
        );
        assert_eq!(
            remove_entry(r"C:\mcao\bin;;C:\Tools", r"C:\mcao\bin"),
            r";C:\Tools"
        );
    }

    #[test]
    fn empty_dir_noop() {
        assert!(!contains_entry(r"C:\Windows;C:\Tools", ""));
        assert!(!contains_entry("", ""));
        assert!(!contains_entry(";;", ""));
        assert_eq!(
            add_entry(r"C:\Windows;C:\Tools", ""),
            r"C:\Windows;C:\Tools"
        );
        assert_eq!(add_entry("", ""), "");
        assert_eq!(
            remove_entry(r"C:\Windows;C:\Tools", ""),
            r"C:\Windows;C:\Tools"
        );
        assert_eq!(remove_entry("", ""), "");
        assert_eq!(remove_entry(";;", ""), ";;");
    }

    #[test]
    fn no_leading_semicolon_after_removing_first_entry() {
        assert_eq!(
            remove_entry(r"C:\mcao\bin;C:\Windows;C:\Tools", r"C:\mcao\bin"),
            r"C:\Windows;C:\Tools"
        );
        assert_eq!(
            remove_entry(r"C:\mcao\bin;C:\Windows", r"C:\mcao\bin"),
            r"C:\Windows"
        );
        assert!(!remove_entry(r"C:\mcao\bin;C:\Windows", r"C:\mcao\bin").starts_with(';'));
    }

    #[test]
    fn lone_quote_entry_does_not_panic_or_match() {
        let path = "\";C:\\Windows";
        assert!(!contains_entry(path, r"C:\mcao\bin"));
        assert_eq!(
            add_entry(path, r"C:\mcao\bin"),
            "\";C:\\Windows;C:\\mcao\\bin"
        );
        assert_eq!(remove_entry(path, r"C:\mcao\bin"), path);
    }

    #[test]
    fn quoted_separator_entry_matches_nothing() {
        assert!(!contains_entry("\"\\\";C:\\Windows", r"C:\mcao\bin"));
        assert_eq!(
            remove_entry("\"\\\";C:\\mcao\\bin", r"C:\mcao\bin"),
            "\"\\\""
        );
    }

    #[test]
    fn only_one_trailing_separator_is_ignored() {
        assert!(!contains_entry(r"C:\Windows;C:\mcao\bin\\", r"C:\mcao\bin"));
        assert!(!contains_entry(r"C:\Windows;C:\mcao\bin", r"C:\mcao\bin\\"));
        assert!(!contains_entry(r"C:\Windows;C:\mcao\bin//", r"C:\mcao\bin"));
        assert_eq!(
            add_entry(r"C:\mcao\bin\\", r"C:\mcao\bin"),
            r"C:\mcao\bin\\;C:\mcao\bin"
        );
        assert_eq!(
            remove_entry(r"C:\mcao\bin\\;C:\Windows", r"C:\mcao\bin"),
            r"C:\mcao\bin\\;C:\Windows"
        );
    }

    #[test]
    fn dir_empty_after_normalization_is_noop() {
        for dir in [r"\", "/", "\"\"", "\"\\\"", "\"/\""] {
            assert!(!contains_entry(&format!(r"C:\A;{dir}"), dir), "dir {dir:?}");
            let added = add_entry(r"C:\A", dir);
            assert_eq!(added, r"C:\A", "dir {dir:?}");
            assert_eq!(add_entry(&added, dir), added, "dir {dir:?}");
            assert_eq!(add_entry("", dir), "", "dir {dir:?}");
            let path = format!(r"C:\A;{dir};C:\B");
            assert_eq!(remove_entry(&path, dir), path, "dir {dir:?}");
        }
    }

    #[test]
    fn dir_containing_semicolon_is_noop() {
        let dir = r"C:\x;y";
        assert!(!contains_entry(r"C:\A;C:\x;y", dir));
        let added = add_entry(r"C:\A", dir);
        assert_eq!(added, r"C:\A");
        assert_eq!(add_entry(&added, dir), added);
        assert_eq!(add_entry("", dir), "");
        assert_eq!(remove_entry(r"C:\A;C:\x;y", dir), r"C:\A;C:\x;y");
        assert_eq!(remove_entry(&added, dir), added);
    }

    #[test]
    fn empty_segments_survive_removal_unless_nothing_else_remains() {
        assert_eq!(remove_entry(";;", r"C:\mcao\bin"), ";;");
        assert_eq!(
            remove_entry(r";;C:\Windows", r"C:\mcao\bin"),
            r";;C:\Windows"
        );
        assert_eq!(remove_entry(r";;C:\mcao\bin", r"C:\mcao\bin"), "");
        assert_eq!(remove_entry(r"C:\mcao\bin;;", r"C:\mcao\bin"), "");
        assert_eq!(
            remove_entry(r";;C:\mcao\bin;C:\Tools", r"C:\mcao\bin"),
            r";;C:\Tools"
        );
        assert_eq!(
            remove_entry(r"C:\Tools;;C:\mcao\bin;", r"C:\mcao\bin"),
            r"C:\Tools;;"
        );
    }
}
