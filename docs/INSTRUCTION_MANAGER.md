# Instruction manager

`/instructions` opens the full-screen **read-only** instruction manager in local
and server-connected TUI sessions. `/prompts` is an alias. `/model-roster` opens
its global model-policy section directly.

Related entry points are `/agent instructions`, `/skills instructions`, and
`/swarm-prompt inspect`. Existing profile selection, skill invocation, and the
legacy `/swarm-prompt` editor command retain their separate behavior.

## What opening it does not do

Inspection does not initialize or upgrade a store, import or copy a skill,
activate an agent, replace a system prompt, dispatch a model request, commit a
file, change branches, or synchronize remotes. An absent store stays absent.
Damaged sources remain available for diagnosis rather than being recreated.
Editing and repository mutation actions are not exposed by this interface.

The connected server owns remote discovery and previews. Client-local files
cannot substitute for the connected session's project or global instructions.
The model roster uses the existing independent provider-construction API.
Availability is catalog/constructor evidence, not a claim about quota or a
successful inference request.

## Navigation

The three panes are repositories/session, resources, and detail. Wide terminals
show all three. Narrow terminals show the focused pane. Tab or Shift-Tab cycles
panes, F1/F2/F3 selects one directly, and Z expands/collapses the focused pane.
The same state machine and actions apply at every width.

| Input | Action |
|---|---|
| Up/Down or J/K | Select a row or scroll detail |
| PageUp/PageDown | Move farther within the current page |
| Home/End | Start/end of the current page |
| Enter on resource | Open metadata |
| Enter on repository | Filter resources to that repository |
| Space on repository/session | Inspect complete repository state or session/default metadata |
| `/` | Edit search by ID, name, kind, or scope |
| Enter/Escape in search | Leave search without closing the manager |
| Ctrl-U in search | Clear the search text |
| F | Cycle resource kinds |
| S | Cycle global/project/all scopes |
| V | Cycle valid/invalid/all |
| E | Cycle effective/shadowed/all |
| O | Cycle managed/legacy/external/all origins |
| G | Group project redefinitions (or other resources) |
| C | Clear filters, including repository filter |
| N/P | Next/previous resource, history, or exact text page |
| R | Capture fresh authoritative inspection state |
| X | Cancel loading |
| `?` | Scrollable help, including current filters |
| Q/Escape | Close the manager |

Pane labels, filter controls, numbered detail tabs, and footer controls are
clickable. Click list rows to select them and use the detail tabs or Enter
control to inspect them. The wheel navigates the pane under the pointer.
Search and filter actions focus the resource list, so a new query cannot keep
targeting an older detail selection. Pasted text belongs to the open search
field, never the hidden composer or file/image drop handlers.

## Detail views

Select a resource, then press:

1. **Source:** complete original UTF-8 file, including exact frontmatter.
2. **Metadata:** identity, availability, template mode, scope, source path,
   validation, owner, code-consumer contracts, and complete frontmatter.
3. **Rendered:** plain/Handlebars result, effective agent component with addenda,
   skill body, or model-roster availability and resolution preview. Available
   typed skill-catalog values are supplied. Occurrence-specific values that do
   not exist in an inspection produce an explicit error, not invented values.
4. **System:** complete current-source primary composition for a selected agent,
   using the inspection's captured capability and skill-catalog inputs. Refresh
   first to recapture those inputs after source or configuration changes.
   For the Session row, this retrieves the exact **stored** system prompt instead.
   An isolated-only agent's component can be inspected but cannot be previewed as
   an eligible primary activation.
5. **Relations:** render dependencies, validation-only references, validated reverse
   resource consumers, registered code consumers, and explicit unresolved-graph
   diagnostics. Failed graphs are not silently treated as an exhaustive empty
   consumer list. Structural framing stays code-owned.
6. **History:** repository or per-resource commits, pinned at inspected HEAD.
7. **Diff:** working changes against HEAD, or a selected two-revision comparison.
8. **Scopes:** complete global and project source versions with explicit headings.

In History, Enter reads a selected revision. A chooses the comparison base.
Select another commit and press B to compare them. The A/B footer controls offer
the same actions. Commit details include author, date, subject, and changed paths.

## Source truth

The resource catalog includes valid, invalid, ambiguous, missing registered,
effective and shadowed candidates. An invalid unrelated resource does not hide
valid rows. Managed paths must be regular files under real directories. Unsafe
symlinks and special files remain diagnostics rather than being read or rendered.

Global/project specificity comes from the instruction runtime. Paired common
and preferred-tool contributions are additive. Skill effectiveness comes from
the skill catalog's invocation precedence, not a second UI resolver. Metadata
states registered consumer scope policies separately from unqualified source
resolution. Project redefinitions are explicitly marked and grouped with G. High-impact
system/profile/control redefinitions show `[HIGH]` in the list and a complete
warning in Metadata. They are identified, not prohibited.

`AGENTS.md` remains a dedicated ecosystem input. Legacy files show import and
compatibility eligibility separately from managed source. A newly added global
legacy file is not described as active after an initialized store has already
cut over to managed sources. External skills are read-only, including shadowed
sources and visible parse failures. Copy is a later mutation workflow.

The roster is global-only. Invalid aliases remain visible alongside valid ones.
A project roster file is shown as invalid/inactive rather than hidden policy.
Human-only notes are visible in human inspection and remain absent from the
roster's model-facing discovery operation.

## Paging, refresh, and failures

Resource and history pages are transport pages, not total catalog limits.
Exact detail is captured as one immutable document, identified by an opaque
snapshot/document pair. Each text page states its byte range and total. N/P or
scrolling at a page boundary reaches the remaining content. A prefix is never
labeled a complete document. Large finite instructions are neither shortened
nor summarized to fit the interface. Terminal control characters are displayed as
explicit Unicode escapes; the captured source bytes remain unchanged.

Only the current text page is wrapped and rendered on the client. Arbitrarily
long documents do not depend on a 16-bit terminal scroll offset. Catalog and
repository state describe their capture, not a filesystem watcher. An explicit
source/render detail operation reads current source. R recaptures the catalog
and repository state after external changes. Exact already-open detail stays
stable until selection, cancellation, refresh, or close.

Requests are correlated before transport by request, session, snapshot, and
exact detail/page identity. Superseded responses cannot replace a newer
selection. Reconnect refreshes the open manager and preserves filters and safe
selection. A source change, absent snapshot, or unavailable transport is visible
without overwriting session prompts. No unsaved edits exist in this manager.

Git inspection disables optional index refreshes, filesystem-monitor hooks,
external diffs and text conversion. It does not run repository scripts or fetch
remote state. Ahead/behind reflects already available local tracking refs.

The content-safe `instruction-manager` client debug command reports selection,
layout, validation flags, page counts, repository flags and pending IDs. It does
not include source or rendered instruction bodies.

## Implementation and verification

- `jcode-instruction-types` owns the pure read-only request/result/page vocabulary.
- `jcode-base::instruction::inspection` projects the existing runtime, composer,
  repository, skill and roster domains and owns cancelable connection-local work.
- App-core supplies server-owned session context and asynchronous replies.
- The TUI shares one reducer, renderer and input model across local/remote paths.

Mechanism coverage uses synthetic content and real temporary Git repositories,
exact multi-page reconstruction, scoped failures, current-source previews,
history, cancellation, real physical input routers, and terminal-cell rendering.
It does not grade or freeze central prompt prose. See
[the acceptance record](dev/INSTRUCTION_MANAGER_ACCEPTANCE.md) for observed
verification and activated-runtime evidence.
