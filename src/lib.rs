pub mod client;
pub mod document;
pub mod events;
pub mod process;
pub mod rpc;
pub mod types;

pub use client::XiClient;
pub use document::{DocumentSnapshot, LineState, StyleSpan};
pub use events::Event;
pub use types::{Annotation, Config, Line, Operation, OperationType, Style, Update, ViewId};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    CoreNotRunning,
    UnknownView(ViewId),
    Spawn(String),
    Rpc(String),
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
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self { Error::Io(e) }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self { Error::Json(e) }
}
