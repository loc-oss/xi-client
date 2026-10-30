use crate::types::{Config, Style, ViewId};

#[derive(Debug, Clone)]
pub enum Event {
    ViewOpened { request_id: u64, view_id: ViewId },
    DocumentUpdated { view_id: ViewId },
    StyleDefined(Style),
    ThemeChanged { name: String },
    ConfigChanged { view_id: ViewId, changes: Config },
    LanguageChanged { view_id: ViewId, language: String },
    FindStatus { view_id: ViewId, matches: u64 },
    ScrollTo { view_id: ViewId, line: u64, col: u64 },
    Alert(String),
    Exited,
}
