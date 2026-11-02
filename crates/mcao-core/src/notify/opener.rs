//! The [`Opener`] trait and the rules every opener applies before it runs a handler.

use std::path::Path;

use crate::McaoError;

const ALLOWED_SCHEMES: [&str; 3] = ["http://", "https://", "file://"];

/// Characters a URI may not carry. The handler runs without a shell, so these are refused as a
/// guard on what reaches it.
const REFUSED_CHARACTERS: [char; 6] = ['$', '`', '\\', ';', '(', ')'];

/// Opens a URI or a local path with the platform's default handler.
pub trait Opener {
    /// Validates `uri` with [`validate_uri`], then opens it through [`Opener::open_validated`].
    fn open_url(&self, uri: &str) -> Result<(), McaoError> {
        validate_uri(uri)?;
        self.open_validated(uri)
    }

    /// Opens `uri` with the platform handler. Callers must pass only a URI that passed
    /// [`validate_uri`]; [`Opener::open_url`] enforces that. Implementors must not override
    /// [`Opener::open_url`], because it is the only path that runs [`validate_uri`].
    fn open_validated(&self, uri: &str) -> Result<(), McaoError>;

    /// Builds the `file://` URI that [`Opener::open_path`] opens for the local file at `path`.
    fn path_uri(&self, path: &Path) -> Result<String, McaoError>;

    /// Opens the local file at `path` through [`Opener::open_url`], so the URI from
    /// [`Opener::path_uri`] passes [`validate_uri`] before any handler runs. Implementors must not
    /// override this method.
    fn open_path(&self, path: &Path) -> Result<(), McaoError> {
        let uri = self.path_uri(path)?;
        self.open_url(&uri)
    }
}

/// Accepts a URI only when its scheme is `http`, `https`, or `file` and it carries no refused
/// character or control byte.
///
/// Parentheses are refused, so a URL that contains them must be percent-encoded first. A `file://`
/// URI built from a local path by the platform opener passes this check.
pub fn validate_uri(uri: &str) -> Result<(), McaoError> {
    if !ALLOWED_SCHEMES.iter().any(|scheme| uri.starts_with(scheme)) {
        return Err(McaoError::Refused(format!(
            "URI {uri:?} must use the http, https, or file scheme"
        )));
    }
    if let Some(refused) = uri
        .chars()
        .find(|c| REFUSED_CHARACTERS.contains(c) || c.is_control())
    {
        return Err(McaoError::Refused(format!(
            "URI {uri:?} contains the refused character {refused:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;

    use super::{Opener, validate_uri};
    use crate::McaoError;

    /// Counts calls to `open_validated`, which stand in for a handler running.
    struct RecordingOpener {
        handler_runs: Cell<usize>,
    }

    impl Opener for RecordingOpener {
        fn open_validated(&self, _uri: &str) -> Result<(), McaoError> {
            self.handler_runs.set(self.handler_runs.get() + 1);
            Ok(())
        }

        fn path_uri(&self, _path: &Path) -> Result<String, McaoError> {
            Ok("file:///tmp/notes.md".to_owned())
        }
    }

    #[test]
    fn open_url_refuses_before_any_handler_runs() {
        let opener = RecordingOpener {
            handler_runs: Cell::new(0),
        };
        assert!(matches!(
            opener.open_url("javascript:x"),
            Err(McaoError::Refused(_))
        ));
        assert_eq!(opener.handler_runs.get(), 0);

        assert!(opener.open_url("https://example.test/").is_ok());
        assert_eq!(opener.handler_runs.get(), 1);
    }

    /// Records each URI that reaches `open_validated`, and builds `path_uri` the way a platform
    /// opener does, as a `file://` URI.
    struct PathOpener {
        uris: RefCell<Vec<String>>,
    }

    impl Opener for PathOpener {
        fn open_validated(&self, uri: &str) -> Result<(), McaoError> {
            self.uris.borrow_mut().push(uri.to_owned());
            Ok(())
        }

        fn path_uri(&self, path: &Path) -> Result<String, McaoError> {
            Ok(format!("file://{}", path.display()))
        }
    }

    #[test]
    fn open_path_routes_through_open_url_and_refuses_metacharacters() {
        let opener = PathOpener {
            uris: RefCell::new(Vec::new()),
        };

        assert!(opener.open_path(Path::new("/tmp/a b")).is_ok());
        assert_eq!(opener.uris.borrow().as_slice(), ["file:///tmp/a b"]);

        assert!(matches!(
            opener.open_path(Path::new("/tmp/a$b")),
            Err(McaoError::Refused(_))
        ));
        assert_eq!(opener.uris.borrow().len(), 1);
    }

    /// Implements only the required methods, as a new platform opener must, and builds a
    /// `file://` URI that carries a refused character.
    struct BareOpener {
        handler_runs: Cell<usize>,
    }

    impl Opener for BareOpener {
        fn open_validated(&self, _uri: &str) -> Result<(), McaoError> {
            self.handler_runs.set(self.handler_runs.get() + 1);
            Ok(())
        }

        fn path_uri(&self, _path: &Path) -> Result<String, McaoError> {
            Ok("file:///tmp/a$b".to_owned())
        }
    }

    #[test]
    fn a_new_implementor_gets_the_uri_check_on_both_entry_points() {
        let opener = BareOpener {
            handler_runs: Cell::new(0),
        };
        assert!(matches!(
            opener.open_path(Path::new("/tmp/x")),
            Err(McaoError::Refused(_))
        ));
        assert!(matches!(
            opener.open_url("javascript:x"),
            Err(McaoError::Refused(_))
        ));
        assert_eq!(opener.handler_runs.get(), 0);
    }

    #[test]
    fn accepts_http_https_and_file_uris() {
        for uri in [
            "http://example.test/a",
            "https://example.test/a?b=c",
            "file:///tmp/notes.md",
        ] {
            assert!(validate_uri(uri).is_ok(), "{uri} should pass");
        }
    }

    #[test]
    fn refuses_other_schemes() {
        for uri in [
            "data:text/html,hi",
            "javascript:void(0)",
            "ftp://example.test/",
        ] {
            assert!(
                matches!(validate_uri(uri), Err(McaoError::Refused(_))),
                "{uri} should be refused"
            );
        }
    }

    #[test]
    fn refuses_metacharacters_and_control_bytes() {
        for uri in [
            "https://example.test/$HOME",
            "https://example.test/`id`",
            "https://example.test/a\\b",
            "https://example.test/a;b",
            "https://example.test/a(b)",
            "https://example.test/a\nb",
        ] {
            assert!(
                matches!(validate_uri(uri), Err(McaoError::Refused(_))),
                "{uri:?} should be refused"
            );
        }
    }
}
