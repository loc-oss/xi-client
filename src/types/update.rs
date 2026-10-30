use serde::{Deserialize, Deserializer};

use super::{Operation, ViewId};

#[derive(Debug, PartialEq, Clone)]
pub struct Update {
    pub operations: Vec<Operation>,
    pub pristine: bool,
    pub view_id: ViewId,
    pub annotations: Vec<Annotation>,
}

#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Annotation {
    #[serde(rename = "type")]
    pub ann_type: String,
    pub ranges: Vec<Vec<u64>>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct InnerUpdate {
    #[serde(rename = "ops")]
    pub operations: Vec<Operation>,
    pub pristine: bool,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Helper {
    pub update: InnerUpdate,
    pub view_id: ViewId,
}

impl<'de> Deserialize<'de> for Update {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Deserialize::deserialize(d).map(|Helper { update, view_id }| Update {
            operations: update.operations,
            pristine: update.pristine,
            view_id,
            annotations: update.annotations,
        })
    }
}
