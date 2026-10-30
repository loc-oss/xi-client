# Integration tests

    cargo test

Everything runs without a real `xi-core`, except `lifecycle.rs`, which is
gated behind the `XI_CORE` env var:

    XI_CORE=$PWD/xi-core cargo test --test lifecycle

`lifecycle.rs` covers the Phase-0 validation from `docs/roadmap.md` plus the
Phase-1 editing methods. Without `XI_CORE` it skips every test (prints a
note, passes).

## Layers

- `types.rs` — serde round-trips for every public wire type.
- `config_merge.rs` — `Config::merge` semantics (null clears, absent leaves,
  malformed keeps old, full-config idempotence).
- `document.rs` — `apply_ops`, `DocumentState::apply`, `snapshot`,
  delta-decoding, newline stripping.
- `process.rs` — `XiProcess` and `SharedStdin`, using `sh` as a stand-in
  for `xi-core`.
- `lifecycle.rs` — real `xi-core`, gated by `XI_CORE`.

## Not covered

- `client::measure_width`, `client::parse_view_id` — private; covered by
  inline unit tests in `src/client.rs`.
- `document::strip_trailing_newline` — private; covered indirectly via
  `snapshot()` tests, plus one inline unit test.
