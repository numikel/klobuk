//! Issue identifiers: `<prefix>-<seq>`.
//!
//! The id doubles as branch name (`feat/<id>`), worktree directory, Herdr agent name and lane,
//! so the grammar is deliberately narrow: `^[a-z][a-z0-9]{0,15}-[1-9][0-9]*$`.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

/// Maximum prefix length (characters).
pub const MAX_PREFIX_LEN: usize = 16;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid issue id `{0}` (expected `<prefix>-<n>`, prefix ^[a-z][a-z0-9]{{0,15}}$, n >= 1 without leading zeros)")]
pub struct ParseIssueIdError(pub String);

/// Returns true when `s` is a valid id prefix.
pub fn is_valid_prefix(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    s.len() <= MAX_PREFIX_LEN && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// A validated issue identifier such as `asf-12`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IssueId(String);

impl IssueId {
    /// Builds an id from a prefix and a sequence number. Panics only on an invalid prefix or
    /// `seq < 1`, which callers must have validated already.
    pub fn new(prefix: &str, seq: i64) -> Self {
        assert!(is_valid_prefix(prefix), "invalid prefix {prefix:?}");
        assert!(seq >= 1, "seq must be >= 1");
        IssueId(format!("{prefix}-{seq}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn split(&self) -> (&str, &str) {
        // Safe: the string was validated on construction.
        self.0.rsplit_once('-').expect("validated id contains '-'")
    }

    pub fn prefix(&self) -> &str {
        self.split().0
    }

    pub fn seq(&self) -> i64 {
        self.split().1.parse().expect("validated id has numeric seq")
    }
}

impl FromStr for IssueId {
    type Err = ParseIssueIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseIssueIdError(s.to_string());
        let (prefix, num) = s.rsplit_once('-').ok_or_else(err)?;
        if !is_valid_prefix(prefix) {
            return Err(err());
        }
        let mut digits = num.chars();
        match digits.next() {
            Some(c) if ('1'..='9').contains(&c) => {}
            _ => return Err(err()),
        }
        if !digits.all(|c| c.is_ascii_digit()) {
            return Err(err());
        }
        num.parse::<i64>().map_err(|_| err())?;
        Ok(IssueId(s.to_string()))
    }
}

impl fmt::Display for IssueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for IssueId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl PartialOrd for IssueId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IssueId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.prefix()
            .cmp(other.prefix())
            .then(self.seq().cmp(&other.seq()))
    }
}

impl AsRef<str> for IssueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_id() {
        let id: IssueId = "asf-12".parse().unwrap();
        assert_eq!(id.prefix(), "asf");
        assert_eq!(id.seq(), 12);
        assert_eq!(id.as_str(), "asf-12");
    }

    #[test]
    fn rejects_bad_ids() {
        for bad in [
            "ASF-1",
            "asf-0",
            "asf-01",
            "asf",
            "a_b-1",
            "-1",
            "asf-",
            "asf-1x",
            "toolongprefix01234-1",
        ] {
            assert!(bad.parse::<IssueId>().is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn roundtrips_display_and_new() {
        let id = IssueId::new("law", 7);
        assert_eq!(id.to_string(), "law-7");
        assert_eq!(id, "law-7".parse().unwrap());
    }

    #[test]
    fn prefix_rules() {
        assert!(is_valid_prefix("asf"));
        assert!(is_valid_prefix("a1b2"));
        assert!(is_valid_prefix("a"));
        assert!(!is_valid_prefix("1asf"));
        assert!(!is_valid_prefix("Asf"));
        assert!(!is_valid_prefix(""));
        assert!(!is_valid_prefix("abcdefghijklmnopq")); // 17 chars
        assert!(is_valid_prefix("abcdefghijklmnop")); // 16 chars
    }

    #[test]
    fn serde_is_transparent() {
        let id = IssueId::new("asf", 3);
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"asf-3\"");
        let back: IssueId = serde_json::from_str("\"asf-3\"").unwrap();
        assert_eq!(back, id);
        assert!(serde_json::from_str::<IssueId>("\"bad id\"").is_err());
    }

    #[test]
    fn ordering_is_by_prefix_then_seq() {
        let mut ids: Vec<IssueId> = ["asf-10", "asf-2", "asf-1"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        ids.sort();
        let s: Vec<String> = ids.iter().map(ToString::to_string).collect();
        assert_eq!(s, ["asf-1", "asf-2", "asf-10"]);
    }
}
