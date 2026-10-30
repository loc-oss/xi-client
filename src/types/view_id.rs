use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, PartialEq, Eq, Hash, Copy, Clone, Ord, PartialOrd)]
pub struct ViewId(pub usize);

impl FromStr for ViewId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let stripped = s.strip_prefix("view-id-").ok_or("expected view-id-N")?;
        Ok(ViewId(stripped.parse().map_err(|e: std::num::ParseIntError| e.to_string())?))
    }
}

impl fmt::Display for ViewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "view-id-{}", self.0)
    }
}

impl Serialize for ViewId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ViewId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ViewId::from_str(&s).map_err(serde::de::Error::custom)
    }
}
