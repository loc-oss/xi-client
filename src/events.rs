use crate::types::{Config, Style, ViewId};

/// Events emitted by the SDK on [`crate::XiClient::events`].
///
/// See `docs/protocol.md` §7 for the full reference. Not every variant
/// corresponds one-to-one with a wire notification: `ViewClosed` is emitted
/// locally, and `ViewOpened` / `DocumentUpdated` are decorated versions of
/// wire events.
///
/// The enum is `#[non_exhaustive]`: new variants will be added as more of the
/// xi-core protocol is exposed. Downstream `match` arms must include a
/// wildcard branch.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// `xi-core` responded to `new_view`.
    ///
    /// `request_id` matches the value returned by
    /// [`crate::XiClient::new_view_async`], or is allocated internally by the
    /// blocking `new_view`. Emitted once per view.
    ///
    /// After this event, `document(view_id)` returns `Some(snapshot)` with a
    /// default (empty) document; the first real content arrives with
    /// [`Event::DocumentUpdated`].
    ViewOpened { request_id: u64, view_id: ViewId },

    /// Every `update` notification from `xi-core`.
    ///
    /// The SDK has *already applied* the ops to the document state when this
    /// event fires. Read the current state with `document(view_id)`. The event
    /// carries no payload — deliberately, so the UI can coalesce updates and
    /// pull on demand.
    DocumentUpdated { view_id: ViewId },

    /// A `def_style` notification. The SDK has already inserted it into its
    /// style registry.
    StyleDefined(Style),

    /// A `theme_changed` notification. Only the theme *name* is exposed; the
    /// full `ThemeSettings` payload is dropped (see `docs/protocol.md` §10).
    ThemeChanged { name: String },

    /// A `config_changed` notification.
    ///
    /// The first event per view carries the complete merged config; subsequent
    /// events carry only the changed keys. The SDK merges diffs into
    /// `DocumentState::config` *before* emitting, so `document(view_id).config`
    /// is always the current merged view.
    ConfigChanged { view_id: ViewId, changes: Config },

    /// A `language_changed` notification. `language` is the new language id
    /// (e.g. `"Rust"`, `"Plain Text"`).
    LanguageChanged { view_id: ViewId, language: String },

    /// A `find_status` notification. `matches` is the number of matches known
    /// so far — xi-core reports incrementally, so this grows over time until
    /// the search reaches EOF.
    FindStatus { view_id: ViewId, matches: u64 },

    /// A `scroll_to` notification. Requests that the viewport be scrolled so
    /// that `(line, col)` is visible.
    ///
    /// `line` is a *logical* line (not formatted). `col` is a UTF-8 byte
    /// offset within the line.
    ScrollTo { view_id: ViewId, line: u64, col: u64 },

    /// An `alert` notification. Used by `xi-core` for user-visible errors
    /// (file changed on disk, save failures, plugin crashes, …).
    ///
    /// The SDK does not interpret the string; display it verbatim.
    Alert(String),

    /// The SDK's reader thread saw EOF on `xi-core`'s stdout.
    ///
    /// Exactly once per `XiClient` instance, whether the exit was requested
    /// via [`crate::XiClient::shutdown`] or unexpected (crash, killed).
    /// After this event, all `XiClient` write methods return
    /// [`crate::Error::CoreNotRunning`].
    Exited,

    /// Emitted locally after a successful `close_view`.
    ///
    /// `xi-core` does not send a notification for view closure, so the SDK
    /// emits this itself to keep consumers in sync with `Inner::docs`.
    ViewClosed { view_id: ViewId },

    /// An `available_languages` notification.
    ///
    /// Sent once after `client_started`, and again if the set of installed
    /// plugins changes.
    AvailableLanguages { languages: Vec<String> },

    /// An `available_themes` notification.
    ///
    /// Sent once after `client_started`.
    AvailableThemes { themes: Vec<String> },
}
