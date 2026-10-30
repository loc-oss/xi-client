//! Protocol layer: parses inbound xi-core notifications into [`Event`]s and
//! mutates the shared [`Inner`] state. Dispatches `measure_width` requests.
//!
//! Lives under `client/` but is not part of the public façade. The parent
//! module ([`super`]) owns [`Inner`] and hands it in via [`ClientHandler::new`].

use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use serde_json::{json, Value};

use xi_rpc::{Handler, RemoteError, RpcCtx};

use crate::events::Event;
use crate::types::{Config, Style, Update, ViewId};

use super::{Inner, WidthMeasureFn};

pub(super) struct ClientHandler {
    inner: Arc<Mutex<Inner>>,
    event_tx: Sender<Event>,
}

impl ClientHandler {
    pub(super) fn new(inner: Arc<Mutex<Inner>>, event_tx: Sender<Event>) -> Self {
        Self { inner, event_tx }
    }

    /// Sends an event without holding the `Inner` lock.
    ///
    /// Callers must drop the lock *before* calling this, so a UI handler that
    /// calls back into `XiClient` doesn't deadlock on `Inner`.
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
        // Wire format varies: some builds send `matches`, others `queries`.
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
        // Cyrillic: one scalar per char, so "привет" is 6.
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
        assert_eq!(measure_width(Some(&params), Some(&cb)), vec![vec![1.0]]);
    }

    #[test]
    fn parse_view_id_round_trip() {
        assert_eq!(parse_view_id(&json!("view-id-1")), Some(ViewId(1)));
        assert_eq!(parse_view_id(&json!("view-id-42")), Some(ViewId(42)));
        assert_eq!(parse_view_id(&json!("nope")), None);
        assert_eq!(parse_view_id(&json!(7)), None);
    }
}
