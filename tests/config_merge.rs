//! `Config::merge` semantics — the state-mutation path for `config_changed`.
//!
//! `client::on_config_changed` relies on this to keep `doc.config` in sync
//! with xi-core. Any change in null-handling or unknown-key-handling breaks
//! live config updates.

use serde_json::json;
use xi_client::Config;

fn cfg(v: serde_json::Value) -> Config {
    serde_json::from_value(v).unwrap()
}

#[test]
fn merge_null_clears_field() {
    let mut c = cfg(json!({"tab_size": 4}));
    c.merge(&json!({"tab_size": null}));
    assert_eq!(c.tab_size, None);
}

#[test]
fn merge_value_sets_field() {
    let mut c = Config::default();
    c.merge(&json!({"tab_size": 2, "font_face": "Mono"}));
    assert_eq!(c.tab_size, Some(2));
    assert_eq!(c.font_face.as_deref(), Some("Mono"));
}

#[test]
fn merge_absent_key_leaves_field_untouched() {
    let mut c = cfg(json!({"tab_size": 4, "font_size": 14.0}));
    c.merge(&json!({"font_size": 12.0}));
    assert_eq!(c.tab_size, Some(4), "absent key must not be cleared");
    assert_eq!(c.font_size, Some(12.0));
}

#[test]
fn merge_unknown_key_ignored() {
    let mut c = cfg(json!({"tab_size": 4}));
    c.merge(&json!({"scroll_past_end": true, "tab_size": 8}));
    assert_eq!(c.tab_size, Some(8));
}

#[test]
fn merge_non_object_noop() {
    let mut c = cfg(json!({"tab_size": 4}));
    c.merge(&json!(null));
    c.merge(&json!("not an object"));
    c.merge(&json!([1, 2, 3]));
    assert_eq!(c.tab_size, Some(4));
}

#[test]
fn merge_malformed_value_for_known_key_keeps_old() {
    // `tab_size` must be a number; a string is a wire error. Keep the old
    // value rather than clobbering it or panicking.
    let mut c = cfg(json!({"tab_size": 4}));
    c.merge(&json!({"tab_size": "four"}));
    assert_eq!(c.tab_size, Some(4));
}

#[test]
fn merge_full_config_over_default_reproduces_deserialize() {
    // Idempotence property: merging the full config over Default must yield
    // the same struct as a direct deserialize.
    let full = json!({
        "tab_size": 4,
        "font_face": "Menlo",
        "font_size": 13.5,
        "translate_tabs_to_spaces": true,
        "word_wrap": false,
        "line_ending": "\n"
    });
    let direct: Config = serde_json::from_value(full.clone()).unwrap();

    let mut merged = Config::default();
    merged.merge(&full);

    assert_eq!(merged, direct);
}

#[test]
fn merge_is_sequential() {
    // Later merges see earlier state; clearing then setting round-trips.
    let mut c = Config::default();
    c.merge(&json!({"tab_size": 4}));
    c.merge(&json!({"tab_size": null}));
    c.merge(&json!({"tab_size": 2}));
    assert_eq!(c.tab_size, Some(2));
}
