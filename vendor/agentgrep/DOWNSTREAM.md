# Agentgrep downstream integration

This directory contains the source library from
[1jehuang/agentgrep](https://github.com/1jehuang/agentgrep), tag `v0.1.6`, commit
`b01b804008ab0662fa14e6b60b10bff61716e6f1`. Upstream declares the MIT license in
its package manifest. `UPSTREAM.json` records the SHA-256 of every original
source file and Cargo manifest. Original documentation and demo images are not
required to build this library and have not been copied into this source package.

## Why the source is tracked here

Jcode previously linked this exact Git revision. The library clipped matching
lines and selected region bodies before returning them, making full retention
impossible in the embedding adapter. It also owned blocking ripgrep subprocesses
without an embedding cancellation boundary. Re-reading source files after the
query would not recover the same version reliably, and duplicating the search
engine in Jcode would lose existing ranking, structure and context behavior.

The downstream patch therefore:

- Preserves complete selected match lines and region bodies through acquisition
  and rendering. Explicit match/file/region limits, ranking and context-driven
  selection remain in the existing owners.
- Adds `ExecutionControl` and a narrow `ExecutionHost` callback for cancellation
  and helper execution. No Jcode dependency, global control state or thread-local
  policy is introduced into the library.
- Checks cancellation during traversal, bounded source reads and worker loops.
  Parallel workers still join before their caller returns.
- Lets the Jcode adapter execute helpers through the same process-group and raw
  capture owner as native commands. Unavailable helper ownership selects the
  existing native search implementation instead of launching an unmanaged child.
- Replaces obsolete clipping tests with complete-content equality checks, while
  retaining upstream search/ranking/context tests. Narrow lint fixes group the
  related region-source inputs and remove redundant casts/borrows.

The standalone library entry points remain available for compatibility. They
use an empty execution control, while Jcode supplies its actual invocation owner.
Native runtime acceptance is macOS arm64. No unsupported-platform process-kill
or performance-parity claim follows from those tests.

## Maintainer checks

Use coordinated self-development tests. Relevant targets are `agentgrep --lib`,
`jcode-app-core`'s Agentgrep tests, real helper-cancellation tests and the shared
execution integration suite. Run strict Clippy over both library and adapter.
When updating the dependency, compare with the recorded upstream revision and
preserve this narrow delta deliberately. Do not replace the vendor directory
with an arbitrary newer branch or silently restore internal output clipping.
