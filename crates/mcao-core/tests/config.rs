mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use mcao_core::config::{Config, ConfigError, Setting, Source, load};
#[cfg(unix)]
use mcao_core::config::{SkipReason, Skipped};
use mcao_core::home::{Env, FmHome, READ_ENV, RootSource, capture_env, resolve};
use support::{TempDir, env, text, write};

/// Every documented key, in the sorted order `Config::iter` yields them.
const DEFAULT_KEYS: [&str; 6] = [
    "backend",
    "branch-prefix",
    "claude-permission-mode",
    "crew-harness",
    "developer-name",
    "max-workers",
];

/// The expected source of one resolved value, with file paths derived from the fixture.
#[derive(Clone, Copy)]
enum Want {
    Flag,
    Env(&'static str),
    File,
    Detected(&'static str),
    Default,
}

impl Want {
    fn source(self, config_dir: &Path, key: &str) -> Source {
        match self {
            Want::Flag => Source::Flag,
            Want::Env(var) => Source::Env(var),
            Want::File => Source::File(config_dir.join(key)),
            Want::Detected(marker) => Source::Detected(marker),
            Want::Default => Source::Default,
        }
    }
}

struct BackendRow {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
    flags: &'static [(&'static str, &'static str)],
    env: &'static [(&'static str, &'static str)],
    value: &'static str,
    source: Want,
}

/// Backend precedence: flag > FM_BACKEND > config/backend > runtime marker > default tmux.
const BACKEND_ROWS: &[BackendRow] = &[
    BackendRow {
        name: "flag beats env, file, and both markers",
        files: &[("backend", "tmux\n")],
        flags: &[("backend", "herdr")],
        env: &[
            ("FM_BACKEND", "tmux"),
            ("TMUX", "/tmp/tmux-1/default,1,0"),
            ("HERDR_ENV", "1"),
        ],
        value: "herdr",
        source: Want::Flag,
    },
    BackendRow {
        name: "FM_BACKEND beats file and markers",
        files: &[("backend", "tmux\n")],
        flags: &[],
        env: &[("FM_BACKEND", "herdr"), ("TMUX", "/tmp/tmux-1/default,1,0")],
        value: "herdr",
        source: Want::Env("FM_BACKEND"),
    },
    BackendRow {
        name: "config file beats markers",
        files: &[("backend", "herdr\n")],
        flags: &[],
        env: &[("TMUX", "/tmp/tmux-1/default,1,0"), ("HERDR_ENV", "1")],
        value: "herdr",
        source: Want::File,
    },
    BackendRow {
        name: "TMUX is checked before HERDR_ENV",
        files: &[],
        flags: &[],
        env: &[("TMUX", "/tmp/tmux-1/default,1,0"), ("HERDR_ENV", "1")],
        value: "tmux",
        source: Want::Detected("TMUX"),
    },
    BackendRow {
        name: "HERDR_ENV=1 alone detects herdr",
        files: &[],
        flags: &[],
        env: &[("HERDR_ENV", "1")],
        value: "herdr",
        source: Want::Detected("HERDR_ENV"),
    },
    BackendRow {
        name: "HERDR_ENV other than 1 does not detect",
        files: &[],
        flags: &[],
        env: &[("HERDR_ENV", "0")],
        value: "tmux",
        source: Want::Default,
    },
    BackendRow {
        name: "no source falls to the default tmux",
        files: &[],
        flags: &[],
        env: &[],
        value: "tmux",
        source: Want::Default,
    },
    BackendRow {
        name: "empty FM_BACKEND is unset",
        files: &[("backend", "herdr\n")],
        flags: &[],
        env: &[("FM_BACKEND", "")],
        value: "herdr",
        source: Want::File,
    },
    BackendRow {
        name: "blank config file is unset and detection applies",
        files: &[("backend", " \t\n\n")],
        flags: &[],
        env: &[("HERDR_ENV", "1")],
        value: "herdr",
        source: Want::Detected("HERDR_ENV"),
    },
    BackendRow {
        name: "empty TMUX is not a marker",
        files: &[],
        flags: &[],
        env: &[("TMUX", ""), ("HERDR_ENV", "1")],
        value: "herdr",
        source: Want::Detected("HERDR_ENV"),
    },
    BackendRow {
        name: "first non-blank line of the file is the value",
        files: &[("backend", "\n\n  herdr  \nignored\n")],
        flags: &[],
        env: &[],
        value: "herdr",
        source: Want::File,
    },
    BackendRow {
        name: "backend file squeezes whitespace inside its line like tr",
        files: &[("backend", "  \t\n  he rdr \n")],
        flags: &[],
        env: &[],
        value: "herdr",
        source: Want::File,
    },
    BackendRow {
        name: "vertical tab around the backend word is whitespace",
        files: &[("backend", "\u{0b}herdr\u{0b}\n")],
        flags: &[],
        env: &[],
        value: "herdr",
        source: Want::File,
    },
    BackendRow {
        name: "FM_BACKEND is used exactly as given",
        files: &[],
        flags: &[],
        env: &[("FM_BACKEND", "tm ux")],
        value: "tm ux",
        source: Want::Env("FM_BACKEND"),
    },
    BackendRow {
        name: "a file not selected by precedence is never read",
        files: &[("backend", "tmux\u{1}\n")],
        flags: &[],
        env: &[("FM_BACKEND", "herdr")],
        value: "herdr",
        source: Want::Env("FM_BACKEND"),
    },
];

fn home(tmp: &TempDir) -> FmHome {
    resolve(
        None,
        &env(&[("FM_HOME", text(&tmp.path().join("home")))]),
        tmp.path(),
        &tmp.path().join("code"),
    )
    .expect("fixture home resolves")
}

fn config_dir(tmp: &TempDir) -> PathBuf {
    tmp.path().join("home").join("config")
}

fn flags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|&(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// Writes `files` under the fixture home's `config/`, then loads with the given flags and env.
fn run(
    tmp: &TempDir,
    files: &[(&str, &str)],
    flag_pairs: &[(&str, &str)],
    env_pairs: &[(&str, &str)],
) -> Result<Config, ConfigError> {
    let home = home(tmp);
    for (name, content) in files {
        write(&home.config.join(name), content);
    }
    load(&home, &flags(flag_pairs), &env(env_pairs))
}

fn setting<'a>(config: &'a Config, key: &str) -> &'a Setting {
    config
        .get(key)
        .unwrap_or_else(|| panic!("expected setting {key}"))
}

#[test]
fn defaults_apply_when_no_config_exists_and_nothing_is_created() {
    let tmp = TempDir::new("cfg-defaults");
    let config = run(&tmp, &[], &[], &[]).expect("load succeeds");

    let got: Vec<(&str, &str, Source)> = config
        .iter()
        .map(|(key, value)| (key, value.value.as_str(), value.source.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("backend", "tmux", Source::Default),
            ("branch-prefix", "moa/", Source::Default),
            ("claude-permission-mode", "bypass", Source::Default),
            ("crew-harness", "default", Source::Default),
            ("developer-name", "Developer", Source::Default),
            ("max-workers", "6", Source::Default),
        ]
    );
    assert!(!config_dir(&tmp).exists(), "loading never creates config/");
}

#[test]
fn backend_precedence_covers_every_source_combination() {
    for row in BACKEND_ROWS {
        let tmp = TempDir::new("cfg-backend");
        let config_dir = config_dir(&tmp);
        let config = run(&tmp, row.files, row.flags, row.env)
            .unwrap_or_else(|error| panic!("{}: unexpected error {error}", row.name));
        let resolved = setting(&config, "backend");

        assert_eq!(resolved.value, row.value, "{}: value", row.name);
        assert_eq!(
            resolved.source,
            row.source.source(&config_dir, "backend"),
            "{}: source",
            row.name
        );
    }
}

#[test]
fn crew_harness_follows_the_shell_whole_file_rule() {
    // Absent and blank files mirror the firstmate's own harness, which "default" stands for.
    let cases: [(Option<&str>, &str, Want); 6] = [
        (None, "default", Want::Default),
        (Some("default\n"), "default", Want::File),
        (Some(" \t\n\n"), "default", Want::Default),
        (Some("cl aude\n"), "claude", Want::File),
        (Some("claude\ncodex\n"), "claudecodex", Want::File),
        (Some("claude\u{0b}\n"), "claude", Want::File),
    ];
    for (content, value, want) in cases {
        let tmp = TempDir::new("cfg-crew");
        let files: Vec<(&str, &str)> = content
            .map(|raw| vec![("crew-harness", raw)])
            .unwrap_or_default();
        let config = run(&tmp, &files, &[], &[]).expect("load succeeds");
        let resolved = setting(&config, "crew-harness");

        let case = format!("{content:?}");
        assert_eq!(resolved.value, value, "{case}: value");
        assert_eq!(
            resolved.source,
            want.source(&config_dir(&tmp), "crew-harness"),
            "{case}: source"
        );
    }

    let tmp = TempDir::new("cfg-crew-control");
    let error = run(&tmp, &[("crew-harness", "claude\u{1}\n")], &[], &[])
        .expect_err("control byte is refused");
    assert_eq!(
        error,
        ConfigError::ControlByte {
            key: "crew-harness".to_owned(),
            source: Source::File(config_dir(&tmp).join("crew-harness")),
            byte: 0x01,
        }
    );
}

#[test]
fn claude_permission_mode_follows_the_spawn_whole_file_rule() {
    // bin/fm-spawn.sh squeezes the whole file, then accepts only bypass or auto. A value that the
    // squeeze leaves outside that set is refused below rather than handed to the consumer.
    let cases: [(Option<&str>, &str, Want); 5] = [
        (None, "bypass", Want::Default),
        (Some("bypass\n"), "bypass", Want::File),
        (Some("au to\n"), "auto", Want::File),
        (Some("\u{0b}auto\n"), "auto", Want::File),
        (Some("auto\r\n"), "auto", Want::File),
    ];
    for (content, value, want) in cases {
        let tmp = TempDir::new("cfg-permission");
        let files: Vec<(&str, &str)> = content
            .map(|raw| vec![("claude-permission-mode", raw)])
            .unwrap_or_default();
        let config = run(&tmp, &files, &[], &[]).expect("load succeeds");
        let resolved = setting(&config, "claude-permission-mode");

        let case = format!("{content:?}");
        assert_eq!(resolved.value, value, "{case}: value");
        assert_eq!(
            resolved.source,
            want.source(&config_dir(&tmp), "claude-permission-mode"),
            "{case}: source"
        );
    }

    // A squeezed value outside the set is refused. A blank file keeps its empty value, so it is
    // refused too rather than falling back to the default.
    let refused: [(&str, &str); 3] = [
        ("bypass\nauto\n", "bypassauto"),
        ("  \t\n\n", ""),
        ("bypass\n# note\n", "bypass#note"),
    ];
    for (content, value) in refused {
        let tmp = TempDir::new("cfg-permission-refused");
        let error = run(&tmp, &[("claude-permission-mode", content)], &[], &[])
            .expect_err("a value outside the accepted set is refused");
        assert_eq!(
            error,
            ConfigError::NotAccepted {
                key: "claude-permission-mode".to_owned(),
                source: Source::File(config_dir(&tmp).join("claude-permission-mode")),
                value: value.to_owned(),
                accepted: &["bypass", "auto"],
            },
            "{content:?}"
        );
    }

    // A flag wins precedence, so the file it shadows is never examined. A flag outside the set
    // is refused.
    let tmp = TempDir::new("cfg-permission-flag");
    let by_flag = run(
        &tmp,
        &[("claude-permission-mode", "bogus\n")],
        &[("claude-permission-mode", "auto")],
        &[],
    )
    .expect("a flag shadows the file");
    assert_eq!(
        setting(&by_flag, "claude-permission-mode").source,
        Source::Flag
    );
    let error = run(&tmp, &[], &[("claude-permission-mode", "bogus")], &[])
        .expect_err("a flag outside the set is refused");
    assert_eq!(
        error,
        ConfigError::NotAccepted {
            key: "claude-permission-mode".to_owned(),
            source: Source::Flag,
            value: "bogus".to_owned(),
            accepted: &["bypass", "auto"],
        }
    );

    let tmp = TempDir::new("cfg-permission-control");
    let error = run(&tmp, &[("claude-permission-mode", "auto\u{1}\n")], &[], &[])
        .expect_err("control byte is refused");
    assert_eq!(
        error,
        ConfigError::ControlByte {
            key: "claude-permission-mode".to_owned(),
            source: Source::File(config_dir(&tmp).join("claude-permission-mode")),
            byte: 0x01,
        }
    );
}

#[test]
fn directory_under_a_key_tested_by_lstat_is_refused() {
    // bin/fm-spawn.sh tests claude-permission-mode with lstat, so a directory of that name is
    // present and refused rather than read as the default.
    let tmp = TempDir::new("cfg-dir-lstat");
    fs::create_dir_all(config_dir(&tmp).join("claude-permission-mode")).expect("create dir");
    let error = run(&tmp, &[], &[], &[]).expect_err("a directory under an lstat key is refused");
    assert_eq!(
        error,
        ConfigError::NotRegular {
            path: config_dir(&tmp).join("claude-permission-mode"),
        }
    );

    // A flag shadows the directory, so it is never consumed.
    let shadowed = TempDir::new("cfg-dir-lstat-flag");
    fs::create_dir_all(config_dir(&shadowed).join("claude-permission-mode")).expect("create dir");
    let config = run(&shadowed, &[], &[("claude-permission-mode", "auto")], &[])
        .expect("a flag shadows the directory");
    assert_eq!(
        setting(&config, "claude-permission-mode").source,
        Source::Flag
    );
}

#[test]
fn directory_under_a_key_tested_by_file_test_reads_as_unset() {
    // Every other reader tests [ -f ], which a directory fails, so the key is unset and its
    // default applies, the same as an absent file.
    let tmp = TempDir::new("cfg-dir-file-test");
    fs::create_dir_all(config_dir(&tmp).join("developer-name")).expect("create dir");

    let config =
        run(&tmp, &[], &[], &[]).expect("a directory under a file-test key does not abort");
    assert_eq!(setting(&config, "developer-name").value, "Developer");
    assert_eq!(setting(&config, "developer-name").source, Source::Default);
    assert!(config.unknown("developer-name").is_none());
    assert!(config.skipped().is_empty());
}

#[test]
fn non_breaking_space_is_text_not_shell_space() {
    // Rust treats U+00A0 as text. Its shell-space set is ASCII-only, so the result does not
    // depend on locale. The shell readers do: under a UTF-8 locale (macOS), `tr` and `sed`
    // strip U+00A0, so they accept a value this rule keeps. The difference is deliberate.
    let tmp = TempDir::new("cfg-nbsp");
    let config = run(
        &tmp,
        &[
            ("developer-name", "\u{a0}Dev\n"),
            ("branch-prefix", "\u{a0}moa/\n"),
            ("crew-harness", "claude\u{a0}\n"),
        ],
        &[],
        &[],
    )
    .expect("load succeeds");
    assert_eq!(setting(&config, "developer-name").value, "\u{a0}Dev");
    assert_eq!(setting(&config, "branch-prefix").value, "\u{a0}moa/");
    assert_eq!(setting(&config, "crew-harness").value, "claude\u{a0}");
}

#[test]
fn branch_prefix_follows_the_shell_first_line_rule() {
    let cases: [(&str, &str, Want); 4] = [
        ("  moa-team/ \nsecond/\n", "moa-team/", Want::File),
        // A blank first line is unset, so the later line is never read.
        ("\n  moa-team/ \nsecond/\n", "moa/", Want::Default),
        ("\u{0b}moa-team/\u{0c}\r\n", "moa-team/", Want::File),
        ("   \nmoa/\n", "moa/", Want::Default),
    ];
    for (content, value, want) in cases {
        let tmp = TempDir::new("cfg-branch-prefix");
        let config = run(&tmp, &[("branch-prefix", content)], &[], &[]).expect("load succeeds");
        let resolved = setting(&config, "branch-prefix");

        assert_eq!(resolved.value, value, "{content:?}: value");
        assert_eq!(
            resolved.source,
            want.source(&config_dir(&tmp), "branch-prefix"),
            "{content:?}: source"
        );
    }
}

#[test]
fn plain_keys_follow_flag_then_file_then_default() {
    // (flag, file, expected value, expected source)
    let cases = [
        (Some("8"), Some("4"), "8", Want::Flag),
        (None, Some("4"), "4", Want::File),
        (Some("8"), None, "8", Want::Flag),
        (None, None, "6", Want::Default),
    ];
    for (flag, file, value, want) in cases {
        let tmp = TempDir::new("cfg-plain");
        let files: Vec<(&str, &str)> = file
            .map(|content| vec![("max-workers", content)])
            .unwrap_or_default();
        let flag_pairs: Vec<(&str, &str)> = flag
            .map(|raw| vec![("max-workers", raw)])
            .unwrap_or_default();
        let config = run(&tmp, &files, &flag_pairs, &[]).expect("load succeeds");
        let resolved = setting(&config, "max-workers");

        let case = format!("flag={flag:?} file={file:?}");
        assert_eq!(resolved.value, value, "{case}: value");
        assert_eq!(
            resolved.source,
            want.source(&config_dir(&tmp), "max-workers"),
            "{case}: source"
        );
    }
}

#[test]
fn blank_file_is_unset_and_the_default_applies() {
    let tmp = TempDir::new("cfg-blank");
    let config = run(&tmp, &[("max-workers", "  \t\n\n")], &[], &[]).expect("load succeeds");
    assert_eq!(setting(&config, "max-workers").value, "6");
    assert_eq!(setting(&config, "max-workers").source, Source::Default);
}

#[test]
fn later_lines_of_a_file_are_not_read() {
    let tmp = TempDir::new("cfg-later-lines");
    let config =
        run(&tmp, &[("branch-prefix", "moa/\n\u{1}bad\n")], &[], &[]).expect("load succeeds");
    assert_eq!(setting(&config, "branch-prefix").value, "moa/");
}

#[test]
fn control_bytes_in_a_file_value_are_refused() {
    let cases: [(&str, u8); 4] = [
        ("moa\u{1}/", 0x01),
        ("moa\t/", 0x09),
        ("moa\0/", 0x00),
        ("moa\u{7f}/", 0x7f),
    ];
    for (content, byte) in cases {
        let tmp = TempDir::new("cfg-control-file");
        let error = run(&tmp, &[("branch-prefix", content)], &[], &[])
            .expect_err("control byte is refused");
        assert_eq!(
            error,
            ConfigError::ControlByte {
                key: "branch-prefix".to_owned(),
                source: Source::File(config_dir(&tmp).join("branch-prefix")),
                byte,
            },
            "content {content:?}"
        );
    }
}

#[test]
fn control_bytes_in_flags_and_env_are_refused() {
    let tmp = TempDir::new("cfg-control-flag");
    let from_flag =
        run(&tmp, &[], &[("developer-name", "Dev\neloper")], &[]).expect_err("flag refused");
    assert_eq!(
        from_flag,
        ConfigError::ControlByte {
            key: "developer-name".to_owned(),
            source: Source::Flag,
            byte: 0x0a,
        }
    );

    let tmp = TempDir::new("cfg-control-env");
    let from_env = run(&tmp, &[], &[], &[("FM_BACKEND", "tmux\n")]).expect_err("env refused");
    assert_eq!(
        from_env,
        ConfigError::ControlByte {
            key: "backend".to_owned(),
            source: Source::Env("FM_BACKEND"),
            byte: 0x0a,
        }
    );
}

#[test]
fn empty_flag_and_unknown_flag_are_refused() {
    let tmp = TempDir::new("cfg-flag-errors");
    let empty = run(&tmp, &[], &[("developer-name", "")], &[]).expect_err("empty flag refused");
    assert_eq!(
        empty,
        ConfigError::EmptyFlag {
            key: "developer-name".to_owned()
        }
    );

    let unknown = run(&tmp, &[], &[("nonsense", "x")], &[]).expect_err("unknown flag refused");
    assert_eq!(
        unknown,
        ConfigError::UnknownKey {
            key: "nonsense".to_owned()
        }
    );
}

#[test]
fn unknown_files_keep_their_exact_bytes_and_presence() {
    let tmp = TempDir::new("cfg-unknown-bytes");
    let files = [
        ("future-key", "alpha\n"),
        ("notes", " \n"),
        ("crew-dispatch.json", "{\n  \"a\": 1\n}\n"),
        ("launch-env-allowlist", "A\nB\nC"),
        // An empty marker file is present, which is what a presence flag means.
        ("wait-no-turns", ""),
    ];
    let config = run(&tmp, &files, &[], &[]).expect("load succeeds");

    for (key, content) in files {
        let entry = config
            .unknown(key)
            .unwrap_or_else(|| panic!("{key} is kept"));
        assert_eq!(entry.bytes, content.as_bytes(), "{key}: exact bytes");
        assert_eq!(entry.path, config_dir(&tmp).join(key), "{key}: path");
    }
    assert_eq!(config.unknown_iter().count(), files.len());
    assert!(config.skipped().is_empty());
}

#[test]
fn unknown_files_never_change_known_settings() {
    let tmp = TempDir::new("cfg-unknown-known");
    let config = run(&tmp, &[("future-key", "alpha\n")], &[], &[]).expect("load succeeds");

    let keys: Vec<&str> = config.iter().map(|(key, _)| key).collect();
    assert_eq!(keys, DEFAULT_KEYS.to_vec());
    assert_eq!(setting(&config, "max-workers").source, Source::Default);

    let unknown: Vec<&str> = config.unknown_iter().map(|(key, _)| key).collect();
    assert_eq!(unknown, vec!["future-key"]);
}

#[test]
fn binary_and_control_byte_unknown_files_never_abort_the_load() {
    // A macOS `.DS_Store`-shaped binary and a file whose only line holds a tab must not stop
    // the load, because nothing consumes them.
    let tmp = TempDir::new("cfg-binary-unknown");
    let home = home(&tmp);
    let store = [0x00u8, 0x00, 0x00, 0x01, 0x09, 0x1b, 0xff];
    write(&home.config.join(".DS_Store"), store);
    write(&home.config.join("tabbed"), "a\tb\n");

    let config = load(&home, &flags(&[]), &Env::new()).expect("stray files do not abort");
    assert_eq!(config.unknown(".DS_Store").expect("kept").bytes, store);
    assert_eq!(config.unknown("tabbed").expect("kept").bytes, b"a\tb\n");
    assert!(config.skipped().is_empty());
}

#[test]
fn subdirectories_under_config_are_skipped_silently() {
    let tmp = TempDir::new("cfg-subdir");
    let config =
        run(&tmp, &[("extensions.d/inside", "ignored\n")], &[], &[]).expect("load succeeds");
    assert!(config.unknown("extensions.d").is_none());
    assert!(config.unknown("inside").is_none());
    assert!(config.skipped().is_empty());
    let keys: Vec<&str> = config.iter().map(|(key, _)| key).collect();
    assert_eq!(keys, DEFAULT_KEYS.to_vec());
}

#[test]
fn non_utf8_content_is_refused_only_when_read() {
    let tmp = TempDir::new("cfg-non-utf8");
    let file = config_dir(&tmp).join("developer-name");
    write(&file, [0xffu8, 0xfe]);

    let error = run(&tmp, &[], &[], &[]).expect_err("non-UTF-8 value is refused");
    assert_eq!(error, ConfigError::NotUtf8 { path: file });

    let config = run(&tmp, &[], &[("developer-name", "Dev")], &[])
        .expect("flag wins without reading the file");
    assert_eq!(setting(&config, "developer-name").value, "Dev");
    assert_eq!(setting(&config, "developer-name").source, Source::Flag);
}

/// Captures `vars` the way a process environment would, then resolves and loads the fixture
/// home. Nothing is written, so the home and its `config/` may be absent.
fn resolve_and_load(base: &Path, vars: &[(&str, String)]) -> (FmHome, Config) {
    let raw = vars
        .iter()
        .map(|(name, value)| (OsString::from(*name), OsString::from(value.as_str())));
    let captured = capture_env(raw).expect("captures");
    let home =
        resolve(None, &captured, &base.join("cwd"), &base.join("default")).expect("home resolves");
    let config = load(&home, &BTreeMap::new(), &captured).expect("config loads");
    (home, config)
}

#[test]
fn every_read_name_reaches_resolve_or_load() {
    // The names this crate reads, spelled out here: dropping one from READ_ENV, or adding one
    // without a case below, fails this test.
    let mut read = READ_ENV.to_vec();
    read.sort_unstable();
    assert_eq!(
        read,
        vec![
            "FM_BACKEND",
            "FM_CONFIG_OVERRIDE",
            "FM_DATA_OVERRIDE",
            "FM_HOME",
            "FM_PROJECTS_OVERRIDE",
            "FM_ROOT",
            "FM_ROOT_OVERRIDE",
            "FM_STATE_OVERRIDE",
            "HERDR_ENV",
            "TMUX",
        ]
    );

    let tmp = TempDir::new("cfg-read-env");
    let base = tmp.path();
    let alt = |name: &str| text(&base.join(name));

    let (home, _) = resolve_and_load(base, &[("FM_HOME", alt("alt-home"))]);
    assert_eq!(home.root, base.join("alt-home"));
    assert_eq!(home.root_source, RootSource::Env("FM_HOME"));

    let (home, _) = resolve_and_load(base, &[("FM_ROOT_OVERRIDE", alt("alt-root-override"))]);
    assert_eq!(home.root, base.join("alt-root-override"));
    assert_eq!(home.root_source, RootSource::Env("FM_ROOT_OVERRIDE"));

    let (home, _) = resolve_and_load(base, &[("FM_ROOT", alt("alt-root"))]);
    assert_eq!(home.root, base.join("alt-root"));
    assert_eq!(home.root_source, RootSource::Env("FM_ROOT"));

    let (home, _) = resolve_and_load(base, &[("FM_CONFIG_OVERRIDE", alt("alt-config"))]);
    assert_eq!(home.config, base.join("alt-config"));

    let (home, _) = resolve_and_load(base, &[("FM_STATE_OVERRIDE", alt("alt-state"))]);
    assert_eq!(home.state, base.join("alt-state"));

    let (home, _) = resolve_and_load(base, &[("FM_DATA_OVERRIDE", alt("alt-data"))]);
    assert_eq!(home.data, base.join("alt-data"));

    let (home, _) = resolve_and_load(base, &[("FM_PROJECTS_OVERRIDE", alt("alt-projects"))]);
    assert_eq!(home.projects, base.join("alt-projects"));

    let (_, config) = resolve_and_load(base, &[("FM_BACKEND", "herdr".to_owned())]);
    assert_eq!(
        setting(&config, "backend"),
        &Setting {
            value: "herdr".to_owned(),
            source: Source::Env("FM_BACKEND"),
        }
    );

    let (_, config) = resolve_and_load(base, &[("TMUX", "/tmp/tmux-1/default,1,0".to_owned())]);
    assert_eq!(
        setting(&config, "backend"),
        &Setting {
            value: "tmux".to_owned(),
            source: Source::Detected("TMUX"),
        }
    );

    let (_, config) = resolve_and_load(base, &[("HERDR_ENV", "1".to_owned())]);
    assert_eq!(
        setting(&config, "backend"),
        &Setting {
            value: "herdr".to_owned(),
            source: Source::Detected("HERDR_ENV"),
        }
    );
}

#[cfg(unix)]
mod unix_entries {
    use std::io::ErrorKind;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn unknown_symlink_is_skipped_with_a_diagnostic() {
        let tmp = TempDir::new("cfg-symlink");
        let config_path = config_dir(&tmp);
        write(&config_path.join("real"), "value\n");
        symlink(config_path.join("real"), config_path.join("alias")).expect("create alias");

        let config = run(&tmp, &[], &[], &[]).expect("an unknown symlink does not abort");
        assert!(config.unknown("alias").is_none(), "a symlink is never read");
        assert_eq!(
            config.unknown("real").expect("regular file kept").bytes,
            b"value\n"
        );
        assert_eq!(
            config.skipped().to_vec(),
            vec![Skipped {
                path: config_path.join("alias"),
                reason: SkipReason::Symlink,
            }]
        );
    }

    #[test]
    fn unknown_symlink_to_a_directory_is_skipped() {
        let tmp = TempDir::new("cfg-symlink-dir");
        let config_path = config_dir(&tmp);
        fs::create_dir_all(config_path.join("real-dir")).expect("create dir");
        symlink(config_path.join("real-dir"), config_path.join("dir-alias")).expect("alias");

        let config = run(&tmp, &[], &[], &[]).expect("a symlink to a directory does not abort");
        assert_eq!(
            config.skipped().to_vec(),
            vec![Skipped {
                path: config_path.join("dir-alias"),
                reason: SkipReason::Symlink,
            }]
        );
    }

    #[test]
    fn consumed_symlink_is_refused() {
        let tmp = TempDir::new("cfg-consumed-link");
        let config_path = config_dir(&tmp);
        write(&config_path.join("real"), "herdr\n");
        symlink(config_path.join("real"), config_path.join("backend")).expect("create link");

        let error = run(&tmp, &[], &[], &[]).expect_err("a consumed symlink is refused");
        assert_eq!(
            error,
            ConfigError::Symlink {
                path: config_path.join("backend"),
            }
        );
    }

    #[test]
    fn shadowed_symlink_is_never_examined() {
        let tmp = TempDir::new("cfg-shadowed-link");
        let config_path = config_dir(&tmp);
        write(&config_path.join("real"), "herdr\n");
        symlink(config_path.join("real"), config_path.join("backend")).expect("create link");

        let by_flag = run(&tmp, &[], &[("backend", "tmux")], &[]).expect("a flag shadows it");
        assert_eq!(setting(&by_flag, "backend").source, Source::Flag);

        let by_env =
            run(&tmp, &[], &[], &[("FM_BACKEND", "herdr")]).expect("FM_BACKEND shadows it");
        assert_eq!(
            setting(&by_env, "backend").source,
            Source::Env("FM_BACKEND")
        );
    }

    #[test]
    fn unknown_special_file_is_skipped_without_opening_it() {
        let tmp = TempDir::new("cfgsock");
        let config_path = config_dir(&tmp);
        fs::create_dir_all(&config_path).expect("create config");
        let _socket = UnixListener::bind(config_path.join("sock")).expect("bind socket fixture");

        let config = run(&tmp, &[], &[], &[]).expect("an unknown socket does not abort");
        assert_eq!(
            config.skipped().to_vec(),
            vec![Skipped {
                path: config_path.join("sock"),
                reason: SkipReason::NotRegular,
            }]
        );
    }

    #[test]
    fn consumed_special_file_is_refused_without_opening_it() {
        let tmp = TempDir::new("cfgsk2");
        let config_path = config_dir(&tmp);
        fs::create_dir_all(&config_path).expect("create config");
        let _socket =
            UnixListener::bind(config_path.join("max-workers")).expect("bind socket fixture");

        let error = run(&tmp, &[], &[], &[]).expect_err("a consumed socket is refused");
        assert_eq!(
            error,
            ConfigError::NotRegular {
                path: config_path.join("max-workers"),
            }
        );
    }

    #[test]
    fn skipped_entries_are_sorted_by_path() {
        // Socket names stay short: a Unix socket path is limited to about 104 bytes.
        let tmp = TempDir::new("cfgord");
        let config_path = config_dir(&tmp);
        fs::create_dir_all(&config_path).expect("create config");
        // Created out of path order, so the sorted result cannot be creation order.
        let _zz = UnixListener::bind(config_path.join("zz")).expect("bind socket fixture");
        let _mm = UnixListener::bind(config_path.join("mm")).expect("bind socket fixture");
        symlink("nowhere", config_path.join("aa")).expect("create dangling link");

        let config = run(&tmp, &[], &[], &[]).expect("unknown entries do not abort");
        assert_eq!(
            config.skipped().to_vec(),
            vec![
                Skipped {
                    path: config_path.join("aa"),
                    reason: SkipReason::Symlink,
                },
                Skipped {
                    path: config_path.join("mm"),
                    reason: SkipReason::NotRegular,
                },
                Skipped {
                    path: config_path.join("zz"),
                    reason: SkipReason::NotRegular,
                },
            ]
        );
    }

    #[test]
    fn unreadable_consumed_file_is_an_io_error_not_an_unset_value() {
        let tmp = TempDir::new("cfgunr");
        let file = config_dir(&tmp).join("max-workers");
        write(&file, "4\n");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).expect("make unreadable");
        // Root reads mode 0o000 files, so only an unprivileged user can observe the refusal.
        let privileged = fs::read(&file).is_ok();
        let result = run(&tmp, &[], &[], &[]);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("restore for cleanup");
        if privileged {
            eprintln!("skipped: process reads mode 0o000 files, so it is privileged");
            return;
        }
        assert_eq!(
            result.expect_err("an unreadable consumed file is refused"),
            ConfigError::Io {
                path: file,
                kind: ErrorKind::PermissionDenied,
            }
        );
    }

    #[test]
    fn unreadable_config_directory_is_an_io_error_not_an_empty_config() {
        let tmp = TempDir::new("cfgnd");
        let config_path = config_dir(&tmp);
        write(&config_path.join("max-workers"), "4\n");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o000))
            .expect("make unlistable");
        // Root lists mode 0o000 directories, so only an unprivileged user can observe the refusal.
        let privileged = fs::read_dir(&config_path).is_ok();
        let result = run(&tmp, &[], &[], &[]);
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o755))
            .expect("restore for cleanup");
        if privileged {
            eprintln!("skipped: process lists mode 0o000 directories, so it is privileged");
            return;
        }
        assert_eq!(
            result.expect_err("an unlistable config directory is refused"),
            ConfigError::Io {
                path: config_path,
                kind: ErrorKind::PermissionDenied,
            }
        );
    }

    // APFS (macOS) rejects non-UTF-8 file names with EILSEQ, so this fixture exists only on
    // filesystems that accept raw bytes in names.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_utf8_entry_name_is_skipped() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new("cfg-name");
        let config_path = config_dir(&tmp);
        let name = OsStr::from_bytes(b"bad-\xff");
        write(&config_path.join(name), "value\n");

        let config = run(&tmp, &[], &[], &[]).expect("a non-UTF-8 name does not abort");
        assert_eq!(
            config.skipped().to_vec(),
            vec![Skipped {
                path: config_path.join(name),
                reason: SkipReason::NameNotUtf8,
            }]
        );
    }
}
