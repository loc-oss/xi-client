//! XiClient façade: lifecycle, configuration, editing, movement, viewport,
//! clipboard, and state access.
//!
//! The wire protocol — parsing inbound notifications, dispatching
//! `measure_width` — lives in [`handler`]; this module only owns the public
//! API and the shared [`Inner`] state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use serde_json::{json, Value};

use xi_rpc::{Peer, RpcLoop, RpcPeer};

use crate::document::DocumentState;
use crate::events::Event;
use crate::process::{SharedStdin, XiProcess};
use crate::types::{Config, ConfigDomain, GestureType, SpawnOptions, Style, Update, ViewId};
use crate::{Error, Result};

mod handler;
use handler::ClientHandler;

/// Width-measuring callback installed via [`XiClient::set_width_measure`].
///
/// Runs on the reader thread. Must not call back into `XiClient` or send
/// RPCs — that would deadlock, since the reader thread is the one that
/// dispatches responses.
type WidthMeasureFn = dyn Fn(&[String]) -> Vec<f64> + Send + Sync;

/// Mutable per-client state, shared between the reader thread and the
/// application thread.
struct Inner {
    docs: HashMap<ViewId, DocumentState>,
    styles: HashMap<u64, Style>,
    /// `update` notifications that arrived before their view was registered.
    ///
    /// `xi-core` may push `update` for a view as soon as it responds to
    /// `new_view`, i.e. before the application thread has had a chance to
    /// insert the `DocumentState`. Without this buffer, those updates would
    /// be silently dropped and the document would render empty. Drained by
    /// [`XiClient::new_view`].
    pending_updates: HashMap<ViewId, Vec<Update>>,
    /// Monotonic counter for `Event::ViewOpened.request_id`. Reserved for a
    /// future `new_view_async`; today it's a logical id the caller can
    /// ignore.
    next_request_id: u64,
    width_measure: Option<Arc<WidthMeasureFn>>,
}

fn client_started_params(opts: &SpawnOptions) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(cd) = &opts.config_dir {
        map.insert("config_dir".into(), json!(cd.to_string_lossy()));
    }
    if let Some(ed) = &opts.client_extras_dir {
        map.insert("client_extras_dir".into(), json!(ed.to_string_lossy()));
    }
    Value::Object(map)
}

pub struct XiClient {
    inner: Arc<Mutex<Inner>>,
    peer: RpcPeer,
    event_rx: Receiver<Event>,
    event_tx: Sender<Event>,
    stdin: SharedStdin,
    /// Set to `true` by the reader thread just before it exits; waited on by
    /// [`XiClient::wait_for_exit`].
    reader_exit: Arc<(Mutex<bool>, Condvar)>,
    /// Cached exit status, populated by [`XiClient::wait_for_exit`].
    exit_status: Mutex<Option<std::process::ExitStatus>>,
    _process: XiProcess,
}

impl XiClient {
    // ───────────────────────── Lifecycle ─────────────────────────

    /// Spawns with default [`SpawnOptions`].
    pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self> {
        Self::spawn_with_options(core_path, SpawnOptions::default())
    }

    /// Spawns with explicit [`SpawnOptions`].
    ///
    /// `client_started` is sent immediately (before returning), with
    /// `config_dir` / `client_extras_dir` if provided.
    pub fn spawn_with_options(core_path: impl AsRef<Path>, opts: SpawnOptions) -> Result<Self> {
        let process = XiProcess::spawn_with_options(core_path, &opts)?;
        let stdin_raw = process
            .take_stdin()
            .ok_or_else(|| Error::Spawn("no stdin".into()))?;
        let stdout = process
            .take_stdout()
            .ok_or_else(|| Error::Spawn("no stdout".into()))?;

        let stdin = SharedStdin::new(stdin_raw);
        let (event_tx, event_rx) = unbounded::<Event>();

        let inner = Arc::new(Mutex::new(Inner {
            docs: HashMap::new(),
            styles: HashMap::new(),
            pending_updates: HashMap::new(),
            next_request_id: 0,
            width_measure: None,
        }));

        let mut looper = RpcLoop::new(stdin.clone());
        let peer = looper.get_raw_peer().box_clone();

        peer.send_rpc_notification("client_started", &client_started_params(&opts));

        let reader_exit = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_exit_for_thread = Arc::clone(&reader_exit);

        let inner_for_reader = Arc::clone(&inner);
        let event_tx_for_reader = event_tx.clone();
        std::thread::Builder::new()
            .name("xi-client-reader".into())
            .spawn(move || {
                let mut handler = ClientHandler::new(inner_for_reader, event_tx_for_reader.clone());
                let _ = looper.mainloop(move || std::io::BufReader::new(stdout), &mut handler);
                let _ = event_tx_for_reader.send(Event::Exited);
                let (lock, cvar) = &*reader_exit_for_thread;
                *lock.lock().unwrap() = true;
                cvar.notify_all();
            })
            .map_err(Error::Io)?;

        Ok(Self {
            inner,
            peer,
            event_rx,
            event_tx,
            stdin,
            reader_exit,
            exit_status: Mutex::new(None),
            _process: process,
        })
    }

    /// Waits for the reader thread to observe EOF and then reaps the child.
    ///
    /// **Does not initiate shutdown.** Call [`shutdown`] first if you want the
    /// process to exit; otherwise this waits up to `timeout` for the process
    /// to exit on its own (e.g. crash) and returns an error.
    ///
    /// Idempotent: subsequent calls return the cached exit status.
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<std::process::ExitStatus> {
        if let Some(status) = *self.exit_status.lock().unwrap() {
            return Ok(status);
        }

        {
            let (lock, cvar) = &*self.reader_exit;
            let mut done = lock.lock().unwrap();
            let deadline = Instant::now() + timeout;
            while !*done {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Error::Rpc(format!(
                        "timeout waiting for xi-core exit after {:?}",
                        timeout
                    )));
                }
                let (guard, _) = cvar.wait_timeout(done, remaining).unwrap();
                done = guard;
            }
        }

        let mut cache = self.exit_status.lock().unwrap();
        if let Some(status) = *cache {
            return Ok(status);
        }
        let status = self._process.wait()?;
        *cache = Some(status);
        Ok(status)
    }

    // ───────────────────────── Width measure ─────────────────────────

    /// Installs a callback that replaces the default `CodepointMono` width
    /// policy. Called whenever xi-core sends `measure_width` (i.e. for word
    /// wrap, once `word_wrap: true` is set in config).
    ///
    /// # Threading
    ///
    /// The callback runs on the **reader thread**. It must not call back into
    /// `XiClient` or send RPCs — that would deadlock, because the reader
    /// thread is the one that dispatches responses.
    ///
    /// Reading `document()` from the callback is safe (it only locks `Inner`),
    /// but doing heavy work there will stall inbound message processing.
    pub fn set_width_measure<F>(&self, f: F)
    where
        F: Fn(&[String]) -> Vec<f64> + Send + Sync + 'static,
    {
        self.inner.lock().unwrap().width_measure = Some(Arc::new(f));
    }

    /// Restores the default `CodepointMono` policy.
    pub fn clear_width_measure(&self) {
        self.inner.lock().unwrap().width_measure = None;
    }

    // ───────────────────────── Views ─────────────────────────

    /// Opens a new view on `path` (or an empty buffer if `None`).
    ///
    /// Blocks until `xi-core` responds. On success, emits
    /// [`Event::ViewOpened`] and — if any `update` notifications arrived
    /// between the response and the insertion of `DocumentState` — one
    /// [`Event::DocumentUpdated`].
    pub fn new_view(&self, path: Option<PathBuf>) -> Result<ViewId> {
        let params = match path {
            Some(p) => json!({ "file_path": p }),
            None => json!({}),
        };

        let result = self.peer.send_rpc_request("new_view", &params)?;
        let view_id: ViewId = serde_json::from_value(result)?;

        let (request_id, had_pending) = {
            let mut lock = self.inner.lock().unwrap();
            lock.docs.insert(view_id, DocumentState::new());

            let pending = lock.pending_updates.remove(&view_id).unwrap_or_default();
            let had_pending = !pending.is_empty();
            if let Some(doc) = lock.docs.get_mut(&view_id) {
                for update in pending {
                    doc.apply(&update);
                }
            }

            let request_id = lock.next_request_id;
            lock.next_request_id = lock.next_request_id.wrapping_add(1);
            (request_id, had_pending)
        };

        let _ = self.event_tx.send(Event::ViewOpened { request_id, view_id });
        if had_pending {
            let _ = self.event_tx.send(Event::DocumentUpdated { view_id });
        }

        Ok(view_id)
    }

    /// Closes a view. Emits [`Event::ViewClosed`] iff the view was known.
    pub fn close_view(&self, view_id: ViewId) -> Result<()> {
        let payload = json!({ "view_id": view_id });
        self.peer.send_rpc_notification("close_view", &payload);

        let existed = {
            let mut lock = self.inner.lock().unwrap();
            lock.pending_updates.remove(&view_id);
            lock.docs.remove(&view_id).is_some()
        };

        if existed {
            let _ = self.event_tx.send(Event::ViewClosed { view_id });
        }
        Ok(())
    }

    /// Returns the ids of all views currently known to the SDK, ascending.
    pub fn view_ids(&self) -> Vec<ViewId> {
        let lock = self.inner.lock().unwrap();
        let mut ids: Vec<ViewId> = lock.docs.keys().copied().collect();
        ids.sort();
        ids
    }

    /// Saves the buffer to `path`. Errors are reported via [`Event::Alert`],
    /// **not** as a return value. Do not assume the file exists after `save`
    /// returns `Ok`.
    pub fn save(&self, view_id: ViewId, path: impl AsRef<Path>) -> Result<()> {
        let payload = json!({
            "view_id": view_id,
            "file_path": path.as_ref().to_string_lossy(),
        });
        self.peer.send_rpc_notification("save", &payload);
        Ok(())
    }

    // ───────────────────────── Config ─────────────────────────

    /// Fetches the merged config for `view_id` (blocking).
    ///
    /// Usually unnecessary: the first `config_changed` for each new view
    /// carries the full config, and subsequent ones are diffs.
    pub fn get_config(&self, view_id: ViewId) -> Result<Config> {
        let payload = json!({ "view_id": view_id });
        let result = self.peer.send_rpc_request("get_config", &payload)?;
        Ok(serde_json::from_value(result)?)
    }

    /// Modifies user config. `changes` is a JSON object; a `null` value
    /// deletes the key.
    ///
    /// Rejections (invalid changes, or `General`/`Syntax` when file-based
    /// config is enabled) surface as [`Event::Alert`], not as an error
    /// return. Do not assume success just because this returned `Ok(())`.
    pub fn modify_user_config(&self, domain: ConfigDomain, changes: Value) -> Result<()> {
        let payload = json!({
            "domain": serde_json::to_value(&domain)?,
            "changes": changes,
        });
        self.peer.send_rpc_notification("modify_user_config", &payload);
        Ok(())
    }

    // ───────────────────────── Editing ─────────────────────────

    /// Inserts `chars` at every cursor.
    pub fn insert(&self, view_id: ViewId, chars: &str) -> Result<()> {
        self.send_edit(view_id, "insert", json!({ "chars": chars }))
    }

    /// Deletes one character before every cursor.
    pub fn delete_backward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_backward", json!([]))
    }

    /// Deletes one character after every cursor.
    pub fn delete_forward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_forward", json!([]))
    }

    /// Inserts a newline at every cursor.
    pub fn insert_newline(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "insert_newline", json!([]))
    }

    /// Inserts a tab at every cursor (honors `translate_tabs_to_spaces`).
    pub fn insert_tab(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "insert_tab", json!([]))
    }

    /// Undoes the last edit.
    pub fn undo(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "undo", json!([]))
    }

    /// Redoes the last undone edit.
    pub fn redo(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "redo", json!([]))
    }

    /// Deletes one word before every cursor.
    pub fn delete_word_backward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_word_backward", json!([]))
    }

    /// Deletes one word after every cursor.
    pub fn delete_word_forward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_word_forward", json!([]))
    }

    /// Transposes characters around the cursor.
    pub fn transpose(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "transpose", json!([]))
    }

    /// Duplicates the current line(s).
    pub fn duplicate_line(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "duplicate_line", json!([]))
    }

    /// Uppercases the selection.
    pub fn uppercase(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "uppercase", json!([]))
    }

    /// Lowercases the selection.
    pub fn lowercase(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "lowercase", json!([]))
    }

    /// Capitalizes the selection.
    pub fn capitalize(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "capitalize", json!([]))
    }

    /// Indents the selected line(s).
    pub fn indent(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "indent", json!([]))
    }

    /// Outdents the selected line(s).
    pub fn outdent(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "outdent", json!([]))
    }

    /// Re-indents the selected line(s). No-op without the syntect plugin.
    pub fn reindent(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "reindent", json!([]))
    }

    // ───────────────────────── Movement ─────────────────────────

    pub fn move_up(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_up", json!([]))
    }

    pub fn move_down(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_down", json!([]))
    }

    pub fn move_left(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_left", json!([]))
    }

    pub fn move_right(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_right", json!([]))
    }

    pub fn move_up_and_modify_selection(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_up_and_modify_selection", json!([]))
    }

    pub fn move_down_and_modify_selection(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_down_and_modify_selection", json!([]))
    }

    pub fn move_left_and_modify_selection(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_left_and_modify_selection", json!([]))
    }

    pub fn move_right_and_modify_selection(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "move_right_and_modify_selection", json!([]))
    }

    // ───────────────────────── Selection ─────────────────────────

    pub fn select_all(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "select_all", json!([]))
    }

    pub fn collapse_selections(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "collapse_selections", json!([]))
    }

    // ───────────────────────── Viewport ─────────────────────────

    /// Notifies `xi-core` of the viewport size in **px/points**, not device
    /// pixels. Required for word-wrap and for `scroll` to be meaningful.
    pub fn resize(&self, view_id: ViewId, width: u64, height: u64) -> Result<()> {
        self.send_edit(view_id, "resize", json!({ "width": width, "height": height }))
    }

    /// Notifies `xi-core` of the visible line range.
    ///
    /// `first..last` are half-open, in **formatted** (soft-wrapped) lines,
    /// not logical lines.
    pub fn scroll(&self, view_id: ViewId, first: u64, last: u64) -> Result<()> {
        self.send_edit(view_id, "scroll", json!([first, last]))
    }

    /// Asks xi-core to make lines `first..last` valid, sending `update` for
    /// any that are currently invalid.
    ///
    /// Call after `scroll` when the viewport enters territory not yet loaded.
    /// Without this, xi-core keeps only the initial viewport valid; scrolling
    /// far away shows blank lines.
    pub fn request_lines(&self, view_id: ViewId, first: u64, last: u64) -> Result<()> {
        self.send_edit(view_id, "request_lines", json!([first, last]))
    }

    // ───────────────────────── Gestures ─────────────────────────

    pub fn gesture(&self, view_id: ViewId, line: u64, col: u64, ty: GestureType) -> Result<()> {
        let ty_json = serde_json::to_value(&ty)?;
        self.send_edit(view_id, "gesture", json!({ "line": line, "col": col, "ty": ty_json }))
    }

    // ───────────────────────── Clipboard ─────────────────────────

    pub fn cut(&self, view_id: ViewId) -> Result<Option<String>> {
        self.send_sync_edit(view_id, "cut")
    }

    pub fn copy(&self, view_id: ViewId) -> Result<Option<String>> {
        self.send_sync_edit(view_id, "copy")
    }

    // ───────────────────────── Theme & language ─────────────────────────

    pub fn set_theme(&self, name: &str) -> Result<()> {
        let payload = json!({ "theme_name": name });
        self.peer.send_rpc_notification("set_theme", &payload);
        Ok(())
    }

    pub fn set_language(&self, view_id: ViewId, language: &str) -> Result<()> {
        let payload = json!({
            "view_id": view_id,
            "language_id": language,
        });
        self.peer.send_rpc_notification("set_language", &payload);
        Ok(())
    }

    // ───────────────────────── State access ─────────────────────────

    /// Returns an immutable snapshot of the current state of `view_id`.
    /// `None` if the view is unknown (closed, or never opened).
    pub fn document(&self, view_id: ViewId) -> Option<crate::DocumentSnapshot> {
        let lock = self.inner.lock().unwrap();
        lock.docs.get(&view_id).map(|doc| doc.snapshot(view_id))
    }

    /// Returns the event channel. Single consumer: `crossbeam_channel::Receiver`
    /// is not `Clone` for normal channels.
    pub fn events(&self) -> &Receiver<Event> {
        &self.event_rx
    }

    /// Initiates a graceful shutdown.
    ///
    /// Closes `xi-core`'s stdin, which makes it exit; the reader thread
    /// observes EOF on stdout and emits exactly one [`Event::Exited`]. The
    /// subprocess is reaped by `XiProcess::Drop` when the `XiClient` is
    /// dropped.
    ///
    /// Non-blocking. To wait for the actual exit, use [`wait_for_exit`].
    pub fn shutdown(&self) {
        self.stdin.close();
    }

    // ───────────────────────── Internal ─────────────────────────

    fn send_edit(&self, view_id: ViewId, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "view_id": view_id,
            "method": method,
            "params": params,
        });
        self.peer.send_rpc_notification("edit", &payload);
        Ok(())
    }

    fn send_sync_edit(&self, view_id: ViewId, method: &str) -> Result<Option<String>> {
        let payload = json!({
            "view_id": view_id,
            "method": method,
            "params": json!([]),
        });
        let result = self.peer.send_rpc_request("edit", &payload)?;
        match result {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(s)),
            other => Err(Error::Rpc(format!("unexpected {method} result: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_started_params_empty_by_default() {
        let opts = SpawnOptions::default();
        assert_eq!(client_started_params(&opts), json!({}));
    }

    #[test]
    fn client_started_params_with_config_dir() {
        let opts = SpawnOptions {
            config_dir: Some(PathBuf::from("/tmp/cfg")),
            ..Default::default()
        };
        assert_eq!(
            client_started_params(&opts),
            json!({"config_dir": "/tmp/cfg"})
        );
    }

    #[test]
    fn client_started_params_with_both_dirs() {
        let opts = SpawnOptions {
            config_dir: Some(PathBuf::from("/cfg")),
            client_extras_dir: Some(PathBuf::from("/extras")),
            ..Default::default()
        };
        assert_eq!(
            client_started_params(&opts),
            json!({"config_dir": "/cfg", "client_extras_dir": "/extras"})
        );
    }
}
