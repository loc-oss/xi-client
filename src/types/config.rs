use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Merged per-view configuration, as reported by xi-core.
///
/// All fields are `Option<T>`: xi-core omits keys the user hasn't set, and
/// different languages may not declare every key. Use [`Config::merge`] to
/// apply `config_changed` diffs — the first `config_changed` after
/// `new_view` carries the full config, subsequent ones are diffs.
#[derive(Default, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_face: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate_tabs_to_spaces: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_wrap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_ending: Option<String>,
}

impl Config {
    /// Applies a `config_changed` diff from xi-core.
    ///
    /// Semantics (see `docs/protocol.md` §8.7):
    /// - `null`        → clear the key (revert to default);
    /// - other value   → set the key, if it deserializes to the field type;
    /// - absent key    → leave the current value untouched;
    /// - unknown key   → ignored (xi-core may ship config keys we don't model).
    ///
    /// Idempotent for the "full config" case: applying the first
    /// `config_changed` over a `Default` yields the same state as
    /// `serde_json::from_value::<Config>(full)`.
    pub fn merge(&mut self, changes: &Value) {
        let Some(obj) = changes.as_object() else {
            // A non-object diff is a wire error; ignore rather than panic.
            return;
        };
        for (key, value) in obj {
            match key.as_str() {
                "tab_size" => merge_field(&mut self.tab_size, value),
                "font_face" => merge_field(&mut self.font_face, value),
                "font_size" => merge_field(&mut self.font_size, value),
                "translate_tabs_to_spaces" => {
                    merge_field(&mut self.translate_tabs_to_spaces, value)
                }
                "word_wrap" => merge_field(&mut self.word_wrap, value),
                "line_ending" => merge_field(&mut self.line_ending, value),
                _ => {}
            }
        }
    }
}

/// Applies a single `key: value` update from a config diff to `dst`.
fn merge_field<T: DeserializeOwned>(dst: &mut Option<T>, value: &Value) {
    match value {
        Value::Null => *dst = None,
        other => {
            if let Ok(parsed) = serde_json::from_value::<T>(other.clone()) {
                *dst = Some(parsed);
            }
            // Malformed diff for a known key: keep the current value rather
            // than clobbering it. xi-core should never send this.
        }
    }
}
