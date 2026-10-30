//! `apply_ops`, `DocumentState::apply`, `snapshot`, delta-decoding.
//!
//! Regression guards for the doc model (§8.2, §8.3, §8.4) and the newline-
//! stripping contract. The two contracts most likely to regress silently.

use serde_json::json;
use xi_client::document::{DocumentState, apply_ops};
use xi_client::types::{Line, Operation, StyleDef, Update, ViewId};

fn op(v: serde_json::Value) -> Operation {
    serde_json::from_value(v).unwrap()
}

fn update(v: serde_json::Value) -> Update {
    serde_json::from_value(v).unwrap()
}

fn line(text: &str) -> Line {
    Line {
        text: Some(text.into()),
        ..Default::default()
    }
}

fn texts(lines: &[Line]) -> Vec<Option<String>> {
    lines.iter().map(|l| l.text.clone()).collect()
}

// ───────────────────────── apply_ops: individual ops ─────────────────────────

#[test]
fn copy_advances_and_appends() {
    let mut lines = vec![line("a\n"), line("b\n"), line("c\n")];
    let ops = vec![op(json!({"op":"copy","n":2}))];
    apply_ops(&mut lines, &ops);
    assert_eq!(texts(&lines), vec![Some("a\n".into()), Some("b\n".into())]);
}

#[test]
fn skip_advances_without_appending() {
    let mut lines = vec![line("a\n"), line("b\n"), line("c\n")];
    let ops = vec![
        op(json!({"op":"skip","n":2})),
        op(json!({"op":"copy","n":1})),
    ];
    apply_ops(&mut lines, &ops);
    assert_eq!(texts(&lines), vec![Some("c\n".into())]);
}

#[test]
fn invalidate_appends_blanks_without_advancing() {
    let mut lines = vec![line("a\n"), line("b\n")];
    let ops = vec![
        op(json!({"op":"invalidate","n":2})),
        op(json!({"op":"copy","n":2})),
    ];
    apply_ops(&mut lines, &ops);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0].text, None);
    assert_eq!(lines[1].text, None);
    assert_eq!(
        texts(&lines[2..]),
        vec![Some("a\n".into()), Some("b\n".into())]
    );
}

#[test]
fn insert_appends_without_advancing() {
    // §8.3 gotcha: `ins` does NOT advance `old_ix`.
    let mut lines = vec![line("a\n")];
    let ops = vec![
        op(json!({"op":"ins","n":1,"lines":[{"text":"x\n"}]})),
        op(json!({"op":"copy","n":1})),
    ];
    apply_ops(&mut lines, &ops);
    assert_eq!(texts(&lines), vec![Some("x\n".into()), Some("a\n".into())]);
}

#[test]
fn update_with_text_replaces() {
    let mut lines = vec![line("a\n"), line("b\n"), line("c\n")];
    let ops = vec![
        op(json!({"op":"copy","n":1})),
        op(json!({"op":"update","n":1,"lines":[{"text":"B\n"}]})),
        op(json!({"op":"copy","n":1})),
    ];
    apply_ops(&mut lines, &ops);
    assert_eq!(
        texts(&lines),
        vec![Some("a\n".into()), Some("B\n".into()), Some("c\n".into())]
    );
}

#[test]
fn update_with_null_text_keeps_old() {
    let mut lines = vec![line("keep\n")];
    let ops = vec![op(json!({"op":"update","n":1,"lines":[{"text":null}]}))];
    apply_ops(&mut lines, &ops);
    assert_eq!(lines[0].text.as_deref(), Some("keep\n"));
}

#[test]
fn copy_past_end_emits_blank_lines() {
    let mut lines = vec![line("a\n")];
    let ops = vec![op(json!({"op":"copy","n":3}))];
    apply_ops(&mut lines, &ops);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].text.as_deref(), Some("a\n"));
    assert_eq!(lines[1].text, None);
    assert_eq!(lines[2].text, None);
}

#[test]
fn apply_ops_empty_batch_clears() {
    // Matches xi-core semantics: the new list starts empty.
    let mut lines = vec![line("a\n")];
    apply_ops(&mut lines, &[]);
    assert!(lines.is_empty());
}

#[test]
fn apply_ops_fully_replaces_lines() {
    // The batch builds a new Vec and swaps at the end.
    let mut lines = vec![line("old\n")];
    let ops = vec![op(json!({"op":"ins","n":1,"lines":[{"text":"new\n"}]}))];
    apply_ops(&mut lines, &ops);
    assert_eq!(texts(&lines), vec![Some("new\n".into())]);
}

// ───────────────────────── DocumentState::new ─────────────────────────

#[test]
fn new_state_has_one_empty_line() {
    // §8.2 invariant: the empty document has exactly one line with text "".
    let doc = DocumentState::new();
    let snap = doc.snapshot(ViewId(1));
    assert_eq!(snap.lines.len(), 1);
    assert_eq!(snap.lines[0].text, "");
    assert!(snap.pristine);
}

// ───────────────────────── DocumentState::apply ─────────────────────────

#[test]
fn apply_insert_replaces_state() {
    let u = update(json!({
        "view_id": "view-id-1",
        "update": {
            "ops": [{"op":"ins","n":2,"lines":[{"text":"a\n"},{"text":"b\n"}]}],
            "pristine": false
        }
    }));
    let mut doc = DocumentState::new();
    doc.apply(&u);
    let snap = doc.snapshot(ViewId(1));
    assert_eq!(snap.lines.len(), 2);
    assert_eq!(snap.lines[0].text, "a");
    assert_eq!(snap.lines[1].text, "b");
    assert!(!snap.pristine);
}

#[test]
fn apply_updates_pristine() {
    let mut doc = DocumentState::new();
    doc.apply(&update(json!({
        "view_id": "view-id-1",
        "update": {"ops": [], "pristine": false}
    })));
    assert!(!doc.snapshot(ViewId(1)).pristine);
}

#[test]
fn apply_extracts_only_selection_annotations() {
    let u = update(json!({
        "view_id": "view-id-1",
        "update": {
            "ops": [],
            "pristine": false,
            "annotations": [
                {"type": "selection", "ranges": [[0,0,0,3],[1,0,1,2]]},
                {"type": "find", "ranges": [[5,0,5,4]]}
            ]
        }
    }));
    let mut doc = DocumentState::new();
    doc.apply(&u);
    assert_eq!(doc.selections, vec![(0, 0, 0, 3), (1, 0, 1, 2)]);
}

#[test]
fn apply_clears_stale_selections() {
    // Two applies in a row: the second one's annotations replace the first's.
    let mut doc = DocumentState::new();
    doc.apply(&update(json!({
        "view_id": "view-id-1",
        "update": {
            "ops": [],
            "pristine": false,
            "annotations": [{"type":"selection","ranges":[[0,0,0,1]]}]
        }
    })));
    doc.apply(&update(json!({
        "view_id": "view-id-1",
        "update": {"ops": [], "pristine": false}
    })));
    assert!(doc.selections.is_empty());
}

// ───────────────────────── DocumentState::snapshot ─────────────────────────

#[test]
fn snapshot_decodes_delta_styles() {
    // §8.4 worked example.
    let doc = DocumentState {
        lines: vec![Line {
            text: Some("abcdefghij\n".into()),
            styles: vec![
                StyleDef {
                    delta: 0,
                    length: 5,
                    style_id: 3,
                },
                StyleDef {
                    delta: -2,
                    length: 4,
                    style_id: 5,
                },
                StyleDef {
                    delta: 0,
                    length: 3,
                    style_id: 8,
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let snap = doc.snapshot(ViewId(1));
    let spans = &snap.lines[0].styles;
    assert_eq!(spans.len(), 3);
    assert_eq!((spans[0].start, spans[0].len, spans[0].style_id), (0, 5, 3));
    assert_eq!((spans[1].start, spans[1].len, spans[1].style_id), (3, 4, 5));
    assert_eq!((spans[2].start, spans[2].len, spans[2].style_id), (7, 3, 8));
}

#[test]
fn snapshot_strips_exactly_one_trailing_newline() {
    // Regression guard: `trim_end_matches('\n')` would strip *all* trailing
    // newlines. We strip exactly one.
    let doc = DocumentState {
        lines: vec![Line {
            text: Some("a\n\n".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(doc.snapshot(ViewId(1)).lines[0].text, "a\n");
}

#[test]
fn snapshot_strips_crlf() {
    let doc = DocumentState {
        lines: vec![Line {
            text: Some("a\r\n".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(doc.snapshot(ViewId(1)).lines[0].text, "a");
}

#[test]
fn snapshot_propagates_cursors() {
    let doc = DocumentState {
        lines: vec![Line {
            text: Some("abc\n".into()),
            cursor: vec![1, 2],
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(doc.snapshot(ViewId(1)).lines[0].cursors, vec![1, 2]);
}

#[test]
fn snapshot_empty_lines_yields_one_empty_line() {
    // §8.2 safety net: even if `lines` was cleared, snapshot returns one
    // empty line, not an empty vec.
    let mut doc = DocumentState::new();
    doc.lines.clear();
    let snap = doc.snapshot(ViewId(1));
    assert_eq!(snap.lines.len(), 1);
    assert_eq!(snap.lines[0].text, "");
}

#[test]
fn snapshot_carries_view_id_and_metadata() {
    let mut doc = DocumentState::new();
    doc.language = "Rust".into();
    doc.pristine = false;
    doc.path = Some(std::path::PathBuf::from("/tmp/f.rs"));
    let snap = doc.snapshot(ViewId(7));
    assert_eq!(snap.view_id, Some(ViewId(7)));
    assert_eq!(snap.language, "Rust");
    assert!(!snap.pristine);
    assert_eq!(
        snap.path.as_deref(),
        Some(std::path::Path::new("/tmp/f.rs"))
    );
}
