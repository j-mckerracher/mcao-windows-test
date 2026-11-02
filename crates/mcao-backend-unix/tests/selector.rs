//! Checks [`backend_for`] names. Building a backend opens no session, so these run without tmux or
//! herdr.

#![cfg(unix)]

use mcao_backend_unix::{SelectorError, backend_for};

#[test]
fn tmux_selects_the_tmux_backend() {
    let backend = backend_for("tmux").expect("tmux is a known backend");

    assert_eq!(backend.name(), "tmux");
}

#[test]
fn herdr_selects_the_herdr_backend() {
    let backend = backend_for("herdr").expect("herdr is a known backend");

    assert_eq!(backend.name(), "herdr");
}

#[test]
fn an_unknown_name_is_refused_with_its_name() {
    match backend_for("zellij") {
        Err(error) => assert_eq!(error, SelectorError::UnknownBackend("zellij".to_owned())),
        Ok(backend) => panic!("zellij should be refused, got {}", backend.name()),
    }
}

#[test]
fn surrounding_whitespace_is_trimmed_from_the_name() {
    let backend = backend_for(" \ttmux\n").expect("a padded tmux name is a known backend");

    assert_eq!(backend.name(), "tmux");
}

#[test]
fn the_name_match_is_case_sensitive() {
    match backend_for("Herdr") {
        Err(error) => assert_eq!(error, SelectorError::UnknownBackend("Herdr".to_owned())),
        Ok(backend) => panic!("Herdr should be refused, got {}", backend.name()),
    }
}
