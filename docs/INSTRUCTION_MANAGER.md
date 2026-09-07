# Instruction manager

`/instructions` opens the full-screen instruction manager in local
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
Editing is explicit through Actions or Ctrl-E. Merely opening, searching, or reading the manager performs no source mutation.

The connected server owns remote discovery and previews. Client-local files
cannot substitute for the connected session's project or global instructions.
The model roster uses the existing independent provider-construction API.
Availability is catalog/constructor evidence, not a claim about quota or a
successful inference request.

## Navigation

Start by browsing or searching the resource list, then **Enter** to read a file.
**Space** opens searchable **Actions**, and **T** opens **Choose a view**. Every
inspection workflow has a named entry, shortcut and explanation. Unavailable
views explain why instead of starting an operation that cannot apply.

**F** opens **Filters**. Choose a field, then an explicit value. The selected
value is labeled, other filters stay unchanged, and active filters remain visible.
Use **C** to clear every filter. Search filters the resource list by ID, name,
type or scope; it never edits source.

The spatial model is repositories/session, resource list, and reading pane:

- At 140+ columns, all three panes are visible.
- At 90–139 columns, the list and reading pane share the screen. F1 exchanges the
  left navigator for repositories/session.
- At 80 columns, in a 60-column split, and down to 24×8, the focused pane occupies
  the available width. No operation requires a hidden pane or unlabeled button.
- Below 24×8, a truthful size message replaces the layout. Resize to continue or
  Q/Escape to close. No resource size limit is involved.

The focused pane has a text marker and strong selection styling, not color alone.
Z expands or collapses the focused pane. Source identity, view and content position
stay visible while reading. Lists scroll continuously and show a scrollbar.

| Input | Action |
|---|---|
| Enter / Right on resource | Read complete source |
| Enter / Right on repository or session | Open its readable Overview |
| Space / `:` / Ctrl-P | Searchable Actions |
| T or Enter in reading pane | Choose a view |
| F | Explicit filter fields and values |
| `/` | Search the resource list |
| Enter/Escape in search | Finish search and retain results |
| Left/Right, Home/End, Delete, Backspace, Ctrl-U in search | Edit search text; Ctrl-U clears it |
| Tab / Shift-Tab | Move between panes |
| F1 / F2 / F3 | Repositories / resources / reading |
| Up/Down or J/K | Navigate a list or scroll |
| PageUp/PageDown | Move farther within the current page |
| Home/End outside search | Start/end of this transport page |
| N/P | Next/previous resource, history, or captured content page |
| Escape / Left / Backspace outside text entry | Back to history or browsing, then repositories |
| S / V / E / O | Direct scope / validation / effectiveness / origin filter menu |
| G | Toggle project-redefinition grouping |
| C | Clear all filters, including search and repository |
| R / X | Refresh sources / cancel inspection loading |
| `?` | Complete, scrollable help |
| Q | Close directly and return to chat |

Menus use arrows and Enter, or type to filter their named entries. Escape backs
out without applying a choice. `?` shows a complete scrollable explanation of a
menu entry. Selecting a disabled entry opens its reason, without a request.
Pasted text belongs to the search/menu field, not the hidden composer or file-drop
handler. Search supports Unicode cursor editing.

Visible pane labels, controls, menu entries and list rows are clickable. The mouse
wheel scrolls the surface under the pointer. A menu clears underlying hit regions,
so clicks cannot trigger invisible actions. Most terminals allow Shift to bypass
mouse capture for terminal-native selection.

Back from a revision returns to the same history selection and comparison base.
Back from a completed reading view keeps its captured content and position; help
has a separate scroll position. Canceling an in-flight request rejects its stale
reply. Refresh/reconnect recaptures source state rather than claiming the old view
is fresh. None of these operations activates instructions.

## Reviewed editing

Use **Actions → Edit instruction** (or **Ctrl-E**) on a managed resource. A draft
captures complete working source, branch, HEAD and file identity without changing
the authoritative file. In the draft, **B** opens the body in a private local
file using `$VISUAL`, then `$EDITOR`, then `nano`. Quoted executable paths and
arguments are parsed without a shell. Editors that launch a GUI must use their
own wait argument. A remote client edits its local draft and sends typed content
to the source-owning server, never a remote shell command.

**M** opens structured metadata. Resource kind and existing identity are fixed;
Rename is a separate reference-aware operation. Agent availability, template mode,
addendum targets, module references, default agents and model-roster entries use
typed fields and searchable choices. Roster candidates can be added, removed and
reordered. Human-only notes remain outside model-facing discovery. Form errors
retain entered values instead of replacing them with a raw configuration editor.

**R** validates the complete proposed commit and affected consumers using typed
synthetic preview data. **D** shows exact committed/working-versus-draft diffs,
and **V** shows affected component previews. These are not the current session's
stored instructions. **S** saves a successfully reviewed draft on an attached
branch. It creates one scoped local commit, or no commit when unchanged. It never
pushes or commits the parent project's gitlink. Ordinary reviewed Save has no
redundant second confirmation.

Named Actions also prepare global/project resources, project redefinitions,
agent addenda, clear-body changes, user-resource renames/deletions, external
working versions, and historical restores. Clear retains identity and intentional
empty semantics. Delete exposes any lower-precedence definition. Same-repository
reference repairs share the reviewed commit. Known cross-repository dependents
require an explicit create-new-ID, repair-references, remove-old-ID sequence.
Unopened projects are not claimed to have been scanned.

Tab/Shift-Tab chooses an affected draft file. Close preserves its recovery record
and releases editing ownership without saving. Discard is explicit and does not
rewrite Git history or undo completed source changes. Validation, editor and Git
failures preserve drafts and distinguish unchanged source from possibly completed
writes. A lost Save receipt is reconciled through its structural operation ID,
not by replaying a guessed new edit.

## Detail views

Select a resource, then press:

1. **Source:** complete original UTF-8 file, including exact frontmatter.
2. **Overview:** identity, availability, template mode, scope, source path,
   validation, owner, code-consumer contracts, and complete frontmatter.
3. **Rendered preview:** plain/Handlebars result, effective agent component with addenda,
   skill body, or model-roster availability and resolution preview. Available
   typed skill-catalog values are supplied. Occurrence-specific values that do
   not exist in an inspection produce an explicit error, not invented values.
4. **System:** complete current-source primary composition for a selected agent,
   using the inspection's captured capability and skill-catalog inputs. Refresh
   first to recapture those inputs after source or configuration changes.
   For the Session row, this retrieves the exact **stored** system prompt instead.
   An isolated-only agent's component can be inspected but cannot be previewed as
   an eligible primary activation.
5. **Dependencies:** render dependencies, validation-only references, validated reverse
   resource consumers, registered code consumers, and explicit unresolved-graph
   diagnostics. Failed graphs are not silently treated as an exhaustive empty
   consumer list. Structural framing stays code-owned.
6. **History:** repository or per-resource commits, pinned at inspected HEAD.
7. **Working changes:** working changes against HEAD, or a selected two-revision comparison.
8. **Compare scopes:** complete global and project source versions with explicit headings.

In History, Enter reads exact revision content. I opens complete commit details,
including author, date, subject and changed paths. A chooses the comparison base.
Select another commit and press B to compare them. The named Actions menu and contextual footer offer the same actions. Commit details include author, date, subject, and changed paths.

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
without overwriting session prompts. Unsaved edits are held separately from inspection snapshots. Refreshing inspection never commits them.

Git inspection disables optional index refreshes, filesystem-monitor hooks,
external diffs and text conversion. It does not run repository scripts or fetch
remote state. Ahead/behind reflects already available local tracking refs.

The content-safe `instruction-manager` client debug command reports selection,
layout, open menu, scroll, validation flags, page counts, repository flags and
pending IDs. It does
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

## Interaction ownership for later manager work

WP-10 extends this same navigation/action model for drafts, metadata forms, diff
review, Save, restore, Copy, setup and explicit sync. Its own mutation safety and
recovery work remains the primary task. Each added action must state its target,
consequence and availability; do not introduce a competing hotkey-only workflow.
WP-11 evaluates complete combined journeys and repairs inconsistencies during
its existing integration acceptance, without starting another UI redesign.
