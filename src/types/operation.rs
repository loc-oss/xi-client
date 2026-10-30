use serde::Deserialize;
use serde_json as json;

use super::line::Line;

#[derive(Deserialize, Debug, PartialEq, Clone)]
pub enum OperationType {
    Copy_,
    Skip,
    Invalidate,
    Update,
    Insert,
}

#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Operation {
    #[serde(rename = "op")]
    #[serde(deserialize_with = "deserialize_operation_type")]
    pub operation_type: OperationType,
    #[serde(rename = "n")]
    pub nb_lines: u64,
    #[serde(rename = "ln")]
    pub line_num: Option<u64>,
    #[serde(default)]
    pub lines: Vec<Line>,
}

fn deserialize_operation_type<'de, D: serde::Deserializer<'de>>(d: D) -> Result<OperationType, D::Error> {
    let v: json::Value = Deserialize::deserialize(d)?;
    match v {
        json::Value::String(ref s) if &*s == "copy" => Ok(OperationType::Copy_),
        json::Value::String(ref s) if &*s == "skip" => Ok(OperationType::Skip),
        json::Value::String(ref s) if &*s == "invalidate" => Ok(OperationType::Invalidate),
        json::Value::String(ref s) if &*s == "update" => Ok(OperationType::Update),
        json::Value::String(ref s) if &*s == "ins" => Ok(OperationType::Insert),
        _ => Err(serde::de::Error::custom("unexpected operation type")),
    }
}
