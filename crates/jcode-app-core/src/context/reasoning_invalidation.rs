//! Replayed reasoning the provider can no longer use (INT-01 DESIGN §4.3 and
//! §5; Mirza's decisions D-WP04-1 and D16).
//!
//! Some models bind each replayed reasoning block to the request prefix that
//! produced it: the system prompt, the tool set and every earlier message
//! (Claude preserved thinking). A context edit, a revert or reapply, or a
//! changed system prompt or tool set can leave such a block bound to a prefix
//! the next request no longer has. The provider would then reject or silently
//! drop it.
//!
//! jcode never lets that happen silently. One jcode-managed
//! reasoning-invalidation transaction holds every block that must not be
//! sent, grouped by cause. This module is its only writer. When the set
//! changes it supersedes the previous managed transaction and applies the new
//! one at the same revision. The set has two parts:
//!
//! - **Derived.** On a route whose model binds reasoning, the set is
//!   recomputed whole from the recorded bindings at every context transition
//!   and before every request (`Provider::replayed_reasoning_invalidations`,
//!   decided on the exact request the runtime would build). Recomputing keeps
//!   revert and reapply exact: a block comes back only when its binding
//!   proves it valid again, and the same recompute suppresses every block
//!   produced while it was absent.
//! - **Provider-reported.** Blocks the provider dropped or rejected cannot be
//!   derived from bindings, so they stay suppressed for the rest of the
//!   conversation (until a clear or a new session) and stack with the derived
//!   part.
//!
//! A route or model that does not bind reasoning changes nothing in the set:
//! it neither lifts nor adds, so returning to a binding model finds the set as
//! it was (D16, INT-01/WP-06 R21). Only a provider report adds to it there.

use crate::message::{Message, ToolDefinition};
use crate::protocol::{ContextReasoningInvalidationSummary, ContextServiceError};
use crate::provider::Provider;
use chrono::Utc;
use jcode_context_core::{
    ContextTargetIndex, ProjectedMessageSource, build_content_target, context_block_kind,
    estimate_content_block_tokens, project_context,
};
use jcode_session_types::{
    StoredContentTarget, StoredContextAuthorization, StoredContextBlockKind,
    StoredContextOperation, StoredContextStatusEvent, StoredContextTransaction,
    StoredContextTransactionStatusKind, StoredContextTransitionKind, StoredContextViewState,
    StoredMessage, StoredReasoningInvalidationCause, StoredReasoningSelection,
    StoredReasoningSuppression,
};
use std::collections::{BTreeMap, BTreeSet};

/// What the next provider request carries before its messages, and the
/// recorded harness transitions that changed it since the previous request.
#[derive(Clone, Debug)]
pub struct ContextRequestPrefix {
    /// The static system prompt, exactly as the request loop passes it.
    pub system: String,
    /// The tool definitions, exactly as the request loop passes them.
    pub tools: Vec<ToolDefinition>,
    /// Labels of recorded prompt or tool-set transitions since the previous
    /// request (for example "skill activation"). They name the cause of
    /// reasoning that stopped matching for a reason other than a context edit.
    pub recorded_transitions: Vec<String>,
}

impl ContextRequestPrefix {
    /// An empty prefix for tests whose provider does not bind reasoning.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            system: String::new(),
            tools: Vec::new(),
            recorded_transitions: Vec::new(),
        }
    }

    /// Fingerprint of the system prompt and tool set. A reviewed draft keeps
    /// it so apply can refuse a review computed under a different prefix.
    pub fn digest(&self) -> u64 {
        jcode_provider_core::stable_hash_json(&(&self.system, &self.tools))
    }
}

/// Supplies the next request's prefix on demand. Reading it composes the
/// system prompt and tool set, so the reasoning owner asks for it only when
/// the route binds replayed reasoning to its prefix and the transcript holds
/// such reasoning, or a managed set must be revisited.
pub trait RequestPrefixSource {
    fn request_prefix(&self) -> Result<ContextRequestPrefix, ContextServiceError>;
}

impl RequestPrefixSource for ContextRequestPrefix {
    fn request_prefix(&self) -> Result<ContextRequestPrefix, ContextServiceError> {
        Ok(self.clone())
    }
}

/// A prefix captured up front, or none because none was needed then. The
/// same provider and transcript need none later either; a change to them
/// makes the draft stale before this is read.
impl RequestPrefixSource for Option<ContextRequestPrefix> {
    fn request_prefix(&self) -> Result<ContextRequestPrefix, ContextServiceError> {
        self.clone().ok_or_else(|| {
            ContextServiceError::Stale(
                "the next request's prefix was not captured with this draft".to_string(),
            )
        })
    }
}

/// Replayed reasoning blocks the provider reported it would not use: dropped
/// from a request, or named by a rejection. `block_ids` are in the runtime's
/// `Provider::replayed_reasoning_block_id` scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderReportedReasoning {
    pub block_ids: Vec<String>,
    pub reason: String,
}

/// Whether replayed reasoning must be checked for this provider, transcript
/// and state: the route binds reasoning and the transcript holds bound
/// reasoning.
pub fn reasoning_reconciliation_needed(
    provider: &dyn Provider,
    messages: &[StoredMessage],
    _state: &StoredContextViewState,
) -> bool {
    messages
        .iter()
        .any(|message| message.content.iter().any(is_bound_reasoning))
        && provider
            .replayed_reasoning_invalidations(&[], &[], "")
            .is_some()
}

/// A recomputed state and what changed for replayed reasoning.
pub struct ReasoningInvalidationOutcome {
    pub state: StoredContextViewState,
    /// `None` when the route does not bind reasoning and nothing was held.
    pub summary: Option<ContextReasoningInvalidationSummary>,
    /// Whether the managed transaction was replaced.
    pub changed: bool,
}

/// Recompute the managed set for a context transition (apply, revert or
/// reapply). `proposed` carries the transition's status event at
/// `proposed.revision`; any managed change is recorded at that revision.
/// Blocks the transition itself invalidates carry its cause; blocks that were
/// already invalid before it receive a request-prefix cause.
pub(crate) fn stage_for_transition(
    provider: &dyn Provider,
    messages: &[StoredMessage],
    previous: &StoredContextViewState,
    proposed: StoredContextViewState,
    prefix: &dyn RequestPrefixSource,
    transaction_id: &str,
    transition: StoredContextTransitionKind,
) -> Result<ReasoningInvalidationOutcome, ContextServiceError> {
    if !reasoning_reconciliation_needed(provider, messages, previous) {
        return Ok(ReasoningInvalidationOutcome {
            state: proposed,
            summary: None,
            changed: false,
        });
    }
    let prefix = prefix.request_prefix()?;
    let revision = proposed.revision;
    let before = invalid_blocks(provider, messages, &without_managed(previous), &prefix)?;
    recompute(
        provider,
        messages,
        proposed,
        &prefix,
        Trigger::Transition {
            cause: StoredReasoningInvalidationCause::ContextTransition {
                transaction_id: transaction_id.to_string(),
                transition,
            },
            invalid_before: before.invalid.unwrap_or_default(),
        },
        revision,
    )
}

/// Recompute the managed set before a provider request, adding what the
/// provider reported since the previous one. Returns `None` when nothing
/// changes; otherwise the state at the next revision.
pub fn reconcile_before_request(
    provider: &dyn Provider,
    messages: &[StoredMessage],
    state: &StoredContextViewState,
    prefix: &ContextRequestPrefix,
    reported: &[ProviderReportedReasoning],
) -> Result<Option<ReasoningInvalidationOutcome>, ContextServiceError> {
    let reported = reported_keys(provider, messages, reported);
    if reported.is_empty() && !reasoning_reconciliation_needed(provider, messages, state) {
        return Ok(None);
    }
    let revision = state
        .revision
        .checked_add(1)
        .ok_or(ContextServiceError::RevisionOverflow)?;
    let outcome = recompute(
        provider,
        messages,
        state.clone(),
        prefix,
        Trigger::Request { reported },
        revision,
    )?;
    Ok(outcome.changed.then_some(outcome))
}

/// The stored blocks the provider's reports name, each with its reason. An
/// identity no stored block carries any more (a rewind removed it) is dropped.
fn reported_keys(
    provider: &dyn Provider,
    messages: &[StoredMessage],
    reported: &[ProviderReportedReasoning],
) -> BTreeMap<BlockKey, String> {
    if reported.is_empty() {
        return BTreeMap::new();
    }
    let mut keys = BTreeMap::new();
    for (message_index, message) in messages.iter().enumerate() {
        for (block_index, block) in message.content.iter().enumerate() {
            let Some(id) = provider.replayed_reasoning_block_id(block) else {
                continue;
            };
            if let Some(report) = reported
                .iter()
                .find(|report| report.block_ids.contains(&id))
            {
                keys.entry((message_index, block_index))
                    .or_insert_with(|| report.reason.clone());
            }
        }
    }
    keys
}

/// One-line account of a change for notices, logs and the cache-invalidation
/// journal. `None` when nothing was suppressed or restored.
pub fn describe_reasoning_invalidation(
    summary: &ContextReasoningInvalidationSummary,
) -> Option<String> {
    if !summary.changes_anything() {
        return None;
    }
    let mut parts = Vec::new();
    let suppressed = summary.invalidated_by_change + summary.invalidated_other;
    if suppressed > 0 {
        parts.push(format!(
            "{suppressed} Claude thinking block(s) can no longer be used and are suppressed"
        ));
    }
    if summary.restored > 0 {
        parts.push(format!(
            "{} suppressed thinking block(s) match again and are replayed",
            summary.restored
        ));
    }
    Some(format!("{} (see /context history)", parts.join("; ")))
}

enum Trigger {
    Transition {
        cause: StoredReasoningInvalidationCause,
        invalid_before: BTreeSet<BlockKey>,
    },
    Request {
        /// Blocks the provider reported, with its reason.
        reported: BTreeMap<BlockKey, String>,
    },
}

/// Stored message index and block ordinal of one replayed block.
type BlockKey = (usize, usize);

struct InvalidBlocks {
    /// `None` when the route does not bind replayed reasoning.
    invalid: Option<BTreeSet<BlockKey>>,
}

fn recompute(
    provider: &dyn Provider,
    messages: &[StoredMessage],
    mut state: StoredContextViewState,
    prefix: &ContextRequestPrefix,
    trigger: Trigger,
    revision: u64,
) -> Result<ReasoningInvalidationOutcome, ContextServiceError> {
    let target_index = ContextTargetIndex::new(messages);
    let active_index = state
        .transactions
        .iter()
        .position(|transaction| transaction.is_reasoning_invalidation() && transaction.is_active());
    let active_keys = match active_index {
        Some(index) => resolve_keys(&target_index, &state.transactions[index])?,
        None => BTreeSet::new(),
    };
    // Causes survive supersession and a transcript edit that ended the
    // previous set: a block keeps the cause it was first suppressed for.
    let mut causes = latest_causes(&target_index, &state);
    // A block a person's own active suppression holds belongs to that
    // transaction: two operations cannot transform one block.
    let user_held = user_suppressed_keys(&target_index, &state);

    // Provider-reported blocks stay suppressed: earlier reports, and the new
    // ones.
    let mut sticky: BTreeSet<BlockKey> = causes
        .iter()
        .filter(|(_, cause)| {
            matches!(
                cause,
                StoredReasoningInvalidationCause::ProviderReported { .. }
            )
        })
        .map(|(key, _)| *key)
        .collect();
    if let Trigger::Request { reported } = &trigger {
        for (key, reason) in reported {
            if is_bound_reasoning(&messages[key.0].content[key.1]) && sticky.insert(*key) {
                causes.insert(
                    *key,
                    StoredReasoningInvalidationCause::ProviderReported {
                        reason: reason.clone(),
                    },
                );
            }
        }
    }
    sticky.retain(|key| !user_held.contains(key));

    // The derived part, recomputed whole on a binding route with the sticky
    // part in force. A route that does not bind keeps the derived part as it
    // was: no lift, no additions.
    let view = with_managed_set(
        messages,
        &without_managed(&state),
        &sticky,
        &causes,
        revision,
    )?;
    let derived = invalid_blocks(provider, messages, &view, prefix)?.invalid;
    let binds = derived.is_some();
    let derived = derived.unwrap_or_else(|| {
        active_keys
            .iter()
            .copied()
            .filter(|key| !sticky.contains(key) && !user_held.contains(key))
            .collect()
    });

    let recorded = StoredReasoningInvalidationCause::RequestPrefixChanged {
        recorded_transitions: prefix.recorded_transitions.clone(),
    };
    let set: BTreeSet<BlockKey> = sticky.union(&derived).copied().collect();
    let mut summary = ContextReasoningInvalidationSummary {
        active: set.len(),
        ..ContextReasoningInvalidationSummary::default()
    };
    for key in &set {
        let fresh_cause = match &trigger {
            Trigger::Transition {
                cause,
                invalid_before,
            } if !invalid_before.contains(key) => cause.clone(),
            _ => recorded.clone(),
        };
        let cause = causes.entry(*key).or_insert(fresh_cause).clone();
        if active_keys.contains(key) {
            continue;
        }
        let tokens = block_tokens(messages, *key);
        match &trigger {
            Trigger::Transition { cause: own, .. } if *own == cause => {
                summary.invalidated_by_change += 1;
                summary.invalidated_by_change_tokens += tokens;
            }
            _ => {
                summary.invalidated_other += 1;
                summary.invalidated_other_tokens += tokens;
            }
        }
    }
    for key in active_keys.difference(&set) {
        if !user_held.contains(key) {
            summary.restored += 1;
            summary.restored_tokens += block_tokens(messages, *key);
        }
    }

    if set == active_keys {
        summary.transaction_id = active_index.map(|index| state.transactions[index].id.clone());
        return Ok(ReasoningInvalidationOutcome {
            state,
            summary: (binds || !set.is_empty()).then_some(summary),
            changed: false,
        });
    }

    let now = Utc::now();
    let replacement =
        (!set.is_empty()).then(|| format!("reasoning-invalidation-{}", uuid::Uuid::new_v4()));
    if let Some(index) = active_index {
        state.transactions[index]
            .status_events
            .push(StoredContextStatusEvent {
                revision,
                timestamp: now,
                kind: StoredContextTransactionStatusKind::Superseded,
                reason: Some(match &replacement {
                    Some(id) => format!("Replaced by reasoning invalidation {id}."),
                    None => "No replayed reasoning is invalid any more.".to_string(),
                }),
            });
    }
    if let Some(id) = &replacement {
        state.transactions.push(managed_transaction(
            messages, &set, &causes, revision, id, now,
        )?);
    }
    state.revision = revision;
    summary.transaction_id = replacement;
    Ok(ReasoningInvalidationOutcome {
        state,
        summary: Some(summary),
        changed: true,
    })
}

/// The view a person's transactions produce: the derived part of the
/// managed set is recomputed from it, so managed invalidations are left out.
fn without_managed(state: &StoredContextViewState) -> StoredContextViewState {
    let mut view = state.clone();
    view.transactions
        .retain(|transaction| !transaction.is_reasoning_invalidation());
    view
}

/// `view` (without managed sets) with `set` held as a managed invalidation.
fn with_managed_set(
    messages: &[StoredMessage],
    view: &StoredContextViewState,
    set: &BTreeSet<BlockKey>,
    causes: &BTreeMap<BlockKey, StoredReasoningInvalidationCause>,
    revision: u64,
) -> Result<StoredContextViewState, ContextServiceError> {
    let mut view = view.clone();
    view.revision = view.revision.max(revision);
    if !set.is_empty() {
        view.transactions.push(managed_transaction(
            messages,
            set,
            causes,
            revision,
            "reasoning-invalidation-candidate",
            Utc::now(),
        )?);
    }
    Ok(view)
}

fn managed_transaction(
    messages: &[StoredMessage],
    set: &BTreeSet<BlockKey>,
    causes: &BTreeMap<BlockKey, StoredReasoningInvalidationCause>,
    revision: u64,
    id: &str,
    now: chrono::DateTime<Utc>,
) -> Result<StoredContextTransaction, ContextServiceError> {
    let mut by_cause: BTreeMap<StoredReasoningInvalidationCause, Vec<BlockKey>> = BTreeMap::new();
    for key in set {
        let cause = causes.get(key).cloned().unwrap_or(
            StoredReasoningInvalidationCause::RequestPrefixChanged {
                recorded_transitions: Vec::new(),
            },
        );
        by_cause.entry(cause).or_default().push(*key);
    }
    let operations = by_cause
        .into_iter()
        .map(|(cause, keys)| {
            suppression(messages, cause, &keys).map(StoredContextOperation::ReasoningSuppression)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StoredContextTransaction {
        id: id.to_string(),
        base_revision: revision.saturating_sub(1),
        created_at: now,
        authorization: StoredContextAuthorization::ReasoningInvalidation,
        operations,
        status_events: vec![StoredContextStatusEvent {
            revision,
            timestamp: now,
            kind: StoredContextTransactionStatusKind::Applied,
            reason: None,
        }],
        application: None,
        economics: None,
        curator_usage: Vec::new(),
        emergency_audit: None,
    })
}

/// The replayed blocks the next request, projected from `view`, would carry
/// that the provider runtime finds invalid. `None` when the route does not
/// bind reasoning.
fn invalid_blocks(
    provider: &dyn Provider,
    messages: &[StoredMessage],
    view: &StoredContextViewState,
    prefix: &ContextRequestPrefix,
) -> Result<InvalidBlocks, ContextServiceError> {
    let projection = project_context(messages, view)
        .map_err(|error| ContextServiceError::Projection(error.to_string()))?;
    let replayed = projection.sources.iter().any(|source| {
        matches!(source, ProjectedMessageSource::RawMessage { stored_index, block_ordinals, .. }
            if block_ordinals
                .iter()
                .any(|ordinal| is_bound_reasoning(&messages[*stored_index].content[*ordinal])))
    });
    if !replayed {
        // Nothing replayable reaches the provider, so nothing can be invalid.
        return Ok(InvalidBlocks {
            invalid: provider
                .replayed_reasoning_invalidations(&[], &prefix.tools, &prefix.system)
                .map(|_| BTreeSet::new()),
        });
    }
    let request_messages = request_messages(projection.messages);
    let Some(verdict) =
        provider.replayed_reasoning_invalidations(&request_messages, &prefix.tools, &prefix.system)
    else {
        return Ok(InvalidBlocks { invalid: None });
    };
    let mut invalid = BTreeSet::new();
    for block in verdict {
        let Some(ProjectedMessageSource::RawMessage {
            stored_index,
            block_ordinals,
            ..
        }) = projection.sources.get(block.message_index)
        else {
            return Err(ContextServiceError::Projection(format!(
                "invalid replayed reasoning at projected message {} has no stored source",
                block.message_index
            )));
        };
        let ordinal = block_ordinals.get(block.block_index).ok_or_else(|| {
            ContextServiceError::Projection(format!(
                "invalid replayed reasoning at projected block {}.{} has no stored block",
                block.message_index, block.block_index
            ))
        })?;
        invalid.insert((*stored_index, *ordinal));
    }
    Ok(InvalidBlocks {
        invalid: Some(invalid),
    })
}

/// The messages exactly as the request loops send them.
pub(crate) fn request_messages(projected: Vec<Message>) -> Vec<Message> {
    if crate::config::config().features.message_timestamps {
        Message::with_timestamps(&projected)
    } else {
        projected
    }
}

fn is_bound_reasoning(block: &crate::message::ContentBlock) -> bool {
    context_block_kind(block) == StoredContextBlockKind::AnthropicThinking
}

fn resolve_keys(
    index: &ContextTargetIndex<'_>,
    transaction: &StoredContextTransaction,
) -> Result<BTreeSet<BlockKey>, ContextServiceError> {
    let mut keys = BTreeSet::new();
    for operation in &transaction.operations {
        if let StoredContextOperation::ReasoningSuppression(suppression) = operation {
            for target in &suppression.targets {
                let resolved = index
                    .resolve_content_target(target)
                    .map_err(|error| ContextServiceError::Stale(error.to_string()))?;
                keys.insert((resolved.message_index, resolved.block_index));
            }
        }
    }
    Ok(keys)
}

/// Blocks the active suppressions of people's own transactions target.
fn user_suppressed_keys(
    index: &ContextTargetIndex<'_>,
    state: &StoredContextViewState,
) -> BTreeSet<BlockKey> {
    let mut keys = BTreeSet::new();
    for transaction in &state.transactions {
        if transaction.is_reasoning_invalidation() || !transaction.is_active() {
            continue;
        }
        for operation in &transaction.operations {
            let StoredContextOperation::ReasoningSuppression(suppression) = operation else {
                continue;
            };
            for target in &suppression.targets {
                if let Ok(resolved) = index.resolve_content_target(target) {
                    keys.insert((resolved.message_index, resolved.block_index));
                }
            }
        }
    }
    keys
}

/// Causes recorded by the latest managed set that has not been superseded,
/// for the targets that still resolve.
fn latest_causes(
    index: &ContextTargetIndex<'_>,
    state: &StoredContextViewState,
) -> BTreeMap<BlockKey, StoredReasoningInvalidationCause> {
    let Some(latest) = latest_unsuperseded(state) else {
        return BTreeMap::new();
    };
    let mut causes = BTreeMap::new();
    for operation in &latest.operations {
        let StoredContextOperation::ReasoningSuppression(StoredReasoningSuppression {
            selection: StoredReasoningSelection::Invalidated { cause },
            targets,
            ..
        }) = operation
        else {
            continue;
        };
        for target in targets {
            if let Ok(resolved) = index.resolve_content_target(target) {
                causes.insert(
                    (resolved.message_index, resolved.block_index),
                    cause.clone(),
                );
            }
        }
    }
    causes
}

/// The latest managed set that was not replaced by a newer one: the active
/// set, or one a transcript edit ended.
fn latest_unsuperseded(state: &StoredContextViewState) -> Option<&StoredContextTransaction> {
    state.transactions.iter().rev().find(|transaction| {
        transaction.is_reasoning_invalidation()
            && transaction.latest_status().map(|status| status.kind)
                != Some(StoredContextTransactionStatusKind::Superseded)
    })
}

fn block_tokens(messages: &[StoredMessage], (message, block): BlockKey) -> usize {
    estimate_content_block_tokens(&messages[message].content[block])
}

fn suppression(
    messages: &[StoredMessage],
    cause: StoredReasoningInvalidationCause,
    keys: &[BlockKey],
) -> Result<StoredReasoningSuppression, ContextServiceError> {
    let targets = keys
        .iter()
        .map(|(message, block)| {
            build_content_target(messages, *message, *block)
                .map_err(|error| ContextServiceError::Projection(error.to_string()))
        })
        .collect::<Result<Vec<StoredContentTarget>, _>>()?;
    Ok(StoredReasoningSuppression {
        selection: StoredReasoningSelection::Invalidated { cause },
        assistant_turns_affected: keys
            .iter()
            .map(|(message, _)| *message)
            .collect::<BTreeSet<_>>()
            .len(),
        replay_block_kinds: vec![StoredContextBlockKind::AnthropicThinking],
        original_token_estimate: keys.iter().map(|key| block_tokens(messages, *key)).sum(),
        validation_evidence_version: 1,
        validation: Vec::new(),
        targets,
    })
}
