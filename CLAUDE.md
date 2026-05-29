# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Cairn

Cairn is a session-manager daemon + CLI for AI coding agents (starting with Claude Code). It spawns and manages PTY sessions over a wRPC protocol, with the goal of providing mobile-friendly remote access, visibility into agent activity, and git/file integration.

## Build & Test

The nix dev shell (via `direnv`) provides the Rust toolchain, zig (needed by `libghostty-vt-sys`), and cargo-nextest. The `GHOSTTY_SOURCE_DIR` and zig cache env vars are set automatically by the shell hook.

```sh
cargo build                              # build all crates
cargo nextest run                        # run all tests (nextest, not cargo test)
cargo nextest run -p cairn-pty           # test a single crate
cargo nextest run -E 'test(~attach)'     # run tests matching a name filter
cargo clippy --all-targets -- -D warnings
cargo fmt --check                        # or `cargo fmt` to fix
```

The `cairn` binary (the CLI) is produced by the `cairn-client` crate — `cargo run -p cairn` to run it. The daemon binary is `cairn-daemon` — `cargo run -p cairn-daemon`.

## Architecture

Four crates in `crates/`, layered bottom-up:

**cairn-pty** — PTY session abstraction. The `PtySession` trait (`session.rs`) defines the async interface (subscribe, write, resize, signal, inject, wait). `GhosttyPty` is the sole implementation, backed by `libghostty-vt` for terminal state tracking. Each session runs on a dedicated OS thread with a current-thread tokio runtime; external callers communicate via a `flume` command channel (`ghostty/mod.rs` → `worker.rs`). Multi-client coordination uses a leader-election model: only the leader can resize or have keystrokes promoted, while `inject()` (backing `cairn send`) bypasses leadership.

**cairn-protocol** — WIT schema (`wit/cairn.wit`) and generated Rust bindings via `wit-bindgen-wrpc`. Two `generate!` invocations produce server-side `Handler` traits and client-side free functions (`client::cairn::daemon::{sessions,meta}::*`). Shared error codes live in `error_codes`. The transport is wRPC over Unix domain sockets (with future websocket support planned).

**cairn-daemon** — The daemon process. `Daemon` (`daemon.rs`) implements the generated `Handler` traits by delegating to `handlers/*`. `SessionRegistry` (`registry.rs`) holds `Arc<SessionEntry>` keyed by UUIDv7 id, resolving by name-then-id. `serve.rs` binds the UDS, wires the wRPC accept loop, and handles graceful shutdown (SIGTERM + drain). Locking discipline: never hold an entry lock across `.await`, never hold two entry locks at once.

**cairn-client** — The `cairn` CLI binary. Subcommands in individual modules mirror the WIT interface: `exec`/`run`, `attach`, `logs`, `send`, `kill`, `wait`, `list`, `inspect`, `rename`, `restart`, `kick`. `connect.rs` resolves the daemon endpoint (UDS default, future WS). `targets.rs` resolves session selectors (name, uuid, `--latest`, globs). `terminal.rs` handles raw-mode TTY setup and detach-key recognition.

### Data flow (attach)

Client enters raw mode → `attach` RPC opens a bidirectional wRPC stream → daemon registers the client in the session's `attached` map → `PtySession::subscribe()` returns a snapshot + live output stream → server-events (snapshot, output, exited) flow back, client-events (input, resize, detach) flow forward → `AttachGuard` RAII cleans up on disconnect.

### Wire protocol

Defined in `wit/cairn.wit` as WIT interfaces (`sessions`, `meta`, `types`). Uses wRPC streaming (`stream<T>` and `future<T>`) for attach, logs, send, and wait. Signal names are carried symbolically to avoid Linux/BSD numbering divergence.

## Integration tests

Daemon integration tests use `DaemonHarness` (`cairn-daemon/tests/common/mod.rs`) which starts a real daemon on a tempdir socket and provides a wRPC client. Client integration tests (`cairn-client/tests/`) spawn a full daemon+client stack.
