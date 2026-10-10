//! Stop declarations and turn outcomes. The rules that decide them are owned
//! by `jcode-base::session_work::stop`; these are the shapes clients see.
use super::*;
use std::fmt;
use std::str::FromStr;

/// What wakes a waiting session: one item, or `any(...)`/`all(...)` of nested
/// conditions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WaitCondition {
    Item(ItemAlias),
    Any(Vec<WaitCondition>),
    All(Vec<WaitCondition>),
}

impl WaitCondition {
    /// Every alias the condition names, in order of first appearance.
    pub fn aliases(&self) -> Vec<ItemAlias> {
        fn visit(condition: &WaitCondition, out: &mut Vec<ItemAlias>) {
            match condition {
                WaitCondition::Item(alias) => {
                    if !out.contains(alias) {
                        out.push(*alias);
                    }
                }
                WaitCondition::Any(children) | WaitCondition::All(children) => {
                    for child in children {
                        visit(child, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        visit(self, &mut out);
        out
    }

    /// Whether the condition holds when exactly the `finished` items have
    /// reached a final state.
    pub fn is_met(&self, finished: &dyn Fn(ItemAlias) -> bool) -> bool {
        match self {
            Self::Item(alias) => finished(*alias),
            Self::Any(children) => children.iter().any(|child| child.is_met(finished)),
            Self::All(children) => children.iter().all(|child| child.is_met(finished)),
        }
    }
}

impl fmt::Display for WaitCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, children) = match self {
            Self::Item(alias) => return write!(f, "{alias}"),
            Self::Any(children) => ("any", children),
            Self::All(children) => ("all", children),
        };
        write!(f, "{name}(")?;
        for (index, child) in children.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{child}")?;
        }
        f.write_str(")")
    }
}

/// Why condition text does not parse. `position` is a one-based character
/// position in the text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionError {
    pub position: usize,
    pub detail: String,
}

impl fmt::Display for ConditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "condition syntax at position {}: {}",
            self.position, self.detail
        )
    }
}

impl std::error::Error for ConditionError {}

impl FromStr for WaitCondition {
    type Err = ConditionError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let chars: Vec<char> = text.chars().collect();
        let mut parser = ConditionParser { chars, at: 0 };
        parser.skip_space();
        if parser.at == parser.chars.len() {
            return Err(parser.error("expected an alias such as `q1`, or `any(...)`/`all(...)`"));
        }
        let condition = parser.condition()?;
        parser.skip_space();
        if parser.at != parser.chars.len() {
            return Err(parser.error("expected the end of the condition"));
        }
        Ok(condition)
    }
}

struct ConditionParser {
    chars: Vec<char>,
    at: usize,
}

impl ConditionParser {
    fn error(&self, detail: &str) -> ConditionError {
        ConditionError {
            position: self.at + 1,
            detail: detail.to_string(),
        }
    }

    fn skip_space(&mut self) {
        while self.chars.get(self.at).is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
    }

    fn word(&mut self) -> String {
        let start = self.at;
        while self
            .chars
            .get(self.at)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_')
        {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect()
    }

    fn condition(&mut self) -> Result<WaitCondition, ConditionError> {
        self.skip_space();
        let start = self.at;
        let word = self.word();
        if word.is_empty() {
            return Err(self.error("expected an alias such as `q1`, or `any(...)`/`all(...)`"));
        }
        self.skip_space();
        let is_group = matches!(word.as_str(), "any" | "all");
        if !is_group {
            if self.chars.get(self.at) == Some(&'(') {
                let at = self.at;
                self.at = start;
                let error = self.error(&format!(
                    "`{word}` is not a condition; use `any(...)` or `all(...)`"
                ));
                self.at = at;
                return Err(error);
            }
            return word
                .parse::<ItemAlias>()
                .map(WaitCondition::Item)
                .map_err(|error| ConditionError {
                    position: start + 1,
                    detail: error.to_string(),
                });
        }
        if self.chars.get(self.at) != Some(&'(') {
            return Err(self.error(&format!("expected `(` after `{word}`")));
        }
        self.at += 1;
        let mut children = Vec::new();
        loop {
            self.skip_space();
            if self.chars.get(self.at) == Some(&')') {
                if children.is_empty() {
                    return Err(self.error(&format!("`{word}(...)` needs at least one item")));
                }
                return Err(self.error("expected an item after `,`"));
            }
            children.push(self.condition()?);
            self.skip_space();
            match self.chars.get(self.at) {
                Some(',') => self.at += 1,
                Some(')') => {
                    self.at += 1;
                    break;
                }
                _ => return Err(self.error("expected `,` or `)`")),
            }
        }
        Ok(if word == "any" {
            WaitCondition::Any(children)
        } else {
            WaitCondition::All(children)
        })
    }
}

impl Serialize for WaitCondition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WaitCondition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A wait's time limit. Expiry never stops or touches the awaited work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitTimeout {
    /// A positive duration in whole seconds.
    After(u64),
    /// No limit.
    Never,
}

impl WaitTimeout {
    /// The shorter limit; `Never` is longest.
    pub fn shorter(self, other: Self) -> Self {
        match (self, other) {
            (Self::After(a), Self::After(b)) => Self::After(a.min(b)),
            (Self::Never, other) | (other, Self::Never) => other,
        }
    }
    /// The longer limit; `Never` is longest.
    pub fn longer(self, other: Self) -> Self {
        match (self, other) {
            (Self::After(a), Self::After(b)) => Self::After(a.max(b)),
            _ => Self::Never,
        }
    }
}

impl fmt::Display for WaitTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::After(mut seconds) = *self else {
            return f.write_str("none");
        };
        for (unit, size) in [("d", 86_400u64), ("h", 3_600), ("m", 60), ("s", 1)] {
            if seconds >= size {
                write!(f, "{}{unit}", seconds / size)?;
                seconds %= size;
            }
        }
        Ok(())
    }
}

impl FromStr for WaitTimeout {
    type Err = String;
    /// `none`, or a sequence of whole amounts with units `d`, `h`, `m`, `s`,
    /// such as `45m` or `1h30m`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("none") {
            return Ok(Self::Never);
        }
        let invalid = || {
            format!(
                "`{text}` is not a duration; use a value such as `45m`, `2h`, `1h30m` or `none`"
            )
        };
        let mut total: u64 = 0;
        let mut digits = String::new();
        let mut parts = 0;
        for c in text.chars() {
            if c.is_ascii_digit() {
                digits.push(c);
                continue;
            }
            let size = match c {
                'd' => 86_400,
                'h' => 3_600,
                'm' => 60,
                's' => 1,
                _ => return Err(invalid()),
            };
            let amount: u64 = digits.parse().map_err(|_| invalid())?;
            digits.clear();
            total = amount
                .checked_mul(size)
                .and_then(|value| total.checked_add(value))
                .ok_or_else(invalid)?;
            parts += 1;
        }
        if !digits.is_empty() || parts == 0 {
            return Err(invalid());
        }
        if total == 0 {
            return Err(format!(
                "`{text}` is zero; use a positive duration or `none`"
            ));
        }
        Ok(Self::After(total))
    }
}

impl Serialize for WaitTimeout {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WaitTimeout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Where a wait's time limit came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutSource {
    Explicit,
    Default,
}

impl TimeoutSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Default => "default",
        }
    }
}

/// What a declaration says about the stop that follows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Declaration {
    /// Pausing until the condition is met.
    Wait {
        condition: WaitCondition,
        timeout: WaitTimeout,
        timeout_source: TimeoutSource,
        summary: String,
    },
    /// The session's work is done; with a workflow, this is how it is
    /// marked complete.
    Complete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        leave_running: Vec<ItemAlias>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        closeout: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        documents: Vec<String>,
        /// The agent's record that Mirza accepted the result. Never verified.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        accepted: bool,
    },
}

impl Declaration {
    pub const fn is_wait(&self) -> bool {
        matches!(self, Self::Wait { .. })
    }
}

/// Why a declaration stopped applying before it was satisfied.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawCause {
    /// The agent withdrew it to keep working.
    Agent,
    /// A new declaration replaced it.
    Replaced,
    /// An awaited item arrived.
    AwaitedArrival,
    /// Mirza revoked it.
    Revoked,
    /// The wait's time limit passed.
    Timeout,
}

/// Where a declaration is in its life.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeclarationState {
    /// Declared in a turn whose final message has not ended yet.
    Pending,
    /// The turn ended with the declaration valid.
    Effective,
    Withdrawn {
        cause: WithdrawCause,
    },
    /// It no longer held when its turn ended.
    Invalidated {
        reason: String,
    },
}

/// One declaration and what became of it. `number` is per session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopRecord {
    pub number: u32,
    pub turn: u32,
    pub declaration: Declaration,
    pub state: DeclarationState,
    pub declared_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The final message that ended the declaring turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_text: Option<String>,
    /// The workflow revision a completion covers. A later revision means
    /// the workflow changed after it was declared complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision: Option<u32>,
}

impl StopRecord {
    /// A declaration that still governs the session: pending, or an effective
    /// wait.
    pub fn is_current(&self) -> bool {
        match self.state {
            DeclarationState::Pending => true,
            DeclarationState::Effective => self.declaration.is_wait(),
            _ => false,
        }
    }
}

/// The counter that ran out.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustedCounter {
    /// Stops without a declaration while the workflow was unfinished.
    Missing,
    /// Declarations that no longer held when the turn ended.
    Invalid,
    /// Tool calls refused under a declaration.
    Refused,
}

/// Coarse classes of a turn that ended with an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// The provider failed or rejected the request.
    Provider,
    /// The request was blocked before dispatch, for example by context pressure.
    Preflight,
    Other,
}

/// How a turn ended. Kept apart from runtime status, interactions and
/// closeout.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum TurnOutcome {
    Completed { declared: bool },
    Waiting { stop: u32 },
    InterruptedByUser,
    InterruptedByReload,
    ProviderRefusal,
    Truncated,
    Failed { class: FailureClass },
    StoppedWithoutDeclaration { cause: ExhaustedCounter },
}

impl TurnOutcome {
    /// Whether the turn is over. A turn interrupted by a reload is continued
    /// by the next turn of its session, which keeps its counters.
    pub const fn settles_turn(&self) -> bool {
        !matches!(self, Self::InterruptedByReload)
    }
}

/// Per-turn correction limits. Each counts the corrections or refusals given;
/// the next occurrence after the limit ends the turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorrectionLimits {
    pub missing: u32,
    pub invalid: u32,
    pub refused: u32,
}

impl Default for CorrectionLimits {
    fn default() -> Self {
        Self {
            missing: 2,
            invalid: 3,
            refused: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(text: &str) -> ItemAlias {
        text.parse().unwrap()
    }

    #[test]
    fn conditions_parse_nest_and_print_canonically() {
        let condition: WaitCondition = " any( q1 ,all(t3,t4), p2.done ) ".parse().unwrap();
        assert_eq!(condition.to_string(), "any(q1, all(t3, t4), p2.done)");
        assert_eq!(
            condition.aliases(),
            vec![alias("q1"), alias("t3"), alias("t4"), alias("p2.done")]
        );
        assert_eq!(
            "c1".parse::<WaitCondition>().unwrap(),
            WaitCondition::Item(alias("c1"))
        );
        let json = serde_json::to_string(&condition).unwrap();
        assert_eq!(
            serde_json::from_str::<WaitCondition>(&json).unwrap(),
            condition
        );
    }

    #[test]
    fn condition_errors_name_the_position() {
        for (text, position) in [
            ("", 1),
            ("any(q1, t3", 11),
            ("any(q1 t3)", 8),
            ("any()", 5),
            ("any(q1,)", 8),
            ("x1", 1),
            ("or(q1)", 1),
            ("q1)", 3),
            ("all q1", 5),
        ] {
            let error = text.parse::<WaitCondition>().unwrap_err();
            assert_eq!(error.position, position, "{text:?}: {error}");
        }
    }

    #[test]
    fn conditions_evaluate_any_and_all() {
        let condition: WaitCondition = "any(q1, all(t3, t4))".parse().unwrap();
        let only = |done: &'static [&'static str]| {
            move |alias: ItemAlias| done.contains(&alias.to_string().as_str())
        };
        assert!(condition.is_met(&only(&["q1"])));
        assert!(!condition.is_met(&only(&["t3"])));
        assert!(condition.is_met(&only(&["t3", "t4"])));
    }

    #[test]
    fn timeouts_parse_compose_and_print() {
        assert_eq!(
            "45m".parse::<WaitTimeout>().unwrap(),
            WaitTimeout::After(2700)
        );
        assert_eq!("1h30m".parse::<WaitTimeout>().unwrap().to_string(), "1h30m");
        assert_eq!("none".parse::<WaitTimeout>().unwrap(), WaitTimeout::Never);
        assert_eq!(WaitTimeout::After(86_400).to_string(), "1d");
        for bad in ["", "45", "m", "0m", "5x", "-1m", "1.5h"] {
            assert!(bad.parse::<WaitTimeout>().is_err(), "{bad:?}");
        }
        let short = WaitTimeout::After(60);
        assert_eq!(short.shorter(WaitTimeout::Never), short);
        assert_eq!(short.longer(WaitTimeout::Never), WaitTimeout::Never);
        assert_eq!(
            short.longer(WaitTimeout::After(120)),
            WaitTimeout::After(120)
        );
    }

    #[test]
    fn records_round_trip_and_reject_unknown_fields() {
        let record = StopRecord {
            number: 1,
            turn: 2,
            declaration: Declaration::Wait {
                condition: "any(q1, t2)".parse().unwrap(),
                timeout: WaitTimeout::After(1800),
                timeout_source: TimeoutSource::Default,
                summary: "synthetic".into(),
            },
            state: DeclarationState::Withdrawn {
                cause: WithdrawCause::Agent,
            },
            declared_at: chrono::Utc::now(),
            settled_at: None,
            final_text: None,
            workflow_revision: None,
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            serde_json::from_value::<StopRecord>(json.clone()).unwrap(),
            record
        );
        let mut extended = json;
        extended["future"] = serde_json::json!(1);
        assert!(serde_json::from_value::<StopRecord>(extended).is_err());
        for outcome in [
            TurnOutcome::Completed { declared: true },
            TurnOutcome::StoppedWithoutDeclaration {
                cause: ExhaustedCounter::Refused,
            },
            TurnOutcome::Failed {
                class: FailureClass::Provider,
            },
            TurnOutcome::InterruptedByReload,
        ] {
            let json = serde_json::to_string(&outcome).unwrap();
            assert_eq!(serde_json::from_str::<TurnOutcome>(&json).unwrap(), outcome);
        }
        assert!(!TurnOutcome::InterruptedByReload.settles_turn());
        assert!(TurnOutcome::InterruptedByUser.settles_turn());
    }
}

/// Default wait time limits by the kind of awaited item. `fallback` applies
/// to kinds without their own default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaitTimeoutDefaults {
    pub background_task: WaitTimeout,
    pub subagent: WaitTimeout,
    pub question: WaitTimeout,
    pub artifact: WaitTimeout,
    pub proposal: WaitTimeout,
    pub launched_session: WaitTimeout,
    pub fallback: WaitTimeout,
}

impl Default for WaitTimeoutDefaults {
    fn default() -> Self {
        Self {
            background_task: WaitTimeout::After(30 * 60),
            subagent: WaitTimeout::After(45 * 60),
            question: WaitTimeout::After(2 * 3_600),
            artifact: WaitTimeout::After(24 * 3_600),
            proposal: WaitTimeout::After(24 * 3_600),
            launched_session: WaitTimeout::Never,
            fallback: WaitTimeout::After(30 * 60),
        }
    }
}

impl WaitTimeoutDefaults {
    pub fn for_kind(&self, kind: ItemKind) -> WaitTimeout {
        match kind {
            ItemKind::Task => self.background_task,
            ItemKind::Child => self.subagent,
            ItemKind::Question => self.question,
            ItemKind::Artifact => self.artifact,
            ItemKind::Proposal => self.proposal,
            ItemKind::ProposalDone => self.launched_session,
            ItemKind::Resume => self.fallback,
        }
    }

    /// The default limit of a condition: `all(...)` takes its longest child,
    /// `any(...)` its shortest, recursively.
    pub fn for_condition(&self, condition: &WaitCondition) -> WaitTimeout {
        match condition {
            WaitCondition::Item(alias) => self.for_kind(alias.kind()),
            WaitCondition::Any(children) => children
                .iter()
                .map(|child| self.for_condition(child))
                .reduce(WaitTimeout::shorter)
                .unwrap_or(self.fallback),
            WaitCondition::All(children) => children
                .iter()
                .map(|child| self.for_condition(child))
                .reduce(WaitTimeout::longer)
                .unwrap_or(self.fallback),
        }
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;

    #[test]
    fn composed_defaults_take_the_shortest_any_and_the_longest_all() {
        let defaults = WaitTimeoutDefaults::default();
        let resolve = |text: &str| defaults.for_condition(&text.parse().unwrap());
        assert_eq!(resolve("q1"), WaitTimeout::After(7_200));
        assert_eq!(resolve("any(q1, t3)"), WaitTimeout::After(1_800));
        assert_eq!(resolve("all(q1, t3)"), WaitTimeout::After(7_200));
        assert_eq!(resolve("any(p1.done, c1)"), WaitTimeout::After(2_700));
        assert_eq!(resolve("all(p1.done, c1)"), WaitTimeout::Never);
        assert_eq!(resolve("any(all(t1, a1), r1)"), WaitTimeout::After(1_800));
        assert_eq!(resolve("r1"), defaults.fallback);
    }
}
