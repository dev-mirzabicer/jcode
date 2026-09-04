# Skills, managed sources, and active snapshots

Jcode discovers skills from managed instruction repositories and supported external compatibility locations. A skill invocation renders current source once, persists the complete rendered text in the session, and uses that exact text until another invocation or a new-context lifecycle clears it.

## Source classes and precedence

The effective skill with a given invocation name is selected in this order:

1. Managed project skill in the configured project instruction repository
2. External project skill, highest to lowest: `.claude/skills`, `.agents/skills`, `.jcode/skills`
3. Managed global skill under `~/.jcode/instructions/skills`
4. External global skill, highest to lowest: `~/.agents/skills`, `~/.jcode/skills`, installed Claude plugin skills

The implementation loads lower-precedence sources first, then replaces them with higher-precedence sources. Project scope remains more specific than global scope. Within one scope, managed source wins over external source.

A present invalid or ambiguous managed skill blocks fallback for its invocation name. An unrelated invalid skill does not block a valid skill. Repository or project-configuration damage that prevents Jcode from knowing the managed catalog fails activation rather than pretending an external fallback is authoritative.

Removing a managed skill reveals the next valid source under the same deterministic order.

## Managed layout

Global managed skills live at:

```text
~/.jcode/instructions/skills/<stable-id>/SKILL.md
```

Project managed skills use the same path inside the configured project instruction repository.

A managed skill is an instruction resource with `kind: skill`. Its stable resource ID may differ from its user-facing invocation name. `name`, `description`, and optional `allowed-tools` metadata retain the existing skill contract.

Plain rendering is the default. Literal `{{ ... }}` remains literal in a plain skill. A managed skill may opt into restricted Handlebars rendering and registered managed module partials. Missing values, missing partials, invalid templates, and cycles fail the invocation without partial output. Jcode adds no skill-source, package, reference, expansion, or rendered-output size limit.

External skills retain their existing plain-text behavior.

## External read-only sources

Supported external sources remain discoverable and invocable:

- Installed Claude plugin skill packages
- `~/.jcode/skills/<name>/SKILL.md`
- `~/.agents/skills/<name>/SKILL.md`
- `<project>/.jcode/skills/<name>/SKILL.md`
- `<project>/.agents/skills/<name>/SKILL.md`
- `<project>/.claude/skills/<name>/SKILL.md`

External packages are read-only through the future central instruction manager. Jcode does not edit or commit them in place.

`/skills` and `skill_manage list` identify local source class. External entries are labeled read-only and point to Copy rather than Edit. Ordinary remote History currently carries skill names for compact display. The complete central manager protocol and UI arrive in Phase 3 WP-09 and WP-10.

## Copy skill backend

The backend Copy operation is available to the later central instruction manager. The mutation UI itself arrives in WP-10.

Copy:

- Accepts an effective external skill and a global or configured project destination
- Copies every nested regular file, including binary reference files
- Preserves the original external `SKILL.md` under `.jcode-source/original-SKILL.md`
- Writes a canonical managed `SKILL.md` whose model-facing output preserves the current name, description, allowed tools, and body
- Records non-model-facing source class, source skill name, and package digest in `.jcode-source.toml`
- Rejects symlinks and unsupported file types rather than following paths outside the package
- Validates the complete destination instruction store before publication
- Creates one isolated instruction-repository commit without staging unrelated state
- Refuses to overwrite a different existing managed package
- Returns no change for an identical repeated Copy

A project-scoped external source remains more specific than a global managed copy. The Copy outcome reports whether the selected destination becomes effective for the source project, allowing the manager to explain that a project destination is required when appropriate.

## Invocation

Supported invocation forms remain:

```text
/skill-name
/skill-name trailing prompt
/My Multi Word Skill trailing prompt
```

A bare invocation activates the skill without a model call. A trailing prompt activates the skill and submits the trailing text as the user turn. Images and paste payloads remain attached to that same turn.

For a local session or direct REPL, Jcode:

1. Builds the latest effective catalog for the session working directory.
2. Resolves and completely renders the selected source.
3. Builds the existing complete active body:

   ```text
   # Skill: <name>

   <description>

   <rendered body>
   ```

4. Persists the invocation name and complete rendered text in `Session.active_skill` before accepting a trailing turn.
5. Uses the stored text in the dynamic prompt slot.

For a remote TUI, a bare invocation uses a dedicated `ActivateSkill` control request. A trailing prompt carries `activate_skill` on the message request so the server renders and persists the skill before accepting the turn. Failure rejects the turn before provider dispatch. `SkillActivated` is the authoritative UI confirmation, and reconnect snapshots restore the active skill identity.

The outer dynamic prompt framing remains:

```text
# Active Skill

<complete stored skill text>
```

## Active rendered-text lifecycle

Disk changes, managed Git changes, and registry reloads do not mutate an active invocation.

- Later turns and provider tool continuations use exact stored text.
- Reinvoking the same or another skill renders current source again and replaces active state.
- Resume, reconnect, takeover, process reload, and split preserve exact rendered text.
- Rewind and undo do not time-travel or clear active skill configuration.
- Ordinary agent-profile changes and explicit true-system replacement retain active skill state.
- Clear and transfer create new contexts and clear active skill state.
- A failed activation or persistence attempt leaves the prior active skill unchanged.

No source hash, Git commit, freshness comparison, version warning, watcher, or automatic reactivation is stored.

## Available-skills system section

The prose for the available-skills section is managed at:

```text
~/.jcode/instructions/system/available-skills.md
```

The composer supplies the sorted effective skill names and complete descriptions as typed values. The complete rendered section is frozen in the system prompt at activation.

Discovery changes and `skill_manage reload_all` affect later listings and later activations. They do not rewrite an existing session's true system prompt. An explicit true-system replacement, clear, transfer, or fresh session renders the current catalog.

## Commands and tool behavior

Existing surfaces remain available:

- `/skills` refreshes discovery for the listing and shows the active name.
- Slash invocation supports trailing prompts and multi-word names.
- `skill_manage list` shows effective skills and endorsed entries.
- `skill_manage load` returns the latest completely rendered skill as a tool result.
- `skill_manage read` shows current effective source content and source class.
- `skill_manage reload` refreshes one loaded external global skill.
- `skill_manage reload_all` refreshes shared external global discovery. Project and managed sources are read at effective-catalog construction, so no reload is required for their next invocation.

Reload changes future discovery only. It does not mutate stored active text or the frozen available-skills system section.

## Inspection, export, and recovery

- `/agent inspect` can return the complete current active rendered skill text.
- Raw session export retains `Session.active_skill` completely.
- Markdown export includes the complete Active Skill section.
- Replay retains exact active text without source access.
- Metadata-only startup stubs retain only the skill identity and allocate no active body bytes.
- Full session load and remote startup recovery retain complete text.

Prompt and skill prose is accepted through human review and field use. Mechanism tests use synthetic content to verify source precedence, rendering, Copy, persistence, lifecycle, protocol, and failure behavior without snapshotting or grading consequential prose.
