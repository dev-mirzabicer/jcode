# Instruction runtime foundation

Phase 3 WP-01 adds the typed, file-backed instruction runtime under `jcode-base::instruction`. It is a foundation for later Phase 3 packages. No production prompt caller uses it yet.

## Ownership

The runtime owns:

- Stable instruction IDs
- Global and project scope
- Managed resource discovery
- YAML frontmatter parsing
- Project-first and explicit-scope resolution
- Plain and restricted Handlebars modes
- Module dependency and reverse-consumer graphs
- Complete rendering
- Agent metadata and activation availability validation
- Typed registered-consumer rendering
- Dedicated global and project `AGENTS.md` reads
- Typed errors for missing, invalid, ambiguous, empty, and cyclic resources

It does not own:

- Git repositories, history, drafts, or mutation
- Session activation or frozen prompt persistence
- Message roles, structural tags, timestamps, receipts, or queueing
- Provider request framing or continuation state
- Startup Context selection or context-control operations
- Tool schemas or permissions
- TUI presentation

## Resource layout

An `InstructionSources` value names the global store and optional project store. The current discovery layout is:

```text
system/*.md
agents/*.md
addenda/*.md
modules/*.md
notifications/*.md
tools/*.md
skills/<name>/SKILL.md
```

The later repository-service package supplies real store paths and lifecycle. WP-01 tests use temporary directories only.

## Document format

Managed Markdown starts with YAML frontmatter:

```markdown
---
id: reviewer
name: Reviewer
kind: agent
description: Reviews completed implementation work.
availability: both
template: handlebars
includes:
  - quality
  - global:safety
---

{{> quality}}

Review {{project.name}}.
```

Rules:

- `id` is stable and uses lowercase ASCII letters, digits, `.`, `_`, and `-`.
- Skills may use their existing `name` as the stable ID.
- `kind` must match the containing resource family.
- Plain text is the default.
- Handlebars is opt-in with `template: handlebars`.
- Agents require nonempty `name`, `description`, and `availability`.
- Addenda require an explicit `target` agent selector.
- `includes` are module references rendered before the resource body.
- `allowed-tools` is parsed for skill compatibility but remains metadata, not a runtime permission grant.
- Empty bodies are valid. A registered consumer can declare that empty prose is not meaningful.

`InstructionDocument::to_markdown` produces deterministic frontmatter for manager and round-trip use. It preserves the body text held by the domain document.

## Resolution

References may be:

```text
shared-module
project:shared-module
global:shared-module
```

Unqualified references use a present project definition first and otherwise global. A present invalid or ambiguous project definition fails. It never silently falls back to global.

Duplicate stable IDs in one kind and scope are ambiguous. They remain visible through `InstructionRuntime::resources` and fail only callers that select them. Invalid unrelated resources remain visible through diagnostics and do not block valid resources.

Dedicated `AGENTS.md` sources are not catalog resources. `render_external_agents` returns global content before project content, matching the accepted Phase 3 target ordering.

## Restricted Handlebars

The adapter uses Handlebars 6 with:

- Strict missing-value behavior
- Serialized typed values
- HTML escaping disabled
- Registered managed module partials only
- Project-first or explicit-scope partial resolution
- Pre-render dependency and cycle validation
- No custom helper registry
- No scripts
- No arbitrary filesystem loader
- No block helpers or subexpressions

Allowed expressions are typed value paths such as:

```handlebars
{{project.name}}
```

Allowed partials are managed module references:

```handlebars
{{> quality}}
{{> project:quality}}
{{> global:quality}}
```

Plain documents treat Handlebars-like text literally.

## Complete rendering and limits

The runtime reads complete UTF-8 files and returns complete rendered text. It adds no source-size, body-size, instruction-ID-length, graph-depth, expansion, or output-size cap.

Dependency traversal is iterative, so a deep finite acyclic graph does not consume the Rust call stack. Filesystem, UTF-8, allocation, or operating-system failures remain explicit errors. The runtime never returns partial text as success.

## Registered consumers

A code-owned consumer registers stable identity and behavioral metadata without embedding the human prose:

```rust
let registration = ConsumerRegistration::new(
    "startup-context.stale",
    "startup-context-stale",
    InstructionKind::Notification,
    "notifications/startup-context-stale.md",
    "Startup Context",
    "Startup Context retains XML, receipt state, role, ordering, and persistence.",
)?;
let consumer = InstructionConsumer::<StartupContextStaleValues>::new(registration);
let rendered = consumer.render(&runtime, &values)?;
```

The generic value type fixes the typed rendering contract at the consumer declaration. The runtime returns text and dependency information. The subsystem still owns delivery and structure.

A required registered singleton that is absent returns `RegisteredResourceMissing` with the expected path. An ordinary user-defined resource that was deleted returns `ResourceNotFound`. These states are deliberately distinct.

## Agent metadata

Agent resources expose:

- Stable ID
- Display name
- Selection description
- `primary`, `isolated`, or `both` availability
- Template mode
- Body and module dependencies

`render_agent` validates availability and renders complete instructions. It does not activate a session or choose a model. Phase 3 WP-03 owns primary session activation and WP-04 owns transitions.

## Verification

Synthetic mechanism tests cover:

- Every resource kind and semantic round trip
- Plain literal braces
- Typed Handlebars values without HTML escaping
- Project-first and explicit-scope partials
- Invalid project shadowing
- Unrelated-invalid-resource isolation
- Empty, missing registered singleton, and deleted user-resource semantics
- Duplicate-ID ambiguity
- Agent availability
- Addendum target validation
- Metadata includes and reverse consumers
- Deep finite graphs
- Cycle rejection
- Missing dependencies
- Unknown values
- Helper rejection
- Multi-megabyte complete rendering
- Dedicated global-before-project `AGENTS.md`

The tests use synthetic prose. They do not snapshot or judge central prompt wording.

The full production instruction migration map is [`INSTRUCTION_INVENTORY.md`](INSTRUCTION_INVENTORY.md).
