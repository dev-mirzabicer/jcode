//! Bounded wire frames for complete instruction-management replies.
use crate::{InstructionManagementReply, ServerEvent};
use serde::{Deserialize, Serialize};

const CHUNK_BYTES: usize = 32 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstructionManagementChunk {
    pub session_id: String,
    pub transfer: String,
    pub offset: usize,
    pub total_bytes: usize,
    pub text: String,
}

pub fn emit_instruction_management_reply(
    id: u64,
    reply: InstructionManagementReply,
    mut emit: impl FnMut(ServerEvent),
) -> Result<(), serde_json::Error> {
    let encoded = serde_json::to_string(&reply)?;
    if encoded.len() <= CHUNK_BYTES {
        emit(ServerEvent::InstructionManagement {
            id,
            reply: Box::new(reply),
        });
        return Ok(());
    }
    // The request ID and session bind this transfer. No per-instruction limit
    // replaces the protocol's existing per-frame allocation guard.
    let transfer = format!("{id}:{}", reply.session_id);
    let mut offset = 0;
    while offset < encoded.len() {
        let mut end = offset.saturating_add(CHUNK_BYTES).min(encoded.len());
        while !encoded.is_char_boundary(end) {
            end -= 1;
        }
        emit(ServerEvent::InstructionManagementChunk {
            id,
            chunk: InstructionManagementChunk {
                session_id: reply.session_id.clone(),
                transfer: transfer.clone(),
                offset,
                total_bytes: encoded.len(),
                text: encoded[offset..end].into(),
            },
        });
        offset = end;
    }
    Ok(())
}

#[derive(Default)]
pub struct InstructionManagementTransfer {
    state: Option<Transfer>,
}
struct Transfer {
    id: u64,
    session: String,
    transfer: String,
    total: usize,
    text: String,
}
impl InstructionManagementTransfer {
    pub fn clear(&mut self) {
        self.state = None;
    }
    pub fn push(
        &mut self,
        id: u64,
        session: &str,
        chunk: InstructionManagementChunk,
    ) -> Result<Option<InstructionManagementReply>, String> {
        let result = self.append(id, session, chunk);
        if result.is_err() {
            self.clear();
        }
        result
    }
    fn append(
        &mut self,
        id: u64,
        session: &str,
        chunk: InstructionManagementChunk,
    ) -> Result<Option<InstructionManagementReply>, String> {
        if chunk.session_id != session || chunk.text.is_empty() {
            return Err("Instruction reply has invalid session or empty progress".into());
        }
        if chunk.offset == 0 {
            if self.state.as_ref().is_some_and(|state| state.id == id) {
                return Err("Instruction reply restarted within the same request".into());
            }
            self.state = Some(Transfer {
                id,
                session: session.into(),
                transfer: chunk.transfer.clone(),
                total: chunk.total_bytes,
                text: String::new(),
            });
        }
        let state = self
            .state
            .as_mut()
            .ok_or("Instruction reply is missing its first chunk")?;
        if state.id != id
            || state.session != session
            || state.transfer != chunk.transfer
            || state.total != chunk.total_bytes
            || state.text.len() != chunk.offset
        {
            return Err("Instruction reply chunk belongs to a different request or offset".into());
        }
        let next = state
            .text
            .len()
            .checked_add(chunk.text.len())
            .ok_or("Instruction reply byte accounting overflow")?;
        if next > state.total {
            return Err("Instruction reply exceeds its declared complete length".into());
        }
        state.text.push_str(&chunk.text);
        if next != state.total {
            return Ok(None);
        }
        let complete = self.state.take().ok_or("Instruction reply disappeared")?;
        let reply: InstructionManagementReply = serde_json::from_str(&complete.text)
            .map_err(|error| format!("Invalid complete instruction reply: {error}"))?;
        if reply.session_id != session {
            return Err("Complete instruction reply has the wrong session".into());
        }
        Ok(Some(reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstructionManagementFailure, InstructionManagementResult};
    #[test]
    fn complete_large_unicode_reply_uses_small_correlated_frames() {
        let reply = InstructionManagementReply {
            session_id: "synthetic-session".into(),
            result: InstructionManagementResult::Failed(InstructionManagementFailure {
                operation: "synthetic".into(),
                detail: "合成🙂\n".repeat(200_000),
                draft: None,
                source_unchanged: true,
            }),
        };
        let mut events = Vec::new();
        emit_instruction_management_reply(41, reply.clone(), |event| events.push(event)).unwrap();
        assert!(events.len() > 1);
        let mut transfer = InstructionManagementTransfer::default();
        let mut result = None;
        for event in events {
            assert!(serde_json::to_vec(&event).unwrap().len() < CHUNK_BYTES * 3);
            let ServerEvent::InstructionManagementChunk { id, chunk } = event else {
                panic!("expected chunk")
            };
            result = transfer
                .push(id, "synthetic-session", chunk)
                .unwrap()
                .or(result);
        }
        assert_eq!(result, Some(reply));
    }
    #[test]
    fn incomplete_or_cross_session_chunks_never_become_success() {
        let mut transfer = InstructionManagementTransfer::default();
        let chunk = InstructionManagementChunk {
            session_id: "session".into(),
            transfer: "transfer".into(),
            offset: 0,
            total_bytes: 100,
            text: "partial".into(),
        };
        assert!(transfer.push(1, "other", chunk.clone()).is_err());
        assert!(
            transfer
                .push(1, "session", chunk.clone())
                .unwrap()
                .is_none()
        );
        assert!(transfer.push(1, "session", chunk).is_err());
        assert!(
            transfer
                .push(
                    1,
                    "session",
                    InstructionManagementChunk {
                        session_id: "session".into(),
                        transfer: "transfer".into(),
                        offset: 7,
                        total_bytes: 100,
                        text: "suffix".into()
                    }
                )
                .is_err()
        );
    }
}
