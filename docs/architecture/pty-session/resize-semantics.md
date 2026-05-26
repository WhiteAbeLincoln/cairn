# Resize Semantics

A PTY exposes a single `(rows, cols)` window size via `TIOCSWINSZ`/`TIOCGWINSZ`.
When N clients are attached, each with its own terminal grid, that single size
becomes a shared resource the policy layer has to arbitrate. This doc covers
the strategies, what zmx does today, what cairn does today, and what cairn
needs to add for multi-client correctness.

Related: [[client-attach-and-election]], [[terminal-state-and-replay]],
[[query-response-delegation]], [[internal-communication]],
[[external-protocol]], [[pty-lifecycle]].

## The Core Problem

The kernel PTY holds exactly one `struct winsize`. The child discovers it via
`TIOCGWINSZ` and is notified of changes via `SIGWINCH`. The headless VT
(libghostty-vt) tracks a parallel grid and reflows wrapped content on resize
(`libghostty-vt-0.1.1/src/terminal.rs:191-208`); the resize call also updates
pixel dims, disables synchronized output, and emits an in-band size report if
mode 2048 is enabled (`terminal.rs:181-190`). Both surfaces must stay in
lock-step or the snapshot replayed to new attachers will diverge from the
child's view.

## Strategies

- **Smallest-attached-wins (tmux).** Use `min(cols)` and `min(rows)` over all
  attached clients. Larger clients see letterboxing. Best for legibility when
  N>1, but the child churns through SIGWINCHes every attach/detach.
- **Leader-wins (zmx).** One elected "leader" client owns the size; followers
  letterbox or scroll. Only one SIGWINCH per leader change, plus whatever the
  leader's window emits.
- **Locked-at-create-time.** Whatever the spawner asked for is permanent.
  Simple, but breaks `SIGWINCH`-aware TUIs whenever the human's window doesn't
  match.
- **Latest-resize-wins (cairn today).** Trait contract:
  `crates/cairn-pty/src/pty/session.rs:21` — "Multi-client coordination is the
  caller's concern; last call wins." Pushes the policy decision up the stack.

Trade-offs: smallest-wins is most predictable for the child but visually
compromised for the largest client; leader-wins keeps the leader pixel-correct
but corrupts oversized followers; locked is only viable for non-interactive
sessions.

## What zmx Actually Does

zmx is **leader-wins with implicit leader election by user input**.

- One `leader_client_fd: ?i32` on `Daemon` (`zmx/src/main.zig:583`) is the
  only client whose resize is honored. `handleResize` early-returns for
  non-leaders (`main.zig:1014`).
- On attach the client sends `Tag.Init` carrying its `ipc.getTerminalSize`
  result (`main.zig:2301-2303`). `handleInit` (`main.zig:933-1000`) installs
  the client as leader if none exists, then applies `TIOCSWINSZ`
  (`main.zig:984`) and `term.resize` (`main.zig:992`).
- Initial size at daemon spawn comes from `getTerminalSize(STDOUT_FILENO)` of
  the **spawning client's tty** (`main.zig:701`), passed into `forkpty`'s
  `winsize` (`main.zig:702-710`). The headless `ghostty_vt.Terminal` is then
  initialized by reading the size back from `pty_fd` itself
  (`main.zig:2468-2473`).
- The leader's `SIGWINCH` signal handler wakes the client loop, which sends
  `Tag.Resize` with the fresh size (`main.zig:2355-2359`).
- `setLeader` proactively pings the new leader for its size by sending an
  empty `.Resize` (`main.zig:643-650`); the client responds with its
  current `getTerminalSize` (`main.zig:2407-2417`).
- `getTerminalSize` falls back to `{ rows = 24, cols = 160 }` when
  `TIOCGWINSZ` fails (`zmx/src/ipc.zig:43-49`).
- Resizes are **not** broadcast to followers. They learn the new size only
  via (a) snapshot replay on their next `.Init` (`main.zig:947-967`), or
  (b) the child's own reflowed output within their oversize/undersize
  viewport.

Leader election is implicit: any client whose input passes `util.isUserInput`
(`main.zig:906`) becomes leader on the next keystroke if none is set.
Leadership is sticky until disconnect (`main.zig:625-631`).

## What Cairn Does Today

Cairn's worker implements the resize **mechanism** but no **policy**:

- `Command::Resize { size, reply }` is dispatched in `worker.rs:372-387`. The
  order is deliberate: `Terminal::resize` first (so the VT grid is updated
  with in-emulator reflow), then `pty.resize(Size::new(rows, cols))` to apply
  `TIOCSWINSZ` and deliver `SIGWINCH`. Both calls pass `0, 0` for cell pixel
  dimensions (`worker.rs:376`) — XTWINOPS pixel reports answer "we don't
  know" (see next section).
- Cached `current_size` is updated only on the happy path
  (`worker.rs:383-385`); on failure, kernel and VT remain authoritative.
  Cairn handles the macOS quirk by opening `pts()` before the initial resize
  (`worker.rs:81-83`, `worker.rs:94-102`).
- Initial size comes from `SpawnOptions::size`, default 80×24
  (`crates/cairn-pty/src/pty/types.rs:9-13`,
  `crates/cairn-pty/src/pty/mod.rs:47`). Spawn is eager — cairn does **not**
  defer to first attach the way zmx does.
- **Resize is not broadcast.** The worker has no concept of "other attached
  clients." Subscribers see the child's reflowed output bytes
  (post-`SIGWINCH`), but no out-of-band size event. Fine for the trait;
  insufficient for the wRPC attach handler that needs to coordinate
  size across multiple clients.

The design spec
(`docs/superpowers/specs/2026-05-22-pty-session-trait-design.md:469-471`)
explicitly punts: "Multi-client resize coordination ... — trait is
policy-free; coordination lives in a higher abstraction if/when needed."

## What the Multi-Client Layer Needs to Add

The session manager (one level above `GhosttyPty`) needs to:

1. **Track per-client advertised size** — each `sessions.attach`
   invocation carries `attach-init { cols, rows, no-stdin }` as its
   args, analogous to zmx's `.Init` payload. See [[external-protocol]].
2. **Run a policy** — recommended: leader-wins to match zmx. Web followers
   letterbox by rendering only the visible subset of the emulator's grid. See
   [[client-attach-and-election]].
3. **Call `PtySession::resize` on policy outcome** — already drives both VT
   and kernel; no worker change required.
4. **Broadcast a size event to all attached clients** so followers can
   letterbox, scroll, or refuse to render. New message kind alongside output
   bytes — tagged enum on the same broadcast channel, or a sibling channel.
   See [[internal-communication]].

## Pixel Dimensions (XTWINOPS, CSI 14/16/18 t)

The VT can answer pixel-size queries only if told the cell pixel size. Cairn
passes `0, 0` (`worker.rs:376`), so size reports return zero or are
suppressed. The fundamental problem: **a headless emulator has no font, so it
cannot answer truthfully** — each attached client has different font metrics.

Options:

- **Refuse / return zero** (today). Safe; breaks TUIs that rely on pixel
  reports for graphics (sixel, Kitty image protocol).
- **Hard-coded fallback (e.g. 10×20 px/cell).** Lies, but produces plausible
  numbers for clients that need *something*.
- **Defer to the leader via `Terminal::on_size`** — the callback at
  `libghostty-vt-0.1.1/src/terminal.rs:972-987` lets us answer per-query by
  routing to the current leader's font metrics. See
  [[query-response-delegation]].

Cell pixel dims also feed `Terminal::resize`'s `cell_width_px`/
`cell_height_px` (image-protocol coordinates). If we ever support sixel/Kitty
graphics, leader-pixel-size has to make it into the resize call too.

## Race Conditions

The worker serializes everything through the `flume` command channel
(`worker.rs:265`), so concurrent client resizes arrive as a totally ordered
stream — no kernel race, no VT race. The semantic question is which order the
policy layer picks, not whether the worker can apply them safely.

Failure modes that still exist:

- **Mid-resize EOF.** If the child exits between `terminal.resize` and
  `pty.resize`, the VT has a new grid but the kernel call may fail. Cairn
  surfaces the error and leaves `current_size` unchanged
  (`worker.rs:383-385`).
- **Broadcast vs. snapshot races.** A new `subscribe` (`worker.rs:355-371`)
  takes a snapshot from current VT state and registers a fresh stream. Once
  we add size events to the broadcast, we need to confirm an in-flight resize
  ends up reflected in *either* the snapshot *or* the first stream item —
  never both, never neither.

## Initial Size

Cairn takes `initial_size` from `SpawnOptions::size`
(`crates/cairn-pty/src/pty/types.rs:22`) at handle construction. Child spawns
immediately; default 80×24 (`types.rs:9-13`).

This diverges from zmx, which defers spawn until the first attaching client
provides a tty size (`main.zig:701`, `main.zig:2301-2303`). The divergence is
intentional: cairn often runs headless agents that have no human at attach
time (e.g. a scheduled Claude run), so the session must make forward progress
before any client shows up. The browser-first design also means there may
**never** be a tty — ghostty-web advertises a virtual size from CSS layout.

## Resize on Detach

When the last client detaches, the PTY size stays put. The child keeps
running at that size until a new leader attaches and resizes, or the child
exits. This matches zmx: `closeClient` (`main.zig:622-641`) clears
`leader_client_fd` but does not touch `TIOCSWINSZ`. A new attacher whose
viewport differs sees reflow on their next resize, exactly as if connecting
to a still-active leader.

Resetting on last-detach is tempting but harmful: long-running TUIs (`htop`,
`vim`) would garble on every detach as they reflow to some default, then
garble again on the next attach. Keep-last is correct.

## Divergences Summary

| Concern | zmx | cairn today | cairn target |
|---|---|---|---|
| Policy | leader-wins | none (trait punts) | leader-wins at manager layer |
| Initial size | first attach's tty | `SpawnOptions::size` (default 80×24) | same as today |
| Spawn timing | defer to first attach | eager | eager |
| Resize broadcast to followers | no | no | yes (size event) |
| Pixel dims in `Terminal::resize` | n/a (zig API takes only cols/rows) | always 0, 0 | leader's font metrics |
| XTWINOPS pixel reports | n/a | unanswered | delegated to leader |

## Open Questions

1. Opt-in "defer spawn until first attach" `SpawnOptions` flag for
   interactive flows? Useful for `cairn attach`, harmful for headless agents.
2. Smallest-wins vs. leader-wins default. zmx's leader-wins is simpler but a
   web-only follower with a small viewport sees corrupted layout. Letterbox
   client-side, scroll, or refuse to render? See [[web-vs-cli-clients]].
3. Broadcast size event payload: `(rows, cols)` or full
   `(rows, cols, cell_px_w, cell_px_h)`? Cell-only is simpler; the quadruple
   matters only for image-protocol-aware clients.
4. Leadership transfer on detach: if A detaches and B takes over, resize
   immediately to B's size, or wait for B's explicit resize? zmx does the
   latter implicitly via the empty-`.Resize` ping in `setLeader`
   (`main.zig:648`). Probably mirror that. See [[client-attach-and-election]].
5. Short-circuit `PtySession::resize` when `current_size == size`?
   `Terminal::resize` is a no-op for unchanged dims (`terminal.rs:185`), but
   `pty.resize` may still emit a redundant `SIGWINCH`. Cheap to add in the
   worker if churn becomes real.
6. Pixel-dim delegation when no leader is attached — who answers `CSI 14 t`?
   Returning `None` from `on_size` (silently dropping the query) is probably
   right but worth confirming against ghostty's behavior. See
   [[query-response-delegation]].
