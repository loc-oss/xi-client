# xi-client Protocol Reference

**Status:** Phase 0–5 shipped. This document reflects the SDK as it exists
on `main` after Phase 5 (hover, plugins, status items).
**Covers:** the `xi-client` crate (this SDK) and the raw `xi-core` JSON-RPC it speaks.
**Authoritative for raw wire:** `xi-editor/rust/core-lib/src/rpc.rs`.
**Authoritative for this document:** this file.

This document has two jobs:

1. **Catalog every raw method** that the SDK is allowed to speak — the wire protocol between `xi-client` and `xi-core`.
2. **Describe the SDK façade** — the methods we expose on `XiClient`, the events we emit on the `crossbeam_channel`, and exactly how the façade translates to and from the raw protocol.

Read §2–§3 first to understand the plumbing, then use §6 and §7 as reference.

---

## Table of contents

- [0. Reading guide](#0-reading-guide)
- [1. Architectural overview](#1-architectural-overview)
- [2. Wire protocol (raw `xi-core`)](#2-wire-protocol-raw-xi-core)
- [3. SDK architecture](#3-sdk-architecture)
- [4. Lifecycle](#4-lifecycle)
- [5. Raw ↔ SDK mapping](#5-raw--sdk-mapping)
- [6. SDK API reference](#6-sdk-api-reference)
- [7. Events reference](#7-events-reference)
- [8. Data model](#8-data-model)
- [9. What the SDK handles automatically](#9-what-the-sdk-handles-automatically)
- [10. Known gaps and design notes](#10-known-gaps-and-design-notes)
- [11. Appendix: raw method catalog](#11-appendix-raw-method-catalog)

---

## 0. Reading guide

The SDK is deliberately thin. For every `XiClient` method there is **one** raw method it calls (or a small fixed sequence). If you can't find a method here, it either isn't exposed yet (§10) or it's not part of the protocol at all.

We use four callouts throughout:

> **Gotcha:** something that will bite you if you're not careful.

> **Raw:** the exact wire-level method(s) used under the hood.

> **Auto:** something the SDK does _for_ you, so you don't have to.

> **Not yet:** explicitly not implemented, with a pointer to §10.

---

## 1. Architectural overview

```
┌──────────────────────────────────────────────────────────────────────────┐
│  your application                                                        │
│                                                                          │
│   XiClient (methods)                            Receiver<Event>          │
│      │                                              ▲                    │
└──────┼──────────────────────────────────────────────┼────────────────────┘
       │                                              │
       ▼                                              │
┌──────────────────────────────┐    ┌────────────────┴───────────────────┐
│ XiClient façade (client.rs)  │    │ events.rs (Event enum)             │
│ - Arc<Mutex<Inner>>          │    │ crossbeam_channel::Sender<Event>   │
│ - SharedStdin (closable)     │    └────────────────▲───────────────────┘
│ - reader JoinHandle state    │                     │
└──────┬────────────────┬──────┘                     │
       │                │                            │
       │ write          │ read (reader thread)       │
       ▼                ▼                            │
┌──────────────┐  ┌──────────────────────────────────┴────────────────────┐
│ SharedStdin  │  │ RpcLoop from xi-rpc (handler = ClientHandler)         │
│ (Arc<Mutex>) │  │ - reads JSON lines from ChildStdout                   │
│              │  │ - dispatches:                                         │
│              │  │   • response  → pending table (owned by xi-rpc)       │
│              │  │   • notification → Inner update → Event               │
│              │  │   • request (measure_width) → auto reply              │
└──────┬───────┘  └───────────────────────────────────────────────────────┘
       │
       ▼
  ChildStdin
       │
┌──────┴───────────────────────────────────────────────────────────────────┐
│  xi-core subprocess                                                      │
│   stdin ◀── newline-delimited JSON ──▶ stdout                            │
└──────────────────────────────────────────────────────────────────────────┘
```

The SDK sits between your UI and the `xi-core` subprocess. It exposes two things:

- **Methods** on `XiClient` that translate into raw JSON-RPC.
- **Events** on a `crossbeam_channel::Receiver<Event>` for everything `xi-core` pushes back.

The subprocess is started and supervised by the SDK. You never touch `ChildStdin` / `ChildStdout`.

---

## 2. Wire protocol (raw `xi-core`)

This section is a summary of the raw wire. It exists so that §5–§11 make sense; the _authoritative_ reference is `core-lib/src/rpc.rs`. If you need to check a wire detail not covered here, go there.

### 2.1 Transport and framing

- `xi-core` is a **stdin/stdout subprocess**. The SDK is the parent.
- Messages are **UTF-8 JSON**, **one JSON value per line**, separated by `\n`.
- No HTTP, no `Content-Length`, no batches.

### 2.2 Message shapes

There are three message shapes on the wire:

| Shape            | Has `id`? | Has `method`? | Has `result`/`error`? | Meaning             |
| ---------------- | --------- | ------------- | --------------------- | ------------------- |
| **Request**      | yes       | yes           | no                    | expects a response  |
| **Notification** | no        | yes           | no                    | fire-and-forget     |
| **Response**     | yes       | no            | yes (one of)          | answer to a Request |

Concretely:

```json
// request
{"id": 7, "method": "new_view", "params": {"file_path": "/tmp/a.txt"}}

// notification (edit namespace)
{"method": "edit", "params": {"view_id": "view-id-1", "method": "insert", "params": {"chars": "x"}}}

// notification (plugin namespace)
{"method": "plugin", "params": {"command": "start", "view_id": "view-id-1", "plugin_name": "syntect"}}

// success response
{"id": 7, "result": "view-id-1"}

// error response
{"id": 7, "error": {"code": 5, "message": "I/O error"}}
```

> **Gotcha:** the two namespaced methods use **different** inner tags:
>
> - `edit` uses `"method": "<inner>"` — e.g. `{"method": "edit", "params": {"method": "insert", ...}}`.
> - `plugin` uses `"command": "<inner>"` — e.g. `{"method": "plugin", "params": {"command": "start", ...}}`.
>
> This is inconsistent in `xi-core` but both are authoritative. Sending the wrong tag for `plugin` will cause `xi-core` to exit with `UnknownRequest("missing field command")`. The SDK does this correctly; see §6.7.

### 2.3 Correlation and IDs

- IDs are **per-direction, monotonically increasing integers**, starting from 0. The SDK does not care about the exact starting value; `xi-rpc`'s `RpcPeer` allocates them.
- The **SDK** (via `xi-rpc`) allocates IDs for its outbound requests.
- `xi-core` allocates its own IDs for its outbound requests (`measure_width` only, see §9.1). The SDK echoes the received `id` verbatim.
- IDs are never reused.

### 2.4 Protocol quirks

1. **No `"jsonrpc": "2.0"` field.** Sending one is not an error but it's not part of the protocol.
2. **Bidirectional peer.** Both sides can send Requests and Notifications. The SDK must handle core-initiated requests, not just responses. The only such request in the current build is `measure_width` (§9.1).
3. **No batch.**
4. **No version negotiation.** The protocol is tied to the `xi-core` build.
5. **`view_id` is a string** of the form `"view-id-<n>"` (`n ≥ 1`), not a number.
6. **`edit`'s inner method has its `view_id` at the outer `params` level** (§6.2).
7. **`plugin`'s inner command has its `view_id` in the same params object** (not nested under a second `params`).
8. **`styles` in `Line` are delta-encoded triplets** (§8.4).
9. **`copy`/`update` ops interact with `old_ix`** (§8.3).
10. **`scroll` uses half-open `[first, last)` in _formatted_ (soft-wrapped) lines**, not logical lines.
11. **`resize` uses px/points**, not device pixels.
12. **`find_status` carries `queries[i].matches`, not a top-level `matches`.** See §6.2.6.

> **Gotcha:** the docs in `xi-editor/api-docs/` disagree with the source in a couple of places (`set_language` uses `view-id` vs `view_id`; `find_status` shape; the `plugin` namespace's tag). The SDK always follows **source**, not the inherited docs.

### 2.5 Errors

Responses that fail carry:

```json
{"id": N, "error": {"code": C, "message": "...", "data": ...}}
```

Known codes:

| Code     | Origin                        | Meaning                                            |
| -------- | ----------------------------- | -------------------------------------------------- |
| `-32600` | JSON-RPC reserved             | invalid request (couldn't deserialize the request) |
| `404`    | SDK                           | `view_id` not found                                |
| `0`      | `RemoteError::custom(0, ...)` | generic (e.g. `WeakXiCore` missing)                |
| `5`      | `FileError::Io`               | open / read file failed                            |
| `6`      | `FileError::UnknownEncoding`  | file isn't UTF-8                                   |
| `7`      | `FileError::HasChanged`       | file modified on disk since we opened it           |

Malformed JSON: `xi-core` tears down the connection. The SDK treats EOF as a clean exit (see §4.4).

---

## 3. SDK architecture

### 3.1 Components

| File                                          | Responsibility                                                                                                                                                                                                                                                                     |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/process.rs` (`XiProcess`, `SharedStdin`) | Spawn/kill the `xi-core` subprocess. Owns the `Child`. `take_stdin()` / `take_stdout()` hand out stdio exactly once. `SharedStdin` wraps `ChildStdin` in `Arc<Mutex<Option<...>>>` so both `RpcLoop` and `XiClient::shutdown` can hold a clone; closing it sends EOF to `xi-core`. |
| `src/client.rs` (`XiClient`, `Inner`)         | The façade. Holds `Arc<Mutex<Inner>>`, `Receiver<Event>`, `Sender<Event>`, `SharedStdin`, reader-exit state, exit status cache. `Inner` holds the docs, styles, buffered `pending_updates`, `next_request_id`, `width_measure` callback.                                           |
| `src/client/handler.rs` (`ClientHandler`)     | The protocol layer. Implements `xi_rpc::Handler<Notification=Value, Request=Value>`. Parses inbound notifications into `Event`s and mutates `Inner`. Dispatches `measure_width` requests.                                                                                          |
| `src/document.rs`                             | `DocumentState` (mutable per-view) and `DocumentSnapshot` (what the UI reads). Contains `apply_ops` (the ops interpreter).                                                                                                                                                         |
| `src/events.rs`                               | The `Event` enum. The entire surface the UI observes.                                                                                                                                                                                                                              |
| `src/types/`                                  | Serde types that mirror the raw wire (`Line`, `Operation`, `Update`, `Config`, `Style`, `ViewId`, `FindQuery`, `FindOptions`, `GestureType`, `Hover`, `Position`, `SpawnOptions`, `ConfigDomain`, …).                                                                              |

Wire transport is provided by the **`xi-rpc`** crate (`RpcLoop`, `RawPeer`, `Peer`, `Handler`). The SDK does not implement its own JSON line reader/writer.

### 3.2 Threading model

Three actors:

1. **Application thread** — your code. Calls `XiClient` methods. Each method either writes a notification through the shared `RpcPeer`, or performs a synchronous `send_rpc_request` (`new_view`, `get_config`, `cut`, `copy`). Reads state with `document(view_id)`, which briefly locks `Inner`.

2. **Reader thread** — spawned by `XiClient::spawn` and named `"xi-client-reader"`. Owns `RpcLoop`. For each message:
   - **Response**: `xi-rpc` matches the `id` in its own pending table and wakes the blocked caller.
   - **Notification**: `ClientHandler::handle_notification` updates `Inner` (e.g. applies an `update` to `docs`), then sends the corresponding `Event`.
   - **Request** (`measure_width`): `ClientHandler::handle_request` synthesizes the response and returns it to `xi-rpc`, which writes it back.
   - **EOF**: sends `Event::Exited`, signals `wait_for_exit`'s condvar, exits.

3. **`xi-core` subprocess** — its stdin/stdout are held by `SharedStdin` (via `RpcLoop`) and `RpcLoop`'s reader (via `ChildStdout`).

There is no dedicated writer thread. The application thread writes directly through `RpcPeer` (which `xi-rpc` internally serializes behind a `Mutex<W>`).

```
app thread:    [lock xi-rpc writer] → write request → [unlock] → wait on event_rx
reader thread: receives notification → [lock Inner] → apply update → [unlock]
               → send Event → loop
```

### 3.3 Locking and state ownership

`Inner` is the single source of truth. It holds:

- `docs: HashMap<ViewId, DocumentState>`
- `styles: HashMap<u64, Style>`
- `pending_updates: HashMap<ViewId, Vec<Update>>` — `update` notifications that arrived before their view was registered (see §4.1).
- `next_request_id: u64` — reserved for a future `new_view_async`; currently used as the logical id in `Event::ViewOpened`.
- `width_measure: Option<Arc<WidthMeasureFn>>` — user-installed callback for `measure_width`.

The reader thread and application thread both `lock()` `Inner`. Neither holds the lock across a blocking operation. Concretely:

- `insert(view_id, "x")` → `send_rpc_notification("edit", ...)`; no `Inner` lock is held across the write.
- Reader sees an `update` notification → lock, `DocumentState::apply(update)`, unlock, `event_tx.send(Event::DocumentUpdated{...})`.
- `document(view_id)` → lock, clone `DocumentState::snapshot(view_id)`, unlock.

> **Gotcha:** `event_tx.send(...)` is done **after unlocking**. That guarantees no deadlock if the UI's event handler calls back into `XiClient::document`, which would try to re-lock `Inner`.

> **Gotcha:** `Inner::pending_updates` is written by the reader thread and drained by `new_view` (which runs on the application thread). Both hold the lock when touching it, so this is safe.

### 3.4 Event delivery

- `XiClient::events(&self) -> &crossbeam_channel::Receiver<Event>`. The `Receiver` lives on `XiClient` directly, not inside `Inner`, so it can be borrowed.
- **Single consumer.** `crossbeam`'s `Receiver` is not `Clone` for normal channels. If you need multiple observers, wrap the `Receiver` in your own multiplexer.
- The channel is **unbounded** (see §10 for the tradeoff).
- Sends from the reader thread never block on the UI.

> **Gotcha:** `Event` is `#[non_exhaustive]`. Downstream `match` arms must include a wildcard branch.

---

## 4. Lifecycle

### 4.1 `XiClient::spawn`

```rust
pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self>;
pub fn spawn_with_options(core_path: impl AsRef<Path>, opts: SpawnOptions) -> Result<Self>;
```

`SpawnOptions`:

```rust
#[derive(Default)]
pub struct SpawnOptions {
    /// Forwarded in `client_started.config_dir`. Enables file-based config in xi-core.
    pub config_dir: Option<PathBuf>,
    /// Forwarded in `client_started.client_extras_dir`.
    pub client_extras_dir: Option<PathBuf>,
    /// Value for the `XI_LOG` env var passed to the child.
    pub log_level: Option<String>,
    /// `-log-dir` flag passed to the child.
    pub log_dir: Option<PathBuf>,
    /// `-log-file` flag passed to the child.
    pub log_file: Option<PathBuf>,
    /// If `true`, pipe stderr and drain it in a background thread instead of
    /// inheriting the parent's stderr. Bytes are discarded; use `log_dir` /
    /// `log_file` to inspect.
    pub capture_stderr: bool,
}
```

Steps performed by `spawn_with_options`:

1. `XiProcess::spawn_with_options(core_path, &opts)` — starts the child with `stdin`/`stdout` piped, applies env/args from `opts`, and optionally drains stderr in a background thread.
2. Wrap `ChildStdin` in `SharedStdin`. Keep one clone on `XiClient`; give another clone to `RpcLoop`.
3. Create `crossbeam_channel::unbounded::<Event>()`. Keep the `Receiver` on `XiClient`; pass a clone of the `Sender` into the reader thread.
4. Create `RpcLoop::new(shared_stdin_clone)`, obtain `RpcPeer` via `get_raw_peer().box_clone()`.
5. Send `client_started` notification (see §4.2) synchronously.
6. Spawn the reader thread named `"xi-client-reader"`. The thread owns `RpcLoop` and `ChildStdout`.
7. Return `XiClient`.

**Buffering the race:** between step 5 (`new_view`'s response on the wire) and the app thread's `docs.insert(view_id, ...)`, `xi-core` may already be pushing `update` notifications for the new view. `ClientHandler::on_update` checks `docs.contains_key(view_id)`; if missing, it pushes into `pending_updates[view_id]`. The next `XiClient::new_view` drains that buffer. Without this, the first screen of content would be silently dropped.

> **Gotcha:** `client_started` must be the _first_ message. `XiCore::inner()` panics if it receives anything else (except `tracing_config`) before `client_started`. The SDK always sends it inside `spawn`, so you can't forget.

> **Gotcha:** the child's stderr is inherited by default. `xi-core`'s log lines go to the parent's stderr — useful for debugging, noisy in production. Use `SpawnOptions::capture_stderr = true` to pipe and discard, or set `-log-dir` / `-log-file` to redirect inside the child.

### 4.2 Handshake: `client_started`

```json
{ "method": "client_started", "params": {} }
```

With `SpawnOptions::config_dir` and/or `client_extras_dir`:

```json
{
  "method": "client_started",
  "params": { "config_dir": "/path/to/config", "client_extras_dir": "/path/to/extras" }
}
```

Effects on `xi-core`:

- Loads `preferences.xiconfig` / `<lang>.xiconfig` from `config_dir` **if and only if** `config_dir` is provided. Once enabled, RPC cannot modify `general` or `syntax` config domains.
- Scans `config_dir/plugins/*/manifest.toml` for plugins.
- Emits `available_languages` and `available_themes` (see §7.10, §7.11).
- Starts plugin processes.

The SDK forwards `available_languages` as `Event::AvailableLanguages` and `available_themes` as `Event::AvailableThemes`.

### 4.3 Graceful shutdown

```rust
pub fn shutdown(&self);
```

Behaviour:

1. `self.stdin.close()` — `SharedStdin` drops the inner `ChildStdin`, sending EOF to `xi-core`.
2. `xi-core` sees EOF, exits.
3. The reader thread sees EOF on stdout, sends `Event::Exited`, signals the `wait_for_exit` condvar, and exits.
4. To _observe_ the exit (and reap the child), call `wait_for_exit(timeout)` (see §6.1).

After `shutdown()`, any subsequent `XiClient` method that writes returns `Error::CoreNotRunning`. `document()` / `events()` keep working until the `XiClient` is dropped.

```rust
client.shutdown();
let status = client.wait_for_exit(Duration::from_secs(5))?;
```

> **Gotcha:** `shutdown()` is non-blocking; it only closes stdin. If you want a synchronous wait, use `wait_for_exit`. Without `wait_for_exit`, the subprocess is reaped by `XiProcess::Drop` when `XiClient` is dropped.

### 4.4 Unexpected exit

Three ways the child can go away:

| Situation                                 | Reader thread sees      | Event emitted |
| ----------------------------------------- | ----------------------- | ------------- |
| Normal `shutdown()`                       | EOF on stdout           | `Exited`      |
| `xi-core` crashes / is killed             | EOF on stdout           | `Exited`      |
| Protocol violation by us (malformed JSON) | `xi-core` closes stdout | `Exited`      |

In all three cases the SDK emits **exactly one** `Event::Exited` and the reader thread stops. Any in-flight requests that `xi-rpc` was blocking on are woken with `Error::PeerDisconnect`, which `lib.rs`'s `From<xi_rpc::Error>` maps to `Error::CoreNotRunning`.

The subprocess is reaped by `XiProcess::Drop`, or explicitly by `wait_for_exit`.

---

## 5. Raw ↔ SDK mapping

This is the master table. Details for each SDK method are in §6; details for each event are in §7.

**Legend:** `[done]` shipped · `[planned]` not yet implemented (see §10).

### 5.1 SDK → core (outbound)

| SDK method                                                        | Raw method                                | Type  | Status      |
| ----------------------------------------------------------------- | ----------------------------------------- | ----- | ----------- |
| `spawn` / `spawn_with_options`                                    | `client_started`                          | notif | `[done]`    |
| `new_view`                                                        | `new_view`                                | req   | `[done]`    |
| `new_view_async`                                                  | `new_view`                                | req   | `[planned]` |
| `close_view`                                                      | `close_view`                              | notif | `[done]`    |
| `save`                                                            | `save`                                    | notif | `[done]`    |
| `insert`                                                          | `edit.insert`                             | notif | `[done]`    |
| `insert_newline` / `insert_tab`                                   | `edit.*`                                  | notif | `[done]`    |
| `delete_backward` / `delete_forward`                              | `edit.*`                                  | notif | `[done]`    |
| `delete_word_backward` / `delete_word_forward`                    | `edit.*`                                  | notif | `[done]`    |
| `undo` / `redo`                                                   | `edit.*`                                  | notif | `[done]`    |
| `transpose` / `duplicate_line`                                    | `edit.*`                                  | notif | `[done]`    |
| `uppercase` / `lowercase` / `capitalize`                          | `edit.*`                                  | notif | `[done]`    |
| `indent` / `outdent` / `reindent`                                 | `edit.*`                                  | notif | `[done]`    |
| `move_up` / `down` / `left` / `right`                             | `edit.*`                                  | notif | `[done]`    |
| `move_*_and_modify_selection` (4)                                 | `edit.*`                                  | notif | `[done]`    |
| `move_word_*`, `move_to_*`, `scroll_page_*` (+ variants)          | `edit.*`                                  | notif | `[planned]` |
| `select_all` / `collapse_selections`                              | `edit.*`                                  | notif | `[done]`    |
| `add_selection_above` / `below`, `selection_into_lines`           | `edit.*`                                  | notif | `[planned]` |
| `paste`                                                           | `edit.paste`                              | notif | `[planned]` |
| `goto_line`                                                       | `edit.goto_line`                          | notif | `[planned]` |
| `delete_to_end_of_paragraph`, `delete_to_beginning_of_line`       | `edit.*`                                  | notif | `[planned]` |
| `increase_number` / `decrease_number`                             | `edit.*`                                  | notif | `[planned]` |
| `scroll`                                                          | `edit.scroll [first,last]`                | notif | `[done]`    |
| `resize`                                                          | `edit.resize {width,height}`              | notif | `[done]`    |
| `request_lines`                                                   | `edit.request_lines [first,last]`         | notif | `[done]`    |
| `gesture`                                                         | `edit.gesture`                            | notif | `[done]`    |
| `find`, `multi_find`, `find_next`, `find_previous`, `find_all`    | `edit.*`                                  | notif | `[done]`    |
| `highlight_find`, `selection_for_find`                            | `edit.*`                                  | notif | `[done]`    |
| `replace`, `replace_next`, `replace_all`, `selection_for_replace` | `edit.*`                                  | notif | `[done]`    |
| `request_hover`                                                   | `edit.request_hover`                      | notif | `[done]`    |
| `get_config`                                                      | `get_config`                              | req   | `[done]`    |
| `cut` / `copy`                                                    | `edit.cut` / `edit.copy`                  | req   | `[done]`    |
| `debug_get_contents`                                              | `debug_get_contents`                      | req   | `[planned]` |
| `set_theme`                                                       | `set_theme`                               | notif | `[done]`    |
| `set_language`                                                    | `set_language`                            | notif | `[done]`    |
| `modify_user_config`                                              | `modify_user_config`                      | notif | `[done]`    |
| `plugin_start`                                                    | `plugin` + `{command: "start", ...}`      | notif | `[done]`    |
| `plugin_stop`                                                     | `plugin` + `{command: "stop", ...}`       | notif | `[done]`    |
| `plugin_rpc`                                                      | `plugin` + `{command: "plugin_rpc", ...}` | notif | `[done]`    |

### 5.2 Core → SDK (inbound)

| Raw notification                     | SDK handler                                | Event                      | Status      |
| ------------------------------------ | ------------------------------------------ | -------------------------- | ----------- |
| `update`                             | apply ops (or buffer in `pending_updates`) | `DocumentUpdated`          | `[done]`    |
| `scroll_to`                          | —                                          | `ScrollTo`                 | `[done]`    |
| `def_style`                          | insert into `styles` map                   | `StyleDefined`             | `[done]`    |
| `theme_changed`                      | —                                          | `ThemeChanged` (name only) | `[done]`    |
| `config_changed`                     | merge into `DocumentState::config`         | `ConfigChanged`            | `[done]`    |
| `language_changed`                   | set `DocumentState::language`              | `LanguageChanged`          | `[done]`    |
| `available_languages`                | —                                          | `AvailableLanguages`       | `[done]`    |
| `available_themes`                   | —                                          | `AvailableThemes`          | `[done]`    |
| `find_status`                        | sum `queries[i].matches`                   | `FindStatus`               | `[done]`    |
| `replace_status`                     | parse `status.{chars,preserve_case}`       | `ReplaceStatus`            | `[done]`    |
| `show_hover`                         | correlate with `request_hover`             | `HoverShown`               | `[done]`    |
| `alert`                              | —                                          | `Alert`                    | `[done]`    |
| `available_plugins`                  | —                                          | `AvailablePlugins`         | `[done]`    |
| `plugin_started`                     | —                                          | `PluginStarted`            | `[done]`    |
| `plugin_stopped`                     | —                                          | `PluginStopped`            | `[done]`    |
| `update_cmds`                        | —                                          | `PluginCommands`           | `[done]`    |
| `add_status_item`                    | —                                          | `StatusItemAdded`          | `[done]`    |
| `update_status_item`                 | —                                          | `StatusItemUpdated`        | `[done]`    |
| `remove_status_item`                 | —                                          | `StatusItemRemoved`        | `[done]`    |
| _(response to `new_view`)_           | create `DocumentState`                     | `ViewOpened`               | `[done]`    |
| _(response to `get_config`)_         | —                                          | `ConfigResponse`           | `[planned]` |
| _(response to `cut`/`copy`)_         | —                                          | `CutResult` / `CopyResult` | `[planned]` |
| _(response to `debug_get_contents`)_ | —                                          | `DocumentContents`         | `[planned]` |
| _(request `measure_width`)_          | **auto-reply**                             | _(none, see §9.1)_         | `[done]`    |
| _(EOF on stdout)_                    | —                                          | `Exited`                   | `[done]`    |

---

## 6. SDK API reference

Every method below is called on `&XiClient`. `ViewId` is a copyable `usize`-backed newtype (§8.1). Methods that only write to `xi-core` return `Result<()>` — `Ok(())` means the bytes were flushed into the child's stdin, **not** that `xi-core` has processed them. Confirm through events.

### 6.0 Return-value conventions

| Return type                                 | Meaning                                                                                   |
| ------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `Result<()>`                                | the notification was written to `xi-core`'s stdin.                                        |
| `Result<T>` where `T: Copy` (e.g. `ViewId`) | a synchronous request/response completed.                                                 |
| `Result<Option<String>>`                    | a synchronous request/response that can produce `None` (e.g. `cut` with empty selection). |
| `Result<u64>` (future `new_view_async`)     | the ID of an outstanding request. The result will arrive as an `Event`.                   |

### 6.1 Lifecycle

#### `spawn` / `spawn_with_options`

- Raw: `client_started`.
- Returns once the subprocess is running and `client_started` has been written.
- Errors: `Error::Spawn` if the process can't be started; `Error::Io` if writing `client_started` fails.

#### `shutdown`

- Raw: none (closes stdin).
- See §4.3.
- After this call, all subsequent write attempts return `Error::CoreNotRunning`.

#### `wait_for_exit`

```rust
pub fn wait_for_exit(&self, timeout: Duration) -> Result<std::process::ExitStatus>;
```

- Waits for the reader thread to observe EOF, then reaps the child and caches the exit status.
- Idempotent: subsequent calls return the cached status.
- Returns `Err(Error::Rpc("timeout ..."))` if the child hasn't exited within `timeout`.
- Does **not** initiate shutdown. Call `shutdown()` first if you want the child to exit.

#### `events`

```rust
pub fn events(&self) -> &crossbeam_channel::Receiver<Event>;
```

- Borrow, not owned. Single consumer.

### 6.2 Namespaced `edit` calls

All editing/movement/selection/viewport/gesture/find calls share this wire shape:

```json
{"method": "edit", "params": {"view_id": "view-id-N", "method": "<inner>", "params": <inner_params>}}
```

Inner params that are "empty" are sent as `[]` (an empty array). `xi-core`'s `EditCommand` deserializer accepts `params` omitted, `{}`, or `[]` interchangeably.

In the tables below, "inner" is the `method` field inside `edit`'s params.

#### 6.2.1 Edits

**Shipped:**

| SDK method                      | inner                  | inner params       | Notes                   |
| ------------------------------- | ---------------------- | ------------------ | ----------------------- |
| `insert(view_id, chars)`        | `insert`               | `{"chars": chars}` | inserts at every cursor |
| `delete_backward(view_id)`      | `delete_backward`      | `[]`               |                         |
| `delete_forward(view_id)`       | `delete_forward`       | `[]`               |                         |
| `delete_word_backward(view_id)` | `delete_word_backward` | `[]`               |                         |
| `delete_word_forward(view_id)`  | `delete_word_forward`  | `[]`               |                         |
| `insert_newline(view_id)`       | `insert_newline`       | `[]`               |                         |
| `insert_tab(view_id)`           | `insert_tab`           | `[]`               |                         |
| `undo(view_id)`                 | `undo`                 | `[]`               |                         |
| `redo(view_id)`                 | `redo`                 | `[]`               |                         |
| `transpose(view_id)`            | `transpose`            | `[]`               |                         |
| `duplicate_line(view_id)`       | `duplicate_line`       | `[]`               |                         |

**Not yet** (Phase 6):

| SDK method                               | inner                         | Why deferred                                                                                                                                       |
| ---------------------------------------- | ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `paste(view_id, chars)`                  | `paste`                       | multi-cursor semantics: one line per cursor if `lines.len() == cursors.len()`, else full string at every cursor. Needs a dedicated lifecycle test. |
| `goto_line(view_id, line)`               | `goto_line {line}`            | Triggers `scroll_to`; separate event semantics.                                                                                                    |
| `delete_to_end_of_paragraph`             | `delete_to_end_of_paragraph`  |                                                                                                                                                    |
| `delete_to_beginning_of_line`            | `delete_to_beginning_of_line` |                                                                                                                                                    |
| `increase_number` / `decrease_number`    | same                          |                                                                                                                                                    |
| `uppercase` / `lowercase` / `capitalize` | same                          | `[done]` actually shipped, just listed here for completeness in §6.2.3                                                                             |

#### 6.2.2 Movement / selection

**Shipped:**

- `move_up`, `move_down`, `move_left`, `move_right`
- `move_up_and_modify_selection`, `move_down_and_modify_selection`,
  `move_left_and_modify_selection`, `move_right_and_modify_selection`
- `select_all`, `collapse_selections`

All take `[]` as params.

**Not yet** (Phase 6):

- `move_word_left` / `move_word_right` (+ `_and_modify_selection`)
- `move_to_left_end_of_line` / `move_to_right_end_of_line` (+ `_and_modify_selection`)
- `move_to_beginning_of_document` / `move_to_end_of_document` (+ `_and_modify_selection`)
- `move_to_beginning_of_paragraph` / `move_to_end_of_paragraph` (+ `_and_modify_selection`)
- `scroll_page_up` / `scroll_page_down` (+ `_and_modify_selection`)
- `add_selection_above` / `add_selection_below` / `selection_into_lines`

These are all one-liners of the form `self.send_edit(view_id, "<inner>", json!([]))`. See §10.

#### 6.2.3 Text transforms

| SDK method            | inner        | inner params |
| --------------------- | ------------ | ------------ |
| `uppercase(view_id)`  | `uppercase`  | `[]`         |
| `lowercase(view_id)`  | `lowercase`  | `[]`         |
| `capitalize(view_id)` | `capitalize` | `[]`         |
| `indent(view_id)`     | `indent`     | `[]`         |
| `outdent(view_id)`    | `outdent`    | `[]`         |
| `reindent(view_id)`   | `reindent`   | `[]`         |

> `reindent` is delegated to the syntect plugin and is a no-op if the plugin isn't running. There is no success/failure event.

#### 6.2.4 Viewport

| SDK method                            | inner           | inner params                         | Notes                      |
| ------------------------------------- | --------------- | ------------------------------------ | -------------------------- |
| `scroll(view_id, first, last)`        | `scroll`        | `[first, last]`                      | formatted lines, half-open |
| `resize(view_id, width, height)`      | `resize`        | `{"width": width, "height": height}` | px/points                  |
| `request_lines(view_id, first, last)` | `request_lines` | `[first, last]`                      |                            |

- `scroll` uses **formatted** (soft-wrapped) lines, half-open `[first, last)`. Report it every time the viewport changes.
- `resize` takes **px/points**, not device pixels.
- `request_lines` asks xi-core to make lines outside the viewport valid, sending `update` for any that are currently invalid. Call after `scroll` when entering unloaded territory.
- **Not yet:** `goto_line`. See §10.

#### 6.2.5 Gestures

```rust
pub enum GestureType {
    Select { granularity: Granularity, multi: bool },
    SelectExtend { granularity: Granularity },
    Drag,
}
pub enum Granularity { Point, Word, Line }

pub fn gesture(&self, view_id: ViewId, line: u64, col: u64, ty: GestureType) -> Result<()>;
```

Raw: `edit.gesture {line, col, ty}` where `ty` serializes as:

- `{"select": {"granularity": "point"|"word"|"line", "multi": bool}}`
- `{"select_extend": {"granularity": "..."}}`
- `"drag"`

> **Gotcha:** the legacy gesture format (`"point_select"`, `"toggle_sel"`, etc.) is still accepted by `xi-core` for backward compatibility but is deprecated. The SDK **only** emits the new format.

#### 6.2.6 Find / Replace

```rust
pub struct FindQuery {
    pub id: Option<u64>,           // None → new query
    pub chars: String,
    pub case_sensitive: bool,
    pub regex: bool,               // default: false
    pub whole_words: bool,         // default: false
}

pub struct FindOptions {
    pub wrap_around: bool,         // default: false
    pub allow_same: bool,          // default: false
    pub modify_selection: ModifySelection, // default: Set
}
pub enum ModifySelection { None, Set, Add, AddRemovingCurrent }
```

| SDK method                                    | inner                   | inner params                                     |
| --------------------------------------------- | ----------------------- | ------------------------------------------------ |
| `find(view_id, q: FindQuery)`                 | `find`                  | `{chars, case_sensitive, regex?, whole_words?}`  |
| `multi_find(view_id, qs: &[FindQuery])`       | `multi_find`            | `{queries: [...]}`                               |
| `find_next(view_id, opts)`                    | `find_next`             | `{wrap_around?, allow_same?, modify_selection?}` |
| `find_previous(view_id, opts)`                | `find_previous`         | same                                             |
| `find_all(view_id)`                           | `find_all`              | `[]`                                             |
| `highlight_find(view_id, visible)`            | `highlight_find`        | `{"visible": visible}`                           |
| `selection_for_find(view_id, case_sensitive)` | `selection_for_find`    | `{"case_sensitive": bool}`                       |
| `replace(view_id, chars)`                     | `replace`               | `{"chars": chars}`                               |
| `replace_next(view_id)`                       | `replace_next`          | `[]`                                             |
| `replace_all(view_id)`                        | `replace_all`           | `[]`                                             |
| `selection_for_replace(view_id)`              | `selection_for_replace` | `[]`                                             |

Results arrive asynchronously:

- `Event::FindStatus { view_id, matches }` fires as matches are found. **Important:** on the wire, `matches` is per-query inside a `queries` array:

  ```json
  {"view_id": "view-id-1", "queries": [{"matches": 3, "id": 1, ...}, ...]}
  ```

  The SDK **sums** `queries[i].matches` into a single `matches` value for the event. Per-query details (chars, case_sensitivity, per-match line numbers) are currently dropped (see §10).

- `Event::ReplaceStatus { view_id, chars, preserve_case }` fires when the replacement string changes.

> **Gotcha:** `find` runs incrementally from the visible range. If you call `find` right after `new_view` without `resize`/`scroll`, `xi-core` may report `matches: 0` and never find anything — the viewport has no size. Always `resize` + `scroll` before searching.

> **Gotcha:** `replace_all` uses the currently-set replacement string. Call `replace(view_id, chars)` before `replace_all`, or nothing gets replaced.

#### 6.2.7 Hover

```rust
pub struct Position { pub line: u64, pub col: u64 }

pub fn request_hover(
    &self,
    view_id: ViewId,
    request_id: u64,
    position: Option<Position>,
) -> Result<()>;
```

Raw: `edit.request_hover {request_id, position?}`, where `position` serializes as `{"line": N, "col": M}`.

`request_id` is an **opaque** id chosen by the caller. It is echoed in `Event::HoverShown { view_id, request_id, result }`. It is _not_ the RPC `id` and is not consumed by the SDK.

`position` columns are **UTF-8 byte offsets within the line**, matching `cursor` and `Line.styles` (§8.4, §8.5). Omit `position` to use the current cursor.

> **Gotcha:** column units. `request_hover` and (the future) `goto_line` accept columns as UTF-8 byte offsets. If your UI tracks UTF-16 or grapheme indices, you must convert.

> **Gotcha:** Without an LSP plugin, `xi-core` may emit `HoverShown { result: None }` or nothing at all. The SDK treats both as valid. Phase 5 lifecycle test is a smoke test only; a real round-trip requires an LSP plugin.

### 6.3 View lifecycle

#### `new_view`

```rust
pub fn new_view(&self, path: Option<PathBuf>) -> Result<ViewId>;
```

Raw: `new_view {file_path?}` request.

- **Blocking** synchronous RPC round-trip.
- Emits `Event::ViewOpened { request_id, view_id }` (where `request_id` is the SDK-allocated `next_request_id` from `Inner`).
- Also emits a `DocumentUpdated` if updates were buffered in `pending_updates` (see §4.1).
- On open error: `Error::Remote(RemoteError::Custom { code, .. })` (mapped from `code = 5/6/7`).

> **Not yet:** `new_view_async` (Phase 8). Blocking is awkward inside a `winit` / `egui` event loop. Until it ships, offload `new_view` to a worker thread.

#### `close_view`

```rust
pub fn close_view(&self, view_id: ViewId) -> Result<()>;
```

Raw: `close_view {view_id}` notification.

- Drops the SDK's `DocumentState`. Emits `Event::ViewClosed { view_id }` iff the view existed.
- Idempotent (further calls are no-ops).

#### `save`

```rust
pub fn save(&self, view_id: ViewId, path: impl AsRef<Path>) -> Result<()>;
```

Raw: `save {view_id, file_path}` notification.

- **No success or failure event.** `xi-core` reports save failure via `Event::Alert`. Do not assume a saved file exists just because `save` returned `Ok(())`.
- `xi-core` appends `line_ending` if the config has `save_with_newline = true`.

#### `document`

```rust
pub fn document(&self, view_id: ViewId) -> Option<DocumentSnapshot>;
```

Reads `Inner::docs[view_id]`, clones the snapshot. `None` if the view is unknown (e.g. after `close_view`).

> **Gotcha:** the snapshot reflects the **latest** state at the moment of the call. It may contain edits from more `update` notifications than the last `Event::DocumentUpdated` you consumed. This is by design; treat the event as "something changed, re-read".

#### `view_ids`

```rust
pub fn view_ids(&self) -> Vec<ViewId>;
```

Returns the currently-known view ids, ascending.

### 6.4 Config

#### `get_config`

```rust
pub fn get_config(&self, view_id: ViewId) -> Result<Config>;
```

Raw: `get_config {view_id}` request.

- **Blocking** synchronous round-trip.
- Response is the merged config object (see `types::Config`).
- On `xi-core` side, missing `view_id` → `error.code = 404`.

> **Gotcha:** you usually don't need `get_config` — `xi-core` pushes the **full** config as the first `config_changed` for each new view, and only diffs after that. `get_config` is useful for tests and for re-fetching after reconnect.

#### `modify_user_config`

```rust
pub enum ConfigDomain {
    General,
    Syntax(String),
    UserOverride(ViewId),
}

pub fn modify_user_config(&self, domain: ConfigDomain, changes: serde_json::Value) -> Result<()>;
```

Raw: `modify_user_config {domain, changes}` notification.

- `changes` is a JSON object. **A `null` value deletes the key.**
- When `xi-core` was started with `config_dir` (i.e. file-based config is on), `General` and `Syntax` modifications are **rejected** (the change is dropped and `xi-core` emits `alert`). Only `UserOverride` works.
- Each modification goes through a validator; invalid changes emit `Event::Alert` and are ignored.

### 6.5 Theme and language

#### `set_theme`

```rust
pub fn set_theme(&self, name: &str) -> Result<()>;
```

Raw: `set_theme {theme_name}` notification.

- On success, `Event::ThemeChanged { name }` fires. The `ThemeSettings` payload (foreground, background, caret colors, …) is currently **dropped** — only the name is surfaced. See §10.
- Names come from `Event::AvailableThemes`.

#### `set_language`

```rust
pub fn set_language(&self, view_id: ViewId, language: &str) -> Result<()>;
```

Raw: `set_language {view_id, language_id}` notification.

- Requires the syntect plugin to actually do anything; without it, no-op at the syntax-highlighting level, but `Event::LanguageChanged` still fires (from `language_changed`).
- On wire, the field is `view_id` (**not** `view-id`), despite what some upstream docs say.

### 6.6 Sync requests

```rust
pub fn cut(&self, view_id: ViewId) -> Result<Option<String>>;
pub fn copy(&self, view_id: ViewId) -> Result<Option<String>>;
```

- Raw: `edit.cut` / `edit.copy` (with `[]` params).
- Return `None` if the selection is empty.
- **Blocking.** Same recommendation as `new_view`: offload to a worker thread in a synchronous UI loop.

> **Not yet:** `debug_get_contents`. See §10.

### 6.7 Plugin control

```rust
pub fn plugin_start(&self, view_id: ViewId, plugin: &str) -> Result<()>;
pub fn plugin_stop(&self, view_id: ViewId, plugin: &str) -> Result<()>;
pub fn plugin_rpc(&self, view_id: ViewId, receiver: &str, rpc: serde_json::Value) -> Result<()>;
```

Raw: all three use the **`plugin` namespace**, but with **`command`**, not `method`:

```json
{"method": "plugin", "params": {"command": "start",       "view_id": "view-id-1", "plugin_name": "syntect"}}
{"method": "plugin", "params": {"command": "stop",        "view_id": "view-id-1", "plugin_name": "syntect"}}
{"method": "plugin", "params": {"command": "plugin_rpc",  "view_id": "view-id-1", "receiver": "syntect", "rpc": {"method": "...", "params": {...}}}}
```

> **Gotcha:** the `plugin` namespace uses `"command"`, while `edit` uses `"method"`. Sending `"method"` for a plugin notification will cause `xi-core` to exit with `UnknownRequest("missing field command")`. This is a protocol quirk of `xi-core`; the SDK always emits `command`.

- `plugin_rpc` sends a custom notification to the named plugin. The `rpc` object must contain `"method"` and `"params"` — that's the plugin-RPC payload, nested inside our outer `plugin` message.
- Notifications from the plugin back to the UI go through `Event::PluginCommands`, `Event::StatusItem*`, `Event::HoverShown`, `Event::Alert`.

---

## 7. Events reference

All events are emitted on the `crossbeam_channel::Receiver<Event>` returned by `events()`. Unless noted otherwise, an event can be emitted zero, one, or many times. `Event` is `#[non_exhaustive]`.

### 7.1 `Event::ViewOpened { request_id: u64, view_id: ViewId }`

- Fires when `xi-core` responds to `new_view`.
- `request_id` is the SDK-allocated logical id from `Inner::next_request_id`. Reserved for a future `new_view_async`; today the caller can ignore it.
- Emitted **once** per view.
- After this event, `document(view_id)` returns `Some(snapshot)` with a default (empty) document; the first real content arrives with `DocumentUpdated`.

### 7.2 `Event::DocumentUpdated { view_id: ViewId }`

- Fires for every `update` notification from `xi-core`.
- The SDK has **already applied** the ops to `Inner::docs[view_id]` when this event fires.
- Read the current state with `document(view_id)`.
- There is no payload beyond `view_id` — deliberate; the snapshot can be large, and pull-on-demand lets the UI coalesce multiple events.

> **Gotcha:** because `document()` re-locks, if `xi-core` pushes a second `update` between your `recv()` and `document()`, you'll see the newer state. That's fine for rendering but means "one event ≠ one snapshot version". If you need strict per-event snapshots, add a `DocumentUpdatedWithSnapshot` variant (see §10).

### 7.3 `Event::StyleDefined(Style)`

- Fires on `def_style` from `xi-core`.
- The SDK has already inserted it into `Inner::styles`.
- `Style` mirrors the raw payload; `id` is the style id that `Line.styles` will reference.
- Ids 0 and 1 are reserved (selection/find) and deprecated; real ids start at 8 in the current `xi-core` build.

### 7.4 `Event::ThemeChanged { name: String }`

- Fires on `theme_changed`.
- Only `name` is exposed. The full `ThemeSettings` payload is dropped (see §10).

### 7.5 `Event::ConfigChanged { view_id: ViewId, changes: Config }`

- Fires on `config_changed`.
- **First event per view**: `changes` is the complete merged config.
- **Subsequent events**: `changes` contains only the keys that changed. The SDK merges into `DocumentState::config` before emitting, so `document(view_id).config` is always the current merged view.

> **Gotcha:** the SDK merges into state **before** emitting. `document()` right after the event shows the new config.

> **Gotcha:** `changes` is a typed `Config`. Any keys not modeled by `Config` (e.g. `scroll_past_end` from newer builds) are merged into state via `Config::merge` (best-effort, they're ignored too) but never surface on the event payload. See §10.

### 7.6 `Event::LanguageChanged { view_id: ViewId, language: String }`

- Fires on `language_changed`.
- The `language` string is the new language id (e.g. `"Rust"`, `"Plain Text"`).

### 7.7 `Event::FindStatus { view_id: ViewId, matches: u64 }`

- Fires on `find_status`.
- `matches` is the **sum** of `queries[i].matches` from the wire. The per-query details are currently dropped. See §10.
- `xi-core` runs find incrementally, so this grows over time until the search reaches EOF.
- **Requires a viewport:** find runs from the visible range. If you haven't called `resize` + `scroll`, `matches` may stay at 0.

### 7.8 `Event::ReplaceStatus { view_id, chars, preserve_case }`

- Fires on `replace_status`.
- `chars` is the current replacement string; `preserve_case` mirrors the `preserve_case` config flag.

### 7.9 `Event::ScrollTo { view_id: ViewId, line: u64, col: u64 }`

- Fires on `scroll_to`.
- Request to scroll the viewport so that `(line, col)` is visible.
- `line` is a **logical** line (not formatted). `col` is a UTF-8 byte offset within the line.

### 7.10 `Event::Alert(String)`

- Fires on `alert`.
- Used by `xi-core` for user-visible errors: file changes on disk, save failures, plugin crashes, invalid config, etc.
- The SDK does not interpret the string; display it verbatim.

### 7.11 `Event::AvailableLanguages` / `Event::AvailableThemes`

```rust
AvailableLanguages { languages: Vec<String> }
AvailableThemes { themes: Vec<String> }
```

- Fire after `client_started` (once, during `spawn`).
- `AvailableThemes` may fire again when the theme set changes; `AvailableLanguages` when the plugin set changes.

### 7.12 `Event::AvailablePlugins { view_id, plugins }`

```rust
AvailablePlugins { view_id: ViewId, plugins: Vec<(String, bool)> }
```

- Fires once per view, right after `new_view`.
- Each tuple is `(plugin_name, is_running)`.
- Empty vec if no plugins are installed.

### 7.13 `Event::PluginStarted` / `Event::PluginStopped`

```rust
PluginStarted { view_id: ViewId, plugin: String }
PluginStopped { view_id: ViewId, plugin: String, code: i32 }
```

- Fire when the corresponding plugin lifecycle transition happens.
- `code == 0` for a normal plugin stop; `code == 1` for a crash.

### 7.14 `Event::PluginCommands { view_id, plugin, cmds }`

```rust
PluginCommands { view_id: ViewId, plugin: String, cmds: Vec<serde_json::Value> }
```

- Fires on `update_cmds`. The plugin has registered or updated its command palette entries.
- `cmds` is a raw JSON array; the schema is defined by the plugin.

### 7.15 `Event::HoverShown { view_id, request_id, result }`

```rust
HoverShown { view_id: ViewId, request_id: u64, result: Option<Hover> }
```

- Fires on `show_hover`, in response to `request_hover`.
- `request_id` echoes the value passed to `request_hover`.
- `result` is `None` if the position has no hover info, or if no LSP plugin is running.

### 7.16 Status items

```rust
StatusItemAdded { view_id, source, key, value, alignment }
StatusItemUpdated { view_id, key, value }
StatusItemRemoved { view_id, key }
```

- Fire on the corresponding `add_status_item` / `update_status_item` / `remove_status_item` notifications.
- `source` is the plugin that owns the item; `alignment` is `"left"` or `"right"`.

### 7.17 `Event::Exited`

- Fires **once** when the reader thread sees EOF on `xi-core`'s stdout.
- Also fires for normal `shutdown()`.
- After this event, all `XiClient` write methods return `Error::CoreNotRunning`.

### 7.18 `Event::ViewClosed { view_id }`

- Fires locally, after a successful `close_view`.
- `xi-core` does not send a notification for view closure; the SDK emits this itself.

---

## 8. Data model

### 8.1 `ViewId`

```rust
pub struct ViewId(pub usize);
```

Wire format: `"view-id-<n>"` (e.g. `"view-id-1"`). The SDK's `Serialize`/`Deserialize` handles the string form; you never see the string.

### 8.2 `DocumentState` and `DocumentSnapshot`

Two types, different audiences:

- **`DocumentState`** (`src/document.rs`) — the mutable, live state. Lives in `Inner::docs`. Updated by the reader thread as `update` notifications arrive.
- **`DocumentSnapshot`** — an immutable clone handed to the UI. Fields:

```rust
pub struct DocumentSnapshot {
    pub view_id: Option<ViewId>,
    pub lines: Vec<LineState>,
    pub selections: Vec<(usize, usize, usize, usize)>,
    pub pristine: bool,
    pub path: Option<PathBuf>,
    pub language: String,
    pub config: Config,
}

pub struct LineState {
    pub text: String,
    pub styles: Vec<StyleSpan>,
    pub cursors: Vec<usize>,
}

pub struct StyleSpan {
    pub start: usize,
    pub len: usize,
    pub style_id: u64,
}
```

Invariants:

- `lines.len()` = the number of logical lines in the view. The empty document has exactly one empty line (`text == ""`).
- `text` has no `\n` and no `\r`. Exactly one trailing `\n` (and an optional preceding `\r`) is stripped from the wire payload.
- `styles` are sorted by `start` and don't overlap unless `xi-core` sent overlapping spans (see §8.4).
- `cursors` are sorted ascending and are valid UTF-8 boundaries.

### 8.3 Lines, operations, and `apply_ops`

`xi-core` sends `update` as a list of ops. The SDK's `apply_ops` in `src/document.rs` is the reference implementation.

The wire shape:

```json
{
  "method": "update",
  "params": {
    "view_id": "view-id-1",
    "update": {
      "ops": [ ... ],
      "pristine": false,
      "annotations": [ ... ]
    }
  }
}
```

Each op:

```ts
interface Op {
  op: 'copy' | 'skip' | 'invalidate' | 'ins' | 'update'
  n: number
  lines?: Line[]
  ln?: number
}
```

Application semantics (`apply_ops`):

| op           | effect                                                                                                   | advances `old_ix`? |
| ------------ | -------------------------------------------------------------------------------------------------------- | ------------------ |
| `copy`       | append `n` old lines starting at `old_ix` to the new list                                                | yes                |
| `skip`       | advance `old_ix` by `n` without appending                                                                | yes                |
| `invalidate` | append `n` blank lines to the new list                                                                   | no                 |
| `ins`        | append `n` new lines from `op.lines`                                                                     | no                 |
| `update`     | replace `n` old lines starting at `old_ix`; for each line, if `text` is `null` copy it from the old line | yes                |

Invariants:

- `n > 0` for every op (checked by `Operation::validate`).
- For `ins` and `update`, `n == op.lines.len()` (checked by `Operation::validate`).
- For other op types, `op.lines` is empty (checked by `Operation::validate`).
- After processing all ops, `new_lines` length equals `xi-core`'s view line count.
- Every old line is copied at most once (in the current batch).

> **Gotcha:** `ins` does **not** advance `old_ix` because it introduces content that wasn't in the old state. A common bug is to advance `old_ix` on `ins`; the result is shifted lines.

> **Gotcha:** `update` with `text: null` means "keep the old text"; `update` with `text: "<...>"` means "replace the text in place".

> **Gotcha:** `apply_ops` must be atomic — apply the whole batch, then publish. `DocumentState::apply` does exactly that (builds `new_lines` locally, then assigns). If you ever change it, keep it atomic; otherwise a UI reading `document()` mid-batch sees a torn state.

### 8.4 Styles

`Line.styles` on the wire is a flat `Vec<i64>` of triplets:

```json
"styles": [ 0, 5, 3, -2, 4, 5, 0, 3, 8 ]
```

Meaning:

- Triplet 0: `delta = 0`, `len = 5`, `style_id = 3`. Absolute `start = 0`, `end = 5`.
- Triplet 1: `delta = -2`, `len = 4`, `style_id = 5`. Absolute `start = 5 + (-2) = 3`, `end = 7`.
- Triplet 2: `delta = 0`, `len = 3`, `style_id = 8`. Absolute `start = 7 + 0 = 7`, `end = 10`.

The SDK stores the raw triplet as `StyleDef { delta, length, style_id }` (§8.4) and decodes absolute offsets in `DocumentState::snapshot`:

```rust
let mut cursor = 0i64;
let mut spans = Vec::new();
for s in &line.styles {
    let start = cursor + s.delta;
    let end = start + s.length as i64;
    spans.push(StyleSpan { start: start.max(0) as usize, len: s.length as usize, style_id: s.style_id });
    cursor = end;
}
```

The example above decodes to `(0,5,3), (3,7,5), (7,10,8)`.

### 8.5 Cursors and selections

Two parallel sources:

- **`Line.cursor`** (wire) → `LineState.cursors` (SDK). Per-line cursor positions as UTF-8 byte offsets. A line with no cursor has an empty `cursors`.
- **`annotations[type="selection"]`** (wire) → `DocumentSnapshot.selections`. Global selection ranges `[start_line, start_col, end_line, end_col]` (all `usize`).

A _caret_ is a selection with `start == end`; it will also appear in the corresponding line's `cursors`.

> **Gotcha:** the SDK does not deduplicate. A caret may be represented both as a one-element `cursors` entry and as a zero-width `selections` entry. Your UI can use either; both are correct.

Column units: UTF-8 byte offsets within the line, matching `Line.text`.

### 8.6 Annotations

The full annotation shape on the wire:

```ts
interface AnnotationSlice {
  type: "selection" | "find" | string;
  ranges: [[number, number, number, number], ...];
  payloads: [any] | null;
  n: number;
}
```

The SDK currently extracts only `type == "selection"`. `find` annotations are ignored (match count is surfaced via `Event::FindStatus` instead). Additional annotation types (plugin-provided) are preserved in the wire payload but not exposed on `DocumentSnapshot`.

> **Not yet:** `DocumentSnapshot.annotations: Vec<AnnotationSlice>` (Phase 7). Needed for LSP diagnostics (`type: "diagnostic"` in newer builds).

### 8.7 `Config`

`Config` is the merged view config. All fields are `Option<T>` because `xi-core` omits keys the user didn't set (in diff events) and different languages may not have all keys. The SDK merges diffs into `DocumentState::config` before emitting `ConfigChanged`, so `document(view_id).config` reflects a complete, cumulative view of the config as far as `xi-core` has told us.

The first `config_changed` after `new_view` is the **full** merged config. Subsequent ones are diffs. `Config::merge` semantics:

- `null` → clear the key (revert to default);
- other value → set the key, if it deserializes to the field type;
- absent key → leave the current value untouched;
- unknown key → ignored (xi-core may ship config keys we don't model).

### 8.8 `Style` definitions

```rust
pub struct Style {
    pub id: u64,
    pub fg_color: Option<u32>,
    pub bg_color: Option<u32>,
    pub weight: Option<u32>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
}
```

The SDK stores each `def_style` in `Inner::styles` and emits `StyleDefined(style)`. `Line.styles[].style_id` references these ids.

Colors are **ARGB in word order** (i.e. `0xAARRGGBB`). Convert to whatever your renderer expects.

### 8.9 `FindQuery`, `FindOptions`, `ModifySelection`

See §6.2.6. Builder pattern:

```rust
FindQuery::new("hello")
    .case_sensitive(true)
    .regex(false)
    .whole_words(true)
    .with_id(7)

FindOptions::default()
    .wrap_around(true)
    .modify_selection(ModifySelection::Add)
```

### 8.10 `GestureType`, `Granularity`

See §6.2.5.

### 8.11 `Position`, `Hover`

```rust
pub struct Position { pub line: u64, pub col: u64 }

pub struct Hover {
    pub content: String,             // markdown
    pub range: Option<Vec<u64>>,     // [start_line, start_col, end_line, end_col]
}
```

### 8.12 `SpawnOptions`, `ConfigDomain`

See §4.1, §6.4.

---

## 9. What the SDK handles automatically

### 9.1 `measure_width` — auto-reply

`xi-core` sends `measure_width` as a **request** (with `id`) whenever it needs real pixel widths, e.g. for word wrap. The SDK **must** reply; otherwise `xi-core`'s word wrap stalls.

Wire shape:

```json
// incoming request
{"id": 7, "method": "measure_width", "params": [
  {"id": 2, "strings": ["hello", "world"]},
  {"id": 3, "strings": ["foo"]}
]}

// outgoing response
{"id": 7, "result": [[5.0, 5.0], [3.0]]}
```

**Default policy: `CodepointMono`.** Each Unicode scalar counts as width 1.0. So `"hello"` → 5.0, `"привет"` → 6.0, `"foo"` → 3.0. Fast, allocation-free, portable; doesn't match a real font's metrics.

**Custom policy:** install a callback with `set_width_measure(f)`:

```rust
client.set_width_measure(|strings: &[String]| -> Vec<f64> {
    // e.g. query your font system for real metrics
    strings.iter().map(|s| s.chars().count() as f64 * 7.5).collect()
});
```

The callback returns one `f64` per input string; the SDK pads short outputs with `0.0` and truncates long outputs to match `strings.len()`.

`clear_width_measure()` restores the default `CodepointMono` policy.

> **Gotcha:** the callback runs on the **reader thread**. It must not call back into `XiClient` (which would deadlock on `Inner`). Reading `document()` from the callback is technically safe (it only locks `Inner`) but doing heavy work there will stall inbound message processing.

> **Gotcha:** if the callback returns fewer widths than strings, the missing ones default to `0.0`; if it returns more, extras are dropped. This is defensive; well-behaved callbacks return exactly `strings.len()` values.

### 9.2 `view_id` string ↔ numeric

The wire uses `"view-id-<n>"`. The SDK's `ViewId` newtype round-trips this and hides it. You never serialize or parse the string form.

### 9.3 Newline framing

You never think about line framing. `xi-rpc`'s `RpcLoop` reads one JSON line at a time and parses it. The SDK's `SharedStdin` sends `\n`-terminated messages and flushes.

### 9.4 `update` ops application

You don't need to interpret `copy`/`skip`/`invalidate`/`ins`/`update`. The SDK applies them to `DocumentState` as soon as a notification arrives. `Event::DocumentUpdated` is a "go re-read" signal, not a payload.

### 9.5 Selection extraction

`annotations[type="selection"]` is extracted into `DocumentSnapshot::selections` automatically. You don't need to walk annotations.

### 9.6 Request correlation

Every outbound request gets an `id` from `xi-rpc`'s internal counter. Blocking callers (`new_view`, `get_config`, `cut`, `copy`) block on a channel that `xi-rpc` wires up; the reader thread's `RpcLoop` dispatches the response back. You never see an `id`.

### 9.7 The `new_view` race

Between `xi-core`'s response to `new_view` and the app thread's `docs.insert(view_id, ...)`, `xi-core` may already be pushing `update` notifications for the new view. `ClientHandler::on_update` buffers these in `Inner::pending_updates[view_id]`. The next `XiClient::new_view` drains and applies them before returning. Without this, the first screen of content would be silently dropped. See §4.1.

---

## 10. Known gaps and design notes

Everything below is deliberate, tracked, and assigned to a phase in
[`docs/roadmap.md`](roadmap.md). Nothing here is a bug in the shipped surface.

### 10.1 API gaps (Phase 6)

- **Movement:** `move_word_left/right`, `move_to_left_end/right_end_of_line`, `move_to_beginning/end_of_document`, `move_to_beginning/end_of_paragraph`, `scroll_page_up/down` — plus all `_and_modify_selection` variants. ~20 methods, all one-liners.
- **Multi-cursor:** `add_selection_above` / `below`, `selection_into_lines`.
- **Editing:** `paste` (needs a dedicated lifecycle test for the multi-cursor semantics), `goto_line`, `delete_to_end_of_paragraph`, `delete_to_beginning_of_line`, `increase_number`, `decrease_number`.

These are trivial to add; the SDK's `send_edit` helper covers them. They were deferred to keep PRs scoped.

### 10.2 Payload gaps (Phase 7)

- **`Event::ConfigChanged { changes: Config }`** loses keys not modeled by `Config`. The state gets them via `Config::merge` (best-effort), but a subscriber on the event won't see `scroll_past_end: true`, etc. Fix: add `raw: serde_json::Value` alongside `changes`.
- **`Event::FindStatus { matches: u64 }`** drops `queries[i].lines` and per-query metadata. Needed for minimap markers and "F6" navigation in Sublime-like UIs. Fix: add `queries: Vec<QueryStatus>` with `{ id, chars, case_sensitive, whole_words, lines: Vec<u64>, matches: u64 }`.
- **`Event::ThemeChanged { name }`** drops `ThemeSettings` (foreground/background/caret/selection colors). Without colors, a frontend can't actually apply the theme from the event. Fix: add `theme: ThemeSettings` with `Option<u32>` color fields.
- **`DocumentSnapshot.annotations`** doesn't exist. LSP diagnostics and other plugin annotations (`type: "diagnostic"`, `type: "hover"`) are preserved on the wire but not surfaced. Fix: add `annotations: Vec<AnnotationSlice>` with `type`, `ranges`, `payloads`, `n`.

### 10.3 Async & performance (Phase 8)

- **`new_view` is blocking.** Same for `cut` / `copy` / `get_config`. Inside a `winit` / `egui` loop, block a worker thread. Fix: `new_view_async` → `Result<u64>` request id; caller awaits `Event::ViewOpened { request_id, .. }`. The infrastructure (`Inner::next_request_id`) is already in place.
- **`copy_async` / `cut_async`.** Same idea, via `xi-rpc`'s `send_rpc_request_async` + callback. Not yet planned as a method; use the sync versions with a worker thread for now.
- **Unbounded event channel.** A slow consumer will grow the channel without bound. Fix: `XiClient::set_event_buffer(size)` for a bounded channel with a drop-oldest policy.
- **`document()` clones every line** on every call. For 10k-line files this is ~10k `String` allocations per render frame. Fine at 60 fps with small files, measurable with large ones. Fix candidate: `Arc<str>` in `LineState` or a dirty-region snapshot API. Benchmark first; don't optimize blind.
- **`Inner` is a single `Mutex`.** All view reads block behind any writer. `DashMap<ViewId, _>` is a possible future optimization. Not a priority until there's a benchmark showing contention.

### 10.4 IDE gaps (Phase 9)

- **`request_completion`** doesn't exist (and may not be present in every `xi-core` build). Needs an LSP plugin and its own event (`Event::CompletionShown`).
- **Diagnostics.** LSP diagnostics arrive as annotations (`type: "diagnostic"`). Surfaced via `DocumentSnapshot.annotations` (Phase 7) — but the API to subscribe per-view and filter by severity is not designed yet.
- **`Event::Alert`** is a single `String`. Doesn't distinguish `Info` / `Warning` / `Error`. `xi-core` doesn't tag it; frontends have to parse. Low priority.

### 10.5 Design tradeoffs we accepted

- **Pull model for documents.** `Event::DocumentUpdated` carries no snapshot; the UI calls `document()`. Pros: cheap to emit, UI coalesces. Cons: a small race window where `document()` returns a newer state than the triggering event. Fix candidate: a separate `Event::DocumentSnapshot(DocumentSnapshot)` variant that carries the snapshot, keeping `DocumentUpdated` as a lightweight signal. Not needed until a frontend actually exhibits the problem.
- **No auto-reconnect.** If `xi-core` exits, the SDK stays dead until you drop and `spawn` again. Matches `xi-core`'s "no version negotiation" stance.
- **Single reader thread.** All inbound JSON parsing and state updates happen on one thread. Matches `xi-core`'s single-threaded frontend handling.
- **`XiClient: Send + !Sync`.** Inherited from `xi-rpc`'s `Box<dyn Peer>`. Use `Arc<Mutex<XiClient>>` if you need shared access across threads.

### 10.6 Things we deliberately don't expose

- Raw `Value` escape hatch (send arbitrary `method`/`params`). If you need a method not in §6, add it there — don't open a back door.
- `debug_rewrap` / `debug_wrap_width` — deprecated by `xi-core`.
- The legacy gesture format — see §6.2.5.
- `measure_width` as an event — the SDK replies automatically; exposing it would force every consumer to implement it.
- `tracing_config` / `save_trace` (trace collection) — out of scope for this SDK; use the raw protocol if you need it.

---

## 11. Appendix: raw method catalog

Compact catalog of every raw method the SDK is allowed to speak. See `core-lib/src/rpc.rs` for authoritative types.

### Client → core, notifications (no response)

| Method                               | Params (shape)                      | SDK method(s)                  |
| ------------------------------------ | ----------------------------------- | ------------------------------ |
| `client_started`                     | `{config_dir?, client_extras_dir?}` | `spawn` / `spawn_with_options` |
| `close_view`                         | `{view_id}`                         | `close_view`                   |
| `save`                               | `{view_id, file_path}`              | `save`                         |
| `set_theme`                          | `{theme_name}`                      | `set_theme`                    |
| `set_language`                       | `{view_id, language_id}`            | `set_language`                 |
| `modify_user_config`                 | `{domain, changes}`                 | `modify_user_config`           |
| `edit.<inner>`                       | see §6.2                            | all editing/movement/find/etc. |
| `plugin` + `{command: "start"}`      | `{view_id, plugin_name}`            | `plugin_start`                 |
| `plugin` + `{command: "stop"}`       | `{view_id, plugin_name}`            | `plugin_stop`                  |
| `plugin` + `{command: "plugin_rpc"}` | `{view_id, receiver, rpc}`          | `plugin_rpc`                   |

### Client → core, requests (with response)

| Method               | Params         | Response                   |
| -------------------- | -------------- | -------------------------- |
| `new_view`           | `{file_path?}` | `"view-id-<n>"`            |
| `get_config`         | `{view_id}`    | merged config object       |
| `edit.cut`           | `{view_id}`    | `String \| null`           |
| `edit.copy`          | `{view_id}`    | `String \| null`           |
| `debug_get_contents` | `{view_id}`    | `String` (not exposed yet) |

### Core → client, notifications

| Method                | Params                                                     | Event                      |
| --------------------- | ---------------------------------------------------------- | -------------------------- |
| `update`              | `{view_id, update: {ops, pristine, annotations}}`          | `DocumentUpdated`          |
| `scroll_to`           | `{view_id, line, col}`                                     | `ScrollTo`                 |
| `def_style`           | `{id, fg_color?, bg_color?, weight?, italic?, underline?}` | `StyleDefined`             |
| `theme_changed`       | `{name, theme}`                                            | `ThemeChanged` (name only) |
| `config_changed`      | `{view_id, changes}`                                       | `ConfigChanged`            |
| `language_changed`    | `{view_id, language_id}`                                   | `LanguageChanged`          |
| `available_languages` | `{languages}`                                              | `AvailableLanguages`       |
| `available_themes`    | `{themes}`                                                 | `AvailableThemes`          |
| `find_status`         | `{view_id, queries: [{id, matches, ...}]}`                 | `FindStatus` (summed)      |
| `replace_status`      | `{view_id, status: {chars, preserve_case}}`                | `ReplaceStatus`            |
| `show_hover`          | `{view_id, request_id, result}`                            | `HoverShown`               |
| `alert`               | `{msg}`                                                    | `Alert`                    |
| `available_plugins`   | `{view_id, plugins}`                                       | `AvailablePlugins`         |
| `plugin_started`      | `{view_id, plugin}`                                        | `PluginStarted`            |
| `plugin_stopped`      | `{view_id, plugin, code}`                                  | `PluginStopped`            |
| `update_cmds`         | `{view_id, plugin, cmds}`                                  | `PluginCommands`           |
| `add_status_item`     | `{view_id, source, key, value, alignment}`                 | `StatusItemAdded`          |
| `update_status_item`  | `{view_id, key, value}`                                    | `StatusItemUpdated`        |
| `remove_status_item`  | `{view_id, key}`                                           | `StatusItemRemoved`        |

### Core → client, requests (client must respond)

| Method          | Params                 | Expected response     | SDK policy                                                        |
| --------------- | ---------------------- | --------------------- | ----------------------------------------------------------------- |
| `measure_width` | `[{id, strings}, ...]` | `[[width, ...], ...]` | `CodepointMono` by default; custom via `set_width_measure` (§9.1) |
