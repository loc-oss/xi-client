use serde::Serialize;

use super::ViewId;

/// Domain for a [`crate::XiClient::modify_user_config`] call.
///
/// Wire format (see `docs/protocol.md` §6.4):
/// - `General` → `"general"`
/// - `Syntax("rust")` → `{"syntax": "rust"}`
/// - `UserOverride(view_id)` → `{"user_override": "view-id-N"}`
///
/// When xi-core was spawned with [`crate::SpawnOptions::config_dir`] set,
/// `General` and `Syntax` modifications are **rejected** (the change is
/// dropped and xi-core emits an `Alert`). Only `UserOverride` works.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDomain {
    General,
    Syntax(String),
    UserOverride(ViewId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_general() {
        assert_eq!(
            serde_json::to_value(&ConfigDomain::General).unwrap(),
            json!("general")
        );
    }

    #[test]
    fn serializes_syntax() {
        assert_eq!(
            serde_json::to_value(ConfigDomain::Syntax("rust".into())).unwrap(),
            json!({"syntax": "rust"})
        );
    }

    #[test]
    fn serializes_user_override() {
        assert_eq!(
            serde_json::to_value(ConfigDomain::UserOverride(ViewId(1))).unwrap(),
            json!({"user_override": "view-id-1"})
        );
    }
}
