use serde::Deserialize;

use super::line::Line;

/// Type of an `update` op, as it arrives on the wire.
///
/// See `docs/protocol.md` §8.3 for the exact semantics of each variant.
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub enum OperationType {
    /// `copy` on the wire. Named `Copy_` to avoid colliding with
    /// `std::marker::Copy` in `use OperationType::*` sites.
    Copy_,
    Skip,
    Invalidate,
    Update,
    /// `ins` on the wire (not `insert`).
    Insert,
}

/// One op inside an `update.ops` array.
///
/// Invariants (see `docs/protocol.md` §8.3):
/// - `nb_lines > 0`;
/// - for `Insert` / `Update`: `nb_lines as usize == lines.len()`;
/// - `lines` is empty for every other op type.
///
/// Use [`Operation::validate`] to check these defensively.
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Operation {
    #[serde(rename = "op", deserialize_with = "deserialize_operation_type")]
    pub operation_type: OperationType,
    #[serde(rename = "n")]
    pub nb_lines: u64,
    #[serde(rename = "ln")]
    pub line_num: Option<u64>,
    #[serde(default)]
    pub lines: Vec<Line>,
}

impl Operation {
    /// Returns `Err` if this op violates the wire invariants documented above.
    ///
    /// Intended to be called as `debug_assert!(op.validate().is_ok())` from
    /// `document::apply_ops`, so malformed input fails loudly in debug builds
    /// without aborting production on a bad batch from xi-core.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.nb_lines == 0 {
            return Err("op.nb_lines must be > 0");
        }
        match self.operation_type {
            OperationType::Insert | OperationType::Update => {
                if self.nb_lines as usize != self.lines.len() {
                    return Err("op.nb_lines does not match op.lines.len()");
                }
            }
            OperationType::Copy_ | OperationType::Skip | OperationType::Invalidate => {
                if !self.lines.is_empty() {
                    return Err("non-insert/update op must not carry lines");
                }
            }
        }
        Ok(())
    }
}

fn deserialize_operation_type<'de, D>(d: D) -> Result<OperationType, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    match s.as_str() {
        "copy" => Ok(OperationType::Copy_),
        "skip" => Ok(OperationType::Skip),
        "invalidate" => Ok(OperationType::Invalidate),
        "update" => Ok(OperationType::Update),
        "ins" => Ok(OperationType::Insert),
        other => Err(serde::de::Error::unknown_variant(
            other,
            &["copy", "skip", "invalidate", "update", "ins"],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_all_op_types() {
        for (wire, expected) in [
            ("copy", OperationType::Copy_),
            ("skip", OperationType::Skip),
            ("invalidate", OperationType::Invalidate),
            ("update", OperationType::Update),
            ("ins", OperationType::Insert),
        ] {
            let json = format!(r#"{{"op":"{wire}","n":1}}"#);
            let op: Operation = serde_json::from_str(&json).unwrap();
            assert_eq!(op.operation_type, expected, "wire = {wire}");
        }
    }

    #[test]
    fn rejects_unknown_op_type() {
        let json = r#"{"op":"frobnicate","n":1}"#;
        let err = serde_json::from_str::<Operation>(json).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown variant"), "got: {msg}");
        assert!(msg.contains("frobnicate"), "got: {msg}");
    }

    #[test]
    fn validates_ins_and_update() {
        // valid: n matches lines.len()
        let op: Operation =
            serde_json::from_str(r#"{"op":"ins","n":1,"lines":[{"text":"x"}]}"#).unwrap();
        assert!(op.validate().is_ok());

        // invalid: n says 2, one line provided
        let op: Operation =
            serde_json::from_str(r#"{"op":"ins","n":2,"lines":[{"text":"x"}]}"#).unwrap();
        assert_eq!(
            op.validate(),
            Err("op.nb_lines does not match op.lines.len()")
        );
    }

    #[test]
    fn validates_non_insert_ops() {
        let op: Operation = serde_json::from_str(r#"{"op":"copy","n":3}"#).unwrap();
        assert!(op.validate().is_ok());

        let op: Operation =
            serde_json::from_str(r#"{"op":"skip","n":2,"lines":[{"text":"x"}]}"#).unwrap();
        assert_eq!(
            op.validate(),
            Err("non-insert/update op must not carry lines")
        );
    }

    #[test]
    fn rejects_zero_n() {
        let op: Operation = serde_json::from_str(r#"{"op":"copy","n":0}"#).unwrap();
        assert_eq!(op.validate(), Err("op.nb_lines must be > 0"));
    }
}
