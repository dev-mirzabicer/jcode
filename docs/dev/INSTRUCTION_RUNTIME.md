# Instruction runtime foundation

**Status:** Phase 3 WP-01 typed foundation. Production prompt callers have not migrated yet.

**Source module:** `crates/jcode-base/src/instruction/`

**Inventory:** [`INSTRUCTION_INVENTORY.md`](INSTRUCTION_INVENTORY.md)

## Purpose and boundary

The instruction runtime is the one typed authority for managed instruction discovery, resource identity, global/project specificity, document parsing, dependency validation, and complete rendering.

It deliberately does not own:

- Git repositories, commits, branches, remotes, locking, or editing
- Session activation, freezing, switching, persistence, or exports
- System versus user-message delivery
- Display roles, timestamps, XML tags, receipts, tool schemas, or provider framing
- Prompt-quality judgment

Those behaviors remain with their current domain owners. Later Phase 3 packages call this runtime for finished text instead of reproducing its internal stages.

WP-01 does not initialize `~/.jcode/instructions`, activate the runtime in production prompt paths, or migrate existing prose. WP-02 supplies the repository roots. WP-03 and later packages migrate callers by the inventory ledger.

## Public operations

`InstructionRuntime` exposes complete caller operations:

- `discover(InstructionSources)` discovers all supported resource families and retains scoped invalid entries for truthful shadowing.
- `resolve(&InstructionSelector)` applies explicit scope or project-first specificity and returns one validated document.
- `render(&InstructionSelector, &typed_values)` resolves dependencies and returns complete rendered text plus the dependency/reverse-consumer graph.
- `render_agent(...)` additionally enforces primary/isolated availability metadata.
- `render_registered(&ConsumerRegistration, &typed_values)` renders a code-owned singleton and distinguishes required missing files from deleted user resources.
- `InstructionConsumer<T>` binds one consumer registration to a serializable value type without taking over delivery behavior.

Callers do not coordinate public load, parse, validate, resolve, graph, partial, and render stages.

## Resource model

Supported kinds are:

- `system`
- `agent`
- `agent-addendum`
- `module`
- `notification`
- `tool-guidance`
- `skill`

Default directories are `system/`, `agents/`, `addenda/`, `modules/`, `notifications/`, `tools/`, and `skills/<name>/SKILL.md`.

Stable IDs use lowercase ASCII letters, digits, `-`, `_`, and `.`, beginning with a letter or digit. Jcode adds no ID-length product limit.

Ordinary resources use YAML frontmatter followed by a complete Markdown body:

```markdown
---
id: synthetic-agent
kind: agent
name: Synthetic agent
description: Synthetic mechanism fixture
availability: both
template: handlebars
includes:
  - common
---

{{> project:quality}}

Hello {{user.name}}.
```

Plain text is the default. Agent resources require a non-empty name, description, and `primary`, `isolated`, or `both` availability. Addenda require an explicit agent target. Skills may use their existing `name` as the stable ID and retain `allowed-tools` compatibility.

Name and description have one domain source of truth in `InstructionMetadata`. `AgentMetadata` carries only agent-specific availability. The parser rejects unknown frontmatter fields rather than silently dropping them during `to_markdown`; new extension metadata must first become an explicit typed field with defined runtime and manager semantics. It also rejects known fields on kinds that do not define them: `availability` is agent-only, `target` is agent-addendum-only, and `allowed-tools` is skill-only.

`InstructionDocument::to_markdown` provides deterministic semantic serialization for manager and protocol work. It preserves the body text rather than interpreting it while serializing.

## Specificity and invalid resources

References may be:

- Unqualified: project first, then global
- `global:<id>`
- `project:<id>`

A present project candidate shadows global even when the project document is invalid. It fails the affected operation instead of silently falling back.

Duplicate IDs in one kind and scope are ambiguous and fail selection. Invalid or ambiguous unrelated resources remain inspectable and do not block a valid resource.

Global and project `AGENTS.md` are dedicated external inputs, not managed catalog documents. `render_external_agents` renders global first and project second. The runtime does not commit or import them.

## Complete rendering

Plain documents treat `{{ ... }}` literally.

Handlebars mode uses version 6 through a restricted adapter:

- Strict missing-value behavior
- Typed `serde::Serialize` values only
- HTML escaping disabled for Markdown/plain text
- Registered managed module partials only
- Project-first and explicit-scope partial lookup
- No helper calls
- No block expressions
- No subexpressions
- No arbitrary filesystem loader
- No script execution
- Pre-render dependency cycle detection

Metadata `includes` render complete modules before the owning body. Handlebars partials render at their exact source position. The graph exposes render dependencies and validation-only dependencies separately. Agent-addendum targets must resolve as valid agents, but their bodies and transitive modules are not rendered with the addendum's values and are not implicitly copied into the addendum body.

The graph traversal is iterative. Jcode adds no source-size, body-size, expansion-depth, graph-depth, or rendered-output cap. Large finite sources and deep finite acyclic graphs render completely. Filesystem, serialization, allocation, and machine failures remain errors. No partial output is returned as success.

## Empty, missing, and delete semantics

- An empty body is valid and renders empty text.
- An empty project definition still shadows the global definition.
- Removing a project definition exposes global on the next discovery.
- Removing a user-defined resource makes it absent.
- A required registered singleton that is absent produces `RegisteredResourceMissing` with its expected path.
- A consumer may declare empty prose invalid through `empty_is_meaningful = false`.
- The runtime never restores a seed merely because a file is empty or absent.

Repository initialization and damaged-store recovery belong to WP-02.

## Diagnostics and errors

Discovery records path-scoped diagnostics for unreadable or unidentified files. A document whose stable ID can be recovered remains a scoped invalid catalog entry so its shadowing semantics stay truthful.

Typed errors distinguish:

- Invalid ID or selector
- Resource absent
- Registered singleton missing
- Scoped ambiguity
- Invalid selected resource and source path
- Empty content rejected by a consumer contract
- Restricted-template violation
- Missing typed value or rendering failure
- Dependency cycle with a resource chain
- Agent availability mismatch
- Value serialization failure

Errors identify the affected resource and operation. Runtime state is immutable after discovery, so a failed render has no partial mutation to roll back.

## Security and cache relevance

The renderer cannot execute repository code, call helpers, load arbitrary paths, or escape configured instruction roots through partial references. Repository and symlink policy remains a WP-02 responsibility.

WP-01 does not change provider prompts, tool definitions, or session cache behavior. Later activation packages render once at their accepted lifecycle boundary and persist exact text. The runtime itself has no watcher, source hash, Git revision, freshness state, truncation, or cache policy.

## Mechanism verification

Synthetic tests cover:

- Every resource kind and semantic Markdown round trip
- Plain literal braces
- Typed Handlebars values without HTML escaping
- Project-first and explicit-scope partials
- Invalid project shadowing
- Unreadable/invalid-UTF-8 project shadowing without global fallback
- Unrelated-resource isolation
- Empty, missing registered, and deleted-user-resource distinctions
- Deep finite graphs and large complete sources
- Cycle, missing dependency, unknown value, helper, and ambiguity failures
- Agent availability
- Typed registered consumers and delivery ownership
- Addendum target validation
- Validation-only addendum targets whose agent templates require unrelated values
- Dependency and reverse-consumer derivation
- Unknown frontmatter rejection and single-source agent metadata serialization
- Kind-incompatible known-frontmatter rejection without silent serialization loss
- Dedicated global/project `AGENTS.md` ordering

The fixtures use synthetic prose. They do not snapshot, require, forbid, or judge Mirza-approved instruction wording.

## Handoff to later packages

- WP-02 supplies validated Git-backed global and project roots plus import receipts.
- WP-03 uses the runtime for complete system composition and the compatibility `jcode` agent.
- WP-05 adopts managed skill discovery and activation snapshots.
- WP-06 and WP-07 migrate inventory rows through registered typed consumers.
- WP-09 and WP-10 use catalog summaries, diagnostics, complete content, graphs, and deterministic document serialization for the manager.
- WP-11 reconciles every inventory row and verifies exact migration equality or an approved exception.
