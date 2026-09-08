//! Source cutover for callers that have not adopted named-profile activation.
//! Their existing slot order, invocation lifetime and full/split delivery remain.

use super::*;

struct CompatibilityInput<'a> {
    skill: Option<&'a str>,
    skills: &'a [SkillInfo],
    selfdev: bool,
    memory: Option<&'a str>,
    working_dir: Option<&'a Path>,
    capabilities: PromptCapabilities,
}

enum Delivery {
    Full,
    Split,
}

impl SystemPromptComposer {
    pub(crate) fn compatibility_full(
        &self,
        skill: Option<&str>,
        skills: &[SkillInfo],
        selfdev: bool,
        memory: Option<&str>,
        working_dir: Option<&Path>,
        capabilities: PromptCapabilities,
    ) -> Result<(String, prompt::ContextInfo), SystemPromptActivationError> {
        let (composed, info) = self.compose_compatibility(
            CompatibilityInput {
                skill,
                skills,
                selfdev,
                memory,
                working_dir,
                capabilities,
            },
            Delivery::Full,
        )?;
        Ok((composed.static_part, info))
    }

    pub(crate) fn compatibility_split(
        &self,
        skill: Option<&str>,
        skills: &[SkillInfo],
        selfdev: bool,
        memory: Option<&str>,
        working_dir: Option<&Path>,
        capabilities: PromptCapabilities,
    ) -> Result<(prompt::SplitSystemPrompt, prompt::ContextInfo), SystemPromptActivationError> {
        self.compose_compatibility(
            CompatibilityInput {
                skill,
                skills,
                selfdev,
                memory,
                working_dir,
                capabilities,
            },
            Delivery::Split,
        )
    }

    fn compose_compatibility(
        &self,
        input: CompatibilityInput<'_>,
        delivery: Delivery,
    ) -> Result<(prompt::SplitSystemPrompt, prompt::ContextInfo), SystemPromptActivationError> {
        let environment =
            self.prepare_environment(Some(input.working_dir.unwrap_or(Path::new("."))))?;
        // This is the compatibility body, not a named-role activation. Do not
        // apply defaults, add the profile kernel/addenda, or persist profile state.
        let profile = render_agent_profile_with_availability(
            &environment,
            AgentSelection::parse(Some(COMPATIBILITY_AGENT_ID))?,
            None,
        )?;
        let mut parts = vec![profile.text];
        if input.capabilities.mermaid {
            parts.push(render_required_system(&environment.runtime, MERMAID_ID)?);
        }
        let mut info = prompt::ContextInfo {
            system_prompt_chars: parts.join("\n\n").len(),
            ..Default::default()
        };
        if input.selfdev {
            let text = match delivery {
                Delivery::Full => prompt::build_selfdev_prompt_for_working_dir(input.working_dir),
                Delivery::Split => {
                    prompt::build_selfdev_prompt_static_for_working_dir(input.working_dir)
                }
            };
            info.selfdev_chars = text.len();
            parts.push(text);
        }
        let mut ecosystem = Vec::new();
        for source in environment.runtime.external_agents().iter().rev() {
            let heading = match source.scope {
                InstructionScope::Project => {
                    info.has_project_agents_md = true;
                    info.project_agents_md_chars = source.content.len();
                    "# Project Instructions (AGENTS.md)"
                }
                InstructionScope::Global => {
                    info.has_global_agents_md = true;
                    info.global_agents_md_chars = source.content.len();
                    "# Global Instructions (~/AGENTS.md)"
                }
            };
            ecosystem.push(format!("{heading}\n\n{}", source.content.trim()));
        }
        if !ecosystem.is_empty() {
            parts.push(ecosystem.join("\n\n"));
        }
        for scope in [InstructionScope::Project, InstructionScope::Global] {
            if let Some((text, size)) = common_section(&environment, scope)? {
                info.prompt_overlay_chars += size;
                parts.push(text);
            }
        }
        for scope in [InstructionScope::Project, InstructionScope::Global] {
            if let Some((text, size)) = preferred_tools_section(&environment, scope)? {
                info.preferred_tools_chars += size;
                parts.push(text);
            }
        }
        let mut dynamic = Vec::new();
        if let Some(memory) = input.memory {
            info.memory_chars = memory.len();
            match delivery {
                Delivery::Full => parts.push(memory.to_string()),
                Delivery::Split => dynamic.push(memory.to_string()),
            }
        }
        if !input.skills.is_empty() {
            let text = render_available_skills(&environment.runtime, input.skills)?;
            info.skills_chars = text.len();
            parts.push(text);
        }
        if let Some(skill) = input.skill {
            let text = format!("# Active Skill\n\n{skill}");
            match delivery {
                Delivery::Full => parts.push(text),
                Delivery::Split => dynamic.push(text),
            }
        }
        let split = prompt::SplitSystemPrompt {
            static_part: parts.join("\n\n"),
            dynamic_part: dynamic.join("\n\n"),
        };
        info.total_chars = split.static_part.len() + split.dynamic_part.len();
        Ok((split, info))
    }
}
