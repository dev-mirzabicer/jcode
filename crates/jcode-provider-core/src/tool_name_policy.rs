//! The one declared place where a provider may rename a registry tool.
//!
//! Every provider receives jcode's registry tool names unchanged unless an
//! entry here says otherwise. An entry exists only to answer a concrete
//! provider constraint, cites the evidence for it, renames exactly one tool
//! bijectively, and never touches a description or schema: those always come
//! from the registry. Tools without an entry, including every new or modified
//! tool, keep their registry identity with no further decision.
//!
//! The Anthropic table is empty. The INT-01 Gate 0 probe (2026-09-29) showed
//! the Claude OAuth endpoint accepts every registry name, so the Claude Code
//! capitalized names jcode used to send are no longer needed. The permanent
//! provider-parity test enumerates each table through [`validate`].
//!
//! [`validate`]: ProviderToolNamePolicy::validate

use std::collections::HashSet;

/// One evidence-cited rename of a registry tool for one provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderToolNameEntry {
    pub registry_name: &'static str,
    pub wire_name: &'static str,
    /// The provider error or documentation this entry answers.
    pub evidence: &'static str,
}

/// A provider's complete tool-name translation table.
#[derive(Clone, Copy, Debug)]
pub struct ProviderToolNamePolicy {
    provider: &'static str,
    entries: &'static [ProviderToolNameEntry],
}

/// Anthropic Messages API, both OAuth and API-key routes. Empty: registry
/// names are sent unchanged (INT-01 decision D3, Gate 0 G0.1).
pub const ANTHROPIC_TOOL_NAME_POLICY: ProviderToolNamePolicy =
    ProviderToolNamePolicy::new("anthropic", &[]);

impl ProviderToolNamePolicy {
    pub const fn new(provider: &'static str, entries: &'static [ProviderToolNameEntry]) -> Self {
        Self { provider, entries }
    }

    pub fn provider(&self) -> &'static str {
        self.provider
    }

    pub fn entries(&self) -> &'static [ProviderToolNameEntry] {
        self.entries
    }

    /// The name the provider sees for a registry tool.
    pub fn wire_name<'a>(&self, registry_name: &'a str) -> &'a str {
        self.entries
            .iter()
            .find(|entry| entry.registry_name == registry_name)
            .map_or(registry_name, |entry| entry.wire_name)
    }

    /// The registry tool a provider-emitted name refers to.
    pub fn registry_name<'a>(&self, wire_name: &'a str) -> &'a str {
        self.entries
            .iter()
            .find(|entry| entry.wire_name == wire_name)
            .map_or(wire_name, |entry| entry.registry_name)
    }

    /// Check every rule an entry must satisfy against the tools actually
    /// registered. Returns one message per violation.
    pub fn validate(&self, registry_names: &[&str]) -> Result<(), Vec<String>> {
        let registry: HashSet<&str> = registry_names.iter().copied().collect();
        let mut errors = Vec::new();
        let mut registry_seen = HashSet::new();
        let mut wire_seen = HashSet::new();
        for entry in self.entries {
            let label = format!(
                "{} entry {} -> {}",
                self.provider, entry.registry_name, entry.wire_name
            );
            if entry.evidence.trim().is_empty() {
                errors.push(format!("{label} cites no evidence"));
            }
            if entry.registry_name == entry.wire_name {
                errors.push(format!("{label} renames nothing"));
            }
            if !registry.contains(entry.registry_name) {
                errors.push(format!("{label} names a tool that is not registered"));
            }
            if !registry_seen.insert(entry.registry_name) {
                errors.push(format!("{label} renames a tool twice"));
            }
            if !wire_seen.insert(entry.wire_name) {
                errors.push(format!("{label} reuses a wire name (not bijective)"));
            }
            if registry.contains(entry.wire_name) {
                errors.push(format!("{label} collides with another registry tool"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVIDENCE: &str = "fixture: provider rejects the registry name";

    #[test]
    fn empty_policy_is_identity_and_valid() {
        assert_eq!(ANTHROPIC_TOOL_NAME_POLICY.wire_name("bash"), "bash");
        assert_eq!(ANTHROPIC_TOOL_NAME_POLICY.registry_name("bash"), "bash");
        assert!(ANTHROPIC_TOOL_NAME_POLICY.entries().is_empty());
        assert_eq!(
            ANTHROPIC_TOOL_NAME_POLICY.validate(&["bash", "read"]),
            Ok(())
        );
    }

    #[test]
    fn an_entry_round_trips_and_leaves_other_tools_alone() {
        static ENTRIES: &[ProviderToolNameEntry] = &[ProviderToolNameEntry {
            registry_name: "bash",
            wire_name: "shell",
            evidence: EVIDENCE,
        }];
        let policy = ProviderToolNamePolicy::new("fixture", ENTRIES);
        assert_eq!(policy.wire_name("bash"), "shell");
        assert_eq!(policy.registry_name("shell"), "bash");
        assert_eq!(policy.wire_name("read"), "read");
        assert_eq!(policy.validate(&["bash", "read"]), Ok(()));
    }

    #[test]
    fn every_rule_is_enforced() {
        static ENTRIES: &[ProviderToolNameEntry] = &[
            ProviderToolNameEntry {
                registry_name: "bash",
                wire_name: "shell",
                evidence: "",
            },
            ProviderToolNameEntry {
                registry_name: "read",
                wire_name: "read",
                evidence: EVIDENCE,
            },
            ProviderToolNameEntry {
                registry_name: "ghost",
                wire_name: "phantom",
                evidence: EVIDENCE,
            },
            ProviderToolNameEntry {
                registry_name: "bash",
                wire_name: "terminal",
                evidence: EVIDENCE,
            },
            ProviderToolNameEntry {
                registry_name: "write",
                wire_name: "shell",
                evidence: EVIDENCE,
            },
            ProviderToolNameEntry {
                registry_name: "edit",
                wire_name: "write",
                evidence: EVIDENCE,
            },
        ];
        let policy = ProviderToolNamePolicy::new("fixture", ENTRIES);
        let errors = policy
            .validate(&["bash", "read", "write", "edit"])
            .expect_err("violations must be reported");
        for fragment in [
            "cites no evidence",
            "renames nothing",
            "not registered",
            "renames a tool twice",
            "not bijective",
            "collides with another registry tool",
        ] {
            assert!(
                errors.iter().any(|error| error.contains(fragment)),
                "missing `{fragment}` in {errors:?}"
            );
        }
    }
}

/// The tool-name rule of a provider's wire API (INT-01/WP-06 R26).
///
/// Built-in tools satisfy every rule and the parity guard checks them.
/// Dynamically named tools (MCP servers name their own) may not: one name a
/// provider rejects makes it reject the whole request. A tool whose wire name
/// breaks the dispatching runtime's rule is therefore never advertised
/// there. A rename that makes it acceptable is a declared
/// [`ProviderToolNamePolicy`] entry, never an ad-hoc edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolNameRule {
    /// The provider, as shown in the notice about a withheld tool.
    pub provider: &'static str,
    /// Longest accepted name, in bytes. Names are ASCII letters, digits,
    /// `_` and `-`.
    pub max_len: usize,
}

/// Anthropic Messages API: `^[a-zA-Z0-9_-]{1,128}$` (API reference; Gate 0
/// G0.1 accepted every registry name).
pub const ANTHROPIC_TOOL_NAME_RULE: ToolNameRule = ToolNameRule {
    provider: "Anthropic",
    max_len: 128,
};

/// OpenAI Responses function tools: `^[a-zA-Z0-9_-]{1,64}$` (API reference).
pub const OPENAI_TOOL_NAME_RULE: ToolNameRule = ToolNameRule {
    provider: "OpenAI",
    max_len: 64,
};

impl ToolNameRule {
    pub fn accepts(&self, wire_name: &str) -> bool {
        (1..=self.max_len).contains(&wire_name.len())
            && wire_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    }

    /// Why `wire_name` is not accepted, for a notice; `None` when it is.
    pub fn violation(&self, wire_name: &str) -> Option<String> {
        (!self.accepts(wire_name)).then(|| {
            format!(
                "{} tool names are 1 to {} characters of letters, digits, `_` and `-`",
                self.provider, self.max_len
            )
        })
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    #[test]
    fn each_provider_rule_bounds_length_and_characters() {
        for rule in [ANTHROPIC_TOOL_NAME_RULE, OPENAI_TOOL_NAME_RULE] {
            assert!(rule.accepts("mcp__server__tool-name_2"));
            assert!(rule.accepts(&"a".repeat(rule.max_len)));
            assert!(!rule.accepts(&"a".repeat(rule.max_len + 1)));
            for invalid in ["", "has space", "dotted.name", "slash/name", "ünïcode"] {
                assert!(!rule.accepts(invalid), "{invalid:?}");
                assert!(rule.violation(invalid).is_some());
            }
            assert_eq!(rule.violation("bash"), None);
        }
        let between = "a".repeat(100);
        assert!(ANTHROPIC_TOOL_NAME_RULE.accepts(&between));
        assert!(!OPENAI_TOOL_NAME_RULE.accepts(&between));
    }
}
