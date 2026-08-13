use crate::agent::Agent;
use crate::context::commit::prepare_context_transition;
use crate::context::draft::ContextTransactionService;
use crate::protocol::{
    ContextOperationCounts, ContextServiceError, ContextTransactionResult,
    ContextTransactionSummary,
};
use chrono::Utc;
use jcode_session_types::{
    StoredContextStatusEvent, StoredContextTransaction, StoredContextTransactionStatusKind,
    StoredContextViewState,
};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

pub fn summarize_context_transaction(
    transaction: &StoredContextTransaction,
) -> ContextTransactionSummary {
    let counts = transaction.operation_counts();
    ContextTransactionSummary {
        id: transaction.id.clone(),
        created_at: transaction.created_at,
        base_revision: transaction.base_revision,
        active: transaction.is_active(),
        latest_status: transaction.latest_status().map(|status| status.kind),
        latest_status_revision: transaction.latest_status().map(|status| status.revision),
        authorization: transaction.authorization.clone(),
        operation_counts: ContextOperationCounts {
            range_summaries: counts.range_summaries,
            reasoning_suppressions: counts.reasoning_suppressions,
            tool_result_distillations: counts.tool_result_distillations,
        },
        application: transaction.application.clone(),
        economics: transaction.economics.clone(),
    }
}

pub fn list_context_transactions(state: &StoredContextViewState) -> Vec<ContextTransactionSummary> {
    state
        .transactions
        .iter()
        .rev()
        .map(summarize_context_transaction)
        .collect()
}

impl ContextTransactionService {
    pub fn list_transactions(
        &self,
        agent: &Arc<AsyncMutex<Agent>>,
    ) -> Result<Vec<ContextTransactionSummary>, ContextServiceError> {
        let agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        Ok(list_context_transactions(agent.context_view_state()))
    }

    pub fn revert_transaction(
        &self,
        agent: &Arc<AsyncMutex<Agent>>,
        transaction_id: &str,
        processing: bool,
    ) -> Result<ContextTransactionResult, ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        let mut agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let previous_state = agent.context_view_state().clone();
        let transaction_index = transaction_index(&previous_state, transaction_id)?;
        if !previous_state.transactions[transaction_index].is_active() {
            return Err(ContextServiceError::TransactionNotActive(
                transaction_id.to_string(),
            ));
        }
        let revision = previous_state
            .revision
            .checked_add(1)
            .ok_or(ContextServiceError::RevisionOverflow)?;
        let mut proposed_state = previous_state.clone();
        proposed_state.revision = revision;
        proposed_state.transactions[transaction_index]
            .status_events
            .push(StoredContextStatusEvent {
                revision,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reverted,
                reason: Some("Reverted through the context transaction service.".to_string()),
            });
        let prepared = prepare_context_transition(
            &agent,
            &previous_state,
            proposed_state,
            transaction_index,
            false,
        )?;
        self.persist_prepared_transition(&mut agent, previous_state, prepared)
    }

    pub fn reapply_transaction(
        &self,
        agent: &Arc<AsyncMutex<Agent>>,
        transaction_id: &str,
        processing: bool,
    ) -> Result<ContextTransactionResult, ContextServiceError> {
        if processing {
            return Err(ContextServiceError::SessionBusy);
        }
        let mut agent = agent
            .try_lock()
            .map_err(|_| ContextServiceError::SessionBusy)?;
        let previous_state = agent.context_view_state().clone();
        let transaction_index = transaction_index(&previous_state, transaction_id)?;
        if previous_state.transactions[transaction_index].is_active() {
            return Err(ContextServiceError::TransactionAlreadyActive(
                transaction_id.to_string(),
            ));
        }
        let revision = previous_state
            .revision
            .checked_add(1)
            .ok_or(ContextServiceError::RevisionOverflow)?;
        let mut proposed_state = previous_state.clone();
        proposed_state.revision = revision;
        proposed_state.transactions[transaction_index]
            .status_events
            .push(StoredContextStatusEvent {
                revision,
                timestamp: Utc::now(),
                kind: StoredContextTransactionStatusKind::Reapplied,
                reason: Some("Reapplied through the context transaction service.".to_string()),
            });
        let prepared = prepare_context_transition(
            &agent,
            &previous_state,
            proposed_state,
            transaction_index,
            true,
        )?;
        self.persist_prepared_transition(&mut agent, previous_state, prepared)
    }
}

fn transaction_index(
    state: &StoredContextViewState,
    transaction_id: &str,
) -> Result<usize, ContextServiceError> {
    state
        .transactions
        .iter()
        .position(|transaction| transaction.id == transaction_id)
        .ok_or_else(|| ContextServiceError::TransactionNotFound(transaction_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_session_types::{
        StoredContextAuthorization, StoredContextStatusEvent, StoredContextTransaction,
        StoredContextTransactionStatusKind,
    };

    #[test]
    fn history_is_newest_first_and_never_discards_reverted_provenance() {
        let transaction = |id: &str, revision: u64, kind| StoredContextTransaction {
            id: id.to_string(),
            base_revision: revision.saturating_sub(1),
            created_at: Utc::now(),
            authorization: StoredContextAuthorization::Manual { initiated_by: None },
            operations: Vec::new(),
            status_events: vec![StoredContextStatusEvent {
                revision,
                timestamp: Utc::now(),
                kind,
                reason: None,
            }],
            application: None,
            economics: None,
            curator_usage: Vec::new(),
        };
        let state = StoredContextViewState {
            revision: 2,
            transactions: vec![
                transaction("first", 1, StoredContextTransactionStatusKind::Reverted),
                transaction("second", 2, StoredContextTransactionStatusKind::Applied),
            ],
            ..StoredContextViewState::default()
        };

        let history = list_context_transactions(&state);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, "second");
        assert_eq!(history[1].id, "first");
        assert!(!history[1].active);
    }
}
