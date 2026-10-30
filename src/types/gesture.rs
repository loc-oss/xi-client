use serde::Serialize;

/// Granularity of a selection gesture (point, word, or line).
///
/// See `docs/protocol.md` §6.2.5. Only the new format is supported;
/// the legacy (`"point_select"`, etc.) is deprecated by xi-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Point,
    Word,
    Line,
}

/// A mouse or touch gesture.
///
/// Serialized in the new wire format, e.g.:
/// ```json
/// {"select": {"granularity": "point", "multi": false}}
/// {"select_extend": {"granularity": "word"}}
/// "drag"
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureType {
    Select {
        granularity: Granularity,
        multi: bool,
    },
    SelectExtend {
        granularity: Granularity,
    },
    Drag,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_select() {
        let g = GestureType::Select {
            granularity: Granularity::Point,
            multi: false,
        };
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            json!({"select": {"granularity": "point", "multi": false}})
        );
    }

    #[test]
    fn serializes_select_extend() {
        let g = GestureType::SelectExtend {
            granularity: Granularity::Word,
        };
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            json!({"select_extend": {"granularity": "word"}})
        );
    }

    #[test]
    fn serializes_drag() {
        let g = GestureType::Drag;
        assert_eq!(serde_json::to_value(&g).unwrap(), json!("drag"));
    }
}
