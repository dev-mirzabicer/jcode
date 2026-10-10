//! Per-session item aliases. Everything that can finish later gets a short
//! alias in the result that creates it (`t3`, `q1`, `p2.done`), resolved only
//! within its own session.
use super::*;
use std::fmt;
use std::str::FromStr;

/// What an alias names.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// `t`: a background task.
    Task,
    /// `c`: a background sub-agent run.
    Child,
    /// `q`: a quick question.
    Question,
    /// `a`: a questionnaire or other artifact interaction.
    Artifact,
    /// `p`: a session proposal decision.
    Proposal,
    /// `p<N>.done`: the declared completion of the session launched from `p<N>`.
    ProposalDone,
    /// `r`: a scheduled resume.
    Resume,
}

impl ItemKind {
    /// The alias prefix. `ProposalDone` shares its proposal's prefix and number.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Task => "t",
            Self::Child => "c",
            Self::Question => "q",
            Self::Artifact => "a",
            Self::Proposal | Self::ProposalDone => "p",
            Self::Resume => "r",
        }
    }

    /// Kinds that receive their own numbers. A `ProposalDone` item is named by
    /// the proposal it belongs to.
    pub const fn allocates_numbers(self) -> bool {
        !matches!(self, Self::ProposalDone)
    }

    pub const ALL: [ItemKind; 7] = [
        Self::Task,
        Self::Child,
        Self::Question,
        Self::Artifact,
        Self::Proposal,
        Self::ProposalDone,
        Self::Resume,
    ];
}

/// A per-session alias such as `t3` or `p2.done`. Numbers start at 1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ItemAlias {
    kind: ItemKind,
    number: u32,
}

impl ItemAlias {
    pub fn new(kind: ItemKind, number: u32) -> Result<Self, AliasError> {
        if number == 0 {
            return Err(AliasError::ZeroNumber);
        }
        Ok(Self { kind, number })
    }
    pub const fn kind(self) -> ItemKind {
        self.kind
    }
    pub const fn number(self) -> u32 {
        self.number
    }
}

impl fmt::Display for ItemAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.kind.prefix(), self.number)?;
        if self.kind == ItemKind::ProposalDone {
            f.write_str(".done")?;
        }
        Ok(())
    }
}

/// Why text is not an item alias.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AliasError {
    Empty,
    UnknownPrefix(String),
    InvalidNumber(String),
    ZeroNumber,
}

impl fmt::Display for AliasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("an alias is empty"),
            Self::UnknownPrefix(text) => write!(
                f,
                "`{text}` is not an alias; aliases start with t, c, q, a, p or r"
            ),
            Self::InvalidNumber(text) => {
                write!(
                    f,
                    "`{text}` is not an alias; expected a number after the letter"
                )
            }
            Self::ZeroNumber => f.write_str("alias numbers start at 1"),
        }
    }
}

impl std::error::Error for AliasError {}

impl FromStr for ItemAlias {
    type Err = AliasError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (body, done) = match text.strip_suffix(".done") {
            Some(body) => (body, true),
            None => (text, false),
        };
        let mut chars = body.chars();
        let prefix = chars.next().ok_or(AliasError::Empty)?;
        let kind = match (prefix, done) {
            ('t', false) => ItemKind::Task,
            ('c', false) => ItemKind::Child,
            ('q', false) => ItemKind::Question,
            ('a', false) => ItemKind::Artifact,
            ('p', false) => ItemKind::Proposal,
            ('p', true) => ItemKind::ProposalDone,
            ('r', false) => ItemKind::Resume,
            _ => return Err(AliasError::UnknownPrefix(text.to_string())),
        };
        let digits = chars.as_str();
        if digits.is_empty()
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
            || digits.starts_with('0') && digits.len() > 1
        {
            return Err(AliasError::InvalidNumber(text.to_string()));
        }
        let number = digits
            .parse::<u32>()
            .map_err(|_| AliasError::InvalidNumber(text.to_string()))?;
        Self::new(kind, number)
    }
}

impl Serialize for ItemAlias {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ItemAlias {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// What happens when an item finishes, chosen when it starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishPolicy {
    #[default]
    Wake,
    Hold,
}

/// One allocated item. Its producers record when it finished; what a finish
/// means for the session is decided by the arrival rules, not stored here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionItem {
    pub alias: ItemAlias,
    pub id: uuid::Uuid,
    pub label: String,
    pub policy: FinishPolicy,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_round_trip_through_text_and_json() {
        for (text, kind, number) in [
            ("t3", ItemKind::Task, 3),
            ("c1", ItemKind::Child, 1),
            ("q12", ItemKind::Question, 12),
            ("a2", ItemKind::Artifact, 2),
            ("p4", ItemKind::Proposal, 4),
            ("p4.done", ItemKind::ProposalDone, 4),
            ("r9", ItemKind::Resume, 9),
        ] {
            let alias: ItemAlias = text.parse().unwrap();
            assert_eq!((alias.kind(), alias.number()), (kind, number));
            assert_eq!(alias.to_string(), text);
            let json = serde_json::to_string(&alias).unwrap();
            assert_eq!(json, format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<ItemAlias>(&json).unwrap(), alias);
        }
    }

    #[test]
    fn malformed_aliases_are_rejected() {
        for text in [
            "", "x1", "t", "t0", "t01", "q-1", "t1.done", "c2.done", "pp1", "T1", "t1 ", "p.done",
        ] {
            assert!(text.parse::<ItemAlias>().is_err(), "{text:?} parsed");
        }
        assert!(ItemAlias::new(ItemKind::Task, 0).is_err());
        assert!(serde_json::from_str::<ItemAlias>("\"z1\"").is_err());
    }

    #[test]
    fn proposal_completion_shares_its_proposal_number() {
        assert!(!ItemKind::ProposalDone.allocates_numbers());
        assert!(
            ItemKind::ALL
                .iter()
                .filter(|kind| **kind != ItemKind::ProposalDone)
                .all(|kind| kind.allocates_numbers())
        );
    }
}
