//! Parity tests for `mcao util realpath`. None of these run Python. Every expected value is a
//! frozen fixture recorded once from the pinned `os.path.realpath` reference; see
//! `tests/fixtures/util-realpath/provenance.txt`.

use std::process::Command;

const REALPATH_USAGE: &str = "usage: mcao util realpath <path>\n";

#[test]
fn realpath_without_exactly_one_path_prints_its_usage() {
    for args in [vec!["util", "realpath"], vec!["util", "realpath", "a", "b"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
            .args(&args)
            .output()
            .expect("run mcao");
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            REALPATH_USAGE,
            "args {args:?}"
        );
    }
}

#[cfg(unix)]
mod fixtures {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    const ROOT_MARK: &str = "@ROOT@";
    const CASE_COUNT: usize = 44;

    /// A fresh temporary directory, canonicalised so `@ROOT@` matches what the binary prints on
    /// any machine. The tree is removed on drop; links are removed as links, not followed.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn create() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos();
            let dir = env::temp_dir().join(format!("mcao-realpath-{}-{stamp}", std::process::id()));
            fs::create_dir(&dir).expect("create temp root");
            let canonical = fs::canonicalize(&dir).expect("canonicalise temp root");
            Self(canonical)
        }

        fn text(&self) -> &str {
            self.0.to_str().expect("temp root is UTF-8")
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixtures_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/util-realpath")
    }

    /// Builds the shared tree from `tree.tsv`. Rows are `dir <path>`, `file <path>`, or
    /// `link <path> <target>`, with `@ROOT@` in a target standing for the temp root.
    fn materialize(root: &TempRoot, manifest: &str) {
        for line in manifest.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["dir", rel] => {
                    fs::create_dir_all(root.0.join(rel)).expect("create dir");
                }
                ["file", rel] => {
                    fs::File::create(root.0.join(rel)).expect("create file");
                }
                ["link", name, target] => {
                    let target = target.replace(ROOT_MARK, root.text());
                    std::os::unix::fs::symlink(target, root.0.join(name)).expect("create link");
                }
                _ => panic!("malformed tree.tsv row: {line:?}"),
            }
        }
    }

    #[test]
    fn realpath_matches_the_saved_reference_fixtures() {
        let fixtures = fixtures_dir();
        let root = TempRoot::create();
        let manifest = fs::read_to_string(fixtures.join("tree.tsv")).expect("read tree.tsv");
        materialize(&root, &manifest);

        let mut case_dirs: Vec<PathBuf> = fs::read_dir(fixtures.join("cases"))
            .expect("read cases dir")
            .map(|entry| entry.expect("read cases entry").path())
            .collect();
        case_dirs.sort();
        assert_eq!(case_dirs.len(), CASE_COUNT, "fixture case count changed");

        for case in &case_dirs {
            let name = case
                .file_name()
                .and_then(|name| name.to_str())
                .expect("case name is UTF-8");
            let arg = fs::read_to_string(case.join("arg")).expect("read arg");
            let arg = arg
                .strip_suffix('\n')
                .expect("arg fixture ends with a newline")
                .replace(ROOT_MARK, root.text());
            let expected = fs::read_to_string(case.join("stdout")).expect("read stdout");
            let expected = expected.replace(ROOT_MARK, root.text());

            let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
                .args(["util", "realpath"])
                .arg(&arg)
                .current_dir(&root.0)
                .env_clear()
                .output()
                .expect("run mcao");
            assert!(
                output.status.success(),
                "case {name}: exit {:?}, stderr {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                output.stdout,
                expected.as_bytes(),
                "case {name} (arg {arg:?})"
            );
        }
    }
}
