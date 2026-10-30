//! SDK for talking to `xi-core`.
//!
//! The façade (`XiClient`) is deliberately thin: every method maps to one raw
//! RPC (or a small fixed sequence), and the reader thread translates inbound
//! notifications into [`Event`]s. See `docs/protocol.md` for the design.

pub mod client;
pub mod document;
pub mod events;
pub mod process;
pub mod types;

pub use client::XiClient;
pub use document::{DocumentSnapshot, LineState, StyleSpan};
pub use events::Event;
pub use types::{
    Annotation, Config, GestureType,
    Granularity, Line, Operation,
    OperationType, Style, Update,
    ViewId,
};

pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the SDK.
///
/// Wire-level errors from `xi-rpc` are folded into this enum by the `From`
/// impls at the bottom; callers do not need to depend on `xi-rpc` directly.
#[derive(Debug)]
pub enum Error {
    /// Underlying I/O error (subprocess stdio, file ops).
    Io(std::io::Error),
    /// JSON serialization/deserialization error.
    Json(serde_json::Error),
    /// `xi-core` is not running (never spawned, or already exited).
    ///
    /// Returned after [`Event::Exited`], or after [`XiClient::shutdown`]
    /// on any subsequent write.
    CoreNotRunning,
    /// `view_id` is not known to the SDK (closed, or never opened).
    UnknownView(ViewId),
    /// Failed to spawn the `xi-core` subprocess.
    Spawn(String),
    /// Any other RPC-layer failure that has no better mapping.
    Rpc(String),
    /// `xi-core` responded with a structured error (`{"code", "message", "data"}`).
    Remote(xi_rpc::RemoteError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Json(e) => write!(f, "json: {e}"),
            Error::CoreNotRunning => write!(f, "xi-core не запущен"),
            Error::UnknownView(v) => write!(f, "неизвестный view: {v}"),
            Error::Spawn(s) => write!(f, "spawn: {s}"),
            Error::Rpc(s) => write!(f, "rpc: {s}"),
            // `xi_rpc::RemoteError` has no `Display` impl; `Debug` is close
            // enough and includes the code/message/data fields.
            Error::Remote(e) => write!(f, "remote: {e:?}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

impl From<xi_rpc::Error> for Error {
    fn from(e: xi_rpc::Error) -> Self {
        use xi_rpc::Error as E;
        match e {
            E::Io(e) => Error::Io(e),
            E::RemoteError(e) => Error::Remote(e),
            // The peer is gone — this is what the application should see as
            // "core not running", not as an RPC-level string.
            E::PeerDisconnect => Error::CoreNotRunning,
            E::InvalidResponse => Error::Rpc("invalid response from xi-core".into()),
        }
    }
}

impl From<xi_rpc::ReadError> for Error {
    fn from(e: xi_rpc::ReadError) -> Self {
        use xi_rpc::ReadError as E;
        match e {
            E::Io(e) => Error::Io(e),
            E::Json(e) => Error::Json(e),
            E::NotObject => Error::Rpc("RPC message was not a JSON object".into()),
            E::UnknownRequest(e) => Error::Rpc(format!("unknown request: {e}")),
            E::Disconnect => Error::CoreNotRunning,
        }
    }
}

impl From<xi_rpc::RemoteError> for Error {
    fn from(e: xi_rpc::RemoteError) -> Self {
        Error::Remote(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_converts() {
        let io = std::io::Error::new(std::io::ErrorKind::Other, "boom");
        let err: Error = io.into();
        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().starts_with("io: "));
    }

    #[test]
    fn json_error_converts() {
        let json = serde_json::from_str::<i32>("not a number").unwrap_err();
        let err: Error = json.into();
        assert!(matches!(err, Error::Json(_)));
    }

    #[test]
    fn peer_disconnect_maps_to_core_not_running() {
        let err: Error = xi_rpc::Error::PeerDisconnect.into();
        assert!(matches!(err, Error::CoreNotRunning));
    }

    #[test]
    fn read_disconnect_maps_to_core_not_running() {
        let err: Error = xi_rpc::ReadError::Disconnect.into();
        assert!(matches!(err, Error::CoreNotRunning));
    }

    #[test]
    fn invalid_response_maps_to_rpc() {
        let err: Error = xi_rpc::Error::InvalidResponse.into();
        assert!(matches!(err, Error::Rpc(_)));
        assert!(err.to_string().contains("invalid response"));
    }

    #[test]
    fn remote_error_preserves_code_and_message() {
        let remote = xi_rpc::RemoteError::custom(404, "view not found", None);
        let err: Error = remote.into();
        match &err {
            Error::Remote(e) => {
                assert_eq!(*e, xi_rpc::RemoteError::custom(404, "view not found", None));
            }
            other => panic!("expected Error::Remote, got {other:?}"),
        }
    }

    #[test]
    fn unknown_view_display() {
        let err = Error::UnknownView(ViewId(42));
        assert_eq!(err.to_string(), "неизвестный view: view-id-42");
    }

    #[test]
    fn io_error_has_source() {
        use std::error::Error as _;
        let io = std::io::Error::new(std::io::ErrorKind::Other, "boom");
        let err: Error = io.into();
        assert!(err.source().is_some());
    }
}
