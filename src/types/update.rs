use serde::{Deserialize, Deserializer};

use super::{Operation, ViewId};

/// A full `update` notification payload, flattened for the SDK.
///
/// On the wire, `view_id` and `update` are siblings inside `params`:
/// ```json
/// {"view_id": "view-id-1", "update": {"ops": [...], "pristine": false, "annotations": [...]}}
/// ```
/// The custom [`Deserialize`] impl below flattens this into one struct.
#[derive(Debug, PartialEq, Clone)]
pub struct Update {
    pub operations: Vec<Operation>,
    pub pristine: bool,
    pub view_id: ViewId,
    pub annotations: Vec<Annotation>,
}

/// One entry of `update.annotations`.
///
/// See `docs/protocol.md` §8.6. Only `type == "selection"` is currently
/// consumed by the SDK; other types are preserved on the wire payload but
/// not surfaced on [`crate::DocumentSnapshot`].
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Annotation {
    #[serde(rename = "type")]
    pub ann_type: String,
    /// Each range is `[start_line, start_col, end_line, end_col]`.
    /// Column units are UTF-8 byte offsets within the line.
    pub ranges: Vec<Vec<u64>>,
}

impl Annotation {
    /// Returns `true` for `type == "selection"` annotations.
    ///
    /// These are the only ones the SDK turns into
    /// [`crate::DocumentSnapshot::selections`].
    pub fn is_selection(&self) -> bool {
        self.ann_type == "selection"
    }

    /// Returns `true` for `type == "find"` annotations.
    ///
    /// Not currently surfaced; find state is reported via
    /// [`crate::Event::FindStatus`] instead.
    pub fn is_find(&self) -> bool {
        self.ann_type == "find"
    }
}

#[derive(Deserialize)]
struct WireUpdate {
    view_id: ViewId,
    update: InnerUpdate,
}

#[derive(Deserialize)]
struct InnerUpdate {
    #[serde(rename = "ops")]
    operations: Vec<Operation>,
    pristine: bool,
    #[serde(default)]
    annotations: Vec<Annotation>,
}

impl<'de> Deserialize<'de> for Update {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let WireUpdate { view_id, update } = WireUpdate::deserialize(d)?;
        Ok(Update {
            operations: update.operations,
            pristine: update.pristine,
            view_id,
            annotations: update.annotations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_wire_payload() {
        let json = r#"{
            "view_id": "view-id-1",
            "update": {
                "ops": [
                    {"op": "ins", "n": 1, "lines": [{"text": "hello\n"}]}
                ],
                "pristine": false,
                "annotations": [
                    {"type": "selection", "ranges": [[0, 0, 0, 3]]}
                ]
            }
        }"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.view_id, ViewId(1));
        assert!(!update.pristine);
        assert_eq!(update.operations.len(), 1);
        assert_eq!(update.annotations.len(), 1);
        assert!(update.annotations[0].is_selection());
        assert!(!update.annotations[0].is_find());
        assert_eq!(update.annotations[0].ranges, vec![vec![0, 0, 0, 3]]);
    }

    #[test]
    fn missing_annotations_default_to_empty() {
        let json = r#"{
            "view_id": "view-id-2",
            "update": {
                "ops": [],
                "pristine": true
            }
        }"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert!(update.annotations.is_empty());
        assert!(update.pristine);
    }

    #[test]
    fn annotation_helpers() {
        let sel: Annotation =
            serde_json::from_str(r#"{"type":"selection","ranges":[]}"#).unwrap();
        let find: Annotation = serde_json::from_str(r#"{"type":"find","ranges":[]}"#).unwrap();
        let other: Annotation = serde_json::from_str(r#"{"type":"other","ranges":[]}"#).unwrap();
        assert!(sel.is_selection() && !sel.is_find());
        assert!(!find.is_selection() && find.is_find());
        assert!(!other.is_selection() && !other.is_find());
    }
}
