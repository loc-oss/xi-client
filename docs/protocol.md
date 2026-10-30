# xi-client Protocol Reference

**Status:** design + reference
**Covers:** the `xi-client` crate (this SDK) and the raw `xi-core` JSON-RPC it speaks
**Authoritative for raw wire:** `xi-editor/rust/core-lib/src/rpc.rs`
**Authoritative for this document:** this file

This document has two jobs:

1. **Catalog every raw method** that the SDK is allowed to speak — the wire protocol between `xi-client` and `xi-core`.
2. **Describe the SDK façade** — the methods we expose on `XiClient`, the events we emit on the `crossbeam_channel`, and exactly how the façade translates to and from the raw protocol.

Read §2–§3 first to understand the plumbing, then use §6 and §7 as reference.

---

## Table of contents

- [0. Reading guide](#0-reading-guide)
- [1. Architectural overview](#1-architectural-overview)
- [2. Wire protocol (raw `xi-core`)](#2-wire-protocol-raw-xi-core)
  - [2.1 Transport and framing](#21-transport-and-framing)
  - [2.2 Message shapes](#22-message-shapes)
  - [2.3 Correlation and IDs](#23-correlation-and-ids)
  - [2.4 Protocol quirks](#24-protocol-quirks)
  - [2.5 Errors](#25-errors)
- [3. SDK architecture](#3-sdk-architecture)
  - [3.1 Components](#31-components)
  - [3.2 Threading model](#32-threading-model)
  - [3.3 Locking and state ownership](#33-locking-and-state-ownership)
  - [3.4 Event delivery](#34-event-delivery)
- [4. Lifecycle](#4-lifecycle)
  - [4.1 `XiClient::spawn`](#41-xiclient_spawn)
  - [4.2 Handshake: `client_started`](#42-handshake-client_started)
  - [4.3 Graceful shutdown](#43-graceful-shutdown)
  - [4.4 Unexpected exit](#44-unexpected-exit)
- [5. Raw ↔ SDK mapping](#5-raw--sdk-mapping)
- [6. SDK API reference](#6-sdk-api-reference)
- [7. Events reference](#7-events-reference)
- [8. Data model](#8-data-model)
- [9. What the SDK handles automatically](#9-what-the-sdk-handles-automatically)
- [10. Known gaps, bugs, and design notes](#10-known-gaps-bugs-and-design-notes)
- [11. Appendix: raw method catalog](#11-appendix-raw-method-catalog)

---

## 0. Reading guide

The SDK is deliberately thin. For every `XiClient` method there is **one** raw method it calls (or a small fixed sequence). If you can't find a method here, it either isn't exposed yet (§10) or it's not part of the protocol at all.

We use four callouts throughout:

> **Gotcha:** something that will bite you if you're not careful.

> **Raw:** the exact wire-level method(s) used under the hood.

> **Auto:** something the SDK does _for_ you, so you don't have to.

> **Status:** implementation progress when it matters.

---

## 1. Architectural overview

```

┌──────────────────────────────────────────────────────────────────────────┐
│ your application │
│ │
│ XiClient (methods) Receiver<Event> │
│ │ ▲ │
└───────────┼─────────────────────────────────────┼────────────────────────┘
│ │
▼ │
┌───────────────────────────────┐ ┌────────────┴───────────────────────┐
│ XiClient façade (client.rs) │ │ events.rs (Event enum) │
│ - RpcWriter (mutexed) │ │ crossbeam_channel::Sender<Event> │
│ - Arc<Mutex<Inner>> │ └────────────▲───────────────────────┘
│ - pending request table │ │
│ - docs / styles state │ │
└──────┬───────────────┬────────┘ │
│ │ │
│ write │ read (reader thread) │
▼ ▼ │
┌──────────────┐ ┌──────────────────────────────┴───────────────────────┐
│ RpcWriter │ │ RpcReader (stdout) │
│ (stdin) │ │ - reads JSON lines │
│ │ │ - dispatches: │
│ │ │ _ response → pending table → event │
│ │ │ _ notification → state update → event │
│ │ │ \* request (measure_width) → auto reply │
└──────┬───────┘ └──────────────────────────────────────────────────────┘
│
▼
ChildStdin
│
┌──────┴───────────────────────────────────────────────────────────────────┐
│ xi-core subprocess │
│ stdin ◀── newline-delimited JSON ──▶ stdout │
└──────────────────────────────────────────────────────────────────────────┘

```

The SDK sits between your UI and the `xi-core` subprocess. It exposes two things:

- **Methods** on `XiClient` that translate into raw JSON-RPC.
- **Events** on a `crossbeam_channel::Receiver<Event>` for everything `xi-core` pushes back.

The subprocess is started and supervised by the SDK. You never touch `ChildStdin`/`ChildStdout`.

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

// notification
{"method": "edit", "params": {"view_id": "view-id-1", "method": "insert", "params": {"chars": "x"}}}

// success response
{"id": 7, "result": "view-id-1"}

// error response
{"id": 7, "error": {"code": 5, "message": "I/O error"}}
```

Note the nesting convention used by xi for its _namespaced_ methods (`edit`, `plugin`): the outer `params` contains an inner `method`/`params`. The SDK flattens this for you (§6.2).

### 2.3 Correlation and IDs

- IDs are **per-direction, monotonically increasing integers**, starting from 0 (the SDK starts from 1; that's fine — the protocol doesn't care).
- The **SDK** allocates IDs for its outbound requests. `RpcWriter::next_id` owns that counter.
- `xi-core` allocates its own IDs for its outbound requests (`measure_width` only, see §9.1). The SDK echoes the received `id` verbatim.
- IDs are never reused.

### 2.4 Protocol quirks

1. **No `"jsonrpc": "2.0"` field.** Sending one is not an error but it's not part of the protocol.
2. **Bidirectional peer.** Both sides can send Requests and Notifications. The SDK must handle core-initiated requests, not just responses. The only such request in the current build is `measure_width` (§9.1).
3. **No batch.**
4. **No version negotiation.** The protocol is tied to the `xi-core` build.
5. **`view_id` is a string** of the form `"view-id-<n>"` (`n ≥ 1`), not a number.
6. **`edit`'s inner method has its `view_id` at the outer `params` level** (§6.2).
7. **`styles` in `Line` are delta-encoded triplets** (§8.4).
8. **`copy`/`update` ops interact with `old_ix`** (§8.3).
9. **`scroll` uses half-open `[first, last)` in _formatted_ (soft-wrapped) lines**, not logical lines.
10. **`resize` uses px/points**, not device pixels.

> **Gotcha:** the docs in `xi-editor/api-docs/` disagree with the source in a couple of places (`set_language` uses `view-id` vs `view_id`; `update.rev` is described as "not in this build" but the plugin-side `update` does carry `rev` — different protocols). The SDK always follows **source**, not the inherited docs.

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

| File                                    | Responsibility                                                                                                                                                                                                      |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/process.rs` (`XiProcess`)          | Spawn/kill the `xi-core` subprocess. Owns the `Child`. Exposes `ChildStdin`/`ChildStdout` exactly once via `take_stdin()`/`take_stdout()`.                                                                          |
| `src/rpc.rs` (`RpcReader`, `RpcWriter`) | Line-oriented JSON I/O. `RpcReader::next → Option<Value>` (blocking). `RpcWriter::{notify, request, respond}`. Owns the outbound `next_id` counter.                                                                 |
| `src/client.rs` (`XiClient`, `Inner`)   | The façade. Holds the `Arc<Mutex<Inner>>`, the `Receiver<Event>`, and `XiProcess`. `Inner` holds the writer, the in-flight request table, the per-view `DocumentState`, the style registry, and the event `Sender`. |
| `src/document.rs`                       | `DocumentState` (mutable per-view) and `DocumentSnapshot` (what the UI reads). Contains `apply_ops` (the _ops_ interpreter).                                                                                        |
| `src/events.rs`                         | The `Event` enum. This is the entire surface the UI observes.                                                                                                                                                       |
| `src/types/`                            | Serde types that mirror the raw wire (`Line`, `Operation`, `Update`, `Config`, `Style`, `ViewId`, …).                                                                                                               |

### 3.2 Threading model

Three actors:

1. **Application thread** — your code. Calls `XiClient` methods. Locks `Inner` briefly to write a message or read state.
2. **Reader thread** — spawned by `XiClient::spawn`. Owns `RpcReader`. Blocks on `RpcReader::next`. For each message:
   - **Response**: match `id` in `Inner::pending`, dispatch the appropriate handler, possibly emit an `Event`.
   - **Notification**: update `Inner` state as necessary (e.g. apply `update` to `documents`), emit the corresponding `Event`.
   - **Request** (`measure_width`): synthesize the response and write it back via `RpcWriter` (also locking `Inner`). **Does not** surface to the UI by default.
3. **`xi-core` subprocess** — its stdin/stdout are held by `RpcWriter`/`RpcReader`.

There is no dedicated writer thread. The application thread writes directly (through the `Mutex<RpcWriter>`). Since writes are just `write_all` + `flush` on a pipe, this is fine.

```
app thread:  [lock Inner] → write request → [unlock] → wait on event_rx / do other work
reader thread: [lock Inner] → apply update → send Event → [unlock] → loop
```

### 3.3 Locking and state ownership

`Inner` is the single source of truth. All of the following live in `Inner`:

- `RpcWriter` (id counter + stdin)
- `pending: HashMap<u64, PendingRequest>` — one entry per in-flight request
- `docs: HashMap<ViewId, DocumentState>`
- `styles: HashMap<u64, Style>`
- `event_tx: Sender<Event>`
- `next_view_request: u64` — sequence counter used only for `new_view` (see §4.1)

The reader thread and application thread both `lock()` `Inner`. Neither holds the lock across a blocking operation. Concretely:

- `insert(view_id, "x")` → lock, `RpcWriter::notify(...)`, unlock.
- Reader sees an `update` notification → lock, `DocumentState::apply(update)`, unlock, `event_tx.send(Event::DocumentUpdated{...})`.
- `document(view_id)` → lock, clone `DocumentState::snapshot(view_id)`, unlock.

> **Gotcha:** `event_tx.send(...)` is done **after unlocking**. That guarantees no deadlock if the UI's event handler calls back into `XiClient::document`, which would try to re-lock `Inner`.

> **Gotcha:** `Inner::pending` is mutated by both threads. `new_view` inserts an entry; the reader thread removes it and dispatches. Since both hold the lock, this is safe.

### 3.4 Event delivery

- `XiClient::events(&self) -> &crossbeam_channel::Receiver<Event>`. The `Receiver` lives on `XiClient` directly (not inside `Inner`), so it can be borrowed.
- **Single consumer.** `crossbeam`'s `Receiver` is not `Clone`. If you need multiple observers, wrap the `Receiver` in your own multiplexer, or have the consumer fan out.
- The channel is **unbounded** (see §10 for the tradeoff).
- Sends from the reader thread never block on the UI.

> **Gotcha:** the current skeleton in `src/client.rs` returns `Receiver<Event>` by value from a `self.event_rx.clone()`. `crossbeam_channel::Receiver` does **not** implement `Clone` for normal channels. The intended signature is `fn events(&self) -> &Receiver<Event>`. Adjust the skeleton accordingly.

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
    /// Level for the `XI_LOG` env var passed to the child (default: unset).
    pub log_level: Option<String>,
    /// `-log-dir` flag passed to the child.
    pub log_dir: Option<PathBuf>,
    /// `-log-file` flag passed to the child.
    pub log_file: Option<PathBuf>,
    /// If `true`, pipe stderr instead of inheriting (default: inherit).
    pub capture_stderr: bool,
}
```

Steps performed by `spawn`:

1. `XiProcess::spawn(core_path)` — starts the child with `stdin`/`stdout` piped.
2. Move `ChildStdin` into `RpcWriter`, `ChildStdout` into `RpcReader`.
3. Create `crossbeam_channel::unbounded::<Event>()`. Keep the `Receiver` on `XiClient`; pass a clone of the `Sender` into the reader thread.
4. Spawn the reader thread named `"xi-client-reader"`.
5. Lock `Inner`, send `client_started` notification (§4.2), unlock.
6. Return `XiClient`.

> **Gotcha:** `client_started` must be the _first_ message. `XiCore::inner()` panics if it receives anything else (except `tracing_config`) before `client_started`. The SDK always sends it inside `spawn`, so you can't forget.

> **Gotcha:** the child's stderr is inherited by default, so `xi-core`'s log lines go to the parent's stderr. This is convenient for debugging but can be noisy in production; use `SpawnOptions::capture_stderr = true` to pipe it, or set `-log-dir`/`-log-file` to redirect it to a file inside the child.

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

Effects on `xi-core` (from its own docs):

- Loads `preferences.xiconfig` / `<lang>.xiconfig` from `config_dir` **if and only if** `config_dir` is provided. Once enabled, RPC cannot modify `general` or `syntax` config domains.
- Scans `config_dir/plugins/*/manifest.toml` for plugins.
- Emits `available_languages` and `available_themes` (see §7.10).
- Starts plugin processes.

The SDK forwards `available_languages` / `available_themes` as `Event::AvailableLanguages` / `Event::AvailableThemes` (🚧 planned; currently these are consumed silently — see §7).

### 4.3 Graceful shutdown

```rust
pub fn shutdown(&self);
```

Behaviour:

1. Lock `Inner`, `take()` the `RpcWriter`, drop it. Dropping `ChildStdin` closes `xi-core`'s stdin.
2. Unlock.
3. `xi-core` sees EOF, exits.
4. The reader thread sees EOF on stdout, emits `Event::Exited`, and exits.
5. When the `XiClient` is dropped, `XiProcess::Drop` reaps the child (see §4.4).

After `shutdown()`, any subsequent `XiClient` method that writes returns `Error::CoreNotRunning`. `document()` / `events()` keep working until the `XiClient` is dropped.

To observe the actual exit:

```rust
client.shutdown();
loop {
    match client.events().recv() {
        Ok(Event::Exited) => break,
        Ok(_) => {}
        Err(_) => break,
    }
}
```

> **Gotcha:** `shutdown()` is non-blocking; it only closes stdin. The `XiClient` and its reader thread keep running until you drop `XiClient` (or until `Event::Exited` fires and you choose to drop). If you want a synchronous wait, use `client.wait_for_exit(Duration)` (🚧 planned, §10).

### 4.4 Unexpected exit

Three ways the child can go away:

| Situation                                 | Reader thread sees      | Event emitted |
| ----------------------------------------- | ----------------------- | ------------- |
| Normal `shutdown()`                       | EOF on stdout           | `Exited`      |
| `xi-core` crashes / is killed             | EOF on stdout           | `Exited`      |
| Protocol violation by us (malformed JSON) | `xi-core` closes stdout | `Exited`      |

In all three cases the SDK emits **exactly one** `Event::Exited` and the reader thread stops. Any in-flight `PendingRequest` entries are dropped with their oneshot `Sender`s; blocking callers (`new_view`, `get_config`, `cut`, `copy`) get `Error::CoreNotRunning` (see §6.7).

The subprocess is reaped by `XiProcess::Drop`. There is no exit-code available via the public API; 🚧 planned `wait() -> Result<ExitStatus>`.

---

## 5. Raw ↔ SDK mapping

This is the master table. Details for each SDK method are in §6; details for each event are in §7.

### 5.1 SDK → core (outbound)

| SDK method                                                                                                                                         | Raw method                                      | Type  | Notes                              |
| -------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- | ----- | ---------------------------------- | ----- |
| `XiClient::spawn`                                                                                                                                  | `client_started`                                | notif | sent during spawn                  |
| `new_view` / `new_view_async`                                                                                                                      | `new_view`                                      | req   | returns `view_id`                  |
| `close_view`                                                                                                                                       | `close_view`                                    | notif |                                    |
| `save`                                                                                                                                             | `save`                                          | notif | errors come back as `alert`        |
| `insert`, `paste`, `delete_*`, `insert_newline`, `insert_tab`, `undo`, `redo`, `transpose`, `duplicate_line`, `increase_number`, `decrease_number` | `edit` + inner `insert`/`paste`/…               | notif | see §6.2                           |
| `move_*`, `scroll_page_*`, `select_all`, `collapse_selections`, `add_selection_above/below`, `selection_into_lines`                                | `edit` + inner `move_*`/…                       | notif |                                    |
| `uppercase`, `lowercase`, `capitalize`, `indent`, `outdent`, `reindent`                                                                            | `edit` + inner transform                        | notif |                                    |
| `scroll`                                                                                                                                           | `edit` + `scroll [first,last]`                  | notif | formatted lines                    |
| `resize`                                                                                                                                           | `edit` + `resize {w,h}`                         | notif | px/points                          |
| `request_lines`                                                                                                                                    | `edit` + `request_lines [first,last]`           | notif |                                    |
| `goto_line`                                                                                                                                        | `edit` + `goto_line {line}`                     | notif |                                    |
| `gesture`                                                                                                                                          | `edit` + `gesture {line,col,ty}`                | notif |                                    |
| `find`, `multi_find`, `find_next`, `find_previous`, `find_all`, `highlight_find`, `selection_for_find`                                             | `edit` + inner                                  | notif |                                    |
| `replace`, `replace_next`, `replace_all`, `selection_for_replace`                                                                                  | `edit` + inner                                  | notif |                                    |
| `request_hover`                                                                                                                                    | `edit` + `request_hover {request_id,position?}` | notif | result comes back via `show_hover` |
| `get_config`                                                                                                                                       | `get_config`                                    | req   |                                    |
| `cut` / `copy`                                                                                                                                     | `edit` + inner `cut`/`copy`                     | req   | returns `String                    | null` |
| `debug_get_contents`                                                                                                                               | `debug_get_contents`                            | req   |                                    |
| `set_theme`                                                                                                                                        | `set_theme`                                     | notif |                                    |
| `set_language`                                                                                                                                     | `set_language`                                  | notif | requires syntect plugin            |
| `modify_user_config`                                                                                                                               | `modify_user_config`                            | notif |                                    |
| `plugin_start`, `plugin_stop`, `plugin_rpc`                                                                                                        | `plugin` + inner                                | notif |                                    |

### 5.2 Core → SDK (inbound)

| Raw notification                     | SDK handler                        | Event                   |
| ------------------------------------ | ---------------------------------- | ----------------------- |
| `update`                             | apply ops to `DocumentState`       | `DocumentUpdated`       |
| `scroll_to`                          | —                                  | `ScrollTo`              |
| `def_style`                          | insert into `styles` map           | `StyleDefined`          |
| `theme_changed`                      | —                                  | `ThemeChanged`          |
| `config_changed`                     | merge into `DocumentState::config` | `ConfigChanged`         |
| `language_changed`                   | set `DocumentState::language`      | `LanguageChanged`       |
| `available_languages`                | —                                  | `AvailableLanguages` 🚧 |
| `available_themes`                   | —                                  | `AvailableThemes` 🚧    |
| `find_status`                        | parse matches count                | `FindStatus`            |
| `replace_status`                     | —                                  | `ReplaceStatus` 🚧      |
| `show_hover`                         | correlate with `request_hover`     | `HoverShown` 🚧         |
| `alert`                              | —                                  | `Alert`                 |
| `available_plugins`                  | —                                  | `AvailablePlugins` 🚧   |
| `plugin_started`                     | —                                  | `PluginStarted` 🚧      |
| `plugin_stopped`                     | —                                  | `PluginStopped` 🚧      |
| `update_cmds`                        | —                                  | `PluginCommands` 🚧     |
| `add_status_item`                    | —                                  | `StatusItemAdded` 🚧    |
| `update_status_item`                 | —                                  | `StatusItemUpdated` 🚧  |
| `remove_status_item`                 | —                                  | `StatusItemRemoved` 🚧  |
| _(response to `new_view`)_           | create `DocumentState`             | `ViewOpened`            |
| _(response to `get_config`)_         | —                                  | `ConfigResponse` 🚧     |
| _(response to `cut`)_                | —                                  | `CutResult` 🚧          |
| _(response to `copy`)_               | —                                  | `CopyResult` 🚧         |
| _(response to `debug_get_contents`)_ | —                                  | `DocumentContents` 🚧   |
| _(request `measure_width`)_          | **auto-reply**                     | _(none, see §9.1)_      |
| _(EOF on stdout)_                    | —                                  | `Exited`                |

---

## 6. SDK API reference

Every method below is called on `&XiClient`. `ViewId` is a copyable `usize`-backed newtype (§8.1). Methods that only write to `xi-core` return `Result<()>` — `Ok(())` means the bytes were flushed into the child's stdin, **not** that `xi-core` has processed them. Confirm through events.

### 6.0 Return-value conventions

| Return type                                 | Meaning                                                                                   |
| ------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `Result<()>`                                | the notification was written to `xi-core`'s stdin.                                        |
| `Result<T>` where `T: Copy` (e.g. `ViewId`) | a synchronous request/response completed.                                                 |
| `Result<Option<String>>`                    | a synchronous request/response that can produce `None` (e.g. `cut` with empty selection). |
| `Result<u64>` (e.g. `new_view_async`)       | the ID of an outstanding request. The result will arrive as an `Event`.                   |

### 6.1 Lifecycle

#### `XiClient::spawn` / `spawn_with_options`

```rust
pub fn spawn(core_path: impl AsRef<Path>) -> Result<Self>;
pub fn spawn_with_options(core_path: impl AsRef<Path>, opts: SpawnOptions) -> Result<Self>;
```

- Raw: `client_started`.
- Returns once the subprocess is running and `client_started` has been written.
- 🚧 `spawn_with_options` is planned; `spawn` sends `client_started` with empty params.
- Errors: `Error::Spawn` if the process can't be started; `Error::Io` if writing `client_started` fails.

#### `shutdown`

```rust
pub fn shutdown(&self);
```

- Raw: none (closes stdin).
- See §4.3.
- After this call, all subsequent write attempts return `Error::CoreNotRunning`.

#### `events`

```rust
pub fn events(&self) -> &crossbeam_channel::Receiver<Event>;
```

- See §3.4.
- **Borrow**, not owned.

### 6.2 Namespaced `edit` calls

All editing/movement/selection/viewport/gesture calls share this wire shape:

```json
{"method":"edit","params":{"view_id":"view-id-N","method":"<inner>","params":<inner_params>}}
```

Inner params that are "empty" are sent as `[]` (an empty array). `xi-core`'s `EditCommand` deserializer accepts `params` omitted, `{}`, or `[]` interchangeably.

The SDK exposes a helper for every inner method. In the tables below, "raw" is `edit.<inner>`.

#### 6.2.1 Edits (fire-and-forget)

| SDK method                             | inner                         | inner params       | Notes                                                                                                                  |
| -------------------------------------- | ----------------------------- | ------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| `insert(view_id, chars)`               | `insert`                      | `{"chars": chars}` | inserts at every cursor                                                                                                |
| `paste(view_id, chars)`                | `paste`                       | `{"chars": chars}` | multi-cursor paste semantics: one line per cursor if line-count matches cursor-count, else full string at every cursor |
| `delete_backward(view_id)`             | `delete_backward`             | `[]`               |                                                                                                                        |
| `delete_forward(view_id)`              | `delete_forward`              | `[]`               |                                                                                                                        |
| `delete_word_backward(view_id)`        | `delete_word_backward`        | `[]`               |                                                                                                                        |
| `delete_word_forward(view_id)`         | `delete_word_forward`         | `[]`               |                                                                                                                        |
| `delete_to_end_of_paragraph(view_id)`  | `delete_to_end_of_paragraph`  | `[]`               |                                                                                                                        |
| `delete_to_beginning_of_line(view_id)` | `delete_to_beginning_of_line` | `[]`               |                                                                                                                        |
| `insert_newline(view_id)`              | `insert_newline`              | `[]`               |                                                                                                                        |
| `insert_tab(view_id)`                  | `insert_tab`                  | `[]`               |                                                                                                                        |
| `undo(view_id)`                        | `undo`                        | `[]`               |                                                                                                                        |
| `redo(view_id)`                        | `redo`                        | `[]`               |                                                                                                                        |
| `transpose(view_id)`                   | `transpose`                   | `[]`               |                                                                                                                        |
| `duplicate_line(view_id)`              | `duplicate_line`              | `[]`               |                                                                                                                        |
| `increase_number(view_id)`             | `increase_number`             | `[]`               |                                                                                                                        |
| `decrease_number(view_id)`             | `decrease_number`             | `[]`               |                                                                                                                        |

#### 6.2.2 Movement / selection

Every `move_*` variant has a `move_*_and_modify_selection` companion; the SDK exposes both. They all take `[]` as params.

`move_up`, `move_down`, `move_left`, `move_right`, `move_word_left`, `move_word_right`, `move_to_beginning_of_paragraph`, `move_to_end_of_paragraph`, `move_to_left_end_of_line`, `move_to_right_end_of_line`, `move_to_beginning_of_document`, `move_to_end_of_document`, `scroll_page_up`, `scroll_page_down` — and their `_and_modify_selection` variants.

Plus:

| SDK method                      | inner                  | inner params |
| ------------------------------- | ---------------------- | ------------ |
| `select_all(view_id)`           | `select_all`           | `[]`         |
| `collapse_selections(view_id)`  | `collapse_selections`  | `[]`         |
| `add_selection_above(view_id)`  | `add_selection_above`  | `[]`         |
| `add_selection_below(view_id)`  | `add_selection_below`  | `[]`         |
| `selection_into_lines(view_id)` | `selection_into_lines` | `[]`         |

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

| SDK method                            | inner           | inner params                         |
| ------------------------------------- | --------------- | ------------------------------------ |
| `scroll(view_id, first, last)`        | `scroll`        | `[first, last]`                      |
| `resize(view_id, width, height)`      | `resize`        | `{"width": width, "height": height}` |
| `request_lines(view_id, first, last)` | `request_lines` | `[first, last]`                      |
| `goto_line(view_id, line)`            | `goto_line`     | `{"line": line}`                     |

- `scroll` uses **formatted** (soft-wrapped) lines, half-open `[first, last)`. Report it every time the viewport changes; `xi-core` uses it to decide which lines to render and keep valid.
- `resize` takes **px/points**, not device pixels.
- `request_lines` asks for lines outside the viewport to be made valid.

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
| `multi_find(view_id, qs: Vec<FindQuery>)`     | `multi_find`            | `{queries: [...]}`                               |
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

- `Event::FindStatus { view_id, matches }` fires as matches are found.
- `Event::ReplaceStatus { view_id, chars, preserve_case }` fires when the replacement string changes.

> **Gotcha:** `find_status` may be sent in two flavours (per `xi-core`'s internal `FindStatusChange`): `All` (full query list, expensive) and `Matches` (match count only). The SDK surfaces only `matches` in `Event::FindStatus` for now; the full query list will be added when we need to display it (§10).

#### 6.2.7 Hover

```rust
pub struct Position { pub line: u64, pub column: u64 }

pub fn request_hover(
    &self,
    view_id: ViewId,
    request_id: u64,
    position: Option<Position>,
) -> Result<()>;
```

Raw: `edit.request_hover {request_id, position?}`.

`request_id` is an **opaque** id chosen by the caller. It is echoed in `Event::HoverShown { view_id, request_id, result }`. It is _not_ the RPC `id` and is not consumed by the SDK.

`position` columns are **UTF-8 byte offsets within the line**, matching `cursor` and `Line.styles` (§8.4, §8.5). Omit `position` to use the current cursor.

> **Gotcha:** column units. `request_hover` and `goto_line` accept columns as UTF-8 byte offsets. If your UI tracks UTF-16 or grapheme indices, you must convert.

### 6.3 View lifecycle

#### `new_view` / `new_view_async`

```rust
pub fn new_view(&self, path: Option<PathBuf>) -> Result<ViewId>;      // blocking
pub fn new_view_async(&self, path: Option<PathBuf>) -> Result<u64>;  // non-blocking, returns request_id
```

Raw: `new_view { file_path? }` request.

- `new_view` (blocking): sends the request, then waits on a per-request oneshot channel for the response. Returns the `ViewId`. `Event::ViewOpened { request_id, view_id }` is still emitted for observers that prefer the event.
- `new_view_async`: sends the request, returns the request id immediately. The caller must `await` `Event::ViewOpened { request_id, .. }` and match on `request_id`.
- Both create a `DocumentState` for the returned `ViewId` before returning / emitting.
- Path `None` → empty buffer.
- The same path cannot have two views simultaneously (`xi-core` restriction).
- On open error: `new_view` returns `Err(Error::Rpc(..))` (mapped from `error.code = 5/6/7`).

> **Gotcha:** with `new_view_async`, if multiple `new_view` calls are in flight, you must correlate by `request_id` from `Event::ViewOpened`. Do not assume `view_id == request_id` — `xi-core` allocates view ids independently of request ids.

> **Gotcha:** in the current skeleton, `client.rs` returns `Result<u64>` from `new_view`. The **recommended** signature is `Result<ViewId>` for the blocking variant and `Result<u64>` for `new_view_async`. The example in `examples/basic.rs` uses the return value only as an opaque token (`_req`), so both signatures compile.

#### `close_view`

```rust
pub fn close_view(&self, view_id: ViewId) -> Result<()>;
```

Raw: `close_view {view_id}` notification.

- Drops the SDK's `DocumentState`. Emits `Event::ViewClosed { view_id }` (🚧 planned; core sends nothing for it).
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

Returns the currently-known view ids.

### 6.4 Config

#### `get_config`

```rust
pub fn get_config(&self, view_id: ViewId) -> Result<Config>;
```

Raw: `get_config {view_id}` request.

- Blocking. On the wire, the response is the merged config object (see `types::Config`).
- On `xi-core` side, missing `view_id` → `error.code = 404`.
- `Event::ConfigResponse { request_id, view_id, config }` (🚧 planned) is emitted alongside.

> **Gotcha:** you usually don't need `get_config` — `xi-core` pushes the **full** config as the first `config_changed` for each new view, and only diffs after that. `get_config` is useful for tests and for re-fetching after reconnect.

#### `modify_user_config`

```rust
pub enum ConfigDomain {
    General,
    Syntax { language: String },
    UserOverride { view_id: ViewId },
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

- On success, `Event::ThemeChanged { name }` fires (payload is only the theme _name_, not the full `ThemeSettings`; the SDK drops the payload — see §10).
- Names come from `Event::AvailableThemes`.

#### `set_language`

```rust
pub fn set_language(&self, view_id: ViewId, language: &str) -> Result<()>;
```

Raw: `set_language {view_id, language_id}` notification.

- Requires the syntect plugin to actually do anything; without it, this is a no-op at the syntax-highlighting level, but `Event::LanguageChanged` still fires (from `language_changed`).
- On wire, the field is `view_id` (**not** `view-id`), despite what some upstream docs say.

### 6.6 Sync requests

These return through a per-request oneshot channel and are only meaningful while the reader thread is alive.

```rust
pub fn cut(&self, view_id: ViewId) -> Result<Option<String>>;
pub fn copy(&self, view_id: ViewId) -> Result<Option<String>>;
pub fn debug_get_contents(&self, view_id: ViewId) -> Result<String>;
```

- Raw: `edit.cut` / `edit.copy` (with `[]` params) and `debug_get_contents {view_id}` respectively.
- `cut` / `copy` return `None` if the selection is empty.
- `debug_get_contents` returns the full buffer text; the request is cheap but not free on large documents.

> **Gotcha:** if `xi-core` dies while one of these is outstanding, the caller wakes with `Error::CoreNotRunning` (the pending oneshot `Sender` is dropped). The SDK does not retry.

### 6.7 Plugin control

```rust
pub fn plugin_start(&self, view_id: ViewId, plugin: &str) -> Result<()>;
pub fn plugin_stop(&self, view_id: ViewId, plugin: &str) -> Result<()>;
pub fn plugin_rpc(&self, view_id: ViewId, receiver: &str, rpc: serde_json::Value) -> Result<()>;
```

Raw: `plugin.{start|stop|plugin_rpc}` notifications.

- `plugin_rpc` sends a custom notification to the named plugin. The `rpc` object must contain `"method"` and `"params"`; it's the payload of a plugin-RPC notification.
- Notifications from the plugin back to the UI go through `Event::PluginCommands` / `Event::StatusItem*` / `Event::HoverShown` / `Event::Alert` (🚧 planned).

---

## 7. Events reference

All events are emitted on the `crossbeam_channel::Receiver<Event>` returned by `events()`. Unless noted otherwise, an event can be emitted zero, one, or many times.

### 7.1 `Event::ViewOpened { request_id: u64, view_id: ViewId }`

- Fires when `xi-core` responds to `new_view`.
- `request_id` matches what `new_view_async` returned (or what the SDK allocated internally for blocking `new_view`).
- Emitted **once** per view.
- After this event, `document(view_id)` returns `Some(snapshot)` with a default (empty) document; the first real content arrives with `DocumentUpdated`.

### 7.2 `Event::DocumentUpdated { view_id: ViewId }`

- Fires for every `update` notification from `xi-core`.
- The SDK has **already applied** the ops to `Inner::docs[view_id]` when this event fires.
- Read the current state with `document(view_id)`.
- There is no payload beyond `view_id` — deliberate; the snapshot can be large, and pull-on-demand lets the UI coalesce multiple events.

> **Gotcha:** because `document()` re-locks, if `xi-core` pushes a second `update` between your `recv()` and `document()`, you'll see the newer state. That's fine for rendering but means "one event ≠ one snapshot version". If you need strict per-event snapshots, use a `DocumentUpdated` variant that carries the snapshot (see §10).

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

### 7.6 `Event::LanguageChanged { view_id: ViewId, language: String }`

- Fires on `language_changed`.
- The `language` string is the new language id (e.g. `"Rust"`, `"Plain Text"`).

### 7.7 `Event::FindStatus { view_id: ViewId, matches: u64 }`

- Fires on `find_status`.
- `matches` is the number of matches known **so far** — `xi-core` runs find incrementally, so this grows over time until the search reaches EOF.
- Does not include per-match line numbers; see §10.

### 7.8 `Event::ScrollTo { view_id: ViewId, line: u64, col: u64 }`

- Fires on `scroll_to`.
- Request to scroll the viewport so that `(line, col)` is visible.
- `line` is a **logical** line (not formatted). `col` is a UTF-8 byte offset within the line.

### 7.9 `Event::Alert(String)`

- Fires on `alert`.
- Used by `xi-core` for user-visible errors: file changes on disk, save failures, plugin crashes, invalid config, etc.
- The SDK does not interpret the string; display it verbatim.

### 7.10 `Event::Exited`

- Fires **once** when the reader thread sees EOF on `xi-core`'s stdout.
- Also fires for normal `shutdown()`.
- After this event, all `XiClient` write methods return `Error::CoreNotRunning`.

### 7.11 🚧 Planned events

The following are designed but not yet implemented. They follow the same conventions.

| Event                                                        | Trigger                          | Payload                                      |
| ------------------------------------------------------------ | -------------------------------- | -------------------------------------------- |
| `ViewClosed { view_id }`                                     | SDK-local after `close_view`     | —                                            |
| `AvailableLanguages { languages }`                           | `available_languages`            | `Vec<String>`                                |
| `AvailableThemes { themes }`                                 | `available_themes`               | `Vec<String>`                                |
| `ReplaceStatus { view_id, chars, preserve_case }`            | `replace_status`                 | —                                            |
| `HoverShown { view_id, request_id, result }`                 | `show_hover`                     | `Result<String, String>` (markdown or error) |
| `ConfigResponse { request_id, view_id, config }`             | response to `get_config`         | —                                            |
| `CutResult { request_id, text }`                             | response to `cut`                | `Option<String>`                             |
| `CopyResult { request_id, text }`                            | response to `copy`               | `Option<String>`                             |
| `DocumentContents { request_id, view_id, text }`             | response to `debug_get_contents` | —                                            |
| `AvailablePlugins { view_id, plugins }`                      | `available_plugins`              | `Vec<(String, bool)>` (name, running)        |
| `PluginStarted { view_id, plugin }`                          | `plugin_started`                 | —                                            |
| `PluginStopped { view_id, plugin, code }`                    | `plugin_stopped`                 | `i32` (0 = user, 1 = crash)                  |
| `PluginCommands { view_id, plugin, cmds }`                   | `update_cmds`                    | `Vec<Value>` (schema "in flux")              |
| `StatusItemAdded { view_id, source, key, value, alignment }` | `add_status_item`                | —                                            |
| `StatusItemUpdated { view_id, key, value }`                  | `update_status_item`             | —                                            |
| `StatusItemRemoved { view_id, key }`                         | `remove_status_item`             | —                                            |

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
    pub lines: Vec<LineState>,                 // one per logical line
    pub selections: Vec<(usize, usize, usize, usize)>, // [start_line, start_col, end_line, end_col]
    pub pristine: bool,
    pub path: Option<PathBuf>,
    pub language: String,
    pub config: Config,
}

pub struct LineState {
    pub text: String,           // line content, no trailing newline
    pub styles: Vec<StyleSpan>, // absolute offsets (§8.4)
    pub cursors: Vec<usize>,    // UTF-8 byte offsets within the line
}

pub struct StyleSpan {
    pub start: usize,           // absolute UTF-8 byte offset in `text`
    pub len: usize,
    pub style_id: u64,
}
```

Invariants:

- `lines.len()` = the number of logical lines in the view. The empty document has exactly one empty line (`text == ""`).
- `text` has no `\n` and no `\r`.
- `styles` are sorted by `start` and don't overlap unless `xi-core` sent overlapping spans (see §8.4).
- `cursors` are sorted ascending and are valid UTF-8 boundaries.

### 8.3 Lines, operations, and `apply_ops`

`xi-core` sends `update` as a list of ops. The SDK's `apply_ops` in `src/document.rs` is the reference implementation and is the same logic used on the wire in `xi-core`'s own frontend documentation.

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
  lines?: Line[] // for ins / update
  ln?: number // logical line number (see below)
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

- `n > 0` for every op.
- For `ins` and `update`, `n == op.lines.len()`.
- After processing all ops, `new_lines` length equals `xi-core`'s view line count.
- Every old line is copied at most once (in the current batch).

> **Gotcha:** `ins` does **not** advance `old_ix` because it introduces content that wasn't in the old state. A common bug is to advance `old_ix` on `ins`; the result is shifted lines.

> **Gotcha:** `update` with `text: null` means "keep the old text"; `update` with `text: "<...>"` means "replace the text in place" (the SDK doesn't currently emit such updates because `xi-core` doesn't either; but the code handles it).

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

So the **decoder** is:

```rust
let mut cursor = 0i64;
let mut spans = Vec::new();
for chunk in styles.chunks(3) {
    let delta = chunk[0];
    let len   = chunk[1] as u64;
    let sid   = chunk[2] as u64;
    let start = cursor + delta;
    let end   = start + len as i64;
    spans.push(StyleSpan { start: start as usize, len: len as usize, style_id: sid });
    cursor = end;
}
```

> **Gotcha (current bug):** `document.rs::snapshot()` currently treats `StyleDef.offset` as absolute: `start: s.offset.max(0) as usize`. It is **not** absolute — it's the delta from the previous span's end. Two fixes are equivalent:
>
> 1. Accumulate deltas inside `deserialize_styles` so `StyleDef.offset` becomes absolute. Simplest, keeps `snapshot()` unchanged.
> 2. Rename `StyleDef.offset` → `StyleDef.delta` and accumulate in `snapshot()`.
>
> The example `styles: [0, 5, 3, -2, 4, 5, 0, 3, 8]` above decodes to `(0,5,3), (3,7,5), (7,10,8)`; the current code would render `(0,5,3), (0,4,5), (0,3,8)`, which is wrong.

### 8.5 Cursors and selections

Two parallel sources:

- **`Line.cursor`** (wire) → `LineState.cursors` (SDK). Per-line cursor positions as UTF-8 byte offsets. A line with no cursor has an empty `cursors`.
- **`annotations[type="selection"]`** (wire) → `DocumentSnapshot.selections`. Global selection ranges `[start_line, start_col, end_line, end_col]` (all `usize`).

A _caret_ is a selection with `start == end`; it will also appear in the corresponding line's `cursors`.

> **Gotcha:** the SDK does not deduplicate. A caret may be represented both as a one-element `cursors` entry and as a zero-width `selections` entry. Your UI can use either; both are correct.

Column units: UTF-8 byte offsets within the line, matching `Line.text` (which is the line content with no trailing `\n`).

### 8.6 Annotations

The full annotation shape on the wire:

```ts
interface AnnotationSlice {
  type: "selection" | "find" | string;
  ranges: [[number, number, number, number], ...]; // [start_line, start_col, end_line, end_col]
  payloads: [any] | null;
  n: number;
}
```

The SDK currently extracts only `type == "selection"`. `find` annotations are ignored (matches count is surfaced via `Event::FindStatus` instead). Additional annotation types (plugin-provided) are preserved in the wire payload but not exposed on `DocumentSnapshot` (🚧 planned: `DocumentSnapshot.annotations: Vec<AnnotationSlice>`).

### 8.7 `Config`

`Config` is the merged view config. All fields are `Option<T>` because `xi-core` omits keys the user didn't set (in diff events) and because different languages may not have all keys. The SDK merges diffs into `DocumentState::config` before emitting `ConfigChanged`, so `document(view_id).config` reflects a complete, cumulative view of the config as far as `xi-core` has told us.

The first `config_changed` after `new_view` is the **full** merged config. Subsequent ones are diffs. The SDK's merge:

```rust
for (k, v) in changes { cfg.set(k, v); } // null → remove
```

### 8.8 `Style` definitions

```rust
pub struct Style {
    pub id: u64,
    pub fg_color: Option<u32>,  // ARGB
    pub bg_color: Option<u32>,  // ARGB
    pub weight: Option<u32>,    // 100..900
    pub italic: Option<bool>,
    pub underline: Option<bool>,
}
```

The SDK stores each `def_style` in `Inner::styles` and emits `StyleDefined(style)`. `Line.styles[].style_id` references these ids.

Colors are **ARGB in word order** (i.e. `0xAARRGGBB`). Convert to whatever your renderer expects.

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

The SDK's default policy is **`CodepointMono`**: each Unicode scalar counts as width 1.0. So `"hello"` → 5.0, `"world"` → 5.0, `"foo"` → 3.0. This is fast, allocation-free, and works everywhere; it just doesn't match a real font's metrics.

> **Auto:** the SDK replies to every `measure_width` from the reader thread while holding `Inner` only long enough to call `RpcWriter::respond`. You don't need to do anything. 🚧 A future `XiClient::set_width_measure` will let you install a callback (`Fn(&[String]) -> Vec<f64>`) for real pixel widths.

> **Gotcha:** if you install a callback later, it runs on the **reader thread**. It must not call back into `XiClient` (which would deadlock on `Inner`).

### 9.2 `view_id` string ↔ numeric

The wire uses `"view-id-<n>"`. The SDK's `ViewId` newtype round-trips this and hides it. You never serialize or parse the string form.

### 9.3 `newline` framing

You never think about line framing. `RpcWriter` always appends `\n` and flushes. `RpcReader` always reads one line at a time and parses it.

### 9.4 `update` ops application

You don't need to interpret `copy`/`skip`/`invalidate`/`ins`/`update`. The SDK applies them to `DocumentState` as soon as a notification arrives. `Event::DocumentUpdated` is a "go re-read" signal, not a payload.

### 9.5 Selection extraction

`annotations[type="selection"]` is extracted into `DocumentSnapshot::selections` automatically. You don't need to walk annotations.

### 9.6 Request correlation

Every outbound request gets an `id` from `RpcWriter::next_id`. `Inner::pending` maps `id → PendingRequest`. `new_view_async` and the synchronous request methods (`get_config`, `cut`, `copy`, `debug_get_contents`) all register entries; the reader thread removes and dispatches them. You never touch `pending`.

### 9.7 `measure_width` again (worth repeating)

There is **one** core-initiated request type in the current build: `measure_width`. The SDK handles it. No other core-initiated requests are expected. If `xi-core` ever adds one, the SDK must be extended (the current reader would fail to dispatch it and would emit `Event::Alert` or drop it — not crash).

---

## 10. Known gaps, bugs, and design notes

### Bugs in the current skeleton

1. **`events()` returns an owned `Receiver`.** `crossbeam_channel::Receiver` is not `Clone`. Change the signature to `fn events(&self) -> &Receiver<Event>` (the `Receiver` lives on `XiClient`, not `Inner`).
2. **`StyleSpan.start` is wrong.** `document.rs::snapshot` treats the delta-coded `StyleDef.offset` as absolute. See §8.4 for the correct decoder and the two equivalent fixes.
3. **`client.rs::spawn` is `unimplemented!()`.** The intended implementation is §4.1.
4. **`XiProcess::stdin` / `.stdout` are `pub` and accessed with `mem::replace(.., unimplemented!())`.** Replace with `take_stdin() -> Option<ChildStdin>` and `take_stdout() -> Option<ChildStdout>`, both called once in `spawn`.
5. **`apply_ops` silently tolerates malformed inputs.** For `copy`, missing old lines push `Line::default()`; for `update`, `op.nb_lines != op.lines.len()` is not validated. The wire is generated by `xi-core` so this is fine in practice, but a defensive `debug_assert!` would help.

### Missing features (🚧 planned)

- **`Event::HoverShown`.** `request_hover` currently returns no event; the response arrives as `show_hover` and is silently dropped. Add a `HoverShown { view_id, request_id, result }` event and correlate by the caller-supplied `request_id` (not the RPC id).
- **`Event::AvailableLanguages` / `AvailableThemes`.** Currently consumed silently.
- **`Event::ConfigResponse` / `CutResult` / `CopyResult` / `DocumentContents`.** Results of synchronous requests currently come back only through the `Result` return value; the events are not emitted. Add them if you have event-driven consumers.
- **`Event::PluginStarted` / `PluginStopped` / `PluginCommands` / `AvailablePlugins`.** Not exposed yet.
- **`Event::StatusItemAdded` / `Updated` / `Removed`.** Not exposed yet.
- **`Event::ReplaceStatus`.** Not exposed yet.
- **`Event::ViewClosed`.** The SDK knows when it called `close_view`; emit it locally.
- **`SpawnOptions`.** Not yet; `spawn` uses empty `client_started`.
- **`wait_for_exit(Duration) -> Result<ExitStatus>`.** Not yet; process is reaped by `XiProcess::Drop`.
- **`set_width_measure(callback)`.** Not yet; `measure_width` uses `CodepointMono` (§9.1).
- **Full `find_status` payload.** Only `matches` count is exposed. The full query list (chars, case sensitivity, whole_words, per-match lines) is dropped.
- **Full `theme_changed` payload.** Only `name` is exposed; the `ThemeSettings` object (foreground, background, caret, selection colors, …) is dropped.

### Design tradeoffs we accepted

- **Pull model for documents.** `Event::DocumentUpdated` carries no snapshot; the UI calls `document()`. Pros: cheap to emit, UI coalesces. Cons: a small race window where `document()` returns a newer state than the triggering event. Alternative: include the snapshot in the event (more allocation, cleaner semantics). If you need the latter, add a separate `Event::DocumentSnapshot(DocumentSnapshot)` and keep `DocumentUpdated` as a lightweight signal.
- **Unbounded event channel.** `crossbeam_channel::unbounded` means the reader thread never blocks on `send`. A slow consumer will grow the channel without bound. For UI-driven clients this is fine; for headless pipelines, consider a bounded channel with a drop policy. 🚧 planned `XiClient::set_event_buffer(usize)`.
- **Single reader thread.** All inbound JSON parsing and state updates happen on one thread. This is the simplest correct model and matches `xi-core`'s single-threaded frontend handling.
- **No auto-reconnect.** If `xi-core` exits, the SDK stays dead until you drop and `spawn` again. This matches `xi-core`'s "no version negotiation" stance.

### Things we deliberately don't expose

- Raw `Value` escape hatch (send arbitrary `method`/`params`). If you need a method not in §6, add it there — don't open a back door.
- `debug_rewrap` / `debug_wrap_width` — deprecated by `xi-core`.
- The legacy gesture format — see §6.2.5.
- `measure_width` as an event — the SDK replies automatically; exposing it would force every consumer to implement it.
- `tracing_config` / `save_trace` (trace collection) — out of scope for this SDK; use the raw protocol if you need it.

---

## 11. Appendix: raw method catalog

Compact catalog of every raw method the SDK is allowed to speak. See `core-lib/src/rpc.rs` for authoritative types.

### Client → core, notifications (no response)

| Method               | Params (shape)                      | SDK method(s)                  |
| -------------------- | ----------------------------------- | ------------------------------ |
| `client_started`     | `{config_dir?, client_extras_dir?}` | `spawn` / `spawn_with_options` |
| `close_view`         | `{view_id}`                         | `close_view`                   |
| `save`               | `{view_id, file_path}`              | `save`                         |
| `set_theme`          | `{theme_name}`                      | `set_theme`                    |
| `set_language`       | `{view_id, language_id}`            | `set_language`                 |
| `modify_user_config` | `{domain, changes}`                 | `modify_user_config`           |
| `edit.<inner>`       | see §6.2                            | all editing/movement/find/etc. |
| `plugin.start`       | `{view_id, plugin_name}`            | `plugin_start`                 |
| `plugin.stop`        | `{view_id, plugin_name}`            | `plugin_stop`                  |
| `plugin.plugin_rpc`  | `{view_id, receiver, rpc}`          | `plugin_rpc`                   |

### Client → core, requests (with response)

| Method               | Params         | Response             |
| -------------------- | -------------- | -------------------- |
| `new_view`           | `{file_path?}` | `"view-id-<n>"`      |
| `get_config`         | `{view_id}`    | merged config object |
| `edit.cut`           | `{view_id}`    | `String \| null`     |
| `edit.copy`          | `{view_id}`    | `String \| null`     |
| `debug_get_contents` | `{view_id}`    | `String`             |

### Core → client, notifications

| Method                | Params                                                     | Event                   |
| --------------------- | ---------------------------------------------------------- | ----------------------- |
| `update`              | `{view_id, update: {ops, pristine, annotations}}`          | `DocumentUpdated`       |
| `scroll_to`           | `{view_id, line, col}`                                     | `ScrollTo`              |
| `def_style`           | `{id, fg_color?, bg_color?, weight?, italic?, underline?}` | `StyleDefined`          |
| `theme_changed`       | `{name, theme}`                                            | `ThemeChanged`          |
| `config_changed`      | `{view_id, changes}`                                       | `ConfigChanged`         |
| `language_changed`    | `{view_id, language_id}`                                   | `LanguageChanged`       |
| `available_languages` | `{languages}`                                              | `AvailableLanguages` 🚧 |
| `available_themes`    | `{themes}`                                                 | `AvailableThemes` 🚧    |
| `find_status`         | `{view_id, queries}`                                       | `FindStatus`            |
| `replace_status`      | `{view_id, status}`                                        | `ReplaceStatus` 🚧      |
| `show_hover`          | `{view_id, request_id, result}`                            | `HoverShown` 🚧         |
| `alert`               | `{msg}`                                                    | `Alert`                 |
| `available_plugins`   | `{view_id, plugins}`                                       | `AvailablePlugins` 🚧   |
| `plugin_started`      | `{view_id, plugin}`                                        | `PluginStarted` 🚧      |
| `plugin_stopped`      | `{view_id, plugin, code}`                                  | `PluginStopped` 🚧      |
| `update_cmds`         | `{view_id, plugin, cmds}`                                  | `PluginCommands` 🚧     |
| `add_status_item`     | `{view_id, source, key, value, alignment}`                 | `StatusItemAdded` 🚧    |
| `update_status_item`  | `{view_id, key, value}`                                    | `StatusItemUpdated` 🚧  |
| `remove_status_item`  | `{view_id, key}`                                           | `StatusItemRemoved` 🚧  |

### Core → client, requests (client must respond)

| Method          | Params                 | Expected response     | SDK policy                |
| --------------- | ---------------------- | --------------------- | ------------------------- |
| `measure_width` | `[{id, strings}, ...]` | `[[width, ...], ...]` | `CodepointMono`, see §9.1 |
