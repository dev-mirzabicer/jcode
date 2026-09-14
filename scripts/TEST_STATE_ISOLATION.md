# Test state isolation

A Cargo test binary contains the current checkout's schema migrations and shipped
instruction seeds. It must not inherit the application's live state while a
shared daemon is running an older build. Compiling code does not activate that
code in the daemon, but executing tests can still migrate stores opened by their
fixtures.

Use `bash scripts/dev_cargo.sh test ...` or the `selfdev test` command. The local
wrapper and `scripts/remote_build.sh` run `test` and `bench` through
`scripts/run_isolated_test.py`. Each invocation gets a private `HOME`,
`JCODE_HOME`, runtime directory, XDG directories, and temporary directory.
Explicit inherited Jcode socket/session overrides are removed. Absolute
`CARGO_HOME` and `RUSTUP_HOME` preserve the existing toolchain and dependency cache.
Build/check commands retain their normal environment.

For an already-built test executable or a custom test launcher:

```sh
python3 scripts/run_isolated_test.py /absolute/path/to/test-binary test_filter
```

A bare Cargo invocation outside the selfdev shim does not use this wrapper.
Route it through `dev_cargo.sh`. Environment isolation is not an OS sandbox:
tests must not hard-code real user-state paths or explicitly reattach to live
sockets. Test-specific environment overrides must also point at private fixtures.
A fixture that removes `JCODE_HOME` still falls back into the private `HOME`.

The wrapper prints its state directory and retains it, including `result.json`
with the command and exit status. Retention avoids deleting files still used by
background fixture processes and preserves failure evidence. Review and clean up
only a known test directory after confirming its workers have exited.

Run wrapper regression checks with:

```sh
python3 -B scripts/test_test_isolation.py
```

For candidate daemon verification, also use a private application home and runtime
directory and an explicit socket. Verify the actual daemon binary identity.
`jcode run`, even with `--socket`, executes locally and cannot prove which binary
owns a shared session.
