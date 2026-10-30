use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use serde_json::{json, Value};

use xi_rpc::{Handler, Peer, RemoteError, RpcCtx, RpcLoop, RpcPeer};

use crate::document::DocumentState;
use crate::events::Event;
use crate::process::{SharedStdin, XiProcess};
use crate::types::{Config, ConfigDomain, GestureType, SpawnOptions, Style, Update, ViewId};
use crate::{Error, Result};

/// Width-measuring callback installed via [`XiClient::set_width_measure`].
///
/// Runs on the reader thread. Must not call back into `XiClient` or send
/// RPCs — that would deadlock, since the reader thread is the one that
/// dispatches responses.
type WidthMeasureFn = dyn Fn(&[String]) -> Vec<f64> + Send + Sync;

struct Inner {
    docs: HashMap<ViewId, DocumentState>,
    styles: HashMap<u64, Style>,
    pending_updates: HashMap<ViewId, Vec<Update>>,
    next_request_id: u64,
    width_measure: Option<Arc<WidthMeasureFn>>,
}

struct ClientHandler {
    inner: Arc<Mutex<Inner>>,
    event_tx: Sender<Event>,
}

impl ClientHandler {
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
            _ => {}
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
            "measure_width" => {
                // Clone the Arc out of the lock, drop the guard, then call.
                // A callback that reenters `Inner` (e.g. `XiClient::document`)
                // will not deadlock; one that sends RPCs will.
                let cb = self.inner.lock().unwrap().width_measure.clone();
                let widths = measure_width(rpc.get("params"), cb.as_deref());
                Ok(json!(widths))
            }
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

        let applied = {
            let mut lock = self.inner.lock().unwrap();
            match lock.docs.get_mut(&view_id) {
                Some(doc) => {
                    doc.apply(&update);
                    true
                }
                None => {
                    lock.pending_updates.entry(view_id).or_default().push(update);
                    false
                }
            }
        };

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
        }

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

/// Resolves a `measure_width` request into per-item width vectors.
///
/// If `callback` is `Some`, it is invoked once per input item; the returned
/// widths are padded or truncated to match `strings.len()`. If it's `None`,
/// falls back to the `CodepointMono` policy (one column per Unicode scalar).
fn measure_width(
    params: Option<&Value>,
    callback: Option<&WidthMeasureFn>,
) -> Vec<Vec<f64>> {
    let Some(items) = params.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let strings = item.get("strings").and_then(Value::as_array);
            let Some(strings) = strings else {
                return Vec::new();
            };
            match callback {
                Some(cb) => {
                    let owned: Vec<String> = strings
                        .iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect();
                    let mut widths = cb(&owned);
                    widths.resize(owned.len(), 0.0);
                    widths
                }
                None => strings
                    .iter()
                    .map(|v| v.as_str().map_or(0.0, |s| s.chars().count() as f64))
                    .collect(),
            }
        })
        .collect()
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
    reader_exit: Arc<(Mutex<bool>, Condvar)>,
    exit_status: Mutex<Option<std::process::ExitStatus>>,
    _process: XiProcess,
}

impl XiClient {
    pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self> {
        Self::spawn_with_options(core_path, SpawnOptions::default())
    }

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
                let mut handler = ClientHandler {
                    inner: inner_for_reader,
                    event_tx: event_tx_for_reader.clone(),
                };
                let _ = looper.mainloop(
                    move || std::io::BufReader::new(stdout),
                    &mut handler,
                );
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

    // ───────────────────────── Lifecycle ─────────────────────────

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
    /// The callback runs on the **reader thread**. It must not call back
    /// into `XiClient` or send RPCs — that would deadlock, because the
    /// reader thread is the one that dispatches responses.
    ///
    /// Reading `document()` from the callback is safe (it only locks
    /// `Inner`), but doing heavy work there will stall inbound message
    /// processing.
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

    pub fn view_ids(&self) -> Vec<ViewId> {
        let lock = self.inner.lock().unwrap();
        let mut ids: Vec<ViewId> = lock.docs.keys().copied().collect();
        ids.sort();
        ids
    }

    pub fn save(&self, view_id: ViewId, path: impl AsRef<Path>) -> Result<()> {
        let payload = json!({
            "view_id": view_id,
            "file_path": path.as_ref().to_string_lossy(),
        });
        self.peer.send_rpc_notification("save", &payload);
        Ok(())
    }

    // ───────────────────────── Config ─────────────────────────

    pub fn get_config(&self, view_id: ViewId) -> Result<Config> {
        let payload = json!({ "view_id": view_id });
        let result = self.peer.send_rpc_request("get_config", &payload)?;
        Ok(serde_json::from_value(result)?)
    }

    pub fn modify_user_config(&self, domain: ConfigDomain, changes: Value) -> Result<()> {
        let payload = json!({
            "domain": serde_json::to_value(&domain)?,
            "changes": changes,
        });
        self.peer.send_rpc_notification("modify_user_config", &payload);
        Ok(())
    }

    // ───────────────────────── Editing ─────────────────────────

    pub fn insert(&self, view_id: ViewId, chars: &str) -> Result<()> {
        self.send_edit(view_id, "insert", json!({ "chars": chars }))
    }

    pub fn delete_backward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_backward", json!([]))
    }

    pub fn delete_forward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_forward", json!([]))
    }

    pub fn insert_newline(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "insert_newline", json!([]))
    }

    pub fn insert_tab(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "insert_tab", json!([]))
    }

    pub fn undo(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "undo", json!([]))
    }

    pub fn redo(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "redo", json!([]))
    }

    pub fn delete_word_backward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_word_backward", json!([]))
    }

    pub fn delete_word_forward(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "delete_word_forward", json!([]))
    }

    pub fn transpose(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "transpose", json!([]))
    }

    pub fn duplicate_line(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "duplicate_line", json!([]))
    }

    pub fn uppercase(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "uppercase", json!([]))
    }

    pub fn lowercase(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "lowercase", json!([]))
    }

    pub fn capitalize(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "capitalize", json!([]))
    }

    pub fn indent(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "indent", json!([]))
    }

    pub fn outdent(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "outdent", json!([]))
    }

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

    pub fn select_all(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "select_all", json!([]))
    }

    pub fn collapse_selections(&self, view_id: ViewId) -> Result<()> {
        self.send_edit(view_id, "collapse_selections", json!([]))
    }

    // ───────────────────────── Viewport ─────────────────────────

    pub fn resize(&self, view_id: ViewId, width: u64, height: u64) -> Result<()> {
        self.send_edit(view_id, "resize", json!({ "width": width, "height": height }))
    }

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

    pub fn document(&self, view_id: ViewId) -> Option<crate::DocumentSnapshot> {
        let lock = self.inner.lock().unwrap();
        lock.docs.get(&view_id).map(|doc| doc.snapshot(view_id))
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.event_rx
    }

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
    fn measure_width_codepoint_mono() {
        let params = json!([
            {"id": 2, "strings": ["hello", "world"]},
            {"id": 3, "strings": ["foo"]}
        ]);
        assert_eq!(
            measure_width(Some(&params), None),
            vec![vec![5.0, 5.0], vec![3.0]]
        );
    }

    #[test]
    fn measure_width_handles_multibyte() {
        let params = json!([{"id": 1, "strings": ["привет"]}]);
        assert_eq!(measure_width(Some(&params), None), vec![vec![6.0]]);
    }

    #[test]
    fn measure_width_empty_and_missing() {
        assert_eq!(measure_width(None, None), Vec::<Vec<f64>>::new());
        assert_eq!(measure_width(Some(&json!([])), None), Vec::<Vec<f64>>::new());
        assert_eq!(
            measure_width(Some(&json!([{"id": 1}])), None),
            vec![Vec::<f64>::new()]
        );
    }

    #[test]
    fn measure_width_uses_callback() {
        let params = json!([
            {"id": 2, "strings": ["hello", "world"]},
            {"id": 3, "strings": ["foo"]}
        ]);
        let cb = |strings: &[String]| -> Vec<f64> {
            strings.iter().map(|_| 42.0).collect()
        };
        assert_eq!(
            measure_width(Some(&params), Some(&cb)),
            vec![vec![42.0, 42.0], vec![42.0]]
        );
    }

    #[test]
    fn measure_width_pads_short_callback_output() {
        let params = json!([{"id": 1, "strings": ["a", "b", "c"]}]);
        let cb = |_: &[String]| -> Vec<f64> { vec![1.0] };
        assert_eq!(
            measure_width(Some(&params), Some(&cb)),
            vec![vec![1.0, 0.0, 0.0]]
        );
    }

    #[test]
    fn measure_width_truncates_long_callback_output() {
        let params = json!([{"id": 1, "strings": ["a"]}]);
        let cb = |_: &[String]| -> Vec<f64> { vec![1.0, 2.0, 3.0] };
        assert_eq!(
            measure_width(Some(&params), Some(&cb)),
            vec![vec![1.0]]
        );
    }

    #[test]
    fn parse_view_id_round_trip() {
        assert_eq!(parse_view_id(&json!("view-id-1")), Some(ViewId(1)));
        assert_eq!(parse_view_id(&json!("view-id-42")), Some(ViewId(42)));
        assert_eq!(parse_view_id(&json!("nope")), None);
        assert_eq!(parse_view_id(&json!(7)), None);
    }

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
