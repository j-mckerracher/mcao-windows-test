mod support;

use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use mcao_core::home::{Env, EnvError, HomeError, RootSource, capture_env, resolve};
use support::{TempDir, env, text, write};

#[test]
fn root_precedence_covers_all_sixteen_presence_combinations() {
    let tmp = TempDir::new("home-root-table");
    let base = tmp.path();
    let cwd = base.join("cwd");
    let default_root = base.join("default");

    // flag, FM_HOME, FM_ROOT_OVERRIDE, FM_ROOT present; expected root name; expected source.
    let rows = [
        (false, false, false, false, "default", RootSource::Default),
        (
            false,
            false,
            false,
            true,
            "fm-root",
            RootSource::Env("FM_ROOT"),
        ),
        (
            false,
            false,
            true,
            false,
            "fm-root-override",
            RootSource::Env("FM_ROOT_OVERRIDE"),
        ),
        (
            false,
            false,
            true,
            true,
            "fm-root-override",
            RootSource::Env("FM_ROOT_OVERRIDE"),
        ),
        (
            false,
            true,
            false,
            false,
            "fm-home",
            RootSource::Env("FM_HOME"),
        ),
        (
            false,
            true,
            false,
            true,
            "fm-home",
            RootSource::Env("FM_HOME"),
        ),
        (
            false,
            true,
            true,
            false,
            "fm-home",
            RootSource::Env("FM_HOME"),
        ),
        (
            false,
            true,
            true,
            true,
            "fm-home",
            RootSource::Env("FM_HOME"),
        ),
        (true, false, false, false, "flag", RootSource::Flag),
        (true, false, false, true, "flag", RootSource::Flag),
        (true, false, true, false, "flag", RootSource::Flag),
        (true, false, true, true, "flag", RootSource::Flag),
        (true, true, false, false, "flag", RootSource::Flag),
        (true, true, false, true, "flag", RootSource::Flag),
        (true, true, true, false, "flag", RootSource::Flag),
        (true, true, true, true, "flag", RootSource::Flag),
    ];

    for (flag, home, root_override, root, expected_name, expected_source) in rows {
        let mut vars: Vec<(&str, String)> = Vec::new();
        if home {
            vars.push(("FM_HOME", text(&base.join("fm-home"))));
        }
        if root_override {
            vars.push(("FM_ROOT_OVERRIDE", text(&base.join("fm-root-override"))));
        }
        if root {
            vars.push(("FM_ROOT", text(&base.join("fm-root"))));
        }
        let flag_value = text(&base.join("flag"));
        let flag_arg = flag.then_some(flag_value.as_str());
        let case =
            format!("flag={flag} FM_HOME={home} FM_ROOT_OVERRIDE={root_override} FM_ROOT={root}");

        let resolved = resolve(flag_arg, &env(vars.as_slice()), &cwd, &default_root)
            .unwrap_or_else(|error| panic!("{case}: unexpected error {error}"));

        assert_eq!(resolved.root, base.join(expected_name), "{case}: root");
        assert_eq!(resolved.root_source, expected_source, "{case}: source");
    }
}

#[test]
fn derived_directories_sit_under_the_root() {
    let tmp = TempDir::new("home-derived");
    let base = tmp.path();
    let root = base.join("home");

    let home = resolve(
        None,
        &env(&[("FM_HOME", text(&root))]),
        base,
        &base.join("default"),
    )
    .expect("home resolves");

    assert_eq!(home.root, root);
    assert_eq!(home.config, root.join("config"));
    assert_eq!(home.state, root.join("state"));
    assert_eq!(home.data, root.join("data"));
    assert_eq!(home.projects, root.join("projects"));
}

#[test]
fn relative_inputs_resolve_against_the_working_directory() {
    let tmp = TempDir::new("home-relative");
    let base = tmp.path();
    let cwd = base.join("cwd");
    let default_root = base.join("default");

    let from_env = resolve(None, &env(&[("FM_HOME", "rel/home")]), &cwd, &default_root)
        .expect("relative FM_HOME resolves");
    assert_eq!(from_env.root, cwd.join("rel/home"));
    assert_eq!(from_env.root_source, RootSource::Env("FM_HOME"));
    assert_eq!(from_env.config, cwd.join("rel/home").join("config"));

    let from_flag = resolve(Some("flag-rel"), &Env::new(), &cwd, &default_root)
        .expect("relative flag resolves");
    assert_eq!(from_flag.root, cwd.join("flag-rel"));
    assert_eq!(from_flag.root_source, RootSource::Flag);
}

#[test]
fn working_directory_with_a_parent_component_still_resolves_relative_input() {
    // Only the input is checked for `..`, so a working directory that carries one does not
    // refuse a relative input that has none.
    let tmp = TempDir::new("home-cwd-parent");
    let base = tmp.path();
    fs::create_dir_all(base.join("a")).expect("create a");
    let cwd = base.join("a").join("..");
    let default_root = base.join("default");

    let home = resolve(Some("home"), &Env::new(), &cwd, &default_root)
        .expect("relative input resolves under a working directory with `..`");
    assert_eq!(home.root, cwd.join("home"));
    assert_eq!(home.root_source, RootSource::Flag);
}

#[test]
fn relative_results_are_refused_when_the_working_directory_is_relative() {
    let from_flag = resolve(
        Some("flag-rel"),
        &Env::new(),
        Path::new("relative-cwd"),
        Path::new("/absolute/default"),
    )
    .expect_err("a relative working directory cannot anchor a home");
    assert_eq!(
        from_flag,
        HomeError::NotAbsolute {
            source: "--fm-home",
            path: PathBuf::from("relative-cwd/flag-rel"),
        }
    );
}

#[test]
fn relative_default_root_is_refused() {
    let tmp = TempDir::new("home-relative-default");
    let error = resolve(None, &Env::new(), tmp.path(), Path::new("relative-default"))
        .expect_err("a relative default root is refused");
    assert_eq!(
        error,
        HomeError::NotAbsolute {
            source: "default root",
            path: PathBuf::from("relative-default"),
        }
    );
}

#[test]
fn trailing_separators_and_dot_components_are_normalized_away() {
    let tmp = TempDir::new("home-normalize");
    let base = tmp.path();
    let root = base.join("home");
    let default_root = base.join("default");

    for raw in [
        text(&root),
        format!("{}/", text(&root)),
        format!("{}/.", text(&root)),
        format!("{}//./", text(&root)),
    ] {
        let home = resolve(
            None,
            &env(&[("FM_HOME", raw.as_str())]),
            base,
            &default_root,
        )
        .unwrap_or_else(|error| panic!("{raw:?}: unexpected error {error}"));
        assert_eq!(home.root, root, "{raw:?} normalizes to the same root");
    }
}

#[test]
fn parent_components_are_refused_in_every_input() {
    let tmp = TempDir::new("home-parent");
    let base = tmp.path();
    let cwd = base.join("cwd");
    let default_root = base.join("default");

    let flag_value = text(&base.join("a").join("..").join("b"));
    let from_flag = resolve(Some(flag_value.as_str()), &Env::new(), &cwd, &default_root)
        .expect_err("a flag with .. is refused");
    assert_eq!(
        from_flag,
        HomeError::ParentDir {
            source: "--fm-home",
            path: base.join("a").join("..").join("b"),
        }
    );

    let relative_flag = resolve(Some("../home"), &Env::new(), &cwd, &default_root)
        .expect_err("a leading .. is refused");
    assert_eq!(
        relative_flag,
        HomeError::ParentDir {
            source: "--fm-home",
            path: cwd.join("..").join("home"),
        }
    );

    let from_home = resolve(None, &env(&[("FM_HOME", "a/../b")]), &cwd, &default_root)
        .expect_err("FM_HOME with .. is refused");
    assert_eq!(
        from_home,
        HomeError::ParentDir {
            source: "FM_HOME",
            path: cwd.join("a").join("..").join("b"),
        }
    );

    let from_override = resolve(
        None,
        &env(&[(
            "FM_STATE_OVERRIDE",
            text(&base.join("x").join("..").join("y")),
        )]),
        &cwd,
        &default_root,
    )
    .expect_err("an override with .. is refused");
    assert_eq!(
        from_override,
        HomeError::ParentDir {
            source: "FM_STATE_OVERRIDE",
            path: base.join("x").join("..").join("y"),
        }
    );
}

#[test]
fn per_directory_overrides_replace_only_their_own_directory() {
    let tmp = TempDir::new("home-overrides");
    let base = tmp.path();
    let cwd = base.join("cwd");
    let root = base.join("home");
    let config_abs = base.join("elsewhere-config");
    let data_abs = base.join("elsewhere-data");

    let home = resolve(
        None,
        &env(&[
            ("FM_HOME", text(&root)),
            ("FM_CONFIG_OVERRIDE", text(&config_abs)),
            ("FM_STATE_OVERRIDE", "state-rel".to_owned()),
            ("FM_DATA_OVERRIDE", text(&data_abs)),
            ("FM_PROJECTS_OVERRIDE", "".to_owned()),
        ]),
        &cwd,
        &base.join("default"),
    )
    .expect("overrides resolve");

    assert_eq!(home.root, root);
    assert_eq!(home.config, config_abs, "absolute config override");
    assert_eq!(
        home.state,
        cwd.join("state-rel"),
        "relative state override joins cwd"
    );
    assert_eq!(home.data, data_abs, "absolute data override");
    assert_eq!(
        home.projects,
        root.join("projects"),
        "empty override is unset"
    );
}

#[test]
fn empty_environment_values_count_as_unset() {
    let tmp = TempDir::new("home-empty");
    let base = tmp.path();
    let fm_root = base.join("fm-root");
    let default_root = base.join("default");

    let home = resolve(
        None,
        &env(&[("FM_HOME", "".to_owned()), ("FM_ROOT", text(&fm_root))]),
        base,
        &default_root,
    )
    .expect("empty FM_HOME falls through");
    assert_eq!(home.root, fm_root);
    assert_eq!(home.root_source, RootSource::Env("FM_ROOT"));

    let fallback = resolve(
        None,
        &env(&[("FM_ROOT_OVERRIDE", ""), ("FM_CONFIG_OVERRIDE", "")]),
        base,
        &default_root,
    )
    .expect("empty values fall through");
    assert_eq!(fallback.root, default_root);
    assert_eq!(fallback.root_source, RootSource::Default);
    assert_eq!(fallback.config, default_root.join("config"));
}

#[test]
fn empty_flag_is_refused() {
    let tmp = TempDir::new("home-empty-flag");
    let error = resolve(
        Some(""),
        &Env::new(),
        tmp.path(),
        &tmp.path().join("default"),
    )
    .expect_err("empty flag is refused");
    assert_eq!(
        error,
        HomeError::Empty {
            source: "--fm-home"
        }
    );
}

#[test]
fn control_bytes_are_refused_in_every_selected_input() {
    let tmp = TempDir::new("home-control");
    let base = tmp.path();
    let cwd = base.join("cwd");
    let default_root = base.join("default");
    let cases: [(&str, u8); 4] = [
        ("a\nb", 0x0a),
        ("a\tb", 0x09),
        ("a\0b", 0x00),
        ("a\u{7f}b", 0x7f),
    ];

    for (raw, byte) in cases {
        let from_flag = resolve(Some(raw), &Env::new(), &cwd, &default_root).unwrap_err();
        assert_eq!(
            from_flag,
            HomeError::ControlByte {
                source: "--fm-home",
                byte
            },
            "flag {raw:?}"
        );

        let from_home = resolve(None, &env(&[("FM_HOME", raw)]), &cwd, &default_root).unwrap_err();
        assert_eq!(
            from_home,
            HomeError::ControlByte {
                source: "FM_HOME",
                byte
            },
            "FM_HOME {raw:?}"
        );

        let from_override = resolve(
            None,
            &env(&[("FM_STATE_OVERRIDE", raw)]),
            &cwd,
            &default_root,
        )
        .unwrap_err();
        assert_eq!(
            from_override,
            HomeError::ControlByte {
                source: "FM_STATE_OVERRIDE",
                byte
            },
            "FM_STATE_OVERRIDE {raw:?}"
        );
    }
}

#[test]
fn unselected_inputs_are_never_validated() {
    let tmp = TempDir::new("home-unselected");
    let base = tmp.path();
    let fm_home = base.join("fm-home");

    let home = resolve(
        None,
        &env(&[
            ("FM_HOME", text(&fm_home)),
            ("FM_ROOT_OVERRIDE", "bad\npath".to_owned()),
            ("FM_ROOT", "bad\0path".to_owned()),
        ]),
        base,
        &base.join("default"),
    )
    .expect("unselected FM_ROOT* values are ignored");
    assert_eq!(home.root, fm_home);

    let flag_value = text(&fm_home);
    let flagged = resolve(
        Some(flag_value.as_str()),
        &env(&[("FM_HOME", "bad\tpath".to_owned())]),
        base,
        &base.join("default"),
    )
    .expect("unselected FM_HOME is ignored when the flag wins");
    assert_eq!(flagged.root, fm_home);
    assert_eq!(flagged.root_source, RootSource::Flag);
}

#[test]
fn missing_home_is_allowed_and_never_created() {
    let tmp = TempDir::new("home-missing");
    let base = tmp.path();
    let missing = base.join("not-yet");

    let home = resolve(
        None,
        &env(&[("FM_HOME", text(&missing))]),
        base,
        &base.join("default"),
    )
    .expect("a missing home resolves");

    assert_eq!(home.root, missing);
    assert!(!missing.exists(), "resolution must not create the home");
    assert!(!home.config.exists(), "resolution must not create config/");
}

#[test]
fn regular_file_is_not_a_home() {
    let tmp = TempDir::new("home-not-dir");
    let base = tmp.path();
    let file = base.join("not-a-home");
    write(&file, "x");

    let error = resolve(
        None,
        &env(&[("FM_HOME", text(&file))]),
        base,
        &base.join("default"),
    )
    .expect_err("a regular file is refused");
    assert_eq!(
        error,
        HomeError::NotDirectory {
            source: "FM_HOME",
            path: file,
        }
    );
}

#[test]
fn derived_directory_that_is_a_file_is_refused() {
    let tmp = TempDir::new("home-derived-file");
    let base = tmp.path();
    let root = base.join("home");
    write(&root.join("data"), "not a directory");

    let error = resolve(
        None,
        &env(&[("FM_HOME", text(&root))]),
        base,
        &base.join("default"),
    )
    .expect_err("a file where data/ belongs is refused");
    assert_eq!(
        error,
        HomeError::NotDirectory {
            source: "data directory",
            path: root.join("data"),
        }
    );
}

#[cfg(unix)]
#[test]
fn path_through_a_file_reports_the_inspect_error() {
    let tmp = TempDir::new("home-inspect");
    let base = tmp.path();
    write(&base.join("file"), "x");
    let through_file = base.join("file").join("child");

    let error = resolve(
        None,
        &env(&[("FM_HOME", text(&through_file))]),
        base,
        &base.join("default"),
    )
    .expect_err("a path through a file cannot be inspected");
    assert_eq!(
        error,
        HomeError::Inspect {
            source: "FM_HOME",
            path: through_file,
            kind: ErrorKind::NotADirectory,
        }
    );
}

#[cfg(unix)]
mod symlinks {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn final_symlink_root_is_refused_and_its_target_untouched() {
        let tmp = TempDir::new("home-link-root");
        let base = tmp.path();
        let real = base.join("real");
        fs::create_dir(&real).expect("create target");
        write(&real.join("marker"), "keep");
        symlink(&real, base.join("link")).expect("create link");

        let error = resolve(
            None,
            &env(&[("FM_HOME", text(&base.join("link")))]),
            base,
            &base.join("default"),
        )
        .expect_err("a symlinked home is refused");

        assert_eq!(
            error,
            HomeError::FinalSymlink {
                source: "FM_HOME",
                path: base.join("link"),
            }
        );
        assert!(
            real.join("marker").exists(),
            "refusal must not mutate the target"
        );
    }

    #[test]
    fn dangling_final_symlink_is_refused() {
        let tmp = TempDir::new("home-dangling");
        let base = tmp.path();
        symlink(base.join("missing"), base.join("dangling")).expect("create dangling link");

        let error = resolve(
            None,
            &env(&[("FM_STATE_OVERRIDE", text(&base.join("dangling")))]),
            base,
            &base.join("default"),
        )
        .expect_err("a dangling override is refused");

        assert_eq!(
            error,
            HomeError::FinalSymlink {
                source: "FM_STATE_OVERRIDE",
                path: base.join("dangling"),
            }
        );
    }

    #[test]
    fn symlinked_parent_directory_is_allowed_and_not_canonicalized() {
        let tmp = TempDir::new("home-parent-link");
        let base = tmp.path();
        let real = base.join("real");
        fs::create_dir_all(real.join("child")).expect("create real child");
        symlink(&real, base.join("alias")).expect("create parent alias");
        let via_alias = base.join("alias").join("child");

        let home = resolve(
            None,
            &env(&[("FM_HOME", text(&via_alias))]),
            base,
            &base.join("default"),
        )
        .expect("a symlinked parent is allowed");

        assert_eq!(home.root, via_alias, "the path is kept, not canonicalized");
        assert_eq!(home.config, via_alias.join("config"));
    }

    #[test]
    fn derived_directory_symlink_is_refused() {
        let tmp = TempDir::new("home-derived-link");
        let base = tmp.path();
        let root = base.join("home");
        fs::create_dir_all(&root).expect("create home");
        fs::create_dir_all(base.join("elsewhere")).expect("create elsewhere");
        symlink(base.join("elsewhere"), root.join("config")).expect("create config link");

        let error = resolve(
            None,
            &env(&[("FM_HOME", text(&root))]),
            base,
            &base.join("default"),
        )
        .expect_err("a symlinked config/ is refused");

        assert_eq!(
            error,
            HomeError::FinalSymlink {
                source: "config directory",
                path: root.join("config"),
            }
        );
    }

    #[test]
    fn trailing_separator_cannot_hide_a_symlinked_root() {
        let tmp = TempDir::new("home-link-slash");
        let base = tmp.path();
        let real = base.join("real");
        fs::create_dir(&real).expect("create target");
        symlink(&real, base.join("link")).expect("create link");

        let error = resolve(
            None,
            &env(&[("FM_HOME", format!("{}/", text(&base.join("link"))))]),
            base,
            &base.join("default"),
        )
        .expect_err("a trailing separator still names the symlink");

        assert_eq!(
            error,
            HomeError::FinalSymlink {
                source: "FM_HOME",
                path: base.join("link"),
            }
        );
    }

    #[test]
    fn dot_suffix_cannot_hide_a_symlinked_root() {
        let tmp = TempDir::new("home-link-dot");
        let base = tmp.path();
        let real = base.join("real");
        fs::create_dir(&real).expect("create target");
        symlink(&real, base.join("link")).expect("create link");

        let error = resolve(
            None,
            &env(&[("FM_HOME", format!("{}/.", text(&base.join("link"))))]),
            base,
            &base.join("default"),
        )
        .expect_err("a trailing /. still names the symlink");

        assert_eq!(
            error,
            HomeError::FinalSymlink {
                source: "FM_HOME",
                path: base.join("link"),
            }
        );
    }

    #[test]
    fn trailing_separator_cannot_hide_a_symlinked_override() {
        let tmp = TempDir::new("home-link-override-slash");
        let base = tmp.path();
        let real = base.join("real");
        fs::create_dir(&real).expect("create target");
        symlink(&real, base.join("cfg-link")).expect("create link");

        let error = resolve(
            None,
            &env(&[(
                "FM_CONFIG_OVERRIDE",
                format!("{}/", text(&base.join("cfg-link"))),
            )]),
            base,
            &base.join("default"),
        )
        .expect_err("a trailing separator on an override still names the symlink");

        assert_eq!(
            error,
            HomeError::FinalSymlink {
                source: "FM_CONFIG_OVERRIDE",
                path: base.join("cfg-link"),
            }
        );
    }

    #[test]
    fn parent_component_cannot_hide_a_symlinked_root() {
        // `link/x/..` names no symlink as its final component, so the final-component check
        // would pass while the path reaches through `link`. It is refused as a `..` instead.
        let tmp = TempDir::new("home-link-parent");
        let base = tmp.path();
        let real = base.join("real");
        fs::create_dir_all(real.join("x")).expect("create target");
        symlink(&real, base.join("link")).expect("create link");
        let through_link = base.join("link").join("x").join("..");

        let error = resolve(
            None,
            &env(&[("FM_HOME", text(&through_link))]),
            base,
            &base.join("default"),
        )
        .expect_err("a .. after a symlinked prefix is refused");
        assert_eq!(
            error,
            HomeError::ParentDir {
                source: "FM_HOME",
                path: through_link.clone(),
            }
        );

        let override_error = resolve(
            None,
            &env(&[("FM_CONFIG_OVERRIDE", text(&through_link))]),
            base,
            &base.join("default"),
        )
        .expect_err("the same bypass is refused for an override");
        assert_eq!(
            override_error,
            HomeError::ParentDir {
                source: "FM_CONFIG_OVERRIDE",
                path: through_link,
            }
        );
    }
}

mod env_capture {
    use super::*;

    fn pairs(raw: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        raw.iter()
            .map(|&(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    /// A value that is not valid UTF-8 on this platform.
    #[cfg(unix)]
    fn not_utf8() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'/', 0xff])
    }

    /// A value that is not valid UTF-8 on this platform: an unpaired UTF-16 surrogate.
    #[cfg(windows)]
    fn not_utf8() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0x2f, 0xd800])
    }

    #[test]
    fn only_read_names_are_kept_with_their_values() {
        let captured = capture_env(pairs(&[
            ("PATH", "/usr/bin"),
            ("FM_HOME", "/work/home"),
            ("HERDR_ENV", "1"),
        ]))
        .expect("captures");
        assert_eq!(
            captured,
            env(&[("FM_HOME", "/work/home"), ("HERDR_ENV", "1")])
        );
    }

    #[test]
    fn non_utf8_value_of_a_read_name_is_refused() {
        let vars = vec![(OsString::from("FM_HOME"), not_utf8())];
        assert_eq!(
            capture_env(vars),
            Err(EnvError::NotUtf8 { name: "FM_HOME" })
        );
    }

    #[test]
    fn non_utf8_value_of_an_unread_name_is_ignored() {
        let captured = capture_env(vec![(OsString::from("PATH"), not_utf8())]).expect("captures");
        assert!(captured.is_empty());
    }

    #[test]
    fn read_name_set_twice_is_refused() {
        let error = capture_env(pairs(&[("FM_HOME", "/a"), ("FM_HOME", "/b")]))
            .expect_err("duplicate is refused");
        assert_eq!(error, EnvError::Duplicate { name: "FM_HOME" });
    }

    #[cfg(not(windows))]
    #[test]
    fn names_match_exactly_off_windows() {
        let captured = capture_env(pairs(&[("fm_home", "/a")])).expect("captures");
        assert!(captured.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn names_match_case_insensitively_on_windows() {
        let captured = capture_env(pairs(&[("fm_home", r"C:\home")])).expect("captures");
        assert_eq!(captured, env(&[("FM_HOME", r"C:\home")]));

        let error = capture_env(pairs(&[("FM_HOME", "a"), ("fm_home", "b")]))
            .expect_err("case-only duplicate is refused");
        assert_eq!(error, EnvError::Duplicate { name: "FM_HOME" });
    }

    /// Native Windows evidence is produced by CI's Windows job, not by this macOS host.
    #[cfg(windows)]
    #[test]
    fn drive_relative_flag_is_refused_after_joining_the_working_directory() {
        let error = resolve(
            Some("C:foo"),
            &Env::new(),
            Path::new("relative-cwd"),
            Path::new(r"C:\code"),
        )
        .expect_err("a drive-relative path is not absolute");
        assert_eq!(
            error,
            HomeError::NotAbsolute {
                source: "--fm-home",
                path: PathBuf::from("C:foo"),
            }
        );
    }
}
