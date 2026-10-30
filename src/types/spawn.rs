use std::path::PathBuf;

/// Options for [`crate::XiClient::spawn_with_options`].
///
/// `SpawnOptions::default()` matches [`crate::XiClient::spawn`]: no config
/// dir, no extras dir, no logging flags, stderr inherited from the parent.
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// Forwarded in `client_started.config_dir`. Enables file-based config
    /// in xi-core: `preferences.xiconfig`, `<lang>.xiconfig`, plugin
    /// manifests under `config_dir/plugins/*/manifest.toml`.
    ///
    /// **Once enabled, `modify_user_config` for `General` / `Syntax` is
    /// rejected by xi-core** (it emits an `Alert`). Use `UserOverride`
    /// instead.
    pub config_dir: Option<PathBuf>,
    /// Forwarded in `client_started.client_extras_dir`.
    pub client_extras_dir: Option<PathBuf>,
    /// Value for the `XI_LOG` env var passed to the child.
    pub log_level: Option<String>,
    /// `-log-dir` flag passed to the child. Support depends on the xi-core
    /// build; see `docs/protocol.md` §4.1.
    pub log_dir: Option<PathBuf>,
    /// `-log-file` flag passed to the child.
    pub log_file: Option<PathBuf>,
    /// If `true`, pipe the child's stderr and drain it in a background
    /// thread instead of inheriting the parent's stderr. The drained bytes
    /// are discarded; use `log_dir` / `log_file` to inspect logs.
    ///
    /// Useful in tests to keep xi-core's fern log lines out of the test
    /// output.
    pub capture_stderr: bool,
}
