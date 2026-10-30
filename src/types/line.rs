use serde::{self, Deserialize, Deserializer};

/// Delta-encoded style triplet, as it arrives on the wire.
///
/// Wire format: `[delta, length, style_id]`. `delta` is relative to the
/// end of the previous span (`delta == 0` means "starts where the previous
/// span ended"). The absolute start of each span is decoded by
/// [`crate::document`] when building a [`crate::StyleSpan`].
///
/// See `docs/protocol.md` §8.4.
#[derive(Default, Debug, PartialEq, Clone)]
pub struct StyleDef {
    /// Start offset, relative to the end of the previous span.
    pub delta: i64,
    pub length: u64,
    pub style_id: u64,
}

/// A single line as it appears in an `update` notification.
#[derive(Default, Deserialize, Debug, PartialEq, Clone)]
pub struct Line {
    /// Line content, without trailing newline. `None` in an `update` op means
    /// "keep the old text" (see `apply_ops` in `document.rs`).
    #[serde(default)]
    pub text: Option<String>,
    /// Cursor positions, as UTF-8 byte offsets within the line.
    #[serde(default)]
    pub cursor: Vec<u64>,
    /// Delta-encoded style spans (see [`StyleDef`]).
    #[serde(default, deserialize_with = "deserialize_styles")]
    pub styles: Vec<StyleDef>,
    /// Logical line number, if the op carries one.
    #[serde(rename = "ln")]
    pub line_num: Option<u64>,
}

/// Deserializes the flat `Vec<i64>` style triplet array into [`StyleDef`]s.
///
/// The wire shape is `[delta, len, style_id, delta, len, style_id, ...]`.
/// Negative `delta` is valid (spans may overlap); negative `len`/`style_id`
/// is a protocol violation and is rejected.
pub fn deserialize_styles<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<StyleDef>, D::Error> {
    let opt = Option::<Vec<i64>>::deserialize(d)?;
    let v = opt.unwrap_or_default();
    if v.len() % 3 != 0 {
        return Err(serde::de::Error::custom("styles length is not a multiple of 3"));
    }
    let mut out = Vec::with_capacity(v.len() / 3);
    for chunk in v.chunks_exact(3) {
        let (delta, length, style_id) = (chunk[0], chunk[1], chunk[2]);
        if length < 0 || style_id < 0 {
            return Err(serde::de::Error::custom(
                "style length and id must be non-negative",
            ));
        }
        out.push(StyleDef {
            delta,
            length: length as u64,
            style_id: style_id as u64,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_triplets() {
        let json = r#"{"text":"abcdefghij","styles":[0,5,3,-2,4,5,0,3,8]}"#;
        let line: Line = serde_json::from_str(json).unwrap();
        assert_eq!(line.styles.len(), 3);
        assert_eq!(line.styles[0], StyleDef { delta: 0, length: 5, style_id: 3 });
        assert_eq!(line.styles[1], StyleDef { delta: -2, length: 4, style_id: 5 });
        assert_eq!(line.styles[2], StyleDef { delta: 0, length: 3, style_id: 8 });
    }

    #[test]
    fn rejects_partial_triplet() {
        let json = r#"{"text":"x","styles":[0,1,2,3,4]}"#;
        let err = serde_json::from_str::<Line>(json).unwrap_err();
        assert!(err.to_string().contains("multiple of 3"), "got: {err}");
    }

    #[test]
    fn rejects_negative_length_or_id() {
        let json = r#"{"text":"x","styles":[0,-1,2]}"#;
        assert!(serde_json::from_str::<Line>(json).is_err());
        let json = r#"{"text":"x","styles":[0,1,-2]}"#;
        assert!(serde_json::from_str::<Line>(json).is_err());
    }

    #[test]
    fn empty_styles() {
        let line: Line = serde_json::from_str(r#"{"text":"x"}"#).unwrap();
        assert!(line.styles.is_empty());
    }

    #[test]
    fn null_styles_are_empty() {
        let line: Line = serde_json::from_str(r#"{"text":"x","styles":null}"#).unwrap();
        assert!(line.styles.is_empty());
    }
}
