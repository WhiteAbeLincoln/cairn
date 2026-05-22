# Query Response Delegation

When the inferior emits a terminal query (DA1 `CSI c`, DA2 `CSI > c`, DA3
`CSI = c`, DSR cursor `CSI 6n`, DECRQM `CSI ? n $p`, XTVERSION `CSI > q`,
XTWINOPS `CSI 14/16/18 t`, color scheme `CSI ? 996 n`, OSC color queries,
kitty keyboard `CSI ? u`, ENQ `0x05`), exactly one party must reply. Cairn
has two candidates: the headless libghostty-vt emulator the daemon runs for
state tracking, and any attached ghostty-web client running its own emulator
in the browser. This document specifies who replies, when, and why.

See [[client-attach-and-election]] for the leader/observer policy this gating
piggybacks on, [[terminal-state-and-replay]] for what the headless emulator is
*for*, and [[external-protocol]] for the wire frames carrying client replies.

## The two-emulator problem

The session worker (`crates/cairn-pty/src/pty/ghostty/worker.rs:200-417`)
feeds every PTY-master byte through a libghostty-vt `Terminal` via `vt_write`
(`worker.rs:287`). libghostty-vt is a real emulator: parsing a query escape
invokes the `on_pty_write` callback
(`libghostty-vt-0.1.1/src/terminal.rs:908-922`). Cairn's current callback
(`worker.rs:223-228`) unconditionally pushes those bytes onto a
`pending_writes` queue, drained back to the PTY master on the next iteration
(`worker.rs:292`, `flush_pending_writes` at `worker.rs:435-446`).

Simultaneously, the same PTY bytes are broadcast to every attached client
(`worker.rs:288-290`). A ghostty-web client has its own emulator that
*also* parses the query and generates a reply, sent upstream as a `Write`
command. The shell sees two DA1 responses, corrupts its parser, and the user
sees garbage. This is the **double-reply problem**.

Doing nothing is also wrong. With no client attached, the backend *must*
reply or shells like fish hang for ~10s on `CSI c`
(`zmx/src/main.zig:2566-2569`).

## How zmx solves it

zmx hardcodes DA1/DA2 responses (`zmx/src/util.zig:142-180`) and only emits
them when no terminal client is attached. The gate is a single bool on the
`Daemon` struct:

```
has_terminal_client: bool = false, // true only after a real attach (.Init received)
                              // — main.zig:592
```

The PTY-read path checks it inline:

```
if (!daemon.has_terminal_client and
    daemon.pty_write_buf.items.len < Daemon.PTY_WRITE_BUF_MAX)
{
    util.respondToDeviceAttributes(daemon.alloc, &daemon.pty_write_buf, buf[0..n]);
}
                              // — main.zig:2572-2576
```

A "terminal client" is one that sent the `.Init` IPC message — an interactive
`zmx attach`, not a `zmx run` tail-only consumer (`main.zig:2569-2571`). The
flag flips to `true` exactly once, inside `handleInit` (`main.zig:996`), and
**never flips back on detach**: `closeClient` clears only `leader_client_fd`
(`main.zig:625-630`). Once any interactive client has ever attached, the
daemon stops answering DA queries for the rest of the session. zmx accepts
this because by then the shell has its initial DA1 reply and won't reprobe
during normal use.

**The gate is independent of leader election.** zmx has a separate leader
notion (`main.zig:583`, `setLeader` at `main.zig:643-650`) used for resize
authority and input routing, but query gating uses neither leader identity
nor any per-client flag — purely "has *any* interactive client ever
attached?". See [[client-attach-and-election]].

zmx handles *only* DA1 and DA2 in its hardcoded path (`util.zig:146-147`,
responses `\x1b[?62;22c` and `\x1b[>1;10;0c`). DSR cursor, DECRQM, XTWINOPS,
XTVERSION fall through libghostty's own internal write path. The hardcoded DA
path exists because libghostty's DA reply requires an explicit
`on_device_attributes` callback (next section) zmx evidently doesn't install.

## libghostty-vt's callback surface

libghostty-vt 0.1.1 splits query handling into two tiers:

**Tier 1 — answered internally via `on_pty_write`.** The terminal parses,
computes the reply, and emits response bytes through `on_pty_write`
(`terminal.rs:908-922`). The doc explicitly cites "DECRQM query or device
status report" (`terminal.rs:910`), and the module example at
`terminal.rs:99-101` shows DECRQM `CSI ? 7 $p` triggering it. DSR cursor
(`CSI 6n`) falls here: no dedicated callback, answered from cursor state.

**Tier 2 — require an explicit embedder callback or the query is silently
dropped.** No honest backend answer; the terminal asks the embedder:

- `on_device_attributes` — DA1/DA2/DA3 (`terminal.rs:1009-1027`). `None`
  silently ignores.
- `on_xtversion` — `CSI > q` (`terminal.rs:946-956`).
- `on_size` — XTWINOPS `CSI 14/16/18 t` (`terminal.rs:972-987`).
- `on_color_scheme` — `CSI ? 996 n` (`terminal.rs:989-1007`).
- `on_enquiry` — ENQ `0x05` (`terminal.rs:936-944`).
- `on_bell` (`terminal.rs:926-933`) and `on_title_changed`
  (`terminal.rs:962-970`) — same dispatch shape, not query responses.

**Tier 2 callbacks fire synchronously inside `vt_write`**
(`terminal.rs:43-45`). The reply decision happens at parse time, before any
client sees the bytes.

Cairn today installs *only* `on_pty_write` (`worker.rs:224-228`): Tier 1
answered by the backend, Tier 2 silently dropped. That is the *opposite* of
what we want once a client is attached, and partial otherwise.

## Proposed gating for cairn

Track an attached-primary count on the worker. The LocalSet is
single-threaded (`worker.rs:65-66`), so no atomics:

```rust
let primary_count: Rc<Cell<usize>> = Rc::new(Cell::new(0));
```

Each callback closure captures a clone:

- **`on_pty_write`**: if `primary_count.get() == 0`, push to
  `pending_writes` as today; else drop. Covers Tier 1 (DECRQM, DSR cursor)
  where libghostty would otherwise speak over the client.
- **`on_device_attributes` / `on_xtversion` / `on_size` / `on_enquiry`**: if
  `primary_count.get() == 0`, return `Some(<canned answer>)`; else `None`.
  Canned DA1/DA2 should match zmx's (`\x1b[?62;22c`, `\x1b[>1;10;0c`).
- **`on_color_scheme`**: return `None` whenever a primary is attached
  (browser has the honest answer); with none attached, return `None` too —
  no honest default, and shells gate this behind a feature probe.

`primary_count` is incremented/decremented by the attach/detach paths in
[[client-attach-and-election]]. The count form lets us experiment with
policies like "any interactive client suppresses backend replies, even
non-leaders" without restructuring callbacks.

## Queries where the client's answer is genuinely better

Some queries have no honest backend answer:

- **`CSI ? 996 n` (color scheme)** — the user's terminal theme; only the
  client knows. Backend has no theme.
- **`CSI 14/16/18 t` (XTWINOPS pixel sizes)** — client's font metrics.
  Backend tracks cells, not pixels.
- **Kitty keyboard flags (`CSI ? u`)** — what the client emulator actually
  supports. The backend can claim support, but if the client doesn't honor
  it the inferior emits sequences the client can't render.
- **OSC color queries (`OSC 10/11/12 ? ST`)** — user's palette.

For these the policy biases toward "let the client answer if one is
attached, otherwise stay silent." A wrong answer is worse than none: shells
recover from timeouts, but a lie locks the inferior into the wrong code path
for the session.

## Race conditions on transition

Two windows matter:

1. **Attach race.** Inferior emits `CSI c`; worker calls `vt_write`,
   `on_device_attributes` fires synchronously. If a client attach increments
   `primary_count` between query read and callback, the callback returns
   `None` — but the client emulator hasn't seen the query yet (broadcast at
   `worker.rs:288-290` runs *after* `vt_write` at line 287). Nobody answers.

   **Mitigation.** The LocalSet is single-threaded
   (`worker.rs:65-66`) and attach is processed via `cmd_rx` on the same
   task, so the increment can only happen between iterations, not
   mid-`vt_write`. The race is bounded to at most one query straddling the
   transition.

2. **Detach race.** Client detaches between `vt_write` and its reply
   reaching the worker upstream. The callback already returned `None`,
   delegating to a client that has now vanished. Harder to mitigate: the
   reply was parsed by the client but lost in transit, and the backend has
   no record that a reply was owed.

## Trust the client, or timeout-fallback?

**Pure delegation** — when a primary is attached, the backend never replies.
Simple, no timers. Vulnerable to the detach race and to misbehaving clients
that silently drop queries.

**Timeout fallback** — when the backend declines to reply, record
`(query_id, deadline)` and at e.g. 200ms send the canned response if no
upstream reply has arrived. Requires inspecting upstream writes to detect
replies, which is fragile: a DA reply and a user typing `\x1b[?62;22c`
literally are byte-identical.

Recommendation: ship **pure delegation** first; add a counter for
client-attached query expirations (see [[observability]]) and invest in
timeout-fallback only if real workloads hit it. The detach race is rare and
recoverable — the shell reissues on the next prompt cycle, or the user
triggers a fresh probe.

## Cross-references

- [[client-attach-and-election]] — primary/leader semantics that
  `primary_count` tracks.
- [[terminal-state-and-replay]] — why the backend emulator exists at all.
- [[external-protocol]] — wire format for client-supplied replies.
- [[internal-communication]] — the `cmd_rx` path attach/detach uses.
- [[resize-semantics]] — `on_size` overlaps with XTWINOPS, documented there.
- [[web-vs-cli-clients]] — CLI and web clients share the query-reply
  contract; gating does not branch on client kind.
- [[observability]] — counters for delegation decisions.
- [[testing]] — fixtures must exercise both `primary_count == 0` and `>= 1`
  paths for each Tier 2 callback.

## Open Questions

1. **Per-query-class gating vs single `primary_count`?** E.g. delegate
   `on_color_scheme` to any attached observer but `on_device_attributes`
   only to the leader. Color answers are equally authoritative from any
   client; DA answers should be stable across attaches.

2. **Reset on last detach?** zmx never resets `has_terminal_client`
   (`main.zig:625-630`). Cairn could re-enable backend replies after the
   last detach so a re-attached client doesn't see a fish-hang on its next
   shell spawn — at the risk of re-answering queries the previous client
   already answered if the shell reprobes mid-transition.

3. **Canonical DA1/DA2 string?** zmx uses `\x1b[?62;22c` /
   `\x1b[>1;10;0c`. Match for fingerprint compatibility, or claim more
   (truecolor, kitty graphics) so feature-gated shell code activates? Depends
   on what the client emulator actually supports — see
   [[web-vs-cli-clients]].

4. **How to test race windows?** Driving `vt_write` and inspecting
   `pending_writes` is straightforward; attach/detach interleavings need a
   harness that flips `primary_count` between `vt_write` and broadcast.

5. **Kitty keyboard `CSI ? u` push protocol.** Stateful (push/pop/set), not
   a one-shot query. If a client pushes flags and detaches, what does the
   backend report to the next attacher? May belong in
   [[terminal-state-and-replay]] but gating policy needs to know.

6. **Unconditional ENQ delegation?** ENQ is rare enough that maintaining a
   canned answer may not be worth it; silence is benign for every modern
   shell.
