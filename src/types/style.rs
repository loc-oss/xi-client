use serde::{Deserialize, Serialize};

/// A style definition, as delivered by a `def_style` notification.
///
/// See `docs/protocol.md` §8.8. `Line.styles[].style_id` references
/// [`Style::id`]. Colors are ARGB in word order (`0xAARRGGBB`).
///
/// This type is only ever deserialized from xi-core. The `Serialize` impl
/// exists for debugging and round-trip tests; the wire never goes the other
/// way, so omitting `None` fields is safe.
#[derive(Default, Deserialize, Serialize, Debug, PartialEq, Clone)]
pub struct Style {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_color: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg_color: Option<u32>,
    /// Weight in 100..900 (`400` = normal, `700` = bold). xi-core does not
    /// validate the range; consumers should clamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_full_payload() {
        let json = r#"{
            "id": 8,
            "fg_color": 4294967295,
            "bg_color": 0,
            "weight": 700,
            "italic": true,
            "underline": false
        }"#;
        let style: Style = serde_json::from_str(json).unwrap();
        assert_eq!(style.id, 8);
        assert_eq!(style.fg_color, Some(u32::MAX));
        assert_eq!(style.bg_color, Some(0));
        assert_eq!(style.weight, Some(700));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(false));
    }

    #[test]
    fn missing_optionals_default_to_none() {
        let style: Style = serde_json::from_str(r#"{"id": 3}"#).unwrap();
        assert_eq!(style.id, 3);
        assert_eq!(style.fg_color, None);
        assert_eq!(style.italic, None);
    }

    #[test]
    fn null_optionals_become_none() {
        let style: Style =
            serde_json::from_str(r#"{"id": 3, "fg_color": null, "italic": null}"#).unwrap();
        assert_eq!(style.fg_color, None);
        assert_eq!(style.italic, None);
    }
}
