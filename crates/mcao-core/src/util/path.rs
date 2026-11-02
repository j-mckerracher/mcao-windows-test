//! Pure port of CPython 3.14 `posixpath.realpath` with `strict=False`, and the Windows
//! verbatim-path rule recorded for slice 864.B AC2.
//!
//! The walk works on strings only. The caller supplies the working directory and an `entry`
//! oracle that reports what the filesystem holds at a path. Nothing here touches the filesystem.
//! The structure follows `posixpath.py` closely so the saved fixtures in
//! `crates/mcao-cli/tests/fixtures/util-realpath` remain a valid oracle.

use std::collections::HashMap;

/// What the filesystem holds at one path, reduced to the cases `realpath` branches on.
#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    /// Not a symbolic link. A missing path, or any lstat or readlink failure, is reported here
    /// too: CPython ignores those errors in non-strict mode and keeps the literal path.
    Plain,
    /// A symbolic link with its target text, exactly as `readlink` returns it.
    Link(String),
}

/// A stack entry: a path component still to resolve, or the end of a symlink's target.
#[derive(Debug, PartialEq, Eq)]
enum Part {
    Name(String),
    /// Popped after the link's target parts. It records the path reached so far as the
    /// resolution of the link, which is what CPython's `None` marker does.
    Settle(String),
}

/// Resolves `filename` against the working directory the way `os.path.realpath(filename)` does
/// on POSIX.
///
/// `cwd` supplies the working directory and must be absolute; it is not checked. It runs only for
/// a relative `filename`, as CPython calls `getcwd()` only then, so an absolute path never depends
/// on it. `entry` is consulted for each component that needs a look. An `Err` from either closure
/// stops the walk and is returned unchanged.
///
/// CPython loops while unprocessed name parts remain. This loop runs until the stack is empty.
/// The only difference is trailing settle markers, which write `seen` and nothing that reaches
/// the result.
pub fn realpath<E>(
    cwd: impl FnOnce() -> Result<String, E>,
    filename: &str,
    mut entry: impl FnMut(&str) -> Result<Entry, E>,
) -> Result<String, E> {
    let mut rest: Vec<Part> = parts_reversed(filename).collect();
    let mut path = if filename.starts_with('/') {
        "/".to_owned()
    } else {
        cwd()?
    };
    // A link is `None` while its target is being resolved (a cycle), and `Some` once settled.
    let mut seen: HashMap<String, Option<String>> = HashMap::new();

    while let Some(part) = rest.pop() {
        let name = match part {
            Part::Settle(link) => {
                seen.insert(link, Some(path.clone()));
                continue;
            }
            Part::Name(name) => name,
        };
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            path = parent(&path);
            continue;
        }
        let newpath = join(&path, &name);
        match entry(&newpath)? {
            Entry::Plain => path = newpath,
            Entry::Link(target) => match seen.get(&newpath).cloned() {
                Some(Some(resolved)) => path = resolved,
                Some(None) => path = newpath,
                None => {
                    if target.starts_with('/') {
                        path = "/".to_owned();
                    }
                    seen.insert(newpath.clone(), None);
                    rest.push(Part::Settle(newpath));
                    rest.extend(parts_reversed(&target));
                }
            },
        }
    }
    Ok(path)
}

/// Components of `text` in pop order: the stack is the reversed split, so the first component
/// is on top.
fn parts_reversed(text: &str) -> impl Iterator<Item = Part> + '_ {
    text.rsplit('/').map(|name| Part::Name(name.to_owned()))
}

/// `path[:path.rindex('/')] or '/'`: the textual parent, clamped at the root.
fn parent(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(index) => path[..index].to_owned(),
    }
}

fn join(path: &str, name: &str) -> String {
    if path == "/" {
        format!("/{name}")
    } else {
        format!("{path}/{name}")
    }
}

/// The Windows verbatim-path rule for slice 864.B AC2.
///
/// `\\?\` is stripped only when what follows is a plain drive path (`\\?\X:\...`). UNC verbatim
/// paths (`\\?\UNC\...`), volume GUID paths, drive-relative forms, and every other verbatim form
/// are returned unchanged.
pub fn strip_verbatim_drive(path: &str) -> &str {
    match path.strip_prefix(r"\\?\") {
        Some(rest) if is_plain_drive(rest) => rest,
        _ => path,
    }
}

fn is_plain_drive(rest: &str) -> bool {
    matches!(rest.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Entry, realpath, strip_verbatim_drive};

    const CWD: &str = "/w";

    /// A fake filesystem: every path in `links` is a symlink to its target; all else is plain.
    fn fake(links: &[(&str, &str)]) -> impl FnMut(&str) -> Result<Entry, String> {
        let links: HashMap<String, String> = links
            .iter()
            .map(|(link, target)| ((*link).to_owned(), (*target).to_owned()))
            .collect();
        move |path: &str| {
            Ok(match links.get(path) {
                Some(target) => Entry::Link(target.clone()),
                None => Entry::Plain,
            })
        }
    }

    fn resolve(links: &[(&str, &str)], filename: &str) -> String {
        realpath(|| Ok(CWD.to_owned()), filename, fake(links))
            .expect("the fake filesystem never fails")
    }

    #[test]
    fn plain_components_normalize_dot_and_dotdot() {
        assert_eq!(resolve(&[], "a/./b/../c"), "/w/a/c");
        assert_eq!(resolve(&[], "a//b/"), "/w/a/b");
    }

    #[test]
    fn empty_and_dot_name_the_working_directory() {
        assert_eq!(resolve(&[], ""), "/w");
        assert_eq!(resolve(&[], "."), "/w");
        assert_eq!(resolve(&[], "a/.."), "/w");
    }

    #[test]
    fn absolute_path_ignores_the_working_directory() {
        assert_eq!(resolve(&[], "/x//y/"), "/x/y");
        assert_eq!(resolve(&[], "/"), "/");
        assert_eq!(resolve(&[], "//x"), "/x");
    }

    #[test]
    fn dotdot_clamps_at_the_root() {
        assert_eq!(resolve(&[], "/../../.."), "/");
        assert_eq!(resolve(&[], "/../a"), "/a");
    }

    #[test]
    fn link_to_a_file_is_followed() {
        assert_eq!(resolve(&[("/w/l", "a/f")], "l"), "/w/a/f");
    }

    #[test]
    fn absolute_link_target_resets_the_walk() {
        assert_eq!(resolve(&[("/w/d/l", "/x/y")], "d/l/z"), "/x/y/z");
    }

    #[test]
    fn relative_link_resolves_against_its_own_directory() {
        assert_eq!(resolve(&[("/w/a/l", "../b")], "a/l"), "/w/b");
    }

    #[test]
    fn chained_links_resolve_fully() {
        assert_eq!(resolve(&[("/w/c1", "c2"), ("/w/c2", "f")], "c1"), "/w/f");
    }

    #[test]
    fn trailing_slash_in_a_link_target_is_ignored() {
        assert_eq!(resolve(&[("/w/trail", "a/inner/")], "trail"), "/w/a/inner");
    }

    #[test]
    fn dangling_link_keeps_the_literal_remainder() {
        assert_eq!(
            resolve(&[("/w/d", "missing/x")], "d/more/deeper"),
            "/w/missing/x/more/deeper"
        );
    }

    #[test]
    fn self_loop_keeps_the_remaining_parts() {
        assert_eq!(
            resolve(&[("/w/loop", "loop")], "loop/child"),
            "/w/loop/child"
        );
    }

    #[test]
    fn two_link_cycle_stops_at_the_first_link() {
        assert_eq!(
            resolve(&[("/w/ping", "pong"), ("/w/pong", "ping")], "ping"),
            "/w/ping"
        );
    }

    #[test]
    fn dotdot_after_a_cycle_pops_the_literal_path() {
        assert_eq!(
            resolve(&[("/w/loop", "loop")], "loop/../a/file"),
            "/w/a/file"
        );
    }

    #[test]
    fn settled_link_revisit_reuses_its_resolution() {
        // `l` settles to /w/a. The second `l` must resolve to that path, not the literal /w/l.
        assert_eq!(resolve(&[("/w/l", "a")], "l/x/../../l/f"), "/w/a/f");
        assert_eq!(resolve(&[("/w/l", "a")], "l/x/../../l/y/z"), "/w/a/y/z");
    }

    #[test]
    fn settled_link_revisit_uses_the_cache_not_a_second_walk() {
        // `a` settles to /w/b, so the later `a` resolves to /w/b and the walk continues there.
        // Walking `a` again would reach /w and yield /w/l, which is not what CPython returns.
        assert_eq!(
            resolve(&[("/w/a", "b"), ("/w/b", "a/./..")], "b/.//a/l"),
            "/w/b/l"
        );
    }

    #[test]
    fn settled_multi_component_link_revisit_reuses_its_resolution() {
        // `l` settles to /w/d/e, so `..` leaves the walk in /w/d; two `..` return to /w.
        assert_eq!(resolve(&[("/w/l", "d/e")], "l/../../l/f"), "/w/d/e/f");
    }

    #[test]
    fn settled_absolute_link_revisit_reuses_its_resolution() {
        assert_eq!(resolve(&[("/w/l", "/w/x")], "l/../l/f"), "/w/x/f");
    }

    #[test]
    fn absolute_path_never_asks_for_the_working_directory() {
        let result = realpath(
            || Err::<String, String>("working directory asked".to_owned()),
            "/x/y",
            |_: &str| Ok::<Entry, String>(Entry::Plain),
        );
        assert_eq!(result, Ok("/x/y".to_owned()));
    }

    #[test]
    fn relative_path_returns_the_working_directory_error() {
        let result = realpath(
            || Err::<String, String>("no working directory".to_owned()),
            "a/b",
            |_: &str| -> Result<Entry, String> {
                unreachable!("the walk must not start without a working directory")
            },
        );
        assert_eq!(result, Err("no working directory".to_owned()));
    }

    #[test]
    fn entry_error_stops_the_walk_and_is_returned() {
        let entry = |path: &str| {
            if path == "/w/bad" {
                Err("boom".to_owned())
            } else {
                Ok(Entry::Plain)
            }
        };
        assert_eq!(
            realpath(|| Ok(CWD.to_owned()), "bad/x", entry),
            Err("boom".to_owned())
        );
    }

    #[test]
    fn verbatim_prefix_is_stripped_from_plain_drive_paths() {
        assert_eq!(strip_verbatim_drive(r"\\?\C:\Users\x"), r"C:\Users\x");
        assert_eq!(strip_verbatim_drive(r"\\?\d:\"), r"d:\");
    }

    #[test]
    fn verbatim_prefix_is_kept_for_unc_and_other_forms() {
        assert_eq!(
            strip_verbatim_drive(r"\\?\UNC\server\share\x"),
            r"\\?\UNC\server\share\x"
        );
        assert_eq!(
            strip_verbatim_drive(r"\\?\Volume{6f1e0c9a-0000-0000-0000-000000000000}\x"),
            r"\\?\Volume{6f1e0c9a-0000-0000-0000-000000000000}\x"
        );
        assert_eq!(strip_verbatim_drive(r"\\?\C:"), r"\\?\C:");
        assert_eq!(strip_verbatim_drive(r"\\?\C:/x"), r"\\?\C:/x");
        assert_eq!(strip_verbatim_drive(r"\\?\1:\x"), r"\\?\1:\x");
    }

    #[test]
    fn paths_without_the_verbatim_prefix_are_unchanged() {
        assert_eq!(strip_verbatim_drive(r"C:\x"), r"C:\x");
        assert_eq!(strip_verbatim_drive(r"\\server\share"), r"\\server\share");
        assert_eq!(strip_verbatim_drive("/posix/path"), "/posix/path");
    }
}
