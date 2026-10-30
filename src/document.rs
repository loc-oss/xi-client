use crate::types::{Config, Line, Operation, OperationType, Update, ViewId};

/// A single style span on a line, in absolute UTF-8 byte offsets.
///
/// Built from [`crate::types::line::StyleDef`] (which stores delta-encoded
/// offsets) in [`DocumentState::snapshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct StyleSpan {
    /// Absolute byte offset where the span starts (0-based, within the line).
    pub start: usize,
    /// Length of the span in bytes.
    pub len: usize,
    /// Style id, referencing an entry emitted via `Event::StyleDefined`.
    pub style_id: u64,
}

/// A single logical line as exposed to the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineState {
    /// Line content, *without* trailing newline.
    pub text: String,
    /// Style spans, sorted by `start`, non-overlapping unless xi-core sent
    /// overlapping spans (see `docs/protocol.md` §8.4).
    pub styles: Vec<StyleSpan>,
    /// Cursor positions on this line, as UTF-8 byte offsets.
    pub cursors: Vec<usize>,
}

/// An immutable snapshot of a document, handed to the UI via
/// [`crate::XiClient::document`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocumentSnapshot {
    /// `Some(view_id)` for snapshots returned by `XiClient::document`;
    /// `None` only for a freshly constructed default.
    pub view_id: Option<ViewId>,
    /// One entry per logical line. The empty document has exactly one line
    /// with `text == ""`.
    pub lines: Vec<LineState>,
    /// Global selections as `[start_line, start_col, end_line, end_col]`,
    /// all as `usize`. Columns are UTF-8 byte offsets within the line.
    pub selections: Vec<(usize, usize, usize, usize)>,
    /// `true` while the buffer has not been modified since the last save.
    pub pristine: bool,
    pub path: Option<std::path::PathBuf>,
    /// Language id, e.g. `"Rust"`. Empty until `language_changed` arrives.
    pub language: String,
    /// Merged view config. See [`Config::merge`].
    pub config: Config,
}

/// Mutable per-view state. Updated by the reader thread as `update`
/// notifications arrive; snapshotted by `XiClient::document`.
#[derive(Debug)]
pub struct DocumentState {
    pub lines: Vec<Line>,
    pub pristine: bool,
    pub path: Option<std::path::PathBuf>,
    pub language: String,
    pub config: Config,
    pub selections: Vec<(usize, usize, usize, usize)>,
}

impl Default for DocumentState {
    fn default() -> Self {
        Self {
            // Invariant (§8.2): the empty document has exactly one empty line.
            lines: vec![Line::default()],
            pristine: true,
            path: None,
            language: String::new(),
            config: Config::default(),
            selections: Vec::new(),
        }
    }
}

impl DocumentState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies an `update` notification to this document.
    ///
    /// The whole batch is applied atomically: `lines` is replaced only after
    /// every op has been processed, so a concurrent `snapshot()` sees either
    /// the pre-update or post-update state, never a torn one.
    pub fn apply(&mut self, update: &Update) {
        apply_ops(&mut self.lines, &update.operations);
        self.pristine = update.pristine;

        self.selections.clear();
        for ann in &update.annotations {
            if !ann.is_selection() {
                continue;
            }
            for r in &ann.ranges {
                if r.len() >= 4 {
                    self.selections.push((
                        r[0] as usize,
                        r[1] as usize,
                        r[2] as usize,
                        r[3] as usize,
                    ));
                }
            }
        }
    }

    /// Builds an immutable view of the current state.
    pub fn snapshot(&self, view_id: ViewId) -> DocumentSnapshot {
        let lines: Vec<LineState> = self
            .lines
            .iter()
            .map(|l| {
                // Decode delta-encoded styles into absolute offsets (§8.4).
                let mut cursor: i64 = 0;
                let mut spans = Vec::with_capacity(l.styles.len());
                for s in &l.styles {
                    let start = cursor + s.delta;
                    let end = start + s.length as i64;
                    spans.push(StyleSpan {
                        start: start.max(0) as usize,
                        len: s.length as usize,
                        style_id: s.style_id,
                    });
                    cursor = end;
                }

                LineState {
                    text: strip_trailing_newline(l.text.as_deref().unwrap_or("")),
                    styles: spans,
                    cursors: l.cursor.iter().map(|&c| c as usize).collect(),
                }
            })
            .collect();

        DocumentSnapshot {
            view_id: Some(view_id),
            // Invariant (§8.2): even a document with no lines yet reports a
            // single empty line. After the first `update`, this is a no-op.
            lines: if lines.is_empty() {
                vec![LineState::default()]
            } else {
                lines
            },
            selections: self.selections.clone(),
            pristine: self.pristine,
            path: self.path.clone(),
            language: self.language.clone(),
            config: self.config.clone(),
        }
    }
}

/// Strips at most one trailing `\n` (and a preceding `\r` if present).
///
/// xi-core sends lines with a single trailing newline. The old implementation
/// used `trim_end_matches('\n')`, which strips *all* trailing newlines and
/// silently drops blank trailing lines.
fn strip_trailing_newline(s: &str) -> String {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let s = s.strip_suffix('\r').unwrap_or(s);
    s.to_string()
}

/// Applies a batch of [`Operation`]s to `lines`, replacing `lines` on success.
///
/// See `docs/protocol.md` §8.3. Key invariants:
/// - `copy` advances `old_ix` and copies old lines;
/// - `skip` advances `old_ix` without copying;
/// - `invalidate` appends blank lines and does *not* advance `old_ix`;
/// - `ins` appends new lines and does *not* advance `old_ix`;
/// - `update` replaces `n` old lines starting at `old_ix`; a line with
///   `text: None` keeps the old text.
pub fn apply_ops(lines: &mut Vec<Line>, ops: &[Operation]) {
    let mut new_lines: Vec<Line> = Vec::with_capacity(lines.len());
    let mut old_ix: usize = 0;

    for op in ops {
        debug_assert!(op.validate().is_ok(), "malformed op from xi-core: {op:?}");

        match op.operation_type {
            OperationType::Copy_ => {
                for _ in 0..op.nb_lines {
                    if old_ix < lines.len() {
                        new_lines.push(lines[old_ix].clone());
                    } else {
                        new_lines.push(Line::default());
                    }
                    old_ix += 1;
                }
            }
            OperationType::Skip => {
                old_ix += op.nb_lines as usize;
            }
            OperationType::Invalidate => {
                for _ in 0..op.nb_lines {
                    new_lines.push(Line::default());
                }
            }
            OperationType::Insert => {
                for l in &op.lines {
                    new_lines.push(l.clone());
                }
            }
            OperationType::Update => {
                for (i, l) in op.lines.iter().enumerate() {
                    let target = old_ix + i;
                    let mut nl = l.clone();
                    if nl.text.is_none() && target < lines.len() {
                        nl.text = lines[target].text.clone();
                    }
                    new_lines.push(nl);
                }
                old_ix += op.nb_lines as usize;
            }
        }
    }
    *lines = new_lines;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Line, StyleDef};
    use serde_json::json;

    fn update_from(v: serde_json::Value) -> Update {
        serde_json::from_value(v).unwrap()
    }

    fn empty_op(op: &str, n: u64) -> Operation {
        serde_json::from_value(json!({"op": op, "n": n})).unwrap()
    }

    #[test]
    fn new_state_has_one_empty_line() {
        let doc = DocumentState::new();
        assert_eq!(doc.lines.len(), 1);
        assert!(doc.lines[0].text.is_none());
        let snap = doc.snapshot(ViewId(1));
        assert_eq!(snap.lines.len(), 1);
        assert_eq!(snap.lines[0].text, "");
        assert!(snap.pristine);
    }

    #[test]
    fn apply_insert() {
        let update = update_from(json!({
            "view_id": "view-id-1",
            "update": {
                "ops": [{"op": "ins", "n": 2, "lines": [
                    {"text": "hello\n"},
                    {"text": "world\n"}
                ]}],
                "pristine": false
            }
        }));
        let mut doc = DocumentState::new();
        doc.apply(&update);
        assert_eq!(doc.lines.len(), 2);
        let snap = doc.snapshot(ViewId(1));
        assert_eq!(snap.lines[0].text, "hello");
        assert_eq!(snap.lines[1].text, "world");
        assert!(!snap.pristine);
    }

    #[test]
    fn copy_skip_invalidate_ins() {
        // Start: ["a\n", "b\n", "c\n"]
        let mut lines = vec![
            Line {
                text: Some("a\n".into()),
                ..Default::default()
            },
            Line {
                text: Some("b\n".into()),
                ..Default::default()
            },
            Line {
                text: Some("c\n".into()),
                ..Default::default()
            },
        ];
        // copy 1, skip 1, ins 1 ["x\n"], copy 1
        let ops = vec![
            empty_op("copy", 1),
            empty_op("skip", 1),
            serde_json::from_value::<Operation>(json!({
                "op": "ins", "n": 1, "lines": [{"text": "x\n"}]
            }))
            .unwrap(),
            empty_op("copy", 1),
        ];
        apply_ops(&mut lines, &ops);
        let texts: Vec<_> = lines.iter().map(|l| l.text.clone().unwrap()).collect();
        assert_eq!(texts, vec!["a\n", "x\n", "c\n"]);
    }

    #[test]
    fn update_with_null_text_keeps_old() {
        let mut lines = vec![Line {
            text: Some("keep\n".into()),
            ..Default::default()
        }];
        let ops = vec![
            serde_json::from_value::<Operation>(json!({
                "op": "update", "n": 1, "lines": [{"text": null}]
            }))
            .unwrap(),
        ];
        apply_ops(&mut lines, &ops);
        assert_eq!(lines[0].text.as_deref(), Some("keep\n"));
    }

    #[test]
    fn apply_ops_is_atomic_on_empty_batch() {
        let mut lines = vec![Line {
            text: Some("a\n".into()),
            ..Default::default()
        }];
        apply_ops(&mut lines, &[]);
        assert_eq!(
            lines.len(),
            0,
            "empty batch clears — matches xi-core semantics"
        );
    }

    #[test]
    fn snapshot_decodes_delta_styles() {
        // §8.4: [0,5,3, -2,4,5, 0,3,8] over "abcdefghij"
        //   span0 = (0..5, style 3)
        //   span1 = (3..7, style 5)  (overlaps span0)
        //   span2 = (7..10, style 8)
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
        assert_eq!(snap.lines[0].text, "abcdefghij");
        assert_eq!(snap.lines[0].styles.len(), 3);
        assert_eq!(
            snap.lines[0].styles[0],
            StyleSpan {
                start: 0,
                len: 5,
                style_id: 3
            }
        );
        assert_eq!(
            snap.lines[0].styles[1],
            StyleSpan {
                start: 3,
                len: 4,
                style_id: 5
            }
        );
        assert_eq!(
            snap.lines[0].styles[2],
            StyleSpan {
                start: 7,
                len: 3,
                style_id: 8
            }
        );
    }

    #[test]
    fn snapshot_strips_only_one_trailing_newline() {
        let doc = DocumentState {
            lines: vec![Line {
                text: Some("hello\n\n".into()), // pathological, but test the contract
                ..Default::default()
            }],
            ..Default::default()
        };
        let snap = doc.snapshot(ViewId(1));
        assert_eq!(snap.lines[0].text, "hello\n");
    }

    #[test]
    fn snapshot_strips_crlf() {
        let doc = DocumentState {
            lines: vec![Line {
                text: Some("hello\r\n".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let snap = doc.snapshot(ViewId(1));
        assert_eq!(snap.lines[0].text, "hello");
    }

    #[test]
    fn selections_extracted_from_selection_annotations_only() {
        let update = update_from(json!({
            "view_id": "view-id-1",
            "update": {
                "ops": [],
                "pristine": false,
                "annotations": [
                    {"type": "selection", "ranges": [[0, 0, 0, 3], [1, 0, 1, 2]]},
                    {"type": "find", "ranges": [[5, 0, 5, 4]]}
                ]
            }
        }));
        let mut doc = DocumentState::new();
        doc.apply(&update);
        assert_eq!(doc.selections, vec![(0, 0, 0, 3), (1, 0, 1, 2)]);
    }

    #[test]
    fn cursors_propagated() {
        let update = update_from(json!({
            "view_id": "view-id-1",
            "update": {
                "ops": [{"op": "ins", "n": 1, "lines": [{"text": "abc\n", "cursor": [1, 2]}]}],
                "pristine": false
            }
        }));
        let mut doc = DocumentState::new();
        doc.apply(&update);
        let snap = doc.snapshot(ViewId(1));
        assert_eq!(snap.lines[0].cursors, vec![1, 2]);
    }

    #[test]
    fn empty_snapshot_has_one_empty_line() {
        // Fresh state with all lines already cleared by an empty update.
        let mut doc = DocumentState::new();
        doc.lines.clear();
        let snap = doc.snapshot(ViewId(1));
        assert_eq!(snap.lines.len(), 1);
        assert_eq!(snap.lines[0].text, "");
    }
}
