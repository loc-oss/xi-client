use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;

/// A view identifier.
///
/// On the wire, view ids are strings of the form `"view-id-<n>"` with
/// `n >= 1` (see `docs/protocol.md` §2.4). The SDK hides the string form:
/// you construct, compare, and serialize `ViewId` directly.
#[derive(Debug, PartialEq, Eq, Hash, Copy, Clone, Ord, PartialOrd)]
pub struct ViewId(pub usize);

impl FromStr for ViewId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let stripped = s
            .strip_prefix("view-id-")
            .ok_or_else(|| format!("expected view-id-N, got {s:?}"))?;
        let n = stripped
            .parse::<usize>()
            .map_err(|e| format!("invalid view id {s:?}: {e}"))?;
        Ok(ViewId(n))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_serialize() {
        let id = ViewId(7);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "\"view-id-7\"");
        let back: ViewId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn display_matches_wire() {
        assert_eq!(ViewId(1).to_string(), "view-id-1");
        assert_eq!(ViewId(42).to_string(), "view-id-42");
    }

    #[test]
    fn parses_valid() {
        assert_eq!("view-id-1".parse::<ViewId>().unwrap(), ViewId(1));
        assert_eq!("view-id-0".parse::<ViewId>().unwrap(), ViewId(0));
    }

    #[test]
    fn rejects_bad_prefix() {
        let err = "1".parse::<ViewId>().unwrap_err();
        assert!(err.contains("expected view-id-N"), "got: {err}");
        let err = "view-1".parse::<ViewId>().unwrap_err();
        assert!(err.contains("expected view-id-N"), "got: {err}");
    }

    #[test]
    fn rejects_bad_number() {
        let err = "view-id-".parse::<ViewId>().unwrap_err();
        assert!(err.contains("invalid view id"), "got: {err}");
        let err = "view-id-abc".parse::<ViewId>().unwrap_err();
        assert!(err.contains("invalid view id"), "got: {err}");
        let err = "view-id--1".parse::<ViewId>().unwrap_err();
        assert!(err.contains("invalid view id"), "got: {err}");
    }

    #[test]
    fn rejects_bad_wire_string() {
        let err = serde_json::from_str::<ViewId>("\"nope\"").unwrap_err();
        assert!(err.to_string().contains("expected view-id-N"), "got: {err}");
    }
}
