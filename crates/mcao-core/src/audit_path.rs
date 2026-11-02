//! Path, URL, and pointer helpers for audit sanitization.

/// Maximum number of Unicode scalar values allowed before truncation.
pub const TRUNCATE_CHARS: usize = 256;

/// Encodes a JSON Pointer token according to RFC 6901: `~` becomes `~0`,
/// then `/` becomes `~1`.
pub fn pointer_token(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

/// Returns the prefix of `text` containing at most `max` Unicode scalar values.
pub fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Normalizes a POSIX path conforming to Python's `posixpath.normpath`.
pub fn posix_normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let initial_slashes = path.bytes().take_while(|&b| b == b'/').count();
    let initial = if initial_slashes == 2 {
        2
    } else if initial_slashes >= 3 {
        1
    } else {
        initial_slashes
    };

    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            if comps.last().is_some_and(|&last| last != "..") {
                comps.pop();
                continue;
            }
            if initial == 0 {
                comps.push("..");
            }
        } else {
            comps.push(comp);
        }
    }

    let mut result = String::new();
    for _ in 0..initial {
        result.push('/');
    }
    result.push_str(&comps.join("/"));
    if result.is_empty() {
        ".".to_string()
    } else {
        result
    }
}

/// Returns the POSIX base name, matching Python's `posixpath.basename`.
/// This is the substring after the last `/`, or the entire string if no `/` is present.
pub fn posix_basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) => &path[pos + 1..],
        None => path,
    }
}

/// Returns the repository-relative path, matching Python's `repo_relative`.
/// Returns `None` if the path lies outside the root or cannot be made relative.
pub fn repo_relative(raw: &str, root: Option<&str>) -> Option<String> {
    let mut raw = raw;
    if let Some(root_val) = root {
        let base = root_val.trim_end_matches('/');
        if raw.trim_end_matches('/') == base {
            return Some(".".to_string());
        }
        if let Some(stripped) = raw
            .strip_prefix(base)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            raw = stripped;
        }
    }
    if raw.starts_with('/') {
        return None;
    }
    let n = posix_normpath(raw);
    if n == ".." || n.starts_with("../") || n.starts_with('/') {
        return None;
    }
    Some(n)
}

/// Extracts the scheme and hostname from a URL, both converted to lowercase.
/// Returns `None` if the URL lacks a valid scheme or hostname.
pub fn url_scheme_host(url: &str) -> Option<(String, String)> {
    let filtered: String = url
        .chars()
        .filter(|&c| c != '\t' && c != '\r' && c != '\n')
        .collect();
    let s = filtered.trim_start_matches(|c: char| (c as u32) <= 0x0020);

    let colon_idx = s.find(':')?;
    if colon_idx == 0 {
        return None;
    }
    let scheme_raw = &s[..colon_idx];
    let mut chars = scheme_raw.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return None,
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    let scheme = scheme_raw.to_ascii_lowercase();

    let after_colon = &s[colon_idx + 1..];
    let after_slashes = after_colon.strip_prefix("//")?;
    let netloc_end = match after_slashes.find(['/', '?', '#']) {
        Some(idx) => idx,
        None => after_slashes.len(),
    };
    let netloc = &after_slashes[..netloc_end];

    let hostpart = match netloc.rfind('@') {
        Some(at_idx) => &netloc[at_idx + 1..],
        None => netloc,
    };
    let host = if let Some(stripped) = hostpart.strip_prefix('[') {
        let close_idx = stripped.find(']')?;
        &stripped[..close_idx]
    } else {
        match hostpart.find(':') {
            Some(colon) => &hostpart[..colon],
            None => hostpart,
        }
    };

    if host.is_empty() {
        return None;
    }
    Some((scheme, host.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pointer_token() {
        assert_eq!(pointer_token("a/b~c"), "a~1b~0c");
        assert_eq!(pointer_token("~/"), "~0~1");
        assert_eq!(pointer_token("plain"), "plain");
        assert_eq!(pointer_token(""), "");
    }

    #[test]
    fn test_truncate_chars() {
        assert_eq!(TRUNCATE_CHARS, 256);
        let a300 = "a".repeat(300);
        let truncated = truncate_chars(&a300, 256);
        assert_eq!(truncated.len(), 256);
        assert_eq!(truncated, "a".repeat(256));
        assert_eq!(truncate_chars("héllo😀", 5), "héllo");
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("abc", 0), "");
    }

    #[test]
    fn test_posix_normpath() {
        assert_eq!(posix_normpath(""), ".");
        assert_eq!(posix_normpath("a"), "a");
        assert_eq!(posix_normpath("a/b"), "a/b");
        assert_eq!(posix_normpath("a//b"), "a/b");
        assert_eq!(posix_normpath("a/./b/"), "a/b");
        assert_eq!(posix_normpath("a/../b"), "b");
        assert_eq!(posix_normpath("../a"), "../a");
        assert_eq!(posix_normpath("a/.."), ".");
        assert_eq!(posix_normpath("/"), "/");
        assert_eq!(posix_normpath("//"), "//");
        assert_eq!(posix_normpath("///"), "/");
        assert_eq!(posix_normpath("//a"), "//a");
        assert_eq!(posix_normpath("///a"), "/a");
        assert_eq!(posix_normpath("/a/.."), "/");
        assert_eq!(posix_normpath("/../a"), "/a");
        assert_eq!(posix_normpath("a/b/../../.."), "..");
        assert_eq!(posix_normpath("./"), ".");
        assert_eq!(posix_normpath("."), ".");
    }

    #[test]
    fn test_posix_basename() {
        assert_eq!(posix_basename("a/b"), "b");
        assert_eq!(posix_basename("a/b/"), "");
        assert_eq!(posix_basename("/"), "");
        assert_eq!(posix_basename(""), "");
        assert_eq!(posix_basename("abc"), "abc");
        assert_eq!(posix_basename("/x/y.txt"), "y.txt");
    }

    #[test]
    fn test_repo_relative() {
        assert_eq!(
            repo_relative("/w/a/b.txt", Some("/w")),
            Some("a/b.txt".to_string())
        );
        assert_eq!(repo_relative("/w", Some("/w")), Some(".".to_string()));
        assert_eq!(repo_relative("/w/", Some("/w/")), Some(".".to_string()));
        assert_eq!(repo_relative("/w//a", Some("/w")), None);
        assert_eq!(repo_relative("a/b", None), Some("a/b".to_string()));
        assert_eq!(repo_relative("./a/../b", Some("/w")), Some("b".to_string()));
        assert_eq!(repo_relative("../x", None), None);
        assert_eq!(repo_relative("/etc/passwd", Some("/w")), None);
        assert_eq!(repo_relative("/wx/a", Some("/w")), None);
        assert_eq!(repo_relative("a/../..", Some("/w")), None);
        assert_eq!(repo_relative("..", None), None);
        assert_eq!(repo_relative("/w/../etc", Some("/w")), None);
        assert_eq!(repo_relative("//a", None), None);
        assert_eq!(repo_relative("", None), Some(".".to_string()));
        assert_eq!(repo_relative("/", Some("/")), Some(".".to_string()));
        assert_eq!(repo_relative("/w/a", Some("/")), Some("w/a".to_string()));
        assert_eq!(repo_relative("x/", Some("/w")), Some("x".to_string()));
        assert_eq!(repo_relative("/a/b", Some("/a/b/")), Some(".".to_string()));
    }

    #[test]
    fn test_url_scheme_host() {
        assert_eq!(
            url_scheme_host("https://Example.COM/a?b#c"),
            Some(("https".to_string(), "example.com".to_string()))
        );
        let userinfo_url = format!("http://{}{}host:8080/x", "user", ":pw@");
        assert_eq!(
            url_scheme_host(&userinfo_url),
            Some(("http".to_string(), "host".to_string()))
        );
        assert_eq!(
            url_scheme_host("ftp://[::1]:21/"),
            Some(("ftp".to_string(), "::1".to_string()))
        );
        assert_eq!(
            url_scheme_host("HTTPS://H"),
            Some(("https".to_string(), "h".to_string()))
        );
        assert_eq!(
            url_scheme_host("  https://a.b/"),
            Some(("https".to_string(), "a.b".to_string()))
        );
        assert_eq!(
            url_scheme_host("ht\ttps://x.y"),
            Some(("https".to_string(), "x.y".to_string()))
        );
        assert_eq!(
            url_scheme_host("https://a.b:99999"),
            Some(("https".to_string(), "a.b".to_string()))
        );
        assert_eq!(
            url_scheme_host("a+b-c.d://h"),
            Some(("a+b-c.d".to_string(), "h".to_string()))
        );
        assert_eq!(
            url_scheme_host("https://u@h@i/"),
            Some(("https".to_string(), "i".to_string()))
        );
        assert_eq!(
            url_scheme_host("https://h?q"),
            Some(("https".to_string(), "h".to_string()))
        );
        assert_eq!(
            url_scheme_host("https://h#f"),
            Some(("https".to_string(), "h".to_string()))
        );
        let cb_url = format!("https://a{}b@C.d:80?x", ":");
        assert_eq!(
            url_scheme_host(&cb_url),
            Some(("https".to_string(), "c.d".to_string()))
        );

        assert_eq!(url_scheme_host("mailto:a@b"), None);
        assert_eq!(url_scheme_host("//host/x"), None);
        assert_eq!(url_scheme_host("host/x"), None);
        assert_eq!(url_scheme_host("https://"), None);
        assert_eq!(url_scheme_host("https:///x"), None);
        assert_eq!(url_scheme_host("https://user@/x"), None);
        assert_eq!(url_scheme_host("1http://h"), None);
        assert_eq!(url_scheme_host("://h"), None);
        assert_eq!(url_scheme_host("https://[::1"), None);
        assert_eq!(url_scheme_host("file:///etc/x"), None);
    }
}
