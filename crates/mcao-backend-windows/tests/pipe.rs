#![cfg(windows)]

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::windows::io::AsHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use mcao_backend_windows::pipe::{PipeError, PipeServer, pipe_name, verify_client_acl};
use mcao_core::state::TaskId;

/// A pipe name no other test or run shares.
fn unique_name(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_nanos();
    let task = TaskId::parse_for_creation(&format!("pipe-{tag}-{nanos}")).expect("valid task id");
    pipe_name(&format!(r"C:\pipe-test\{tag}\{nanos}"), &task)
}

#[test]
fn same_user_connects_passes_acl_and_teardown_refuses_new_clients() {
    let name = unique_name("teardown");

    let mut server = PipeServer::create(&name).expect("create pipe server");
    let client_end = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&name)
        .expect("same user connects");
    let client = server.accept().expect("accept the connected client");
    verify_client_acl(client.as_handle()).expect("same user passes the ACL check");

    drop(client);
    drop(client_end);
    drop(server);

    let err = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&name)
        .expect_err("new connections fail after teardown");
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn second_server_with_the_same_name_is_refused() {
    let name = unique_name("first-instance");
    let _server = PipeServer::create(&name).expect("create pipe server");

    let err = PipeServer::create(&name).expect_err("the name already has an instance");
    assert!(
        matches!(
            err,
            PipeError::Os {
                call: "CreateNamedPipeW",
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

#[test]
fn server_recovers_after_a_client_connects_and_drops_before_accept() {
    let name = unique_name("connect-drop");
    let mut server = PipeServer::create(&name).expect("create pipe server");

    // The client closes before accept runs, so ConnectNamedPipe fails with ERROR_NO_DATA.
    // A race that still reports a connection is also acceptable; the dead handle is dropped.
    drop(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .expect("client connects"),
    );
    match server.accept() {
        Ok(client) => drop(client),
        Err(err) => assert!(
            matches!(
                err,
                PipeError::Os {
                    call: "ConnectNamedPipe",
                    ..
                }
            ),
            "unexpected error: {err}"
        ),
    }

    let client_end = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&name)
        .expect("a new client connects after the dead instance");
    let client = server.accept().expect("accept the new client");
    verify_client_acl(client.as_handle()).expect("same user passes the ACL check");
    drop(client);
    drop(client_end);
}
