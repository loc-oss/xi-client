//! Serde types that mirror the xi-core wire protocol.
//!
//! See `docs/protocol.md` §8 for the data model. These types are deserialized
//! directly from `update` / `def_style` / `config_changed` payloads; the SDK
//! façade (`DocumentSnapshot`, `Event`) re-exports or adapts them.

pub mod config;
pub mod config_domain;
pub mod gesture;
pub mod line;
pub mod operation;
pub mod spawn;
pub mod style;
pub mod update;
pub mod view_id;

pub use config::Config;
pub use config_domain::ConfigDomain;
pub use gesture::{GestureType, Granularity};
pub use line::{Line, StyleDef};
pub use operation::{Operation, OperationType};
pub use spawn::SpawnOptions;
pub use style::Style;
pub use update::{Annotation, Update};
pub use view_id::ViewId;
