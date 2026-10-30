use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use serde_json::{json, Value};

use xi_rpc::{Handler, RemoteError, RpcCtx, RpcLoop, RpcPeer};

use crate::document::DocumentState;
use crate::events::Event;
use crate::process::{SharedStdin, XiProcess};
use crate::types::{Config, Style, Update, ViewId};
use crate::{Error, Result};

/// Mutable per-client state, shared between the reader thread and the
/// application thread.
struct Inner {
    docs: HashMap<ViewId, DocumentState>,
    styles: HashMap<u64, Style>,
    /// `update` notifications that arrived before their view was registered.
    ///
    /// `xi-core` may push `update` for a view as soon as it responds to
    /// `new_view`, i.e. before the application thread has had a chance to
    /// insert the `DocumentState`. Without this buffer, those updates would be
    /// silently dropped and the document would render empty. Drained by
    /// [`XiClient::new_view`].
    pending_updates: HashMap<ViewId, Vec<Update>>,
    /// Monotonic counter for `Event::ViewOpened.request_id`. Reserved for a
    /// future `new_view_async`; today it's a logical id the caller can ignore.
    next_request_id: u64,
}

struct ClientHandler {
    inner: Arc<Mutex<Inner>>,
    event_tx: Sender<Event>,
}

impl ClientHandler {
    /// Sends an event without holding the `Inner` lock.
    ///
    /// Per `docs/protocol.md` §3.3, callers must drop the lock *before*
    /// calling this, so a UI handler that calls back into `XiClient` doesn't
    /// deadlock on `Inner`.
    fn send_event(&self, event: Event) {
        let _ = self.event_tx.send(event);
    }
}

impl Handler for ClientHandler {
    type Notification = Value;
    type Request = Value;

    fn handle_notification(&mut self, _ctx: &RpcCtx, rpc: Self::Notification) {
        let Some(method) = rpc.get("method").and_then(Value::as_str) else {
            return;
        };
        let params = rpc.get("params").cloned();

        match method {
            "update" => self.on_update(params),
            "def_style" => self.on_def_style(params),
            "alert" => self.on_alert(params),
            "config_changed" => self.on_config_changed(params),
            "language_changed" => self.on_language_changed(params),
            "theme_changed" => self.on_theme_changed(params),
            "scroll_to" => self.on_scroll_to(params),
            "find_status" => self.on_find_status(params),
            "available_languages" => self.on_available_languages(params),
            "available_themes" => self.on_available_themes(params),
            _ => {
                // Unknown notification; xi-core may add new ones. Ignore.
            }
        }
    }

    fn handle_request(
        &mut self,
        _ctx: &RpcCtx,
        rpc: Self::Request,
    ) -> std::result::Result<Value, RemoteError> {
        let Some(method) = rpc.get("method").and_then(Value::as_str) else {
            return Err(RemoteError::custom(400, "missing method", None));
        };

        match method {
            "measure_width" => Ok(json!(measure_width(rpc.get("params")))),
            other => Err(RemoteError::custom(
                404,
                format!("method not found: {other}"),
                None,
            )),
        }
    }
}

impl ClientHandler {
    fn on_update(&mut self, params: Option<Value>) {
        let Some(params) = params else { return };
        let Ok(update) = serde_json::from_value::<Update>(params) else {
            return;
        };
        let view_id = update.view_id;

        // Buffer the update if its view isn't registered yet; see
        // `Inner::pending_updates`.
        let applied = {
            let mut lock = self.inner.lock().unwrap();
            match lock.docs.get_mut(&view_id) {
                Some(doc) => {
                    doc.apply(&update);
                    true
                }
                None => {
                    lock.pending_updates
                        .entry(view_id)
                        .or_default()
                        .push(update);
                    false
                }
            }
        }; // lock released

        if applied {
            self.send_event(Event::DocumentUpdated { view_id });
        }
    }

    fn on_def_style(&mut self, params: Option<Value>) {
        let Some(params) = params else { return };
        let Ok(style) = serde_json::from_value::<Style>(params) else {
            return;
        };
        {
            let mut lock = self.inner.lock().unwrap();
            lock.styles.insert(style.id, style.clone());
        }
        self.send_event(Event::StyleDefined(style));
    }

    fn on_alert(&mut self, params: Option<Value>) {
        let Some(msg) = params
            .as_ref()
            .and_then(|p| p.get("msg"))
            .and_then(Value::as_str)
        else {
            return;
        };
        self.send_event(Event::Alert(msg.to_string()));
    }

    fn on_config_changed(&mut self, params: Option<Value>) {
        let Some(params) = params else { return };
        let Some(view_id) = params.get("view_id").and_then(parse_view_id) else {
            return;
        };
        let Some(changes_value) = params.get("changes") else {
            return;
        };

        // Try to model the diff so the event payload is a typed `Config`.
        // Unknown keys are silently dropped by serde; they're still merged
        // into state below on the best-effort path.
        let Ok(diff) = serde_json::from_value::<Config>(changes_value.clone()) else {
            let mut lock = self.inner.lock().unwrap();
            if let Some(doc) = lock.docs.get_mut(&view_id) {
                doc.config.merge(changes_value);
            }
            return;
        };

        {
            let mut lock = self.inner.lock().unwrap();
            if let Some(doc) = lock.docs.get_mut(&view_id) {
                doc.config.merge(changes_value);
            }
        } // lock released

        self.send_event(Event::ConfigChanged { view_id, changes: diff });
    }

    fn on_language_changed(&mut self, params: Option<Value>) {
        let Some(params) = params else { return };
        let Some(view_id) = params.get("view_id").and_then(parse_view_id) else {
            return;
        };
        let Some(language) = params.get("language_id").and_then(Value::as_str) else {
            return;
        };
        let language = language.to_string();
        {
            let mut lock = self.inner.lock().unwrap();
            if let Some(doc) = lock.docs.get_mut(&view_id) {
                doc.language = language.clone();
            }
        }
        self.send_event(Event::LanguageChanged { view_id, language });
    }

    fn on_theme_changed(&mut self, params: Option<Value>) {
        let Some(params) = params else { return };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return;
        };
        self.send_event(Event::ThemeChanged { name: name.to_string() });
    }

    fn on_scroll_to(&mut self, params: Option<Value>) {
        let Some(params) = params else { return };
        let Some(view_id) = params.get("view_id").and_then(parse_view_id) else {
            return;
        };
        let line = params.get("line").and_then(Value::as_u64).unwrap_or(0);
        let col = params.get("col").and_then(Value::as_u64).unwrap_or(0);
        self.send_event(Event::ScrollTo { view_id, line, col });
    }

    fn on_find_status(&mut self, params: Option<Value>) {
        let Some(params) = params else { return };
        let Some(view_id) = params.get("view_id").and_then(parse_view_id) else {
            return;
        };
        // Wire format varies: some builds send `matches`, others `queries`
        // (see `docs/protocol.md` §6.2.6). Surface `matches` when present.
        let matches = params.get("matches").and_then(Value::as_u64).unwrap_or(0);
        self.send_event(Event::FindStatus { view_id, matches });
    }

    fn on_available_languages(&mut self, params: Option<Value>) {
        let languages = params
            .as_ref()
            .and_then(|p| p.get("languages"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        self.send_event(Event::AvailableLanguages { languages });
    }

    fn on_available_themes(&mut self, params: Option<Value>) {
        let themes = params
            .as_ref()
            .and_then(|p| p.get("themes"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        self.send_event(Event::AvailableThemes { themes });
    }
}

fn parse_view_id(v: &Value) -> Option<ViewId> {
    serde_json::from_value(v.clone()).ok()
}

/// `CodepointMono` width policy: one column per Unicode scalar.
///
/// See `docs/protocol.md` §9.1. Each input item must yield one inner `Vec`,
/// one entry per input string.
fn measure_width(params: Option<&Value>) -> Vec<Vec<f64>> {
    let Some(items) = params.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            item.get("strings")
                .and_then(Value::as_array)
                .map(|strings| {
                    strings
                        .iter()
                        .map(|s| s.as_str().map_or(0.0, |s| s.chars().count() as f64))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

pub struct XiClient {
    inner: Arc<Mutex<Inner>>,
    peer: RpcPeer,
    event_rx: Receiver<Event>,
    event_tx: Sender<Event>,
    stdin: SharedStdin,
    _process: XiProcess,
}

impl XiClient {
    pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self> {
        let process = XiProcess::spawn(core_path)?;
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
        }));

        let mut looper = RpcLoop::new(stdin.clone());
        let peer = looper.get_raw_peer().box_clone();

        // `client_started` must be the first message; `xi-core` panics
        // otherwise (see `docs/protocol.md` §4.2).
        peer.send_rpc_notification("client_started", &json!({}));

        let inner_for_reader = Arc::clone(&inner);
        let event_tx_for_reader = event_tx.clone();
        std::thread::Builder::new()
            .name("xi-client-reader".into())
            .spawn(move || {
                let mut handler = ClientHandler {
                    inner: inner_for_reader,
                    event_tx: event_tx_for_reader.clone(),
                };
                // `move` is required: `stdout` is owned by this closure, and
                // `mainloop` requires `RF: Send + FnOnce() -> R`.
                let _ = looper.mainloop(
                    move || std::io::BufReader::new(stdout),
                    &mut handler,
                );
                // EOF on stdout (shutdown, crash, or malformed protocol) —
                // send exactly one `Exited`.
                let _ = event_tx_for_reader.send(Event::Exited);
            })
            .map_err(Error::Io)?;

        Ok(Self {
            inner,
            peer,
            event_rx,
            event_tx,
            stdin,
            _process: process,
        })
    }

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

            // Drain any updates that arrived before the doc existed.
            let pending = lock
                .pending_updates
                .remove(&view_id)
                .unwrap_or_default();
            let had_pending = !pending.is_empty();
            if let Some(doc) = lock.docs.get_mut(&view_id) {
                for update in pending {
                    doc.apply(&update);
                }
            }

            let request_id = lock.next_request_id;
            lock.next_request_id = lock.next_request_id.wrapping_add(1);
            (request_id, had_pending)
        }; // lock released

        // Emit events after unlocking (§3.3).
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

    /// Saves the buffer to `path`.
    ///
    /// Errors are reported via [`Event::Alert`], **not** as a return value.
    /// Do not assume the file exists after `save` returns `Ok`.
    pub fn save(&self, view_id: ViewId, path: impl AsRef<Path>) -> Result<()> {
        let payload = json!({
            "view_id": view_id,
            "file_path": path.as_ref().to_string_lossy(),
        });
        self.peer.send_rpc_notification("save", &payload);
        Ok(())
    }

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

    /// Notifies `xi-core` of the visible line range.
    ///
    /// `first..last` are half-open, in **formatted** (soft-wrapped) lines,
    /// not logical lines. `xi-core` uses this to decide which lines to render
    /// and keep valid.
    pub fn scroll(&self, view_id: ViewId, first: u64, last: u64) -> Result<()> {
        self.send_edit(view_id, "scroll", json!([first, last]))
    }

    /// Sets the theme by name. Names come from [`Event::AvailableThemes`].
    pub fn set_theme(&self, name: &str) -> Result<()> {
        let payload = json!({ "theme_name": name });
        self.peer.send_rpc_notification("set_theme", &payload);
        Ok(())
    }

    /// Sets the language for a view. Requires the syntect plugin to actually
    /// do syntax highlighting; without it, [`Event::LanguageChanged`] still
    /// fires (from the `language_changed` notification).
    pub fn set_language(&self, view_id: ViewId, language: &str) -> Result<()> {
        let payload = json!({
            "view_id": view_id,
            "language_id": language,
        });
        self.peer.send_rpc_notification("set_language", &payload);
        Ok(())
    }

    /// Returns an immutable snapshot of the current state of `view_id`.
    ///
    /// `None` if the view is unknown (closed, or never opened). May reflect
    /// state from `update` notifications beyond the last `DocumentUpdated`
    /// event you observed — treat the event as "something changed, re-read".
    pub fn document(&self, view_id: ViewId) -> Option<crate::DocumentSnapshot> {
        let lock = self.inner.lock().unwrap();
        lock.docs.get(&view_id).map(|doc| doc.snapshot(view_id))
    }

    /// Returns the event channel.
    ///
    /// Single consumer: `crossbeam_channel::Receiver` is not `Clone` for
    /// normal channels. Wrap it in your own multiplexer if you need to fan
    /// out to multiple observers.
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
    /// Non-blocking. To wait for the actual exit, drain `events()` until
    /// `Exited` arrives.
    pub fn shutdown(&self) {
        self.stdin.close();
    }

    fn send_edit(&self, view_id: ViewId, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "view_id": view_id,
            "method": method,
            "params": params,
        });
        self.peer.send_rpc_notification("edit", &payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_width_codepoint_mono() {
        let params = json!([
            {"id": 2, "strings": ["hello", "world"]},
            {"id": 3, "strings": ["foo"]}
        ]);
        assert_eq!(
            measure_width(Some(&params)),
            vec![vec![5.0, 5.0], vec![3.0]]
        );
    }

    #[test]
    fn measure_width_handles_multibyte() {
        // Cyrillic: one scalar per char, so "привет" is 6.
        let params = json!([{"id": 1, "strings": ["привет"]}]);
        assert_eq!(measure_width(Some(&params)), vec![vec![6.0]]);
    }

    #[test]
    fn measure_width_empty_and_missing() {
        assert_eq!(measure_width(None), Vec::<Vec<f64>>::new());
        assert_eq!(measure_width(Some(&json!([]))), Vec::<Vec<f64>>::new());
        assert_eq!(
            measure_width(Some(&json!([{"id": 1}]))),
            vec![Vec::<f64>::new()]
        );
    }

    #[test]
    fn parse_view_id_round_trip() {
        assert_eq!(parse_view_id(&json!("view-id-1")), Some(ViewId(1)));
        assert_eq!(parse_view_id(&json!("view-id-42")), Some(ViewId(42)));
        assert_eq!(parse_view_id(&json!("nope")), None);
        assert_eq!(parse_view_id(&json!(7)), None);
    }
}
