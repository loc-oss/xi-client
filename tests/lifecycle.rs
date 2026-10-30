//! End-to-end tests against a real `xi-core` subprocess.
//!
//! Skipped (with a note) unless `XI_CORE` points at the binary. With the
//! binary checked out at the repo root:
//!
//!     XI_CORE=$PWD/xi-core cargo test --test lifecycle
//!
//! These are the Phase-0 validation tests from `docs/roadmap.md`. They
//! protect the transport layer and the event pipeline against regressions
//! that mocks cannot catch.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use xi_client::{Event, ViewId, XiClient};

fn core_path() -> Option<PathBuf> {
    std::env::var_os("XI_CORE").map(PathBuf::from)
}

macro_rules! core_or_skip {
    () => {
        match core_path() {
            Some(p) => p,
            None => {
                eprintln!("[skip] XI_CORE not set; skipping lifecycle test");
                return;
            }
        }
    };
}

fn wait_for<F: Fn(&Event) -> bool>(
    client: &XiClient,
    timeout: Duration,
    pred: F,
) -> Option<Event> {
    let rx = client.events();
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match rx.recv_timeout(remaining) {
            Ok(ev) if pred(&ev) => return Some(ev),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

fn shutdown_and_wait(client: &XiClient) {
    client.shutdown();
    wait_for(client, Duration::from_secs(5), |e| matches!(e, Event::Exited));
}

#[test]
fn spawn_and_shutdown_emits_exited() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    client.shutdown();
    let exited = wait_for(&client, Duration::from_secs(5), |e| matches!(e, Event::Exited));
    assert!(exited.is_some(), "Exited not received after shutdown");
}

#[test]
fn new_view_empty_emits_view_opened() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");
    let ev = wait_for(&client, Duration::from_secs(2), |e| {
        matches!(e, Event::ViewOpened { view_id: v, .. } if *v == view_id)
    });
    assert!(ev.is_some(), "ViewOpened not received");
    shutdown_and_wait(&client);
}

#[test]
fn new_view_with_file_loads_contents() {
    let core = core_or_skip!();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hello.txt");
    std::fs::write(&path, "hello\nworld\n").unwrap();

    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(Some(path)).expect("new_view");

    let ok = wait_for(&client, Duration::from_secs(5), |e| {
        matches!(e, Event::DocumentUpdated { view_id: v } if *v == view_id)
    })
    .is_some();
    assert!(ok, "no DocumentUpdated for new view");

    let doc = client.document(view_id).expect("document");
    assert!(doc.lines.len() >= 2, "expected >= 2 lines, got {}", doc.lines.len());
    assert_eq!(doc.lines[0].text, "hello");
    assert_eq!(doc.lines[1].text, "world");

    shutdown_and_wait(&client);
}

#[test]
fn insert_appends_at_cursor() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    // Drain the initial DocumentUpdated for the empty view.
    wait_for(&client, Duration::from_secs(2), |e| {
        matches!(e, Event::DocumentUpdated { view_id: v } if *v == view_id)
    });

    client.insert(view_id, "hi").expect("insert");

    let ok = wait_for(&client, Duration::from_secs(2), |e| {
        matches!(e, Event::DocumentUpdated { view_id: v } if *v == view_id)
    });
    assert!(ok.is_some(), "no DocumentUpdated after insert");

    let doc = client.document(view_id).expect("document");
    assert!(
        doc.lines.iter().any(|l| l.text.contains("hi")),
        "document does not contain inserted text: {:?}",
        doc.lines.iter().map(|l| &l.text).collect::<Vec<_>>()
    );

    shutdown_and_wait(&client);
}

#[test]
fn close_view_emits_view_closed() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id: ViewId = client.new_view(None).expect("new_view");

    client.close_view(view_id).expect("close_view");

    let ev = wait_for(&client, Duration::from_secs(2), |e| {
        matches!(e, Event::ViewClosed { view_id: v } if *v == view_id)
    });
    assert!(ev.is_some(), "ViewClosed not received");
    assert!(client.document(view_id).is_none(), "doc still present after close");

    shutdown_and_wait(&client);
}
