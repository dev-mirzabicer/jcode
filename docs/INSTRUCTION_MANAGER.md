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

## Types, scopes and source ownership

The first page shows **Agents**, not a mixed file inventory. The type navigator
also provides Skills, Project additions, Shared modules, Notifications,
System/workflow guidance, Tool guidance, Model roster and AGENTS.md guidance.
Imports and original compatibility files have a secondary page. All-types search
is available when you need it.

Choose a type with the mouse or F1 and arrows. The [+] marker identifies the
active page and the list updates immediately. Scope stays visible in the header:

- **Effective here** groups equivalent sources under one named item. It preserves
  the runtime's actual applicability rules, including invalid shadows and paired
  additive contributions. It does not describe the running session's frozen text.
- **Global** browses global definitions directly.
- **Project** browses definitions stored for this project. Empty project scope
  offers setup or creation rather than pretending global files belong to it.

Skill rows display invocation names and descriptions, never only `SKILL.md`.
[G] and [P] indicate scope; `ext` identifies externally installed sources before
you select them. The source overview includes complete versions and technical
details. Select **Sources** to view or edit a specific global/project version.

Selecting an existing item never changes its owning source. **Edit global** and
**Edit project** name the exact destination. **Copy to project/global and edit**
creates a private draft of the complete source in the other scope. Existing
versions are shown as alternatives and cannot be overwritten implicitly. External
skills offer complete-package Copy instead of in-place editing.

Repository administration is separate under **Repositories & sync**. Global and
project controls cover setup, branches, history and explicit synchronization.
The header offers **Project setup** directly, including the conventional submodule
workflow for Git projects. Enclosing Git repositories for external files are
source metadata, not extra managed instruction stores in the navigator.

**Current session instructions** is an inspect-only snapshot page. It is not a
save destination, and editing source elsewhere does not replace that snapshot.

| Input | Action |
|---|---|
| F1 / F2 / F3 | Types and pages / item list / reading |
| Arrows or J/K | Select a type, item or scroll content |
| Enter | Open the selected page or complete overview |
| S | Explicit scope choices |
| E or Ctrl-E | Edit the selected source |
| V | Source versions and cross-scope copying |
| F4 / F5 | Project setup/controls / global repository controls |
| F6 | Create in an explicitly selected scope |
| Space / colon / Ctrl-P | Searchable contextual Actions |
| F | Additional filters |
| Slash | Search within the current type and scope |
| C | Clear filters and search across all instruction types |
| N/P | Next/previous transport page |
| PageUp/PageDown, Home/End | Scroll the current content page |
| Escape / Left / Backspace | Back without saving |
| Q | Close and preserve unsaved drafts |
| R / X | Refresh sources / cancel inspection loading |
| ? | Complete help; inside a draft, contextual draft actions |

At wide widths the type navigator, item list and reading pane share the screen.
At 80/60 columns the focused pane fills the view. Scope/setup and contextual
controls remain reachable at the tested 24×8 minimum. Mouse hit regions match
visible controls. Search and metadata fields own pasted text rather than leaking
it into the chat composer. No pagination or instruction-size limit changed.

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

## Repository controls

Actions exposes **Global repository controls** and **Project repository controls**.
Choose an operation through typed fields, then read the complete plan. **Y**
applies that plan once. Enter or N returns to the retained form without applying
anything. Setup shows the actual session project, destination, branch, URL and
parent-project consequences. An external checkout path refers to the server,
not the remote client's filesystem.

Available operations cover genuinely new global initialization, explicit seed
recreation with an operation-specific preserved backup, project submodule setup,
private external cloning, existing checkout attachment, non-Git standalone setup,
missing-checkout repair, remote configuration and branch selection. No real
project is configured merely by opening these controls.

Fetch is explicit and leaves the working branch unchanged. Pull is explicitly
**fast-forward-only** and rejects divergence rather than creating a surprise
merge or rebasing history. Existing conflicts remain visible for deliberate Git
resolution. Push lists outgoing commits against locally known tracking state and
sends the exact reviewed local commit, without force, tag following or recursive
submodule pushes. Fetch-and-select can retrieve a named remote branch even when
the initial clone fetched only one branch. Branch names are literal, not reflog
shortcuts. Creating a branch at the current detached commit preserves unrelated
working files; other checkout/pull actions require clean working state. Attached
drafts block branch changes.

Project configuration is rechecked under its setup lease. Repository files,
branch, index and references are checked again before a reviewed Git operation.
A changed review is rejected rather than applied to another state. Configured
missing remote checkouts can be restored explicitly. Submodule recovery retains
parent history and uses surviving Git metadata where available. A missing local
checkout with no configured remote needs a backup or an explicitly selected
replacement, never silent seed substitution.

Repository plans and receipts are private durable records. **Recover last
repository outcome** reads the receipt without replaying the action. It
separates a live running operation from an interrupted uncertain outcome. A
terminal receipt survives response loss; an uncertain network operation is not
blindly repeated. Failed requests retain form values for correction and a new
review. Seed recreation keeps old source and Git history in its named backup.
Existing sessions continue to use their stored instructions throughout.

## Skill packages

Select an external skill, including a shadowed source, then choose **Copy skill to
global** or **Copy skill to project**. An optional package ID handles invocation
names that are not valid repository IDs. Copy captures the complete package,
binary references, original entry-point source and attribution into an unsaved
draft. It does not initialize a missing destination or execute package scripts.
Review shows the destination and whether that copy will be effective in the
source project. Existing active skill text stays frozen.

Managed skill Rename, Delete and project redefinition cover the entire package,
not only `SKILL.md`. Rename changes the stable resource ID and package path while
preserving its invocation name unless metadata is explicitly edited. Existing
colliding packages are not overwritten. Auxiliary text files use plain body
editing. Binary files show their byte count and SHA-256 while retaining complete
original bytes in the private draft. They cannot be rewritten through a text
editor. On Unix, executable file modes survive Copy, rename and ordinary edits;
private stores retain owner execute access without granting group/other access.
Mode changes are visible in review and included in stale-state checks.

Invalid UTF-8 instruction files remain recoverable through a valid historical
revision. Draft capture preserves their original bytes and does not mistake them
for missing or empty source. Private draft schema 2 adds mode-aware file state
and still reads existing schema-1 recovery records.

## History, legacy imports, and ecosystem inputs

**Restore committed version** prepares current Git HEAD for review. **Restore
revision** restores selected historical source through a new commit, never reset
or history rewrite. For a skill, restoration covers the complete historical
package, including reference bytes and executable modes. Newer files absent in
that package revision are listed for removal in the same diff. Repository-wide
history offers a changed-path picker, so deleted user resources remain restorable.

**Export selected revision** writes exact historical file or package bytes into a
new owner-only client-local directory under `instruction-exports` in durable
state. The final path is shown in the manager. Export does not change source,
Git history, or session instructions, and never overwrites an existing export.
Binary references remain binary, not summaries.

**Import legacy source** captures an eligible compatibility file and creates a
reviewed draft containing both its managed definition and cutover receipt. The
original file remains untouched. A pre-existing managed destination is shown in
the diff rather than silently replaced. An already imported source directs you
to its managed definition, preventing duplicate contributions. Global and project
imports use the same canonical target identities as activation.

`AGENTS.md` remains a dedicated ecosystem input. Its editor opens a private draft,
then Review and Save update only the working file. The review explicitly says
**no parent commit**. It never stages or commits the parent's unrelated files.
A concurrent external edit blocks Save while retaining both versions. Its
private draft and completed working-file receipt survive reconnect/reload. Small
owner-only edit-lock metadata under the source parent's `.jcode` directory
coordinates writers without adopting that parent as an instruction repository.

Typed metadata forms retain complete values. **Edit complete selected value in
external editor** supports long or multiline descriptions and human notes without
a native multiline editor. Constrained enum fields stay pickers. Invalid roster
aliases do not disappear: a damaged roster exposes its complete original source
for explicit repair. Submitting unchanged metadata preserves original formatting.

Global rename analysis uses global ownership for global consumers. If automatic
repair would change a reference supplied by an active project shadow, it stops
and asks for an explicit cross-repository migration instead. Confirmations remain
bound to their original resource, revision and draft generation.

## Retained drafts and unsent client values

**Recover drafts and operations** lists retained server drafts and repository
receipts for this session. Listing reads metadata without loading every draft
body. Invalid records remain reported rather than hiding valid ones. Resume
loads the exact private draft, not a new system prompt. A completed Save is
recognized and shown as completed without creating another commit.

For stale source, **Compare draft with current source** exposes complete opened,
current-working and proposed versions. **Edit current source instead** creates a
new private draft from the compared working version. **Keep proposed text on
compared base** creates a new private draft retaining the proposal against that
base. Neither choice automatically merges text or writes source. The original
draft remains available, later source changes reject the comparison, and another
validated Review/Save is required.

Unsubmitted client forms and unacknowledged typed updates have separate private
recovery capsules under the client's durable state. They are partitioned by
session and client, so another client cannot overwrite them. A coalesced
background writer preserves current intent before mutation transport, and
orderly reload flushes the final values. **Recover local unsent changes** loads a
retained capsule explicitly. Source actions, Save and network operations are
never automatically replayed from it. A live owner prevents another client from
adopting its unsent values.

Reconnect preserves values and recovers the server draft. Retained forms bind to
their exact original target/generation. If that changed, **Review current target
and retained values** displays the current and retained values before an explicit
rebind. Pending updates can likewise be reviewed and applied only to a private
draft before another Save. Closing with unacknowledged values archives them before
clearing the active client state, so the next edit cannot overwrite them.

If local recovery storage fails, mutation dispatch blocks and reports the failure.
Repair storage and choose **Retry local recovery storage** explicitly. Existing
source, server drafts and any completed commits are not rolled back. Abrupt
termination can recover the last successfully persisted client state; it cannot
guarantee keystrokes that had not reached durable storage during a storage failure.
No prompt-source watcher or runtime prompt-version state is involved.

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
sources and visible parse failures. Explicit Copy captures them into a reviewed managed package.

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

Complete draft/review/export replies use correlated bounded wire chunks when
needed. The existing general frame-size guard remains intact, while complete
management content has no instruction-specific size cap. Missing, reordered or
cross-session chunks fail visibly, preserve pending intent and never report a
partial document as complete. Recovery distinguishes an incomplete reply from an
operation that may already have committed.

## Implementation and verification

- `jcode-instruction-types` owns the pure inspection, editing, repository-action and recovery vocabulary.
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

The manager uses one navigation/action model for drafts, metadata forms, diff
review, Save, restore, Copy, setup and explicit sync. Mutation safety and
recovery remain with the repository or dedicated ecosystem owner. Each added action must state its target,
consequence and availability; do not introduce a competing hotkey-only workflow.
WP-11 evaluates complete combined journeys and repairs inconsistencies during
its existing integration acceptance, without starting another UI redesign.
