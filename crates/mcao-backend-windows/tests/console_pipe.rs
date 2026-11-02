//! Windows integration tests for the console pipe (DPES-883).
//!
//! Covers the replacement listener (F1), the `ERROR_NO_DATA` recovery branch (F6), and a read-only
//! session served end to end over a real named pipe.

#![cfg(windows)]

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mcao_backend_windows::console_server::serve_connection;
use mcao_backend_windows::pipe::{PipeError, PipeServer};
use mcao_core::console::frame::{Frame, decode_from, encode};
use mcao_core::console::ring::RingBuffer;
use windows_sys::Win32::Foundation::ERROR_NO_DATA;

fn unique_pipe_name(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_nanos();
    format!(r"\\.\pipe\mcao-test-{}-{tag}-{nanos}", std::process::id())
}

fn connect(name: &str) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(name)
}

/// A replacement listener exists as soon as `accept` returns, so a second client connects while
/// the first one is still being served.
#[test]
fn second_client_connects_while_first_is_connected() {
    let name = unique_pipe_name("replace");
    let mut server = PipeServer::create(&name).expect("create pipe");
    let first_name = name.clone();
    let first = thread::spawn(move || connect(&first_name).expect("first client connects"));

    let _first_conn = server.accept().expect("accept first client");
    let _first_client = first.join().expect("first client thread");

    let _second = connect(&name).expect("second client connects while the first is connected");
}

/// A client that connects and leaves before `ConnectNamedPipe` makes the call fail with
/// `ERROR_NO_DATA`. The server must replace the dead listener so the next client is accepted.
#[test]
fn accept_reports_no_data_for_early_departure_then_recovers() {
    let name = unique_pipe_name("nodata");
    let mut server = PipeServer::create(&name).expect("create pipe");
    drop(connect(&name).expect("client connects and leaves"));

    match server
        .accept()
        .expect_err("accept fails for a client that already left")
    {
        PipeError::Os { call, source } => {
            assert_eq!(call, "ConnectNamedPipe");
            assert_eq!(source.raw_os_error(), Some(ERROR_NO_DATA as i32));
        }
        other => panic!("unexpected error: {other}"),
    }

    let next_name = name.clone();
    let next = thread::spawn(move || connect(&next_name).expect("next client connects"));
    let _conn = server
        .accept()
        .expect("accept after the listener was replaced");
    let _next_client = next.join().expect("next client thread");
}

/// SUBSCR(0) replays the ring, and DETACH from the client ends the session with `Ok`.
#[test]
fn subscribed_client_receives_ring_bytes_then_detach_ends_session() {
    let payload = b"hello console";
    let name = unique_pipe_name("e2e");
    let mut server = PipeServer::create(&name).expect("create pipe");
    let ring = Arc::new(Mutex::new(RingBuffer::with_default_capacity()));
    ring.lock().expect("ring lock").append(payload);

    let client_name = name.clone();
    let client = thread::spawn(move || {
        let mut file = connect(&client_name).expect("client connects");
        let subscribe = encode(&Frame::Subscribe { offset: 0 }).expect("encode SUBSCR");
        file.write_all(&subscribe).expect("send SUBSCR");

        let mut received = Vec::new();
        while received.len() < payload.len() {
            match decode_from(&mut file).expect("decode server frame") {
                Some(Frame::Output(bytes)) => received.extend(bytes),
                other => panic!("unexpected frame from server: {other:?}"),
            }
        }

        let detach = encode(&Frame::Detach).expect("encode DETACH");
        file.write_all(&detach).expect("send DETACH");
        received
    });

    let conn = server.accept().expect("accept client");
    serve_connection(conn, &ring).expect("session ends cleanly after DETACH");

    assert_eq!(client.join().expect("client thread"), payload);
}

#[test]
fn partial_frame_disconnect_closes_session() {
    let name = unique_pipe_name("partial");
    let mut server = PipeServer::create(&name).expect("create pipe");
    let ring = Arc::new(Mutex::new(RingBuffer::with_default_capacity()));
    let (accepted_tx, accepted_rx) = mpsc::channel();

    let client_name = name.clone();
    let client = thread::spawn(move || {
        let mut file = connect(&client_name).expect("client connects");
        file.write_all(&[1, 0])
            .expect("send a partial frame header");
        accepted_rx.recv().expect("server accepted the client");
    });

    let conn = server.accept().expect("accept client");
    accepted_tx.send(()).expect("notify client after accept");
    let started = Instant::now();
    serve_connection(conn, &ring).expect("truncated frame closes cleanly");

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "disconnect during a partial frame must not strand the session"
    );
    client.join().expect("client thread");
}

/// A client that subscribes, never reads, and floods INPUT. Each refusal is an ERROR the server
/// cannot write into the full pipe, so the writer stalls in `WriteFile`. The refused frames count
/// against the backlog, so the session must end with the backlog error, not `Ok`, and must not
/// hang on the stalled write.
#[test]
fn stalled_client_input_flood_ends_session_with_backlog_error() {
    let name = unique_pipe_name("flood");
    let mut server = PipeServer::create(&name).expect("create pipe");
    let ring = Arc::new(Mutex::new(RingBuffer::with_default_capacity()));

    let client_name = name.clone();
    let client = thread::spawn(move || {
        let mut file = connect(&client_name).expect("client connects");
        let mut flood: Vec<u8> = encode(&Frame::Subscribe { offset: 0 }).expect("encode SUBSCR");
        let input = encode(&Frame::Input(vec![b'k'])).expect("encode INPUT");
        for _ in 0..100_000 {
            flood.extend_from_slice(&input);
        }
        // The client never reads. The write fails once the server closes its end.
        let _ = file.write_all(&flood);
    });

    let conn = server.accept().expect("accept client");
    let started = Instant::now();
    let err = serve_connection(conn, &ring).expect_err("a stalled flood must not end cleanly");

    assert_eq!(err.to_string(), "client backlog exceeded");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the stalled write is cancelled instead of blocking the session"
    );
    client.join().expect("client thread");
}

/// A client that subscribes, reads the replayed ring, and then disconnects while the session is
/// idle. The next read is expected to fail with a broken pipe, which the session treats as a
/// truncated frame, so the session closes with `Ok` instead of reporting a read failure.
#[test]
fn idle_client_disconnect_closes_session_cleanly() {
    let name = unique_pipe_name("idle");
    let mut server = PipeServer::create(&name).expect("create pipe");
    let mut ring = RingBuffer::with_default_capacity();
    ring.append(b"hello");
    let ring = Arc::new(Mutex::new(ring));

    let client_name = name.clone();
    let client = thread::spawn(move || {
        let mut file = connect(&client_name).expect("client connects");
        file.write_all(&encode(&Frame::Subscribe { offset: 0 }).expect("encode SUBSCR"))
            .expect("send SUBSCR");
        // The replay proves the session is serving before this client leaves.
        assert_eq!(
            decode_from(&mut file).expect("replay decodes"),
            Some(Frame::Output(b"hello".to_vec()))
        );
    });

    let conn = server.accept().expect("accept client");
    let started = Instant::now();
    serve_connection(conn, &ring).expect("an idle client that disconnects closes cleanly");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an idle disconnect must not strand the session"
    );
    client.join().expect("client thread");
}

/// A client that subscribes, detaches, and leaves a partial frame open without closing its end.
/// DETACH ends the session while the reader may be blocked mid-frame, so shutdown must cancel
/// that synchronous read. The session then returns `Ok` promptly. The client holds its end open
/// until the session returns, so a reader that stayed blocked would fail cleanup instead.
#[test]
fn stalled_partial_frame_is_cancelled_at_shutdown() {
    let name = unique_pipe_name("stalled");
    let mut server = PipeServer::create(&name).expect("create pipe");
    let ring = Arc::new(Mutex::new(RingBuffer::with_default_capacity()));
    let (served_tx, served_rx) = mpsc::channel::<()>();

    let client_name = name.clone();
    let client = thread::spawn(move || {
        let mut file = connect(&client_name).expect("client connects");
        let mut bytes = encode(&Frame::Subscribe { offset: 0 }).expect("encode SUBSCR");
        bytes.extend(encode(&Frame::Detach).expect("encode DETACH"));
        // Two of the four length-header bytes never complete the frame.
        bytes.extend_from_slice(&[1, 0]);
        file.write_all(&bytes)
            .expect("send frames and a partial header");
        served_rx.recv().expect("session returned");
    });

    let conn = server.accept().expect("accept client");
    let started = Instant::now();
    serve_connection(conn, &ring).expect("DETACH closes the session despite the stalled frame");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a stalled partial frame must not strand shutdown"
    );
    served_tx.send(()).expect("release the client");
    client.join().expect("client thread");
}
