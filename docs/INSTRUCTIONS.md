# Instructions: sources, active sessions and model policy

Jcode separates editable instruction **source**, exact **active session text**,
and the **model roster**. Opening or saving a source is not an agent switch.

## Choose the operation

| Goal | Entry point | Effect |
|---|---|---|
| Browse or edit global/project guidance | `/instructions` (`/prompts`) | Type-first manager with explicit source ownership, drafts, review and Save |
| Configure project instruction storage | Manager → Project setup | Reviewed submodule, external checkout or non-Git standalone setup |
| Select a primary agent | `/agent`, `/agent <selector>`, `jcode --agent <selector>` | Selects instructions, not model, tools or permissions |
| Inspect what this session actually uses | `/agent inspect`, manager → Current session instructions | Exact stored text, not a current-source preview |
| Refresh the true system prompt deliberately | `/agent replace <selector>` while idle | Complete current-source composition and one intentional prefix transition |
| Invoke a skill | `/<skill-name> [prompt]` | Captures its latest complete rendered text; later edits do not change that invocation |
| Inspect or edit launch policy | `/model-roster` | Global alias policy for future explicitly adopting callers, not interactive model selection |
| Change existing special-role model settings | `/agent-models` | Existing Swarm/review/judge/ambient configuration, separate from profiles and roster |

The manager starts with Agents. Select a type, then **Effective here**, **Global**
or **Project**. Effective here describes current source precedence, not the
running session's frozen instructions. **Sources** names every stored version
and the destination of Edit or Copy. Repository administration and active-session
snapshots are separate pages. See the [manager guide](INSTRUCTION_MANAGER.md) for
keyboard/mouse navigation, external editing and recovery.

## When changes take effect

| Material | Read boundary | Existing session after a disk edit |
|---|---|---|
| Complete primary system, including agent, modules, AGENTS.md and available-skills list | New activation or explicit replacement | Keeps exact stored text |
| Ordinary post-dispatch profile switch | Explicit idle `/agent` operation | Appends complete target profile with user authority; old system prefix stays |
| Active skill | Invocation or reinvocation | Keeps exact stored rendered text |
| Notification | Each occurrence | Earlier messages stay unchanged; later occurrence uses current source |
| Workflow instructions | Owning workflow's invocation/request boundary | No rewrite of earlier turns or already captured instructions |
| Swarm routing tool description | Successful preflight before first capture | Reuses exact session-owned description across turns, resume and split |
| Model roster | Explicit new-execution resolution | A resumed execution keeps its stored concrete route, model and effort |

Resume, reconnect, takeover, process reload and split preserve session-owned
instruction text. Clear and transfer create new contexts, retain the selected
agent and render current source. They clear the active skill and recapture
Startup Context under its existing rules. A source-free exact-qualified
same-agent selection is a no-op; explicit replacement is the refresh operation.

## Source and composition

Global files live in `~/.jcode/instructions`, a private Git repository separate
from sessions, credentials and logs. Configured project stores use the same
resource layout. Unqualified resource references resolve project first, then
global. An invalid project definition does not disappear behind global fallback.
Common guidance and preferred-tool guidance are intentionally paired additive
contributions, global before project. Agent addenda explicitly target an agent.

Use [system-prompt configuration](SYSTEM_PROMPT_CONFIG.md) for exact static slot
order and selection precedence, [stores](INSTRUCTION_STORES.md) for repository
modes and Git safety, and [migration](INSTRUCTION_MIGRATION.md) for old prompt
files and old saved sessions. The [runtime reference](dev/INSTRUCTION_RUNTIME.md)
documents IDs, frontmatter and restricted Handlebars.

Managed content is complete. Jcode adds no instruction source, rendered-output
or acyclic-expansion-depth limit. Plain text is the default. Opt-in Handlebars
allows typed values and registered modules, not scripts, arbitrary file reads,
helpers or block expressions. Invalid templates fail without a partial render.
Provider preflight counts the complete request and may block it without reducing
instructions or losing the pending prompt. Paging is a transport/display mechanism,
not permission to label a clipped body complete.

## Protected boundaries

- [Startup Context](STARTUP_CONTEXT.md) remains explicitly selected, exact
  user-authority file messages with receipts. It is not a system-prompt include.
- [Context control](CONTEXT_CONTROL.md) remains explicit, reversible provider
  projection over the one authoritative transcript. The active appended profile
  is locked while active and survives rewind. No automatic compaction is added.
- [Memory policy](MEMORY_POLICY.md) remains globally hard-disabled in this
  downstream. Profiles, skills and roster entries cannot enable memory.
- Provider-required text, tool schemas, structural tags, self-development
  mechanics and context-curator instructions remain code-owned. Editing guidance
  cannot change a tool permission, risk gate or workflow trigger.
- Central prose is accepted through human review and field use. Synthetic
  mechanism tests verify transport, rendering, precedence, persistence and
  failure, not whether a prompt is well written.

## Further reading

- [Agent profiles](AGENT_PROFILES.md): initial selection, append versus replacement,
  cache transitions, busy rejection, context lock, rewind, exports and SDKs.
- [Skills](SKILLS.md): source precedence, complete packages, explicit Copy and
  exact active-text lifecycle. References are read as needed, not all injected.
- [Notifications](NOTIFICATIONS.md) and [workflows](WORKFLOW_INSTRUCTIONS.md):
  occurrence-time source ownership and failure behavior.
- [Model roster](MODEL_ROSTER.md): ordered route-qualified candidates, effort,
  independent provider construction and durable future-caller resolution.
