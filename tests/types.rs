//! Serde round-trips for every public wire type.
//!
//! These are regression guards for the on-the-wire representation. Any change
//! to a `Deserialize`/`Serialize` impl, or to the delta-encoding scheme, must
//! fail here before it reaches a consumer.

use serde_json::json;
use xi_client::types::{
    Annotation, Config, Line, Operation, OperationType, Style, StyleDef, Update, ViewId,
};

// ───────────────────────── ViewId ─────────────────────────

#[test]
fn view_id_parses_wire_format() {
    assert_eq!("view-id-1".parse::<ViewId>().unwrap(), ViewId(1));
    assert_eq!("view-id-42".parse::<ViewId>().unwrap(), ViewId(42));
    assert_eq!("view-id-0".parse::<ViewId>().unwrap(), ViewId(0));
}

#[test]
fn view_id_rejects_bad_prefix() {
    for s in ["1", "view-1", "view_id-1", "VIEW-ID-1", "view-id"] {
        assert!(s.parse::<ViewId>().is_err(), "should reject {s:?}");
    }
}

#[test]
fn view_id_rejects_bad_number() {
    for s in ["view-id-", "view-id-abc", "view-id--1", "view-id-1x"] {
        assert!(s.parse::<ViewId>().is_err(), "should reject {s:?}");
    }
}

#[test]
fn view_id_display_roundtrips() {
    assert_eq!(ViewId(7).to_string(), "view-id-7");
    assert_eq!(ViewId(7).to_string().parse::<ViewId>().unwrap(), ViewId(7));
}

#[test]
fn view_id_json_roundtrip() {
    let s = serde_json::to_string(&ViewId(5)).unwrap();
    assert_eq!(s, "\"view-id-5\"");
    let back: ViewId = serde_json::from_str(&s).unwrap();
    assert_eq!(back, ViewId(5));
}

// ───────────────────────── StyleDef / Line ─────────────────────────

#[test]
fn line_decodes_delta_styles() {
    // §8.4 worked example: [0,5,3, -2,4,5, 0,3,8] over "abcdefghij".
    // `delta` is relative to the end of the previous span.
    let json = r#"{"text":"abcdefghij","styles":[0,5,3,-2,4,5,0,3,8]}"#;
    let line: Line = serde_json::from_str(json).unwrap();
    assert_eq!(line.styles.len(), 3);
    assert_eq!(
        line.styles[0],
        StyleDef {
            delta: 0,
            length: 5,
            style_id: 3
        }
    );
    assert_eq!(
        line.styles[1],
        StyleDef {
            delta: -2,
            length: 4,
            style_id: 5
        }
    );
    assert_eq!(
        line.styles[2],
        StyleDef {
            delta: 0,
            length: 3,
            style_id: 8
        }
    );
}

#[test]
fn line_rejects_non_multiple_of_three() {
    let err = serde_json::from_str::<Line>(r#"{"text":"x","styles":[0,1,2,3,4]}"#).unwrap_err();
    assert!(err.to_string().contains("multiple of 3"), "got: {err}");
}

#[test]
fn line_rejects_negative_length_or_id() {
    assert!(serde_json::from_str::<Line>(r#"{"text":"x","styles":[0,-1,2]}"#).is_err());
    assert!(serde_json::from_str::<Line>(r#"{"text":"x","styles":[0,1,-2]}"#).is_err());
}

#[test]
fn line_null_styles_are_empty() {
    let line: Line = serde_json::from_str(r#"{"text":"x","styles":null}"#).unwrap();
    assert!(line.styles.is_empty());
}

#[test]
fn line_missing_fields_default() {
    let line: Line = serde_json::from_str(r#"{"ln":42}"#).unwrap();
    assert_eq!(line.text, None);
    assert!(line.cursor.is_empty());
    assert!(line.styles.is_empty());
    assert_eq!(line.line_num, Some(42));
}

// ───────────────────────── Operation ─────────────────────────

#[test]
fn operation_decodes_all_types() {
    for (wire, expected) in [
        ("copy", OperationType::Copy_),
        ("skip", OperationType::Skip),
        ("invalidate", OperationType::Invalidate),
        ("update", OperationType::Update),
        ("ins", OperationType::Insert),
    ] {
        let json = format!(r#"{{"op":"{wire}","n":1}}"#);
        let op: Operation = serde_json::from_str(&json).unwrap();
        assert_eq!(op.operation_type, expected, "wire = {wire}");
    }
}

#[test]
fn operation_rejects_unknown_type() {
    let err = serde_json::from_str::<Operation>(r#"{"op":"frobnicate","n":1}"#).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown variant"), "got: {msg}");
    assert!(msg.contains("frobnicate"), "got: {msg}");
}

#[test]
fn operation_validate_ins_and_update() {
    let ok: Operation =
        serde_json::from_str(r#"{"op":"ins","n":1,"lines":[{"text":"x"}]}"#).unwrap();
    assert!(ok.validate().is_ok());

    let bad: Operation =
        serde_json::from_str(r#"{"op":"ins","n":2,"lines":[{"text":"x"}]}"#).unwrap();
    assert!(bad.validate().is_err());
}

#[test]
fn operation_validate_non_insert_ops() {
    let ok: Operation = serde_json::from_str(r#"{"op":"copy","n":3}"#).unwrap();
    assert!(ok.validate().is_ok());

    let bad: Operation =
        serde_json::from_str(r#"{"op":"skip","n":2,"lines":[{"text":"x"}]}"#).unwrap();
    assert!(bad.validate().is_err());
}

#[test]
fn operation_validate_rejects_zero_n() {
    let op: Operation = serde_json::from_str(r#"{"op":"copy","n":0}"#).unwrap();
    assert!(op.validate().is_err());
}

// ───────────────────────── Update / Annotation ─────────────────────────

#[test]
fn update_decodes_wire_payload() {
    let json = json!({
        "view_id": "view-id-1",
        "update": {
            "ops": [{"op": "ins", "n": 1, "lines": [{"text": "hi\n"}]}],
            "pristine": false,
            "annotations": [{"type": "selection", "ranges": [[0, 0, 0, 2]]}]
        }
    });
    let u: Update = serde_json::from_value(json).unwrap();
    assert_eq!(u.view_id, ViewId(1));
    assert!(!u.pristine);
    assert_eq!(u.operations.len(), 1);
    assert_eq!(u.annotations.len(), 1);
    assert_eq!(u.annotations[0].ranges, vec![vec![0u64, 0, 0, 2]]);
}

#[test]
fn update_missing_annotations_defaults_empty() {
    let json = json!({
        "view_id": "view-id-2",
        "update": {"ops": [], "pristine": true}
    });
    let u: Update = serde_json::from_value(json).unwrap();
    assert!(u.annotations.is_empty());
    assert!(u.pristine);
}

#[test]
fn annotation_classifies_by_type() {
    let sel: Annotation = serde_json::from_value(json!({"type":"selection","ranges":[]})).unwrap();
    let find: Annotation = serde_json::from_value(json!({"type":"find","ranges":[]})).unwrap();
    let other: Annotation = serde_json::from_value(json!({"type":"other","ranges":[]})).unwrap();

    assert!(sel.is_selection() && !sel.is_find());
    assert!(!find.is_selection() && find.is_find());
    assert!(!other.is_selection() && !other.is_find());
}

// ───────────────────────── Config ─────────────────────────

#[test]
fn config_decodes_all_fields() {
    let json = json!({
        "tab_size": 4,
        "font_face": "Menlo",
        "font_size": 13.5,
        "translate_tabs_to_spaces": true,
        "word_wrap": false,
        "line_ending": "\n"
    });
    let cfg: Config = serde_json::from_value(json).unwrap();
    assert_eq!(cfg.tab_size, Some(4));
    assert_eq!(cfg.font_face.as_deref(), Some("Menlo"));
    assert_eq!(cfg.font_size, Some(13.5));
    assert_eq!(cfg.translate_tabs_to_spaces, Some(true));
    assert_eq!(cfg.word_wrap, Some(false));
    assert_eq!(cfg.line_ending.as_deref(), Some("\n"));
}

#[test]
fn config_unknown_keys_are_ignored() {
    let json = json!({"tab_size": 2, "scroll_past_end": true});
    let cfg: Config = serde_json::from_value(json).unwrap();
    assert_eq!(cfg.tab_size, Some(2));
}

#[test]
fn config_none_fields_skip_serialization() {
    let cfg = Config {
        tab_size: Some(4),
        ..Default::default()
    };
    let v = serde_json::to_value(&cfg).unwrap();
    assert_eq!(v, json!({"tab_size": 4}));
}

// ───────────────────────── Style ─────────────────────────

#[test]
fn style_decodes_full_payload() {
    let json = json!({
        "id": 8,
        "fg_color": u32::MAX,
        "bg_color": 0,
        "weight": 700,
        "italic": true,
        "underline": false
    });
    let s: Style = serde_json::from_value(json).unwrap();
    assert_eq!(s.id, 8);
    assert_eq!(s.fg_color, Some(u32::MAX));
    assert_eq!(s.bg_color, Some(0));
    assert_eq!(s.weight, Some(700));
    assert_eq!(s.italic, Some(true));
}

#[test]
fn style_missing_optionals_are_none() {
    let s: Style = serde_json::from_value(json!({"id": 3})).unwrap();
    assert_eq!(s.fg_color, None);
    assert_eq!(s.italic, None);
}

// ───────────────────────── Compile-time bounds ─────────────────────────

#[test]
fn public_types_are_send_and_debug() {
    fn assert_send<T: Send>() {}
    fn assert_debug<T: std::fmt::Debug>() {}
    fn assert_clone<T: Clone>() {}

    assert_send::<xi_client::Event>();
    assert_debug::<xi_client::Event>();
    assert_clone::<xi_client::Event>();

    assert_send::<xi_client::Error>();
    assert_debug::<xi_client::Error>();

    assert_send::<ViewId>();
    assert_clone::<ViewId>();
    assert_send::<Config>();
    assert_clone::<Config>();
}

// ───────────────────────── GestureType ─────────────────────────

#[test]
fn gesture_type_serializes_to_new_format() {
    use xi_client::types::{GestureType, Granularity};

    let select = GestureType::Select {
        granularity: Granularity::Point,
        multi: false,
    };
    assert_eq!(
        serde_json::to_value(&select).unwrap(),
        json!({"select": {"granularity": "point", "multi": false}})
    );

    let extend = GestureType::SelectExtend {
        granularity: Granularity::Line,
    };
    assert_eq!(
        serde_json::to_value(&extend).unwrap(),
        json!({"select_extend": {"granularity": "line"}})
    );

    let drag = GestureType::Drag;
    assert_eq!(serde_json::to_value(&drag).unwrap(), json!("drag"));
}

// ───────────────────────── SpawnOptions ─────────────────────────

#[test]
fn spawn_options_default_is_all_empty() {
    use xi_client::SpawnOptions;
    let opts = SpawnOptions::default();
    assert!(opts.config_dir.is_none());
    assert!(opts.client_extras_dir.is_none());
    assert!(opts.log_level.is_none());
    assert!(opts.log_dir.is_none());
    assert!(opts.log_file.is_none());
    assert!(!opts.capture_stderr);
}

// ───────────────────────── ConfigDomain ─────────────────────────

#[test]
fn config_domain_serializes() {
    use xi_client::ConfigDomain;
    assert_eq!(
        serde_json::to_value(&ConfigDomain::General).unwrap(),
        json!("general")
    );
    assert_eq!(
        serde_json::to_value(ConfigDomain::Syntax("rust".into())).unwrap(),
        json!({"syntax": "rust"})
    );
    assert_eq!(
        serde_json::to_value(ConfigDomain::UserOverride(ViewId(1))).unwrap(),
        json!({"user_override": "view-id-1"})
    );
}

// ───────────────────────── FindQuery / FindOptions ─────────────────────────

#[test]
fn find_query_minimal_wire_shape() {
    use xi_client::FindQuery;
    let q = FindQuery::new("hello");
    assert_eq!(
        serde_json::to_value(&q).unwrap(),
        json!({"chars": "hello", "case_sensitive": false})
    );
}

#[test]
fn find_query_full_wire_shape() {
    use xi_client::FindQuery;
    let q = FindQuery::new("h.llo")
        .regex(true)
        .case_sensitive(true)
        .whole_words(true);
    assert_eq!(
        serde_json::to_value(&q).unwrap(),
        json!({
            "chars": "h.llo",
            "case_sensitive": true,
            "regex": true,
            "whole_words": true
        })
    );
}

#[test]
fn find_query_builder_chains() {
    use xi_client::FindQuery;
    let q = FindQuery::new("x")
        .case_sensitive(true)
        .regex(true)
        .whole_words(true)
        .with_id(42);
    assert_eq!(q.id, Some(42));
    assert_eq!(q.chars, "x");
    assert!(q.case_sensitive);
    assert!(q.regex);
    assert!(q.whole_words);
}

#[test]
fn find_options_default_serializes_to_empty_object() {
    use xi_client::FindOptions;
    assert_eq!(
        serde_json::to_value(FindOptions::default()).unwrap(),
        json!({})
    );
}

#[test]
fn find_options_full_wire_shape() {
    use xi_client::{FindOptions, ModifySelection};
    let o = FindOptions::default()
        .wrap_around(true)
        .allow_same(true)
        .modify_selection(ModifySelection::Add);
    assert_eq!(
        serde_json::to_value(&o).unwrap(),
        json!({
            "wrap_around": true,
            "allow_same": true,
            "modify_selection": "add"
        })
    );
}

#[test]
fn modify_selection_wire_names() {
    use xi_client::ModifySelection;
    for (variant, wire) in [
        (ModifySelection::None, "none"),
        (ModifySelection::Set, "set"),
        (ModifySelection::Add, "add"),
        (ModifySelection::AddRemovingCurrent, "add_removing_current"),
    ] {
        assert_eq!(serde_json::to_value(variant).unwrap(), json!(wire));
    }
}

// ───────────────────────── Hover / Position ─────────────────────────

#[test]
fn position_serializes_as_object() {
    use xi_client::Position;
    let p = Position { line: 3, col: 7 };
    assert_eq!(
        serde_json::to_value(p).unwrap(),
        json!({"line": 3, "col": 7})
    );
}

#[test]
fn position_deserializes() {
    use xi_client::Position;
    let p: Position = serde_json::from_value(json!({"line": 1, "col": 0})).unwrap();
    assert_eq!(p.line, 1);
    assert_eq!(p.col, 0);
}

#[test]
fn hover_decodes_with_range() {
    use xi_client::Hover;
    let json = json!({
        "content": "**bold**",
        "range": [0, 1, 0, 5]
    });
    let h: Hover = serde_json::from_value(json).unwrap();
    assert_eq!(h.content, "**bold**");
    assert_eq!(h.range, Some(vec![0, 1, 0, 5]));
}

#[test]
fn hover_decodes_without_range() {
    use xi_client::Hover;
    let h: Hover = serde_json::from_value(json!({"content": "plain"})).unwrap();
    assert_eq!(h.range, None);
}
