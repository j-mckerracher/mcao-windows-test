//! Frozen cross-platform pin of `config::load` (DPES-753 AC1).
//!
//! `tests/fixtures/config-pilot/config/` is one pilot `config/` directory. Each
//! `expected-*.txt` beside it is the frozen result of loading it under one set of flags and
//! environment variables. The same bytes and the same expectations run on Linux, macOS, and
//! Windows, so a platform that resolves a value or a source differently fails here. See
//! `tests/fixtures/config-pilot/provenance.txt`. No expected value is generated from Rust.
//!
//! A path is rendered relative to `config/` with `/` separators, because an absolute path differs
//! on every machine.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use mcao_core::config::{self, Config, Source};
use mcao_core::home::{FmHome, resolve};
use support::env;

fn pilot() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config-pilot")
}

fn pilot_home() -> FmHome {
    let root = pilot();
    resolve(
        None,
        &env(&[("FM_HOME", root.to_string_lossy().into_owned())]),
        &root,
        &root,
    )
    .expect("pilot home resolves")
}

/// Renders `path` relative to `config_dir`, joined with `/`.
fn relative(config_dir: &Path, path: &Path) -> String {
    path.strip_prefix(config_dir)
        .expect("a config file lies under config/")
        .components()
        .map(|component| match component {
            Component::Normal(name) => name.to_string_lossy().into_owned(),
            other => panic!("unexpected path component {other:?}"),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn render_source(config_dir: &Path, source: &Source) -> String {
    match source {
        Source::Flag => "flag".to_owned(),
        Source::Env(variable) => format!("env {variable}"),
        Source::File(path) => format!("file {}", relative(config_dir, path)),
        Source::Detected(marker) => format!("detected {marker}"),
        Source::Default => "default".to_owned(),
    }
}

fn render(home: &FmHome, config: &Config) -> String {
    let mut text = String::new();
    for (key, setting) in config.iter() {
        text.push_str(&format!(
            "setting {key} = {:?} [{}]\n",
            setting.value,
            render_source(&home.config, &setting.source)
        ));
    }
    for (key, unknown) in config.unknown_iter() {
        text.push_str(&format!(
            "unknown {key} = {}\n",
            unknown.bytes.escape_ascii()
        ));
    }
    for skipped in config.skipped() {
        text.push_str(&format!(
            "skipped {} {:?}\n",
            relative(&home.config, &skipped.path),
            skipped.reason
        ));
    }
    text
}

fn expected(name: &str) -> String {
    fs::read_to_string(pilot().join(name)).expect("expectation is readable")
}

fn load(flags: &[(&str, &str)], variables: &[(&str, &str)]) -> String {
    let home = pilot_home();
    let flags: BTreeMap<String, String> = flags
        .iter()
        .map(|&(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let config = config::load(&home, &flags, &env(variables)).expect("pilot config loads");
    render(&home, &config)
}

#[test]
fn pilot_config_resolves_to_the_frozen_files_contract() {
    assert_eq!(load(&[], &[]), expected("expected-files.txt"));
}

#[test]
fn pilot_config_resolves_to_the_frozen_overrides_contract() {
    assert_eq!(
        load(&[("branch-prefix", "feat/")], &[("FM_BACKEND", "tmux")]),
        expected("expected-overrides.txt")
    );
}

#[test]
fn the_pilot_directory_keeps_the_raw_bytes_the_contract_depends_on() {
    let config_dir = pilot().join("config");
    let read = |name: &str| fs::read(config_dir.join(name)).expect("fixture is readable");
    assert_eq!(
        read("developer-name"),
        b"Pilot Dev\r\n",
        "CRLF is not rewritten"
    );
    assert_eq!(read("claude-permission-mode"), b"auto\r\n");
    assert_eq!(
        read("notes.txt"),
        b"line one\r\nline two",
        "no final newline is added"
    );
    assert_eq!(
        read("blob.bin"),
        b"\x00\xff\n\x80",
        "binary bytes are not rewritten"
    );
}
