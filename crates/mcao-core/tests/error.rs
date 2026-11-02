use std::error::Error as _;
use std::io;
use std::io::ErrorKind;

use mcao_core::McaoError;
use mcao_core::state::TaskId;

#[test]
fn a_task_id_refused_at_creation_exits_with_usage_code_two() {
    let refused = TaskId::parse_for_creation("con").expect_err("device name is refused");
    let error = McaoError::from(refused);

    assert_eq!(error.exit_code(), 2);
    assert!(
        error.to_string().starts_with("Usage: "),
        "usage wording: {error}"
    );
}

#[test]
fn each_variant_maps_to_its_contract_exit_code() {
    assert_eq!(McaoError::Usage("bad flag".into()).exit_code(), 2);
    assert_eq!(McaoError::Preflight("git missing".into()).exit_code(), 3);
    assert_eq!(McaoError::Refused("not allowed".into()).exit_code(), 4);
    assert_eq!(McaoError::Uncertain("no answer".into()).exit_code(), 5);
    assert_eq!(McaoError::StateConflict("held".into()).exit_code(), 6);
    assert_eq!(
        McaoError::Unsupported {
            backend: "mock".into(),
            op: "capture",
        }
        .exit_code(),
        7
    );
    assert_eq!(McaoError::Backend("pty closed".into()).exit_code(), 1);
    assert_eq!(
        McaoError::from(io::Error::new(ErrorKind::NotFound, "missing")).exit_code(),
        1
    );
}

#[test]
fn display_matches_the_contract_wording() {
    assert_eq!(
        McaoError::Usage("bad flag".into()).to_string(),
        "Usage: bad flag"
    );
    assert_eq!(
        McaoError::Preflight("git missing".into()).to_string(),
        "Preflight check failed: git missing"
    );
    assert_eq!(
        McaoError::Refused("not allowed".into()).to_string(),
        "Refused: not allowed"
    );
    assert_eq!(
        McaoError::Uncertain("no answer".into()).to_string(),
        "Uncertain: no answer"
    );
    assert_eq!(
        McaoError::StateConflict("held".into()).to_string(),
        "State conflict: held"
    );
    assert_eq!(
        McaoError::Unsupported {
            backend: "mock".into(),
            op: "capture",
        }
        .to_string(),
        "Unsupported: mock does not support capture"
    );
    assert_eq!(
        McaoError::Backend("pty closed".into()).to_string(),
        "Backend error: pty closed"
    );
    assert_eq!(
        McaoError::from(io::Error::new(ErrorKind::NotFound, "missing")).to_string(),
        "I/O error: missing"
    );
}

#[test]
fn io_failures_convert_with_from_and_expose_their_source() {
    let error = McaoError::from(io::Error::new(ErrorKind::PermissionDenied, "denied"));

    assert_eq!(error.exit_code(), 1);
    assert_eq!(
        error.source().map(ToString::to_string),
        Some("denied".to_string())
    );
    assert!(matches!(error, McaoError::Io(_)));
}

#[test]
fn non_io_failures_have_no_source() {
    assert!(McaoError::Usage("bad flag".into()).source().is_none());
    assert!(McaoError::Backend("pty closed".into()).source().is_none());
}
