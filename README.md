# xi-client

**An ergonomic Rust SDK for [`xi-core`](https://github.com/xi-editor/xi-editor)** —
the headless text engine that powers the xi-editor family.

`xi-client` hides the entire JSON-RPC protocol (framing, request/response
correlation, delta-encoded updates, style triplets, `view-id` strings,
plugin callbacks) behind a typed façade. You get a `XiClient` with methods
like `insert`, `move_left`, `find`, `copy`, and a stream of `Event`s on a
`crossbeam_channel::Receiver`. You write the UI; the SDK writes the protocol.

The SDK itself has **no GUI dependencies** — it can be used from Tauri,
egui, winit, a web backend via WASM bridge, or a headless test harness.

---

## Table of contents

- [Why](#why)
- [What is xi-core](#what-is-xi-core)
- [Quick start](#quick-start)
- [Feature overview](#feature-overview)
- [Architecture](#architecture)
- [Public API at a glance](#public-api-at-a-glance)
- [Events](#events)
- [Testing](#testing)
- [Known limitations](#known-limitations)
- [Roadmap](#roadmap)
- [License](#license)

---

## Why

`xi-core` speaks a JSON-RPC variant over stdin/stdout. The wire protocol is
unforgiving:

- Messages are newline-delimited JSON, **not** HTTP-framed.
- The protocol is **bidirectional**: both sides can send requests and
  notifications. The client must answer `measure_width` from the core, or
  word-wrap stalls.
- Text updates arrive as a **delta-encoded op stream** (`copy` / `skip` /
  `invalidate` / `ins` / `update`), not as full document dumps.
- Styles arrive as **delta-encoded triplets** (`[delta, length, style_id]`).
- View ids are strings (`"view-id-1"`), and the namespace for `edit.*` and
  `plugin.*` uses different JSON tags (`method` vs `command`).
- One race condition is built in: `xi-core` may push an `update` for a view
  _before_ the client has registered it, and naive implementations silently
  drop the first screen of content.

Writing all that correctly is a month of work. `xi-client` does it once.

---

## What is xi-core

`xi-core` is a headless text engine. It owns the rope (the document), the
undo history, the selection model, syntax highlighting via plugins, find /
replace state, and word-wrap calculations. It **does not render anything**:
no fonts, no colors on screen, no window. That's the frontend's job.

Communication is one subprocess: `xi-client` spawns `xi-core`, they talk JSON
over pipes, and the frontend draws whatever the client says the document
looks like.

The parent project (`xi-editor`) has reference frontends for macOS, GTK, and
Windows. `xi-client` is intended as the Rust building block for **new**
frontends — in particular, Sublime-like editors where you want full control
over rendering and input handling.

---

## Quick start

```toml
# Cargo.toml
[dependencies]
xi-client = { git = "https://github.com/loc-oss/xi-client", branch = "main" }
```

```rust
use std::path::PathBuf;
use xi_client::{Event, XiClient};

fn main() -> xi_client::Result<()> {
    // 1. Spawn the xi-core subprocess.
    let client = XiClient::spawn("./xi-core")?;

    // 2. Open a view. Blocks until xi-core responds.
    let view_id = client.new_view(Some(PathBuf::from("hello.txt")))?;

    // 3. Drive the document.
    client.insert(view_id, "hello world\n")?;

    // 4. Observe the state.
    let events = client.events();
    loop {
        match events.recv().unwrap() {
            Event::DocumentUpdated { view_id: v } if v == view_id => {
                if let Some(doc) = client.document(view_id) {
                    for (i, line) in doc.lines.iter().enumerate() {
                        println!("{:>3}: {}", i + 1, line.text);
                    }
                }
            }
            Event::Exited => break,
            _ => {}
        }
    }

    // 5. Shut down gracefully.
    client.shutdown();
    client.wait_for_exit(std::time::Duration::from_secs(5))?;
    Ok(())
}
```

That's the whole loop: spawn → new_view → edit → observe → shutdown.

For a real frontend you'll also call `resize`, `scroll`, `request_lines` as
the viewport changes, and `gesture` on mouse events. See
[Feature overview](#feature-overview).

---

## Feature overview

The SDK covers the xi-core protocol on three levels. Completed phases are
usable today; remaining phases are tracked in
[`docs/roadmap.md`](docs/roadmap.md).

### Lifecycle (complete)

`spawn` / `spawn_with_options` / `new_view` / `close_view` / `view_ids` /
`shutdown` / `wait_for_exit` / `save`

`SpawnOptions` forwards `config_dir`, `client_extras_dir`, logging flags,
and `capture_stderr` to the child.

### Configuration (complete)

`get_config` / `modify_user_config` with `ConfigDomain::{General, Syntax, UserOverride}`.
The first `config_changed` per view is the full merged config; subsequent ones
are diffs. The SDK merges diffs into state _before_ emitting the event.

### Editing (complete for basic + movement)

**Text input**: `insert`, `insert_newline`, `insert_tab`, `delete_backward`,
`delete_forward`, `delete_word_backward`, `delete_word_forward`.

**History**: `undo`, `redo`, `transpose`, `duplicate_line`.

**Transforms**: `uppercase`, `lowercase`, `capitalize`, `indent`, `outdent`,
`reindent`.

**Movement**: `move_up` / `down` / `left` / `right`, plus `_and_modify_selection`
variants. `select_all`, `collapse_selections`.

### Viewport (complete)

`resize` (px/points, **not** device pixels), `scroll` (formatted lines,
half-open range), `request_lines` (for large files), and a configurable
width-measure callback (`set_width_measure`) for real word wrap. Default
policy is `CodepointMono` (one column per Unicode scalar).

### Find & replace (complete)

All of: `find`, `multi_find`, `find_next`, `find_previous`, `find_all`,
`highlight_find`, `selection_for_find`, `replace`, `replace_next`,
`replace_all`, `selection_for_replace`, plus `FindQuery`, `FindOptions`, and
`ModifySelection` builder types.

Match counts arrive via `Event::FindStatus`.

### Mouse input (complete)

`gesture(view_id, line, col, GestureType)` where `GestureType` is
`Select { granularity, multi }`, `SelectExtend { granularity }`, or `Drag`.
The SDK emits the **new** wire format; the legacy `point_select` style is
deprecated by xi-core and not supported.

### Clipboard (complete, synchronous)

`cut` and `copy` return `Result<Option<String>>` — `None` when the selection
is empty. These block on a synchronous RPC round-trip.

### Hover (complete)

`request_hover(view_id, request_id, Option<Position>)`. The result arrives
as `Event::HoverShown { request_id, result }`. Requires an LSP plugin to
actually return content; without one, `result` is `None`.

### Plugins (complete)

`plugin_start` / `plugin_stop` / `plugin_rpc`. Plugin lifecycle events
(`PluginStarted`, `PluginStopped`, `PluginCommands`) and status items
(`StatusItemAdded`, `StatusItemUpdated`, `StatusItemRemoved`) are surfaced
as `Event` variants.

---

## Architecture

Three actors, one shared state, one rule about locks.

```
┌─────────────┐  sends RPC     ┌──────────────────┐  JSON       ┌──────────┐
│ Your app    │ ─────────────▶ │ XiClient façade  │ ──────────▶ │ xi-core  │
│ thread      │                │ (lock Inner)     │             │ (child)  │
└─────────────┘                └──────────────────┘             └──────────┘
      │                                ▲                              │
      │  reads DocumentSnapshot        │  writes Inner.docs           │
      │  via client.document()         │                              │
      ▼                                │                              │
┌──────────────────────────────────────┴─────────────────────────────────┐
│  Inner: Arc<Mutex<{ docs, styles, pending_updates, width_measure }>>   │
└──────────────────────────────────────▲─────────────────────────────────┘
                                       │  applies `update` ops
                                       │
                              ┌────────┴────────┐  reads JSON
                              │ Reader thread   │ ◀────────────
                              │ (ClientHandler) │
                              └─────────────────┘
```

**Reader thread** — spawned by `XiClient::spawn`. Owns the stdout pipe.
For each JSON line from `xi-core`:

- **Notification** (`update`, `def_style`, `alert`, …) → mutate `Inner`,
  then send an `Event` on the crossbeam channel.
- **Request** (`measure_width`) → synthesize a response and write it back.
- **EOF** → send `Event::Exited`, signal `wait_for_exit`.

**App thread** — calls `XiClient` methods. Each method either writes a
notification (fire-and-forget) or, for `new_view` / `get_config` / `cut` /
`copy`, performs a synchronous RPC round-trip. `document(view_id)` locks
`Inner`, clones a `DocumentSnapshot`, unlocks.

**The lock rule**: send events _after_ releasing `Inner`. A UI handler that
reacts to `Event::DocumentUpdated` by calling `client.document()` must not
deadlock on the lock held by the reader.

**`XiClient` is `Send` but not `Sync`** — inherited from `xi-rpc`'s
`Box<dyn Peer>`. For multi-threaded access, wrap in `Arc<Mutex<XiClient>>`.

---

## Public API at a glance

```rust
pub struct XiClient { /* opaque */ }

impl XiClient {
    // Lifecycle
    pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self>;
    pub fn spawn_with_options(core_path: impl AsRef<Path>, opts: SpawnOptions) -> Result<Self>;
    pub fn shutdown(&self);
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<std::process::ExitStatus>;

    // Views
    pub fn new_view(&self, path: Option<PathBuf>) -> Result<ViewId>;
    pub fn close_view(&self, view_id: ViewId) -> Result<()>;
    pub fn view_ids(&self) -> Vec<ViewId>;
    pub fn save(&self, view_id: ViewId, path: impl AsRef<Path>) -> Result<()>;

    // State
    pub fn document(&self, view_id: ViewId) -> Option<DocumentSnapshot>;
    pub fn events(&self) -> &crossbeam_channel::Receiver<Event>;

    // ~60 more methods, see rustdoc
}
```

Key types:

| Type                                          | Role                                                          |
| --------------------------------------------- | ------------------------------------------------------------- |
| `ViewId`                                      | A view handle. `Copy`. Wire format `"view-id-N"` is hidden.   |
| `DocumentSnapshot`                            | Immutable view of a document at a moment in time.             |
| `LineState`                                   | One line: text, style spans, cursor offsets.                  |
| `StyleSpan`                                   | Absolute byte range + style id (references `Style`).          |
| `Event`                                       | Everything the core pushes back. `#[non_exhaustive]`.         |
| `Config`                                      | Merged per-view config. `merge(&Value)` applies diffs.        |
| `SpawnOptions`                                | `config_dir`, `client_extras_dir`, logging, `capture_stderr`. |
| `GestureType`                                 | Mouse gesture in the new wire format.                         |
| `FindQuery`, `FindOptions`, `ModifySelection` | Find/replace parameters.                                      |
| `Position`, `Hover`                           | Hover requests and results.                                   |
| `ConfigDomain`                                | `General` / `Syntax(language)` / `UserOverride(view_id)`.     |

---

## Events

Delivered on `client.events()`, a `crossbeam_channel::Receiver<Event>`.
**Single consumer** — `Receiver` is not `Clone` for normal channels.

| Event                                             | Fires when                                 |
| ------------------------------------------------- | ------------------------------------------ |
| `ViewOpened { request_id, view_id }`              | After a successful `new_view`              |
| `ViewClosed { view_id }`                          | Locally, after `close_view`                |
| `DocumentUpdated { view_id }`                     | Every `update` — read via `document()`     |
| `StyleDefined(Style)`                             | Every `def_style`                          |
| `ThemeChanged { name }`                           | `theme_changed` — only name for now        |
| `ConfigChanged { view_id, changes }`              | `config_changed` — merged into state       |
| `LanguageChanged { view_id, language }`           | `language_changed`                         |
| `FindStatus { view_id, matches }`                 | `find_status` — cumulative match count     |
| `ReplaceStatus { view_id, chars, preserve_case }` | `replace_status`                           |
| `ScrollTo { view_id, line, col }`                 | `scroll_to` — viewport scroll request      |
| `Alert(String)`                                   | `alert` — display verbatim                 |
| `AvailableLanguages { languages }`                | After `client_started`                     |
| `AvailableThemes { themes }`                      | After `client_started`                     |
| `AvailablePlugins { view_id, plugins }`           | Per view, after `new_view`                 |
| `PluginStarted` / `PluginStopped`                 | Plugin lifecycle                           |
| `PluginCommands { view_id, plugin, cmds }`        | `update_cmds` from a plugin                |
| `StatusItemAdded` / `Updated` / `Removed`         | Plugin status bar                          |
| `HoverShown { view_id, request_id, result }`      | `show_hover` — response to `request_hover` |
| `Exited`                                          | Reader thread saw EOF — exactly once       |

`Event` is `#[non_exhaustive]`: match arms must include a wildcard.

---

## Testing

The SDK is tested at three layers, and every layer has a specific job.

### 1. Inline unit tests (`#[cfg(test)] mod tests` inside `src/`)

Test **private helpers**: `measure_width`, `parse_view_id`,
`client_started_params`, `apply_ops` invariants, `DocumentState::snapshot`,
`Config::merge` semantics, error conversions, `StyleDef` decoding,
`Operation::validate`.

Run: `cargo test --lib`.

**~60 tests.**

### 2. Integration tests (`tests/`)

Test **public API surface** as a downstream consumer would use it. These
do not spawn `xi-core`.

| File                    | What it covers                                                              |
| ----------------------- | --------------------------------------------------------------------------- |
| `tests/types.rs`        | Wire-format round-trips for every public type                               |
| `tests/config_merge.rs` | `Config::merge` semantics (null clears, absent leaves, malformed keeps old) |
| `tests/document.rs`     | `apply_ops` per op type, `snapshot`, delta decoding, newline stripping      |
| `tests/process.rs`      | `XiProcess` and `SharedStdin` (using `sh` as a fake core)                   |

Run: `cargo test`.

**~66 tests.**

### 3. Lifecycle tests (`tests/lifecycle.rs`) — **gated by `XI_CORE`**

Test **against a real `xi-core` subprocess**. These catch wire-format
divergences that mocks cannot: real `find_status` payloads, real
`plugin_start` JSON tags, real `update` ordering, real `Exited` on EOF.

```bash
# Without XI_CORE — skipped (prints a note, passes)
cargo test --test lifecycle

# With XI_CORE — runs against real core
XI_CORE=/path/to/xi-core cargo test --test lifecycle -- --test-threads=1
```

`--test-threads=1` is recommended: each test spawns its own `xi-core` and
they all write to the same log file (`~/.local/share/xi-core/xi-core.log`).

**~31 tests.**

### Why this split

Unit tests are fast and can be debugged in seconds. Integration tests
catch public API drift. Lifecycle tests are the **only** way to detect
protocol bugs (Phase 0 found one, Phase 4–5 found two more). If you're
modifying anything that touches the wire — add a lifecycle test, not just
a unit test.

### Total

**~160 tests**, ~10 seconds wall-clock with `XI_CORE` set, <1 second
without.

---

## Known limitations

These are deliberate trade-offs, not bugs. Each links to the phase in
[`docs/roadmap.md`](docs/roadmap.md) where it's scheduled.

### API

- **`new_view` is blocking.** A synchronous RPC round-trip on the app
  thread. Acceptable for CLI and small UI, awkward inside a `winit` /
  `egui` event loop. `new_view_async` is planned (Phase 8).
- **`cut` / `copy` are blocking.** Same reason.
- **`Event::ConfigChanged { changes: Config }` drops non-modeled keys.**
  The state gets them (via `Config::merge`), the event payload doesn't.
  If xi-core sends `scroll_past_end: true` or any key not in
  `Config`, a subscriber on the event won't see it. Fix: add `raw: Value`
  alongside `changes` (Phase 7).
- **`Event::FindStatus { matches: u64 }` carries only the count.**
  xi-core already sends `queries[i].lines` (line numbers with matches),
  useful for minimap and "F6" navigation. We drop it. (Phase 7)
- **`Event::ThemeChanged { name: String }` drops `ThemeSettings`.**
  Without colors, a frontend can't actually apply the theme from the
  event. (Phase 7)
- **`request_hover` is smoke-tested, not round-trip tested.** No LSP
  plugin is present in test builds, so `HoverShown` is never observed
  with real content. Add a lifecycle test once an LSP plugin is
  available.

### Rust-specific

- **`XiClient: Send`, not `Sync`.** Inherited from `xi-rpc`'s
  `Box<dyn Peer>`. Use `Arc<Mutex<XiClient>>` if you need shared access
  across threads.
- **`Receiver<Event>` is single-consumer.** Not `Clone`. Multiplex yourself
  if you need fan-out.
- **Edition 2024** — requires Rust 1.85+. Pinned in `Cargo.toml`.

### Performance

- **`document()` clones every line on every call.** For a 10k-line file
  this is ~10k `String` allocations. Fine at 60 fps with small files,
  measurable with large ones. Not optimized yet; benchmark first.
- **`Inner` is a single `Mutex`.** All view reads block behind any writer.
  `DashMap<ViewId, _>` is a possible future optimization.
- **Event channel is unbounded.** A slow consumer will grow the channel
  without bound. Bounded mode with a drop policy is planned (Phase 8).

---

## Roadmap

Six phases are complete (0–5): full transport, all editing primitives,
viewport, config, find/replace, hover, plugins, status items.

Phase 6–9 (movement gaps, full payloads, async, IDE features) are tracked
in [`docs/roadmap.md`](docs/roadmap.md). Each phase is independently
shippable; nothing later depends on anything earlier staying stable.

The detailed wire protocol (what each method sends, what each event parses)
is in [`docs/protocol.md`](docs/protocol.md).

---

## License

Apache-2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE) for details.
