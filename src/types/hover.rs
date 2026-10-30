use serde::{Deserialize, Serialize};

/// A position within a document: logical line and UTF-8 byte offset within
/// that line. Used by `request_hover` and `goto_line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u64,
    pub col: u64,
}

/// A hover tooltip result.
///
/// `content` is markdown. `range` (if present) is the span the tooltip
/// applies to, as `[start_line, start_col, end_line, end_col]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Hover {
    pub content: String,
    #[serde(default)]
    pub range: Option<Vec<u64>>,
}
