# Repository Guidelines

## Development Workflow

- **Stay on your own branch** - Do not take, cherry-pick, merge, or copy code from other
  people's or other agents' branches unless the source branch belongs to a repository
  maintainer and the user explicitly asks you to integrate it. Only work from your branch
  and its base (e.g. `main`) otherwise. Never integrate branches owned by non-maintainers
  or other agents yourself; tell the user and let them decide how to proceed.

## Install Notes
- `~/.local/bin/jcode` is the launcher symlink used from `PATH`.
- `~/.jcode/builds/current/jcode` is the active local/source-build channel; self-dev builds and `scripts/install_release.sh` point the launcher here.
- `~/.jcode/builds/stable/jcode` is the stable release channel; `scripts/install.sh` installs this and points the launcher here.
- `~/.jcode/builds/versions/<version>/jcode` stores immutable binaries.
- `~/.jcode/builds/canary/jcode` still exists for canary/testing flows, but it is not the primary self-dev install path.
- On Windows, the equivalents are `%LOCALAPPDATA%\\jcode\\bin\\jcode.exe` for the launcher, `%LOCALAPPDATA%\\jcode\\builds\\stable\\jcode.exe` for stable, and `%LOCALAPPDATA%\\jcode\\builds\\versions\\<version>\\jcode.exe` for immutable installs; `scripts/install.ps1` currently installs the stable channel.
- Ensure `~/.local/bin` is **before** `~/.cargo/bin` in `PATH`.

## Verifying a change at runtime

`cargo build` alone does not validate a running shared session. Interactive shared
sessions use the long-lived daemon at `~/.jcode/builds/shared-server/jcode`, which
points into `~/.jcode/builds/versions/<version>/`. Until coordinated reload restarts
that daemon, shared-session checks still exercise its old binary.

Standalone `jcode run` is different: it constructs a local Agent and executes
ordinary tools in that command's process/runtime. Its `--json` and `--ndjson`
outputs follow that same local route. The global `--socket` argument does not move
this Run command onto the daemon. Use the stable Harness API or an attached TUI
for daemon-backed workflows, and verify the actual execution owner's identity.

For isolated daemon testing, use a private `JCODE_HOME` and runtime directory,
start the candidate with an explicit socket, then attach a TUI or Harness client
to that socket. Standalone smoke tests instead invoke the candidate directly:

```bash
# After a coordinated selfdev build, with isolated fixture state configured:
./target/selfdev/jcode --no-update --socket /path/to/private/test.sock serve
# Separate standalone route, not served by the daemon above:
./target/selfdev/jcode --no-update run '<prompt>'
```

Two things that waste time otherwise:

- `crate::logging::info` writes to a log file, not stderr, so instrumenting a
  code path with it produces no visible output under `--trace`. Use `eprintln!`
  for throwaway diagnostics and delete it before committing.
- Confirm which binary you are actually inspecting. `strings` on
  `builds/shared-server/jcode` reads a 70-byte symlink, not a program; resolve it
  with `readlink -f` first.
