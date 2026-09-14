#!/usr/bin/env python3
"""Run a test command with private application state, including HOME fallbacks.

The parent Cargo/build process keeps its real toolchain/cache locations. Every
invocation receives a distinct state root; failed fixtures are retained for
diagnosis. This is state isolation, not a sandbox for arbitrary test code.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    command = sys.argv[1:]
    if not command:
        raise SystemExit("usage: run_isolated_test.py COMMAND [ARG ...]")
    env = os.environ.copy()
    original_home = Path.home()
    for key, default in (("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")):
        env[key] = str(Path(env.get(key, str(original_home / default))).resolve())
    # Short paths also leave room for fixture Unix-domain socket names on macOS.
    root = Path(tempfile.mkdtemp(prefix="jct-", dir="/tmp" if os.name == "posix" else None))
    for name in ("home", "jcode", "runtime", "config", "cache", "data", "state", "tmp"):
        (root / name).mkdir(mode=0o700)
    env.update({
        "HOME": str(root / "home"),
        "JCODE_HOME": str(root / "jcode"),
        "JCODE_RUNTIME_DIR": str(root / "runtime"),
        "XDG_RUNTIME_DIR": str(root / "runtime"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_STATE_HOME": str(root / "state"),
        "TMPDIR": str(root / "tmp"),
        "JCODE_TEST_STATE_ROOT": str(root),
    })
    # Explicit endpoints and state paths take precedence over the private home.
    # A nested fixture must opt into its own endpoints inside this invocation.
    for key in ("JCODE_SOCKET", "JCODE_DEBUG_SOCKET", "JCODE_REPO_DIR",
                "JCODE_SESSION_ID", "JCODE_RUNTIME_PROVIDER", "JCODE_ACTIVE_PROVIDER",
                "JCODE_RUST_ACTION_LOG_PATH", "JCODE_CLIENT_SELFDEV"):
        env.pop(key, None)
    print(f"test state: {root}", file=sys.stderr, flush=True)
    result = subprocess.run(command, env=env)
    (root / "result.json").write_text(json.dumps({
        "command": command, "returncode": result.returncode,
    }, indent=2) + "\n")
    # Retain even successful state: tests can leave independently owned workers.
    # Automatic recursive deletion here could race them and hide evidence.
    return result.returncode if result.returncode >= 0 else 128 - result.returncode


if __name__ == "__main__":
    sys.exit(main())
