# Testing strategy

PTY-managers sit at an awkward testing intersection: kernel-allocated devices,
SIGCHLD races, two file descriptors per session that can desync, and per-OS
quirks. zmx and cairn share the same overall shape (binary embedding a
headless libghostty emulator) but differ in transport and runtime. This doc
captures cairn's layered strategy with references to zmx as the prior art.

## zmx baseline

zmx splits testing into three tiers:

1. **A test-aggregator stub** at `/Users/abe/Projects/zmx/src/test.zig:1-6`. The
   file is six lines — `comptime { _ = @import("main.zig"); ... }` — and exists
   only so `zig build test` walks every source module and runs its embedded
   `test "..." {}` blocks. There's no test harness or fixture layer here.

2. **Inline unit tests** colocated with the code they cover. These dominate
   zmx's test surface. Categories:
   - **Pure-function tests** — `isCtrlBackslash`, `isUserInput`, `shellQuote`,
     `rewritePromptRedraw` at `/Users/abe/Projects/zmx/src/util.zig:261-319,
     772-823, 880-969, 1277-1419`. No fixtures, no I/O.
   - **Wire-format freezes** — `/Users/abe/Projects/zmx/src/ipc.zig:255-280`
     asserts `@sizeOf(Info) == 552`, every `Tag` enum's discriminant value, and
     that `std.mem.zeroes(Info)` produces no stack garbage in tail padding.
     These exist because zmx ships the struct over a Unix socket as raw bytes
     (see [[internal-communication]]); any layout change is a wire break.
   - **Path-length boundary tests** — `/Users/abe/Projects/zmx/src/socket.zig:
     122-190` pins `sockaddr_un.sun_path` ABI against the platform header and
     drives `getSocketPath` through its at-limit / over-limit branches. This
     class of test is exactly the kind of thing that fails only on one platform
     in one container, so it's locked down early.
   - **Terminal-state serialization roundtrips** — `/Users/abe/Projects/zmx/
     src/util.zig:971-1229`. The richest tests in the codebase. Pattern:
     build a `ghostty_vt.Terminal`, feed it a controlled VT byte stream
     (`\x1b[2J\x1b[10;20H...`), run `serializeTerminalState`, feed the result
     into a fresh `Terminal`, assert that `plainString` matches and that the
     cursor lands at the expected coordinates. The helper trio
     (`testCreateTerminal`, `expectScreensMatch`, `expectCursorAt`,
     `expectMarkerAtRow`, `serializeRoundtrip`) at `util.zig:999-1057` is the
     fixture layer. The `ALT_MARK` / `MAIN_MARK` test at
     `util.zig:1190-1209` specifically guards that alternate-screen content
     does not leak into the serialized stream — a real bug pattern that would
     corrupt replay on re-attach (see [[terminal-state-and-replay]]).

3. **End-to-end BATS tests** in `/Users/abe/Projects/zmx/test/`. Three files:
   - `session.bats` (293 lines, ~24 tests) — drives a real `zig-out/bin/zmx`
     binary against an isolated `ZMX_DIR=$BATS_TEST_TMPDIR/zmx-sockets`,
     covering session create/list/kill/wait/print/send and rapid-churn FD
     stress (`session.bats:257-267`).
   - `stdin_run.bats` (81 lines) — covers the bash-only / SHELL-override case.
   - `test_helper.bash` (40 lines) — builds the binary once, exports
     `ZMX_DIR`, and tears down via `zmx kill --force` on every session in the
     dir.

   Three notes. The tests exclusively drive the public CLI; the daemon is
   never imported as a library. The comment at `session.bats:1-10` flags a
   hard-won bug — without an FD-close fix in the daemon, `bats` hangs because
   it waits for its own FDs 3+ to close, which the daemon inherited. Polling
   helpers (`wait_for_session` at `test_helper.bash:29-40`) replace fixed
   sleeps with bounded retry loops — the session list is the sync barrier.

## cairn baseline

cairn's current tests sit in two places:

- **Inline unit tests** at `/Users/abe/Projects/cairn/crates/cairn-pty/src/
  pty/mod.rs:18-152`. These are type-system / API-shape tests:
  `PtyError` conversion, `TermSize` is `Copy + Eq`, `SpawnOptions` builder
  defaults, `PtySession` object-safety, `GhosttyPty: Send + Sync`. No PTY is
  ever spawned in this module.

- **Integration tests** in `/Users/abe/Projects/cairn/crates/cairn-pty/
  tests/` — one file per feature axis:
  - `pty_lifecycle.rs` (117 lines): spawn/wait, kill, drop-kills-child,
    write-after-exit.
  - `pty_io.rs` (222 lines): broadcast-to-subscribers (`printf hello-cairn`),
    write-to-stdin (using `cat` as an echo), late-subscriber snapshot replay,
    DA1 query-response without a client (see
    [[query-response-delegation]]), broadcast Close on child exit.
  - `pty_resize.rs` (30 lines): resize updates `size()` query (see
    [[resize-semantics]]).

  All integration tests use a real `pty_process::Pty` against a real child
  (`true`, `sleep`, `printf`, `cat`, `sh`) — no mocking. Synchronization uses
  a shared helper `read_until_contains` (`pty_io.rs:10-36`) that polls the
  broadcast channel until a needle is seen or a tokio timeout elapses. This is
  cairn's analog of zmx's `wait_for_session`: the broadcast channel itself is
  the barrier, replacing fixed sleeps.

- **Manual smoke** at `/Users/abe/Projects/cairn/crates/cairn-pty/examples/
  echo.rs`. Twenty-five lines that spawn `bash -i`, write `echo
  hello-from-cairn`, and print bytes received. Used as a hand-run sanity
  check; not part of CI.

## Divergence: where cairn must do more than zmx

zmx's BATS suite is sufficient as an end-to-end gate because zmx exposes a
single CLI binary over Unix sockets. cairn's daemon exposes two transports
(UDS for local CLI, WebTransport for remote browser and CLI — see
[[external-protocol]], [[transports]], [[web-vs-cli-clients]]). That
cleaves the test surface in two:

- **Wire-protocol conformance** rides on the wRPC + WIT schema in
  `crates/cairn-protocol/`. The codec is wRPC's canonical-ABI; we don't
  re-test it. What we *do* test is the schema's encoding of representative
  messages over the wire — the round-trip tests at
  `crates/cairn-protocol/tests/round_trip.rs` are the existing template.
- **Daemon-as-process integration tests** are harder because there's no
  equivalent of `nc -U` for wRPC over WebTransport. The UDS path is easier
  — spawn the daemon binary, connect via `wrpc_transport::unix::Client`
  from a `#[tokio::test]`. WebTransport requires a QUIC client (likely
  `wtransport`'s Rust client) inside the test to cover that path.

## Recommended layering for cairn

| Layer | What | Where | Notes |
| --- | --- | --- | --- |
| libghostty-vt | Upstream terminal correctness | not ours | We pin behavior with golden snapshots, mirror zmx's `serializeTerminalState` roundtrip suite. |
| Worker (single-session) | Spawn + write + broadcast + resize + exit | `crates/cairn-pty/tests/pty_*.rs` | Real PTY, real child, broadcast as sync barrier. Already exists. |
| Daemon (multi-client) | Session registry, election, detach cascade | TBD `tests/daemon_*.rs` | Spawn daemon as subprocess; drive via in-process wRPC client (UDS) and a wtransport-based client (WT). See [[client-attach-and-election]]. |
| Wire protocol | WIT schema round-trip; per-operation encoding | `crates/cairn-protocol/tests/round_trip.rs` | Stub `Handler` impl + in-process server on a tempdir UDS; assert server-produced values arrive intact at the client. Three tests exist (`meta.version`, `sessions.list-all`, `meta.authenticate`); extend as new operations get implementations. |
| Browser | ghostty-web attach + render | out of scope here | Separate repo; integration boundary documented in [[web-vs-cli-clients]]. |

### Snapshot regression for replay

Port zmx's pattern at `util.zig:1097-1209` directly. Concretely: store a
canonical "complex inferior session" as a checked-in byte stream
(scrollback + ALT_MARK leak guard + CUP-positioned markers + resize
mid-session), feed it through `libghostty-vt::Terminal`, serialize via the
`Formatter`, and compare against a golden file. This is the only practical
defense against an upstream emulator regression silently changing replay
output (see [[terminal-state-and-replay]]).

### PTY-related gotchas to assert against

Document and test, don't just hope:

- **SIGCHLD races** — `pty_lifecycle.rs:42-77` (`drop_kills_running_child`) is
  the right shape: subscribe before drop, then time-bound the close
  observation. Without this you can't distinguish "kill worked" from "test
  process exited before assertion ran."
- **EIO on second read after EOF** — Linux returns EIO on the master after the
  last slave closes; macOS may return EOF differently. cairn's
  `subscribers_observe_close_on_child_exit` (`pty_io.rs:197-222`) covers the
  externally-visible contract; the worker-internal read loop in
  `pty/ghostty/worker.rs` should grow a dedicated unit test for EIO-vs-EOF
  handling.
- **Slave-fd leaks** — zmx documents this at `session.bats:1-10`; the symptom
  is hung tests, not failed assertions. A `lsof`-style assertion or a
  rapid-churn loop (zmx does 5 iterations at `session.bats:257-267`) is
  cheaper than diagnosing it later.
- **Container/CI quirks** — no controlling tty, restricted `/dev/pts` mounts.
  Tests should not assume a tty on stdin; spawning explicitly via
  `pty_process::Pty::new()` sidesteps this for the worker layer but the
  daemon process model (see [[daemon-process-model]]) needs an explicit
  decision for CI.

### Async test infrastructure

cairn already uses `#[tokio::test]` with the `test-util` feature
(`Cargo.toml:21`). For deterministic timing in backpressure / scrollback
eviction tests (see [[backpressure]]), `tokio::time::pause()` plus
`advance(Duration)` lets us avoid wall-clock sleeps without sacrificing
realism. The existing `read_until_contains` helper is the right primitive for
data-driven barriers; do not add fixed sleeps.

### wRPC transport testing

Two patterns to pick between:

- **In-process** — spawn a wRPC server on a tempdir UDS path inside the
  test, connect via `wrpc_transport::unix::Client::from(path)`. Fast,
  deterministic, no subprocess. The `cairn-protocol` round-trip tests
  use this pattern; reuse the `spawn_server` helper. Best for
  protocol-conformance tests of [[external-protocol]] and [[authentication]].
  For WebTransport coverage, a `wtransport`-based client peer connects to a
  daemon-hosted WT endpoint on `127.0.0.1:0` — same shape, different
  carrier.
- **Subprocess** — spawn the daemon binary, connect from the test. Slower
  but catches packaging issues (env vars, default paths, signal handling).
  Best for the equivalent of zmx's BATS suite. See [[observability]] for
  the log-capture story.

### Performance / fuzz

Out of scope for the v1 test plan but worth scaffolding:

- Backpressure stress — feed the worker a producer faster than subscribers
  drain; assert eviction policy, see [[backpressure]].
- Many-sessions — spawn N sessions, list, kill all; equivalent to zmx's churn
  test at `session.bats:257-267` but parameterized over N.
- Fuzz the wRPC canonical-ABI decode path with `cargo-fuzz` once the
  schema stabilises — though most of the surface is already covered by
  wRPC's own upstream tests; focus on cairn-specific WIT types
  (variants with binary payloads, nested options).

### Organization

Follow Rust convention and zmx's split: one integration-test file per feature
axis (already in place — `pty_io.rs`, `pty_lifecycle.rs`, `pty_resize.rs`).
Avoid a single `tests.rs` mega-file. Inline unit tests stay next to the code
they cover, like `pty/mod.rs:18-152`. Name tests as
`verb_noun_observation` (e.g. `kill_terminates_long_running_child`), matching
the existing style.

## Open questions

- Does cairn want a BATS-equivalent shell harness for the CLI client, or are
  in-process Rust tests sufficient given there's no socket-path edge case
  (the UDS transport's path is fixed by `$XDG_RUNTIME_DIR/cairn/` plus a
  fixed basename, well under the `sockaddr_un` 108-byte limit zmx tests at
  `socket.zig:122-190`)?
- How do we handle CI without a controlling tty for the daemon process model
  ([[daemon-process-model]])? Forked `setsid` + redirect, or rely on
  `pty_process::Pty` to allocate one?
- Should the golden-snapshot replay corpus live in `crates/cairn-pty/tests/
  fixtures/`, or in a dedicated `cairn-replay-snapshots` crate that can be
  shared with ghostty-web for cross-implementation validation?
- Browser-side rendering verification (ghostty-web attach + paint) is out of
  scope here, but the integration boundary needs a contract test on the
  wRPC server-event stream. Where does that live — in this repo or downstream?
- Do we need a fault-injection layer ([[error-recovery]]) for "PTY master
  read errors" beyond what `pty_process` surfaces naturally, or is killing
  the child sufficient coverage?
- [[configuration]] interacts with tests via env vars (`ZMX_DIR` analog).
  What's the cairn equivalent, and should tests have a discovery mechanism
  to avoid hardcoding it?
