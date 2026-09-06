# Model roster

The global roster defines task-oriented names for **new executions**. It does not
select the model of an ordinary interactive session. Swarm, review, judge,
ambient and scheduled callers keep their existing model settings until their
owning features explicitly adopt this API.

## Configuration

The authoritative working file is `~/.jcode/instructions/model-roster.toml` in
the private global instruction Git repository. There is no project roster or
project alias shadowing. Shipped seed 27 adds the file without overwriting an
existing working copy or resurrecting a committed deletion. The instruction
manager's inspection and editing UI are separate, later Phase 3 work.

Initial policy:

| Alias | Ordered candidates | Default effort |
|---|---|---|
| `expensive-expert` | `claude-oauth:claude-fable-5`, `openai-oauth:gpt-6-astra` | `max` |
| `architect` | `claude-oauth:claude-fable-5`, `openai-oauth:gpt-6-astra` | `xhigh` |
| `coder` | `openai-oauth:gpt-6-astra`, `claude-oauth:claude-opus-5` | `xhigh` |
| `researcher` | `openai-oauth:gpt-6-astra`, `claude-oauth:claude-opus-5` | `low` |
| `fast-worker` | `openai-oauth:gpt-5.6-terra` | Provider/model default |

Models, order, descriptions and efforts are editable data, not a compiled model
enum. For example:

```toml
[aliases.example]
description = "Description of when a future caller should select this alias."
models = ["openai-oauth:gpt-6-astra", "claude-api:claude-opus-5"]
default_effort = "high"
notes = "Optional human-only rationale."
```

Alias IDs use the instruction ID vocabulary: lowercase ASCII letters, digits,
`-`, `_`, and `.`, beginning with a letter or digit. Descriptions are required and
candidates must be nonempty. Effort is optional and uses canonical provider
values, not Jcode workflow sentinels such as `swarm`. Unknown fields, tags and
alias chaining are rejected. `[aliases]` with no entries is an intentional empty
roster. A blank or malformed file is invalid, not permission to restore a seed.

Native Anthropic and OpenAI candidates must pin authentication, such as
`claude-oauth:`, `claude-api:`, `openai-oauth:` or `openai-api:`. Bare `claude:` and
`openai:` are not sufficiently qualified. Named endpoints use
`profile-id:model` or `openai-compatible:profile-id:model`. OpenRouter candidates
use `openrouter:vendor/model` or `openrouter:vendor/model@Provider`. Managed
subscription routes use `jcode-subscription:model`. Context profiles, endpoint
namespaces and provider pins are retained rather than normalized away.

## Resolution and failures

For each new execution, the caller supplies a current production route catalog.
The resolver checks route-qualified credentials through existing auth state,
catalog model availability, exact independent runtime construction and effort
support. It does not make a model request. Existing provider credential refresh
and catalog refresh behavior remain provider-owned. Local availability is not a
claim that an untested provider will accept the next request or has remaining quota.

Candidates are tried in order. Effort precedence is explicit request, alias
default, then the fresh provider/model default. Provider-owned normalization is
retained, including OpenRouter's accepted `max` alias for `xhigh`. The effective
value, not an assumed input spelling, is persisted. Unsupported effort rejects
that candidate and permits the next candidate.

An explicit model override bypasses the alias's candidate list, not its default
effort or validation. A named request still requires a valid alias. A
concrete-only request has no alias and needs no roster source. An invalid
explicit model never falls back to alias candidates or the coordinator's model.

Success includes the concrete resolution and earlier candidate rejections.
Failure accounts for every attempted candidate in priority order. Typed failures
distinguish source/schema/alias/request/catalog errors, missing credentials,
unavailable route or model, ambiguous route, unsupported effort and runtime
rejection. Catalog-only or auto-auth routes without a safe independent
constructor are rejected, not silently sent over another endpoint. The initial
roster does not depend on those unsupported routes.

Valid aliases remain usable alongside invalid unrelated aliases. Syntax errors
that prevent parsing the whole TOML file fail the load. Ordinary primary
composition does not validate an unused roster. Required selected instruction
damage still blocks, and full-store validation remains at initialization, seed
upgrades and Git publication.

## Working files, Git and recovery

Each service operation reads current source. A valid uncommitted edit affects
later resolutions immediately. Existing execution records remain unchanged.
Repository edits use the existing scoped Git draft/commit/history/restore
service and its concurrency and retry protections. Invalid roster writes cannot
be published through a new commit or blessed by retry.

Normal callers use `InstructionReadPolicy::WorkingTreeOnly`. Future unattended
callers may explicitly use `AllowHeadFallback`: only a genuinely absent working
file may be read from current Git `HEAD`. Present malformed, empty, invalid UTF-8,
symlinked or unreadable files do not trigger fallback. Missing initialized stores
remain damage. First installation and seed adoption use the existing instruction
store owner. No push, profile switch, model request or retry is implicit in a
roster edit or read.

## Backend interface and execution ownership

`jcode_base::model_roster` provides:

- `ModelRosterService::{load,list,inspect,validate,availability,preview,resolve}`
- `ModelRoster` for inspecting/resolving one immutable parse
- `RosterCatalog::from_provider` for current provider-owned route/auth state
- `ModelRosterRequest` for optional alias, concrete override and effort override
- `PreparedRosterExecution` with an independently constructed provider, concrete
  resolution and rejected earlier candidates
- `ModelRosterResolution` with serializable requested alias, exact route/model,
  effective effort, and override provenance

The resolution retains the complete structured provider selection, including
OpenRouter pin identity. Accessors expose model, provider key, route API method,
effort and provenance without requiring callers to parse a routed string.

```rust,ignore
let catalog = RosterCatalog::from_provider(provider.as_ref())?;
let prepared = ModelRosterService::new(repositories.clone()).resolve(
    &ModelRosterRequest::alias("coder"),
    &catalog,
    InstructionReadPolicy::WorkingTreeOnly,
)?;
// Owner validates complete context and permissions, persists
// prepared.resolution atomically with its execution, then dispatches through
// prepared.provider. No execution is published before those checks succeed.
```

Resume deserializes the stored `ModelRosterResolution` and uses
`restore_provider()`. It never opens the roster or resolves the alias again.
Unavailable stored identities fail rather than switch provider. This reusable
execution-state type deliberately adds no alias field to ordinary primary
sessions. No roster hashes or instruction Git revisions are persisted.

### Future owners

**Phase 4, isolated delegation:** expose only relevant alias descriptions, accept
optional overrides, resolve at child launch, persist the concrete result and
return failures as ordinary tool errors. Own prompt context, attachments,
permissions, cancellation, concurrency, result handling and post-launch retry or
replacement policy.

**Phase 9, asynchronous execution:** store aliases in job definitions only when
future runs should track edits. Resolve each new run and persist its concrete
result. Own quotas, approvals, unattended context failures, retries and
replacement execution. Explicitly opt into the missing-file Git fallback rule.

**Phase 3 WP-09/WP-10:** consume the list/inspection/diagnostic/availability views
and existing repository service for central management. Model-facing discovery
contains only alias and description, never human-only notes. Do not build another
route parser or credential detector in the TUI.

**Phase 10, maintenance:** evolve roster data deliberately as models and routes
change. Preserve running execution identities and distinguish shipped seed
updates from accepted downstream working policy.
