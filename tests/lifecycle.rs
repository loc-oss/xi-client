//! End-to-end tests against a real `xi-core` subprocess.
//!
//! Skipped (with a note) unless `XI_CORE` points at the binary. With the
//! binary checked out at the repo root:
//!
//!     XI_CORE=$PWD/xi-core cargo test --test lifecycle -- --test-threads=1
//!
//! `--test-threads=1` is recommended: each test spawns its own `xi-core`,
//! and they all write to the same `~/.local/share/xi-core/xi-core.log`, so
//! running them in parallel interleaves the log noise (harmless, but ugly).

use std::path::PathBuf;
use std::time::{Duration, Instant};
use serde_json::json;

use xi_client::{DocumentSnapshot, Event, ViewId, XiClient};

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

/// Waits for an event matching `pred`, up to `timeout`.
///
/// Use this only when you want the *event itself*. To assert on document
/// state after one or more fire-and-forget edits, use [`wait_until_doc`]
/// instead — a single `DocumentUpdated` does not mean the reader has
/// applied every pending op.
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

/// Waits until `document(view_id)` satisfies `pred`, or `timeout` elapses.
///
/// Necessary whenever several fire-and-forget edits are sent without an
/// intervening wait: each edit generates its own `DocumentUpdated`, and
/// matching just one event does not mean the reader has caught up. This
/// helper polls the *state* and re-checks after every inbound event.
fn wait_until_doc<F: Fn(&DocumentSnapshot) -> bool>(
    client: &XiClient,
    view_id: ViewId,
    timeout: Duration,
    pred: F,
) -> bool {
    let rx = client.events();
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(doc) = client.document(view_id) {
            if pred(&doc) {
                return true;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let _ = rx.recv_timeout(remaining);
    }
}

/// Shuts the client down and asserts that `Exited` arrives.
fn shutdown_and_wait(client: &XiClient) {
    client.shutdown();
    let exited = wait_for(client, Duration::from_secs(5), |e| matches!(e, Event::Exited));
    assert!(exited.is_some(), "Exited not received after shutdown");
}

/// Convenience: panics with the current document line contents if `ok` is
/// false. Useful for debugging flaky failures.
fn assert_doc_state(
    client: &XiClient,
    view_id: ViewId,
    ok: bool,
    what: &str,
) {
    if !ok {
        let lines: Vec<String> = client
            .document(view_id)
            .map(|d| d.lines.iter().map(|l| l.text.clone()).collect())
            .unwrap_or_default();
        panic!("{what}; current lines: {lines:?}");
    }
}

// ───────────────────────── Phase 0: lifecycle ─────────────────────────

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

    let ok = wait_until_doc(&client, view_id, Duration::from_secs(5), |doc| {
        doc.lines.len() >= 2 && doc.lines[0].text == "hello" && doc.lines[1].text == "world"
    });
    assert_doc_state(&client, view_id, ok, "file contents not loaded");

    shutdown_and_wait(&client);
}

#[test]
fn insert_appends_at_cursor() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.insert(view_id, "hi").expect("insert");
    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines.iter().any(|l| l.text.contains("hi"))
    });
    assert_doc_state(&client, view_id, ok, "inserted text not visible");

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
    // `document()` is synchronous against Inner; no race here.
    assert!(client.document(view_id).is_none(), "doc still present after close");

    shutdown_and_wait(&client);
}

// ───────────────────────── Phase 1: editing ─────────────────────────

#[test]
fn undo_removes_inserted_text() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.insert(view_id, "hello").expect("insert");
    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text.contains("hello")
    });
    assert_doc_state(&client, view_id, ok, "insert did not land");

    client.undo(view_id).expect("undo");
    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        !doc.lines[0].text.contains("hello")
    });
    assert_doc_state(&client, view_id, ok, "undo did not remove text");

    shutdown_and_wait(&client);
}

#[test]
fn redo_restores_after_undo() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.insert(view_id, "abc").expect("insert");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text.contains("abc")
    }));

    client.undo(view_id).expect("undo");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        !doc.lines[0].text.contains("abc")
    }));

    client.redo(view_id).expect("redo");
    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text.contains("abc")
    });
    assert_doc_state(&client, view_id, ok, "redo did not restore text");

    shutdown_and_wait(&client);
}

#[test]
fn move_left_then_insert_inserts_mid_line() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.insert(view_id, "abc").expect("insert");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text == "abc"
    }));

    client.move_left(view_id).expect("move_left");
    // Cursor moved, but the text is unchanged — can't wait on text here.
    // Instead, wait until the cursor annotation lands on column 2.
    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.selections.iter().any(|&(_, sc, _, ec)| sc == 2 && ec == 2)
    });
    assert!(
        ok,
        "cursor did not move to column 2; selections: {:?}",
        client.document(view_id).map(|d| d.selections)
    );

    client.insert(view_id, "X").expect("insert");
    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text == "abXc"
    });
    assert_doc_state(&client, view_id, ok, "insert-at-cursor did not land mid-line");

    shutdown_and_wait(&client);
}

#[test]
fn select_all_produces_full_range_selection() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.insert(view_id, "hello\nworld").expect("insert");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines.len() >= 2 && doc.lines[0].text == "hello"
    }));

    client.select_all(view_id).expect("select_all");
    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        !doc.selections.is_empty() && {
            let (sl, sc, el, _) = doc.selections[0];
            sl == 0 && sc == 0 && el >= 1
        }
    });
    assert!(
        ok,
        "select_all did not produce a full-range selection: {:?}",
        client.document(view_id).map(|d| d.selections)
    );

    shutdown_and_wait(&client);
}

#[test]
fn resize_does_not_crash() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.resize(view_id, 800, 600).expect("resize");
    client.resize(view_id, 400, 300).expect("resize small");

    shutdown_and_wait(&client);
}

#[test]
fn insert_newline_and_tab() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    // Three fire-and-forget edits in a row.
    client.insert(view_id, "a").expect("insert");
    client.insert_newline(view_id).expect("newline");
    client.insert(view_id, "b").expect("insert");

    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines.len() >= 2
            && doc.lines[0].text == "a"
            && doc.lines[1].text.starts_with('b')
    });
    assert_doc_state(&client, view_id, ok, "newline/tab did not produce expected state");

    shutdown_and_wait(&client);
}

// ───────────────────────── Phase 1: clipboard ─────────────────────────

#[test]
fn copy_returns_selected_text() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.insert(view_id, "copy me").expect("insert");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text == "copy me"
    }));

    client.select_all(view_id).expect("select_all");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        !doc.selections.is_empty()
    }), "select_all did not register before copy");

    let text = client.copy(view_id).expect("copy");
    assert_eq!(text.as_deref(), Some("copy me"));

    shutdown_and_wait(&client);
}

#[test]
fn cut_empties_the_buffer() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    client.insert(view_id, "cut me").expect("insert");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text == "cut me"
    }));

    client.select_all(view_id).expect("select_all");
    assert!(wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        !doc.selections.is_empty()
    }), "select_all did not register before cut");

    let text = client.cut(view_id).expect("cut");
    assert_eq!(text.as_deref(), Some("cut me"));

    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.lines[0].text.is_empty()
    });
    assert_doc_state(&client, view_id, ok, "cut did not empty the buffer");

    shutdown_and_wait(&client);
}

// ───────────────────────── Phase 3: options, config, lifecycle ─────────────────────────

#[test]
fn spawn_with_options_capture_stderr() {
    let core = core_or_skip!();
    use xi_client::SpawnOptions;
    let opts = SpawnOptions {
        capture_stderr: true,
        ..Default::default()
    };
    let client = XiClient::spawn_with_options(&core, opts).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");
    assert!(client.document(view_id).is_some());
    shutdown_and_wait(&client);
}

#[test]
fn view_ids_lists_opened_views() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    assert!(client.view_ids().is_empty());

    let v1 = client.new_view(None).expect("new_view 1");
    let v2 = client.new_view(None).expect("new_view 2");

    let ids = client.view_ids();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&v1));
    assert!(ids.contains(&v2));

    client.close_view(v1).expect("close");
    let ids = client.view_ids();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], v2);

    shutdown_and_wait(&client);
}

#[test]
fn wait_for_exit_after_shutdown() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    client.shutdown();
    let status = client
        .wait_for_exit(Duration::from_secs(5))
        .expect("wait_for_exit");
    // Don't assert on status.success() — depends on xi-core's exit code.
    let _ = status;
}

#[test]
fn wait_for_exit_times_out_when_running() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let _view = client.new_view(None).expect("new_view");
    // Don't shutdown; wait should time out.
    let result = client.wait_for_exit(Duration::from_millis(100));
    assert!(result.is_err(), "expected timeout, got {:?}", result);
    shutdown_and_wait(&client);
}

#[test]
fn wait_for_exit_is_idempotent() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    client.shutdown();
    let s1 = client.wait_for_exit(Duration::from_secs(5)).expect("first");
    let s2 = client.wait_for_exit(Duration::from_secs(1)).expect("second");
    assert_eq!(s1, s2);
}

#[test]
fn get_config_returns_config() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    // Lenient: we don't assert specific values (they vary by build), only
    // that the request round-trips and deserializes into Config.
    let _cfg = client.get_config(view_id).expect("get_config");

    shutdown_and_wait(&client);
}

#[test]
fn modify_user_config_does_not_panic() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    use xi_client::ConfigDomain;

    // No config_dir → General modifications are accepted (or rejected with
    // an Alert, depending on build). Either way, the call must not panic.
    client
        .modify_user_config(ConfigDomain::General, json!({"tab_size": 2}))
        .expect("modify_user_config");

    shutdown_and_wait(&client);
}

#[test]
fn modify_user_config_user_override_updates_view() {
    let core = core_or_skip!();
    let client = XiClient::spawn(&core).expect("spawn");
    let view_id = client.new_view(None).expect("new_view");

    use xi_client::ConfigDomain;
    client
        .modify_user_config(
            ConfigDomain::UserOverride(view_id),
            json!({"tab_size": 2}),
        )
        .expect("modify_user_config");

    let ok = wait_until_doc(&client, view_id, Duration::from_secs(2), |doc| {
        doc.config.tab_size == Some(2)
    });
    assert!(
        ok,
        "UserOverride config not reflected; got {:?}",
        client.document(view_id).map(|d| d.config.tab_size)
    );

    shutdown_and_wait(&client);
}
