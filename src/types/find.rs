use serde::{Deserialize, Serialize};

/// A find query.
///
/// See `docs/protocol.md` §6.2.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindQuery {
    /// Query id. `None` when sending a new query; xi-core echoes it back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub chars: String,
    pub case_sensitive: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub regex: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub whole_words: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl FindQuery {
    /// Builds a plain text query (case-insensitive, no regex, no whole-word).
    pub fn new(chars: impl Into<String>) -> Self {
        Self {
            id: None,
            chars: chars.into(),
            case_sensitive: false,
            regex: false,
            whole_words: false,
        }
    }

    pub fn case_sensitive(mut self, on: bool) -> Self {
        self.case_sensitive = on;
        self
    }

    pub fn regex(mut self, on: bool) -> Self {
        self.regex = on;
        self
    }

    pub fn whole_words(mut self, on: bool) -> Self {
        self.whole_words = on;
        self
    }

    pub fn with_id(mut self, id: u64) -> Self {
        self.id = Some(id);
        self
    }
}

/// Where to put the cursor after a successful find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifySelection {
    None,
    #[default]
    Set,
    Add,
    AddRemovingCurrent,
}

/// Options for `find_next` / `find_previous`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FindOptions {
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrap_around: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_same: bool,
    #[serde(default, skip_serializing_if = "is_default_modify")]
    pub modify_selection: ModifySelection,
}

fn is_default_modify(m: &ModifySelection) -> bool {
    *m == ModifySelection::Set
}

impl FindOptions {
    pub fn wrap_around(mut self, on: bool) -> Self {
        self.wrap_around = on;
        self
    }

    pub fn allow_same(mut self, on: bool) -> Self {
        self.allow_same = on;
        self
    }

    pub fn modify_selection(mut self, m: ModifySelection) -> Self {
        self.modify_selection = m;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn find_query_minimal() {
        let q = FindQuery::new("hello");
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"chars": "hello", "case_sensitive": false})
        );
    }

    #[test]
    fn find_query_full() {
        let q = FindQuery::new("h.llo")
            .regex(true)
            .case_sensitive(true)
            .whole_words(true);
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({
                "chars": "h.llo",
                "case_sensitive": true,
                "regex": true,
                "whole_words": true
            })
        );
    }

    #[test]
    fn find_query_with_id() {
        let q = FindQuery::new("x").with_id(7);
        assert_eq!(serde_json::to_value(&q).unwrap()["id"], json!(7));
    }

    #[test]
    fn find_options_default_is_empty() {
        let o = FindOptions::default();
        assert_eq!(serde_json::to_value(&o).unwrap(), json!({}));
    }

    #[test]
    fn find_options_full() {
        let o = FindOptions::default()
            .wrap_around(true)
            .allow_same(true)
            .modify_selection(ModifySelection::Add);
        assert_eq!(
            serde_json::to_value(&o).unwrap(),
            json!({
                "wrap_around": true,
                "allow_same": true,
                "modify_selection": "add"
            })
        );
    }

    #[test]
    fn modify_selection_wire_names() {
        for (variant, wire) in [
            (ModifySelection::None, "none"),
            (ModifySelection::Set, "set"),
            (ModifySelection::Add, "add"),
            (ModifySelection::AddRemovingCurrent, "add_removing_current"),
        ] {
            assert_eq!(serde_json::to_value(variant).unwrap(), json!(wire));
        }
    }
}
