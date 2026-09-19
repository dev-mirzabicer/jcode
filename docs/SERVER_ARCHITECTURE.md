# Server Architecture

See also:

- [`SERVER_SERVICE_SPLIT_PLAN.md`](./SERVER_SERVICE_SPLIT_PLAN.md)
- [`SWARM_ARCHITECTURE.md`](./SWARM_ARCHITECTURE.md)
- [`MULTI_SESSION_CLIENT_ARCHITECTURE.md`](./MULTI_SESSION_CLIENT_ARCHITECTURE.md)

## Overview

jcode uses a **single-server, multi-client** architecture. One server process
manages all sessions and state; TUI clients connect over a Unix socket and
can reconnect transparently after disconnects or server reloads.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              SERVER (🔥 blazing)                              │
│                                                                             │
│  jcode serve                                                                │
│  ├── Unix socket:  /run/user/$UID/jcode.sock                                │
│  ├── Debug socket: /run/user/$UID/jcode-debug.sock                          │
│  ├── Registry:     ~/.jcode/servers.json                                    │
│  ├── Provider (Claude/OpenAI/OpenRouter)                                    │
│  ├── MCP pool (shared across sessions)                                      │
│  └── Sessions:                                                              │
│        ├── 🦊 fox   (active)  → "🔥 blazing 🦊 fox"                         │
│        ├── 🐻 bear  (active)  → "🔥 blazing 🐻 bear"                        │
│        └── 🦉 owl   (idle)    → "🔥 blazing 🦉 owl"                         │
└─────────────────────────────────────────────────────────────────────────────┘
         │              │              │
         ▼              ▼              ▼
    ┌─────────┐   ┌─────────┐   ┌─────────┐
    │ Client 1│   │ Client 2│   │ Client 3│
    │ 🦊 fox  │   │ 🐻 bear │   │ 🦉 owl  │
    └─────────┘   └─────────┘   └─────────┘
```

## Naming

```
SERVER = Adjective/Verb modifier          SESSIONS = Animal nouns
────────────────────────────              ────────────────────────
🔥 blazing   ❄️ frozen   ⚡ swift          🦊 fox    🐻 bear   🦉 owl
🌀 rising    🍂 falling  🌊 rushing        🌙 moon   ⭐ star   🔥 fire
✨ bright    🌑 dark     💫 spinning       🐺 wolf   🦁 lion   🐋 whale

Combined: "🔥 blazing 🦊 fox" = server + session
```

The server gets a random adjective/verb name on startup (e.g., "blazing").
Each session gets an animal noun (e.g., "fox"). Together they form a natural
phrase displayed in the UI: "🔥 blazing 🦊 fox".

The server name persists across reloads via the registry (`~/.jcode/servers.json`).
When the server execs into a new binary on `/reload`, the new process registers
with a fresh name. Stale entries are cleaned up automatically.

## Lifecycle

```
  START                          CONNECT                     RELOAD
  ─────                          ───────                     ──────
  jcode (first run)              jcode (subsequent)          /reload
       │                              │                          │
       ├─▶ No server? Spawn daemon    ├─▶ Server exists?         ├─▶ Server execs into
       ├─▶ Wait for socket            │   Connect directly       │   new binary (same PID)
       ├─▶ Connect as client          │                          ├─▶ All clients disconnect
       └─▶ Create session             └─▶ Create/resume session  └─▶ Clients auto-reconnect
```

### Server Startup

When you run `jcode`, it checks if a server is already running:

1. **Server exists**: connect directly as a client
2. **No server**: spawn `jcode serve` as a detached daemon (with `setsid`),
   wait for the socket, then connect

The server is fully detached from the spawning client via `setsid()`, so killing
any client never affects the server or other clients.

Long-lived deployments can give the daemon a stable client-visible identity with
`jcode serve --server-name <name>` or the `JCODE_SERVER_NAME` environment
variable. The optional `JCODE_SERVER_DISPLAY_NAME` environment variable is also
accepted for service managers that prefer a display-oriented name. CLI input wins
over environment input. Names are normalized to registry-safe lowercase labels,
so `mount-cloud/fabian` displays as `mount-cloud-fabian`.

### Server Shutdown

Ordinary shared servers do not exit because every client disconnected. Primary
turns, their completion and Stop controls belong to the runtime, not a terminal.
Connection teardown releases subscriptions and editor leases without closing
the primary or discarding its output. Explicit runtime exit and reload remain
separate operations. Explicitly temporary servers retain their owner/idle policy.

Run/REPL inference remains process-owned. This change does not install a login
service or give standalone callers detached execution guarantees. Managed
workspace launch and its human controls remain separately gated.

### Client delivery

Primary output fans out through per-connection queues rather than awaiting a
client socket. A queue admits at most 256 events with an 8 MiB ordinary serialized
byte budget. One larger complete response can occupy an otherwise empty queue.
Responses are never truncated to fit that budget. Overflow disconnects that
client, not its primary. Authoritative conversation and execution output stay
with their existing storage owners.

Socket writes have a 30-second **no-progress** deadline, renewed after each
successful write. There is no total-duration or response-size deadline. Session
bindings also carry an internal generation: queued or late events for a departed
target cannot follow the connection to another session. Attachment snapshot
publication precedes that target's queued output. In-process observers use their
existing local channels and do not own primary lifetime.

Sequenced presentation is explicitly negotiated with
`primary_stream_subscribe` before ordinary `Subscribe`. The response is
`primary_stream_capabilities` version 1. Negotiation creates no Agent or Session.
The TUI negotiates this mode, while an older server's unsupported-request response
keeps the legacy presentation path without claiming cursor support. Clients that
do not negotiate are not sent snapshot replay semantics.

For negotiated clients, History comes from an ordered primary checkpoint, followed
by exactly the announced number of in-flight replay frames. Each frame's optional
`primary_stream` metadata identifies its snapshot/replay/live phase and cursor.
The cursor identifies the session, runtime stream, published sequence, originating
connection and request. It is a presentation cutoff, not a replacement Session
revision or a permission token. Live sequence gaps and incomplete replay require
a new snapshot, never another provider call or tool execution.

`primary::presentation` holds disposable copies of canonical Session checkpoints
and losslessly coalesced subsequent display events. Agent checkpoint markers share
the same ordered channel as deltas and are consumed before network delivery.
Canonical history retains its existing owner and Startup Context privacy policy.
Terminal delivery is drained before admitting the next turn. Unseen terminal and
prompt-recovery receipts remain available, and a stopped in-flight preview is not
inserted into authoritative history merely to display it. This transient preview
does not promise recovery of uncommitted provider bytes after process loss.

The TUI replaces its transient display before snapshot replay. It distinguishes
attachment acknowledgements from primary completion, including equal request
numbers belonging to different connections. Writing a frame is not proof that a
human read it. Reconnection obtains fresh state through the same snapshot path.

### Remote Client Working Directory

For a fresh session, the client supplies its command working directory to the
server. Attaching, resuming, reconnecting or navigating to an existing session
retains that session's stored working directory. A Subscribe report never moves
an existing session, even while it is busy. Project-local MCP discovery and
presence metadata use the same server-owned directory, not the attaching
client's path. Socket forwarding wrappers can select the initial server path
separately with `--remote-working-dir`:

```bash
jcode --socket /tmp/jcode.sock -C /local/checkout --remote-working-dir /remote/checkout
```

`-C` must exist on the client. `--remote-working-dir` must be an absolute path
that exists on the server.

### Client Reconnection

Clients have a built-in reconnect loop. When the connection drops (server
reload, network issue, etc.):

1. Client shows "Connection lost - reconnecting..."
2. Retries with exponential backoff (1s, 2s, 4s... up to 30s)
3. On reconnect, resumes the same session (session state persists on disk)
4. If server was reloaded, client may also re-exec itself if a newer
   client binary is available

### Hot Reload (`/reload`)

1. Client sends `Request::Reload` to server
2. Server sends `Reloading` event to the requesting client
3. Server calls `exec()` into the new binary with `serve` args
4. New server process starts on the same socket
5. All clients auto-reconnect
6. The initiating client also re-execs if its binary is outdated

## Socket Paths

```
/run/user/$UID/
├── jcode.sock          # Main communication socket
└── jcode-debug.sock    # Debug/testing socket
```

## Self-Dev Mode

When running `jcode` inside the jcode repository:

1. Auto-detects the repo and enables self-dev mode
2. Connects to the normal shared jcode server
3. Marks that session as canary/self-dev via subscribe metadata
4. Enables selfdev prompt/tooling only for that session
5. `/reload` still hot-reloads the shared server and clients reconnect

## Key Behaviors

| Scenario | Behavior |
|----------|----------|
| First `jcode` run | Spawns server daemon, connects |
| Subsequent `jcode` | Connects to existing server |
| Kill a client | Server + other clients unaffected |
| `/reload` | Server execs new binary, clients reconnect |
| All clients close | Server idle-timeout after 5 min |
| Resume session | `jcode --resume fox` reconnects to existing session |

Hosted tool-stdin requests are also primary-owned. A new attachment can inspect
pending prompts and answer through the existing `stdin_response` protocol.
Foreign-primary, closed and duplicate responses fail rather than reporting false
success. Input bodies are not stored in the pending-prompt view. The current TUI's
limited interactive-terminal notice is unchanged; this does not introduce a new
stdin UI or the later questionnaire framework.
