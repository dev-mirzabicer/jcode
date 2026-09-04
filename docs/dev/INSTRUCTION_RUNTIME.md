# Instruction runtime foundation

**Status:** Phase 3 typed runtime, Git-backed repository service, complete primary composition, frozen activation, append profile transitions, and explicit true-system replacement.

**Source module:** `crates/jcode-base/src/instruction/`

**Inventory:** [`INSTRUCTION_INVENTORY.md`](INSTRUCTION_INVENTORY.md)

## Purpose and boundary

The instruction runtime is the one typed authority for managed instruction discovery, resource identity, global/project specificity, document parsing, dependency validation, and complete rendering.

The runtime deliberately does not own:

- Session delivery, switching, exports, or provider continuation policy
- System versus user-message delivery
- Display roles, timestamps, XML tags, receipts, tool schemas, or provider framing
- Prompt-quality judgment

The sibling `instruction::repository` domain owns Git repositories, commits, branches, remotes, mutation locking, drafts, history, restore, project repository configuration, and legacy-import receipts. `SystemPromptComposer` owns complete primary selection and composition over those sources. Session and caller layers own atomic persistence, first dispatch, lifecycle, delivery, and errors.

The app-core server owns an `InstructionRepositoryService`. Construction remains side-effect free. A new primary activation initializes or validates the global seed, resolves any project store, composes current sources, and persists exact rendered text before publication. Later packages reuse these authorities instead of adding another prompt loader or Git path.

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

Repository initialization and damaged-store recovery are implemented by WP-02 as described below.

## Frozen primary composition

`SystemPromptComposer::activate` is the caller-level operation for a new primary context. It:

1. Initializes or validates the shipped global store and durable legacy imports.
2. Resolves the configured project repository and dedicated global/project `AGENTS.md` inputs.
3. Applies explicit, project-default, global-default, or compatibility selection.
4. Enforces agent specificity and primary availability.
5. Renders the profile kernel, complete agent, modules, redefinitions, applicable addenda, common guidance, capability slots, self-development mechanics, preferred-tool compatibility sources, and available-skill snapshot. Applicable invalid or ambiguous addenda fail; unrelated damaged addenda remain isolated.
6. Returns exact prompt text plus stable agent identity.

The app-core `Agent` persists that result before Startup Context publication. Request builders read the stored static text. Resume, reload, reconnect, takeover, and split do not read instruction source. Server and direct clear plus transfer call the same composer with the retained identity and current sources, and do not replace the live context until the complete new context is durable.

Frozen prompt and active-skill scalars live in full session snapshots. Their changes force a checkpoint and do not copy large text into each append-journal entry. Metadata-only startup stubs deserialize agent, dispatch, transition, and skill identity without prompt or skill bodies. Full restore and remote history startup retain exact complete text. The first-provider-dispatch boundary is independent of Startup Context and is persisted before provider opening.

Old sessions without frozen state stage explicit `global:jcode`, clear stored provider-native continuation, and persist the migration before live Agent assignment. A failure leaves prior session, provider, tool policy, queues, and continuation unchanged. This matches the old current-source behavior without claiming historical bytes.

## Profile lifecycle operations

`SystemPromptComposer` also exposes complete caller operations for:

- `render_agent_transition`: resolves current target specificity, availability, modules, compatible project redefinition, and applicable addenda, then renders the managed transition sentence. It does not include kernel or independent system slots.
- `replace_system_prompt`: composes the complete latest true system prompt and renders the managed replacement audit sentence with typed agent names.
- `list_primary_agents`: returns valid global and project primary-capable agents with explicit scope and descriptions for TUI, remote, Harness, and SDK callers.

The shipped seed is versioned separately from the store schema. A newer binary can commit paths introduced by a newer seed to an existing valid store. The operation preserves existing working files, rejects damaged committed-but-missing paths, updates the durable seed version once, and treats later deletion as user authority rather than a reason to recreate content.

Session delivery remains outside this module. `Session` atomically owns appended message content, active identity, active transition ID, and the structural profile-message identity index. App-core and TUI callers own idle gating, persistence, provider preflight, continuation reset, cache attribution, context accounting, UI cards, and errors.

Before provider preparation or context mutation, `Session::validate_active_agent_profile` requires the active ID to identify exactly one structurally indexed, code-framed user-authority profile message. Profile mutation invalidates any earlier rewind-undo snapshot. Context APIs derive protection from authoritative Session or Agent state and persist the captured active ID through ready-draft apply validation. The deprecated Claude CLI adapter preserves the complete consecutive trailing user-authority sequence rather than extracting only the latest user message.

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

The renderer cannot execute repository code, call helpers, load arbitrary paths, or escape configured instruction roots through partial references. The repository service owns path and symlink policy.

Static prompt bytes are stable for the lifetime of one activation. Disk and config changes do not mutate them. The runtime itself still has no watcher, source hash, Git revision, freshness state, truncation, or cache policy. Session request paths keep dynamic skill text and turn reminders separate and preserve tool-definition locking.

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
- Applicable invalid and ambiguous addenda fail without weakening unrelated-invalid-resource isolation
- Legacy compatibility fallback only on `ResourceNotFound`, with explicit scope and unqualified specificity preserved
- Atomic direct clear and old-session migration, including provider continuation invalidation
- Four-MiB metadata-only startup-stub loading without prompt or skill-body materialization
- Harness `initial_agent_selection` capability negotiation and older-server rejection
- Versioned shipped-seed upgrades without overwriting working files or recreating adopted deletions
- Current-source transition and replacement composition with exact specificity and applicable addenda
- Atomic append switching, same-agent no-op, replacement continuation/cache transition, and tool-lock independence
- Active-profile Context Editor locking before curator work and rewind/undo identity preservation
- Raw, Markdown, replay, protocol, Harness, Rust SDK, TypeScript SDK, and wide/narrow TUI mechanisms

The fixtures use synthetic prose. They do not snapshot, require, forbid, or judge Mirza-approved instruction wording.

## Handoff to later packages

- WP-02 supplies validated Git-backed global and project roots plus import receipts through `InstructionRepositoryService`.
- WP-03 supplies complete primary composition, exact session freezing, initial selection, and lifecycle foundations.
- WP-04 supplies append transitions, explicit system replacement, context protection, rewind pinning, inspection, exports, and public control interfaces.
- WP-05 adopts managed skill discovery and activation snapshots.
- WP-06 and WP-07 migrate inventory rows through registered typed consumers.
- WP-09 and WP-10 use catalog summaries, diagnostics, complete content, graphs, and deterministic document serialization for the manager.
- WP-11 reconciles every inventory row and verifies exact migration equality or an approved exception.

## Git-backed instruction repository service

`InstructionRepositoryService` is the server-owned authority for instruction-store topology and mutation. Its public operations return typed repository/configuration/history state rather than raw Git output.

### Store topology

- Global: `~/.jcode/instructions`, owner-only standalone Git repository on local `main` by default.
- Git project default: a true instruction submodule, conventionally `<project>/.jcode/instructions`.
- Project external remote: an explicit URL and branch cloned under private Jcode state.
- Project external local: an explicit existing checkout path.
- Non-Git project: a standalone Git repository scoped to the project, conventionally `<project>/.jcode/instructions`, without making the parent project a repository.
- Project configuration: `<project>/.jcode/instructions.toml`, schema-versioned and fail-closed. Invalid, ordinary-symlink, and dangling-symlink explicit configuration never becomes silent global-only behavior.

Project identity reuses the accepted Startup Context Git-common-directory or canonical-directory helper, but instruction state, receipts, locks, and configuration are independent.

### Initialization and health

First initialization:

1. Acquires the repository mutation lease.
2. Plans complete eligible legacy imports without changing their sources.
3. Materializes the schema manifest and complete seed/import resources.
4. Validates the complete store with the instruction runtime.
5. Initializes Git and creates one baseline commit through an isolated index.
6. Hardens the global/private repository tree.
7. Writes a private initialization receipt.

Repeated initialization first validates the complete working manifest, resources, and dependency graph, then returns a no-op for a valid store. An existing initialized or otherwise committed invalid store is damage, not a first install. It is never silently overwritten with the shipped seed. Recovery operations include restoring a missing committed file from current `HEAD` and explicit seed recreation. Recreation preflights the complete replacement seed, imports, and branch before renaming the damaged store aside. If a later materialization fails, the error reports changed state and the exact backup path.

`inspect` exposes health, current `HEAD`, attached/detached branch, upstream and ahead/behind, parsed working/index changes, conflicts, active mutation lease, configured-branch warnings, and parent gitlink state for submodules. `validate_repository` separately exposes complete runtime diagnostics and valid/invalid resource summaries so one invalid resource does not hide unrelated valid resources.

### Read authority and paths

- A present working file is authoritative, including an empty file.
- Present invalid UTF-8 or invalid resource text remains present and is reported by the affected runtime operation.
- `AllowHeadFallback` is an explicit caller policy. It is not used by ordinary interactive reads.
- Managed paths must be UTF-8, repository-relative, traversal-free, and outside `.git`.
- Repository roots and every existing path component are checked with `symlink_metadata`. Managed reads and writes fail closed rather than following a symlink outside the configured repository.
- Project configuration discovery, reads, and writes also reject a symlinked `.jcode` directory or configuration file, including dangling symlinks.

### Drafts and commits

`open_draft` records:

- Repository and target path
- Current complete content, if present
- Exact target fingerprint
- Current complete `HEAD`

Save validates the same base, writes through a same-directory temporary file plus atomic replacement, compares affected manifest/resource/dependency behavior against a complete temporary snapshot of the expected Git `HEAD`, stages only owned paths in a private index, creates one commit with a structural operation trailer, publishes it with compare-and-swap `update-ref`, and refreshes only those paths in the ordinary index. A newly invalid manifest or resource, missing reference, or dependency cycle blocks the commit and leaves the working edit visible for repair. A retry cannot reclassify the rejected edit as pre-existing. Existing unrelated working-tree or committed invalid resources remain isolated, and a mutation may repair them. Unrelated staged, dirty, and untracked Git state remains unchanged.

Operation IDs make retry idempotent. Retry can recognize an already published commit. It also accepts the intended final working-file state after interruption between file write and commit. If a process stops after commit publication but before legacy-import working-file materialization, retry proves the commit identity and rematerializes the committed manifest and resource without another commit.

Save is disabled on detached `HEAD`. Restore validates committed content as complete UTF-8 before any working-tree write. No-op Save and restore-to-current-content create no commit.

### Mutations, history, and synchronization

The service supports:

- Write, clear body, rename, delete, and one multi-path commit for reference repair
- Commit an externally edited version
- Per-resource or repository history
- Complete UTF-8 content at a revision
- Revision comparison
- Restore through a new commit
- Explicit branch checkout or creation
- Explicit remote configuration, fetch, pull, and push
- True submodule setup with parent `.gitmodules` and gitlink changes left uncommitted

Pull requires a clean instruction repository. Fast-forward-only and visible merge modes are distinct. Conflicts remain in the working tree and typed inspection reports them. Jcode never pushes automatically, never force-pushes, never rewrites history, and never commits a parent project's gitlink.

### Mutation concurrency

One cross-process lease exists per instruction repository. Initialization, explicit recreation, project setup, branch and remote actions, imports, and content commits all hold that lease across their complete mutation. Setup callers supply structural operation IDs, and retry reuses an existing valid submodule, external checkout, or standalone store instead of duplicating it. Reuse validates the requested checkout origin, attached branch, and complete store, so retry cannot silently rebind a project to different Git identity or publish configuration for an ordinary repository. Unix uses nonblocking kernel `flock`; Windows uses an exclusive no-sharing file handle. Both release ownership on process death and do not expire a live long-running operation. Other non-Unix, non-Windows targets retain an expiry-file fallback and are an explicit unsupported safety boundary. Read-only inspection remains available while another process owns mutation. Owner metadata includes operation ID, PID, and a diagnostic deadline.

### Legacy import backend

Typed discovery covers current global and project:

- `.jcode/system-prompt.md`
- `.jcode/prompt-overlay.md`
- `.jcode/preferred-tools.md`
- `.jcode/swarm-prompt.md`

It deliberately excludes `AGENTS.md` and external skills. Import copies the complete source into a typed managed Markdown resource, records source SHA-256 and separate empty/blank semantics, validates the complete prospective manifest/resource/dependency graph, commits the resource and receipt together, and leaves the original untouched. A target that exists without a matching receipt is a conflict, not permission to overwrite it. An already-completed retry only restores missing committed files; divergent working files are preserved and listed in the typed outcome. Primary composition deactivates imported global system-prompt and overlay sources only when their durable receipts exist.

### Production boundary

App-core `Server` owns the repository service. Construction performs no repository I/O. Primary activation is live across shared-server sessions, app-core turns, local prompt construction, CLI run, direct REPL, ACP, and Harness creation. Clear and transfer reuse the same server-owned service. WP-09 and WP-10 add the central manager protocol and TUI over the existing typed service.
