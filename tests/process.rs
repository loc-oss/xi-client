//! `XiProcess` and `SharedStdin`.
//!
//! Uses `sh` as a cheap stand-in for `xi-core`: present on every Unix, reads
//! stdin, exits on EOF. No JSON is exchanged — this file tests process
//! lifecycle and stdin semantics only.

use std::io::Write;
use xi_client::process::{SharedStdin, XiProcess};

#[test]
fn spawn_take_stdin_stdout_are_one_shot() {
    let process = XiProcess::spawn("sh").unwrap();
    assert_eq!(process.path(), std::path::Path::new("sh"));

    assert!(process.take_stdin().is_some());
    assert!(process.take_stdin().is_none(), "take_stdin must be one-shot");

    assert!(process.take_stdout().is_some());
    assert!(process.take_stdout().is_none(), "take_stdout must be one-shot");

    process.kill();
}

#[test]
fn is_running_reflects_kill() {
    let process = XiProcess::spawn("sh").unwrap();
    assert!(process.is_running());
    process.kill();
    assert!(!process.is_running());
}

#[test]
fn kill_is_idempotent() {
    let process = XiProcess::spawn("sh").unwrap();
    process.kill();
    process.kill();
    process.kill();
}

#[test]
fn spawn_missing_binary_errors() {
    let result = XiProcess::spawn("/nonexistent/xi-core-xyz");
    assert!(result.is_err(), "expected spawn to fail");
    let err = result.err().unwrap();
    assert!(err.to_string().starts_with("spawn:"), "got: {err}");
}

#[test]
fn shared_stdin_close_sends_eof_and_is_idempotent() {
    let process = XiProcess::spawn("sh").unwrap();
    let stdin = process.take_stdin().unwrap();
    let shared = SharedStdin::new(stdin);

    assert!(shared.is_open());
    assert!(shared.close(), "first close returns true");
    assert!(!shared.is_open());
    assert!(!shared.close(), "second close returns false");

    process.kill();
}

#[test]
fn shared_stdin_write_after_close_fails_with_broken_pipe() {
    let process = XiProcess::spawn("sh").unwrap();
    let stdin = process.take_stdin().unwrap();
    let mut shared = SharedStdin::new(stdin);
    shared.close();

    let err = shared.write(b"x").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);

    // flush on a closed pipe is forgiving — no error surfaces.
    assert!(shared.flush().is_ok());

    process.kill();
}

#[test]
fn shared_stdin_clone_shares_state() {
    let process = XiProcess::spawn("sh").unwrap();
    let stdin = process.take_stdin().unwrap();
    let shared = SharedStdin::new(stdin);
    let clone = shared.clone();

    shared.close();
    assert!(!clone.is_open());
    assert!(!clone.close());

    process.kill();
}
