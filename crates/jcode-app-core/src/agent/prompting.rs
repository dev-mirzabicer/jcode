use super::Agent;
use crate::logging;
use crate::message::{Message, ToolDefinition};

impl Agent {
    pub(super) fn log_prompt_prefix_accounting(
        &self,
        split: &crate::prompt::SplitSystemPrompt,
        tools: &[ToolDefinition],
    ) {
        let system_tokens = split.estimated_tokens();
        let tool_tokens = ToolDefinition::aggregate_prompt_token_estimate(tools);
        let prefix_tokens = system_tokens + tool_tokens;
        let startup = self.session.startup_context_accounting();
        logging::info(&format!(
            "Prompt prefix estimate: total={} tokens (system={} tools={}); startup context: files={} bytes={} estimated_file_tokens={} state={:?} batch_delivery={:?}",
            prefix_tokens,
            system_tokens,
            tool_tokens,
            startup.file_count,
            startup.captured_bytes,
            startup.estimated_tokens,
            startup.state,
            startup.batch_delivery,
        ));
    }

    pub(super) fn build_memory_prompt_nonblocking_shared(
        &self,
        messages: std::sync::Arc<[Message]>,
        _memory_event_tx: Option<crate::memory::MemoryEventSink>,
    ) -> Option<crate::memory::PendingMemory> {
        if !self.memory_enabled() {
            return None;
        }

        let session_id = &self.session.id;

        let fresh_user_turn = crate::message::ends_with_fresh_user_turn(&messages);
        let pending = if fresh_user_turn {
            crate::memory::reserve_pending_memory(session_id)
        } else {
            None
        };

        // Use the persistent memory-agent pipeline as the single source of truth.
        // Running both this and the legacy MemoryManager background retrieval path
        // can prepare overlapping pending prompts for the same turn, which makes
        // memory injection feel overly aggressive.
        // Relevance results are consumed only at the start of a fresh user turn.
        // Enqueuing again after every tool result runs the local embedding model
        // for each provider continuation without creating an additional injection
        // opportunity. One update per user turn keeps memory current while avoiding
        // redundant 512-token inference during tool-heavy agent loops.
        if fresh_user_turn {
            crate::memory_agent::update_context_sync_with_dir(
                session_id,
                messages,
                self.session.working_dir.clone(),
            );
        }

        pending
    }

    fn append_current_turn_system_reminder(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        let Some(reminder) = self
            .current_turn_system_reminder
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        else {
            return;
        };

        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str("# System Reminder\n\n");
        split.dynamic_part.push_str(reminder);
    }

    /// Build split system prompt for better caching
    /// Returns static (cacheable) and dynamic (not cached) parts separately
    pub(super) fn build_system_prompt_split(
        &self,
        memory_prompt: Option<&str>,
    ) -> crate::prompt::SplitSystemPrompt {
        if let Some(ref override_prompt) = self.system_prompt_override {
            return crate::prompt::SplitSystemPrompt {
                static_part: override_prompt.clone(),
                dynamic_part: String::new(),
            };
        }

        let skill_prompt = self
            .session
            .active_skill
            .as_ref()
            .map(|skill| skill.rendered_text.as_str());

        let mut split = if let Some(static_part) = self.session.system_prompt_text() {
            crate::prompt::SplitSystemPrompt {
                static_part: static_part.to_string(),
                dynamic_part: String::new(),
            }
        } else {
            let skills = self.current_skills_snapshot();
            let available_skills = skills
                .list()
                .iter()
                .map(|skill| crate::prompt::SkillInfo {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                })
                .collect::<Vec<_>>();
            let working_dir = self
                .session
                .working_dir
                .as_ref()
                .map(std::path::PathBuf::from);
            crate::prompt::build_system_prompt_split(
                None,
                &available_skills,
                self.session.is_canary,
                None,
                working_dir.as_deref(),
            )
            .0
        };
        if let Some(memory_prompt) = memory_prompt {
            split.dynamic_part.push_str(memory_prompt);
        }
        if let Some(skill_prompt) = skill_prompt {
            if !split.dynamic_part.is_empty() {
                split.dynamic_part.push_str("\n\n");
            }
            split.dynamic_part.push_str("# Active Skill\n\n");
            split.dynamic_part.push_str(skill_prompt);
        }

        self.append_current_turn_system_reminder(&mut split);
        crate::prompt::append_swarm_effort_directive(
            &mut split,
            self.provider.reasoning_effort().as_deref(),
        );

        split
    }

    /// Non-blocking memory prompt - takes pending result and spawns check for next turn
    #[cfg(test)]
    pub(super) fn build_memory_prompt_nonblocking(
        &self,
        messages: &[Message],
        _memory_event_tx: Option<crate::memory::MemoryEventSink>,
    ) -> Option<crate::memory::PendingMemory> {
        self.build_memory_prompt_nonblocking_shared(messages.to_vec().into(), _memory_event_tx)
    }
}
