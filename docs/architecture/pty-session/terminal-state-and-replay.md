# Terminal State and Replay

How cairn brings a newly-attached client up to the current state of a
long-running session, and how that handoff stays consistent with the live
broadcast that follows. See [[client-attach-and-election]] for the attach
sequence around this snapshot and [[pty-lifecycle]] for the session worker
that owns the emulator.

## The embedded emulator

Each session owns one headless `libghostty_vt::Terminal`, constructed in the
worker thread at startup
(`crates/cairn-pty/src/pty/ghostty/worker.rs:210-221`):

```rust
Terminal::new(TerminalOptions {
    cols: s.initial_size.cols,
    rows: s.initial_size.rows,
    max_scrollback: s.scrollback_lines,
})
```

libghostty-vt is the same VT core ghostty.org uses for its native terminals,
exposed via a C ABI and wrapped in Rust. The terminal object tracks the
active screen grid with per-cell glyphs and SGR styling, the scrollback
ring, cursor position and visibility, the alternate screen, DECSET/DECRSET
modes (wraparound, application cursor keys, bracketed paste, alt-screen
1049, synchronized output 2026, etc.), keyboard protocol mode, palette,
scrolling region, tabstops, and OSC 7 working directory. The Rust bindings
do not expose the fields directly — the public surface is `vt_write(&[u8])`
to push bytes in and the `fmt::Formatter` family to read state out
(`libghostty-vt-0.1.1/src/terminal.rs:1-60`).

Every chunk read from the PTY master is fed into the emulator before being
broadcast (`worker.rs:283-290`):

```rust
terminal.borrow_mut().vt_write(&chunk);
if let Some(tx) = bcast_tx.borrow().as_ref() {
    let _ = tx.send(chunk);
}
```

The emulator therefore always reflects exactly what the broadcast has
delivered to subscribers — there is no "behind" state.

## When the snapshot is generated

The snapshot is **built on demand per `Subscribe` command** — not maintained
continuously (`worker.rs:355-370`). When a client calls `subscribe()`, the
dispatcher synchronously runs `format_snapshot(&terminal.borrow())` and
pairs the bytes with a fresh `broadcast::Sender::subscribe()` receiver in a
`Subscription` (`crates/cairn-pty/src/pty/subscription.rs:16-19`).
Snapshot cost is paid only at attach. For a quiescent multi-client session,
snapshot work is zero — the broadcast channel handles steady-state fan-out.

## Wire format: raw VT escape stream

`format_snapshot` uses libghostty's `Formatter` in `Format::Vt` mode with
`trim: false, unwrap: false` (`worker.rs:481-498`). The output is a
self-contained byte stream of VT/ANSI escape sequences that, fed to any
VT100-family emulator (xterm.js, ghostty-web, a native terminal),
reconstructs the visible screen. `format_alloc` returns a libghostty
allocation; cairn immediately copies it into `bytes::Bytes` so the
libghostty buffer is freed on drop (`worker.rs:489-497`).

**Why raw VT bytes instead of structured JSON?** Cairn's clients are
already VT emulators. A structured grid would force them to translate back
into draw calls; escape sequences make the snapshot indistinguishable from
normal PTY output, so the same client code path handles both. The cost is
opacity — debugging "what's in the snapshot" means running it through a
parser. See [[external-protocol]] for transport.

## What libghostty's `Format::Vt` includes

cairn uses the **default** `FormatterTerminalExtra`
(`libghostty-vt-sys-0.1.1/src/bindings.rs:1188-1208`). All extra-state
flags default to false; the snapshot includes visible-screen content
(cells + SGR + links) and scrollback. The flags cairn does **not** set:
`modes`, `scrolling_region`, `tabstops`, `pwd`, `keyboard`, `palette` —
a known gap relative to zmx (see below).

## How zmx does it (divergence)

zmx's `serializeTerminalState`
(`/Users/abe/Projects/zmx/src/util.zig:479-586`) does a **two-phase**
serialization to avoid corrupting cursor position when scrollback is
present:

1. **Phase 1 — scrollback only.** A `TerminalFormatter` runs over a
   `Selection` covering rows above the active viewport with `extra = .none`
   (plain content, no modes/cursor) (`util.zig:511-528`). These rows scroll
   past the visible area into the client's local scrollback.
2. **Phase 2 — visible screen with full extras.** zmx emits
   `\x1b[2J\x1b[H\x1b[0m` to clear the visible screen and reset SGR
   (`util.zig:533`), then runs a second formatter restricted to the active
   viewport with `palette: false, modes: true, scrolling_region: true,
   tabstops: false, pwd: true, keyboard: true, screen: .all`
   (`util.zig:559-567`).

zmx also temporarily disables DECSET 2026 (synchronized output) during
serialization (`util.zig:488-491`) so a new client doesn't sit in
deferred-render until its local timeout fires; the original mode is
restored before returning (`util.zig:578-580`).

**Cairn does not yet do any of this.** It runs one
`Formatter::new(...).format_alloc(...)` with default extras. The zmx tests
at `util.zig:1097-1135` and `util.zig:1190-1209` demonstrate the failure
modes cairn has not hardened against: scrollback corrupting cursor on the
receiving side, and alt-screen content potentially leaking ("ALT_MARK" must
not appear after exiting alt mode). Verifying cairn against the same tests
is open work.

## Alt-screen handling

The emulator tracks both screens; only one is active at a time. The
formatter's screen-extra controls which screens contribute to the output.
zmx sets `screen: .all` (`util.zig:566`), which means "emit whichever
screen is currently active, plus the DECSET sequence to restore that mode."

If `vim` or `htop` holds the alt screen at snapshot time, the snapshot
consists of the alt-screen content plus `\x1b[?1049h` so the client
switches into alt mode locally. Scrollback is **not** replayed because the
alt screen does not have scrollback. When the app exits and DECRST 1049
flows through the live stream, the client returns to its prior main screen
— preserved client-side, never touched by the snapshot. The zmx
roundtrip test (`util.zig:1190-1209`) covers "alt entered, written,
exited, then snapshot taken," asserting the result contains `MAIN_MARK`
but not `ALT_MARK`. Cairn relies on libghostty's defaults here and lacks
an equivalent test.

## Scrollback bounds

Cairn: configurable via `SpawnOptions::scrollback_lines`, default **1000**
lines (`crates/cairn-pty/src/pty/types.rs:26-37`,
`crates/cairn-pty/src/pty/types.rs:50-53`). The value is passed straight
through to libghostty's `TerminalOptions::max_scrollback`
(`worker.rs:210-214`).

zmx: configurable via `Cfg.max_scrollback`, default **10_000_000** lines
(`/Users/abe/Projects/zmx/src/main.zig:473`,
`/Users/abe/Projects/zmx/src/main.zig:2472`). zmx is positioned as a
long-running session persister (the user runs commands over hours/days and
expects `zmx history` to find old output), so its default is four orders of
magnitude larger.

The right cairn default is an open question — see [[configuration]] for the
broader settings story.

## Snapshot cost

Emulator memory is proportional to `cols × rows × (visible + scrollback)`.
With the default `max_scrollback = 1000`, the upper bound is ~1000 rows ×
80 cols ≈ 80 KB of cells. Snapshot generation walks those rows and emits
VT bytes; raw text dominates the output, plus SGR transitions between
styled runs. Mostly-empty sessions produce hundreds of bytes; dense
colored output produces tens to hundreds of kilobytes.

The work is single-threaded and runs on the worker thread, blocking PTY
reads for its duration. Attach is rare relative to data flow so this is
acceptable, but a session with megabytes of scrollback could see
noticeable latency on attach. Benchmarking is open work.

## HTML output: zmx-only, on-demand

zmx exposes `zmx history <name> [--vt|--html]` for offline export
(`/Users/abe/Projects/zmx/src/main.zig:118-135`). The IPC message is
`Type.History` with a single-byte format selector
(`/Users/abe/Projects/zmx/src/main.zig:1117-1128`). The daemon runs
`util.serializeTerminal` with the chosen format
(`/Users/abe/Projects/zmx/src/util.zig:594-635`); `Format.Html` emits HTML
with inline styles via libghostty's HTML formatter (`util.zig:605, 620`).

HTML is **only produced on demand**; the live broadcast is always raw
bytes. There is no "HTML stream" mode. Cairn does not expose `history` or
`--html` today; the binding supports it (`libghostty-vt-0.1.1/src/fmt.rs:174`),
so adding it later is mechanical. See [[external-protocol]].

## Replay-while-attached: no missed or duplicated bytes

The guarantee is *structural*, not coordination-based. The worker runs as
a single-threaded `LocalSet` task handling one `tokio::select!` branch per
iteration (`worker.rs:259-416`). PTY data enters via the `pty.read` branch;
subscriptions are created in the `Command::Subscribe` arm. These cannot
run concurrently — when one arm executes, no other branch's future is
being polled.

The `Subscribe` arm runs `format_snapshot(&terminal.borrow())` and
`tx.subscribe()` **back-to-back with no `.await` between them**
(`worker.rs:355-370`). Because `vt_write` and `tx.send(chunk)` in the read
arm are also synchronous and run in the same select iteration
(`worker.rs:283-290`), the emulator and the broadcast head pointer are
always in lockstep. `subscribe()` returns a `Receiver` positioned at the
current tail; the next byte the broadcast emits is the first byte *after*
whatever the emulator absorbed into the snapshot.

Clients therefore see:
1. The snapshot — emulator state through the last `vt_write` the worker
   processed.
2. The stream, starting with the next PTY chunk the worker reads.

No gap (no PTY read can sneak in between snapshot and subscribe; no
`.await` separates them). No overlap (the receiver is at the broadcast's
current tail, not earlier).

The one failure mode is `RecvError::Lagged` — if the subscriber falls
behind `broadcast_capacity` chunks, tokio's broadcast channel drops the
oldest entries. The documented recovery is to drop the `Subscription` and
call `subscribe()` again to get a fresh snapshot
(`crates/cairn-pty/src/pty/subscription.rs:11-15`). See [[backpressure]]
for the broader capacity-tuning discussion.

## Open Questions

1. **Should cairn adopt zmx's two-phase serialization?** The
   cursor-corruption case (`/Users/abe/Projects/zmx/src/util.zig:1097-1135`)
   is real for any session with scrollback. If libghostty's defaults don't
   handle it, the same fix should land in `format_snapshot`.
2. **Which `FormatterTerminalExtra` flags should cairn set?** zmx enables
   `modes`, `scrolling_region`, `pwd`, `keyboard`, `screen: .all`
   (`/Users/abe/Projects/zmx/src/util.zig:559-567`). cairn uses defaults.
   `modes` especially matters — without it, `bracketed_paste`,
   `application_cursor`, etc., do not survive the snapshot.
3. **Synchronized-output (DECSET 2026) handling.** zmx disables it during
   serialization (`/Users/abe/Projects/zmx/src/util.zig:488-491`); cairn
   does not. A client attaching while 2026 is held will defer rendering
   until its local timeout fires.
4. **Default `scrollback_lines = 1000`.** Way under zmx's 10M. Intentional
   for cairn's positioning (agent attach vs. zmx's "leave running for
   days") or a placeholder? See [[configuration]].
5. **HTML / plain-text export.** Not exposed yet. Worth adding to
   [[external-protocol]] for a `cairn history`-style flow?
6. **Snapshot cost at large scrollback.** No benchmark exists. A
   million-line scrollback in `Format::Vt` could be tens of megabytes —
   does the WebTransport carrier (see [[transports]]) handle that as a
   single `server-event::snapshot` payload, or do we need to fragment
   it across multiple stream messages?
7. **Snapshot determinism across libghostty versions.** Format is
   whatever libghostty emits today. A bump could change bytes. Worth a
   snapshot test in [[testing]].
8. **Resize ordering.** Unlike zmx (which serializes state *before*
   applying resize to capture pre-reflow cursor —
   `/Users/abe/Projects/zmx/src/main.zig:942-967`), cairn's `Resize`
   command applies the resize first and never re-snapshots; mid-session
   clients see resize via the live stream. See [[resize-semantics]].
