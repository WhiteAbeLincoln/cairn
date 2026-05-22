# Web Client vs CLI Client

Cairn's attach surface is one WebSocket protocol
([[external-protocol]]) but two very different consumers: the CLI
client in a real terminal (`cairn attach`), and `ghostty-web`, a WASM
build of libghostty running in a browser tab. The wire format is
identical; the **environments around the wire** are not. zmx solves
only the CLI half (`src/main.zig:1935-2028` `attach`,
`src/main.zig:2284-2456` `clientLoop`); cairn inherits that verbatim
for its CLI client and grafts on a new transport for browsers.

## CLI client concerns

### Termios, raw mode, and restore

A local TTY's cooked-mode line discipline swallows control characters,
echoes bytes, and generates signals before the CLI can forward
anything. zmx handles this in three steps (`src/main.zig:1957-1985`):

1. `tcgetattr` against `STDIN_FILENO`. **If it fails, stdin is not a
   TTY** — skip all terminal munging (`src/main.zig:1958`). This is
   the "`cairn attach foo > log.txt`" case; degrade gracefully, don't
   apply undefined termios bytes.
2. `cfmakeraw` plus three overrides: disable `VLNEXT` (Ctrl-V
   literal-next), disable `VQUIT` so Ctrl+\ is delivered as a byte
   instead of SIGQUIT, and set `VMIN=1 / VTIME=0` for one-byte reads
   (`src/main.zig:1974-1984`).
3. `defer` restoration with `TCSAFLUSH` (discards unread input on
   detach, `src/main.zig:1960-1967`) plus `\x1bc` (RIS) to stdout to
   kick the outer terminal out of alt-screen, mouse mode, bracketed
   paste, etc. that the inferior set.

Cairn's CLI client copies this pattern. Rust equivalent:
`nix::sys::termios::cfmakeraw` plus a `Drop` guard. Avoid `tokio`'s
stdio adapters here — they wrap a blocking fd in a thread pool and mask
`EAGAIN`, making detach-hotkey latency unpredictable. Direct `poll(2)`
on `STDIN_FILENO` matches zmx (`src/main.zig:2314-2321`).

### Detach hotkey: must not reach the inferior

zmx uses **Ctrl+\\** (0x1C) as detach and intercepts it client-side
(`src/main.zig:2372-2377`). The check is `util.isCtrlBackslash`
(`src/util.zig:353-357`): the raw 0x1C byte **or** the Kitty keyboard
protocol's CSI-u encoding (`\x1b[92;5u` and variants,
`src/util.zig:880-941`). Cairn must detect both — a terminal in Kitty
mode never delivers the raw 0x1C — and should scan only the **first**
byte boundary so a mid-stream 0x1C inside a paste doesn't trigger
detach. zmx scans the whole read buffer, which is a known quirk;
narrow this in cairn.

The detach byte never reaches the daemon's `Input` channel: the daemon
sees `Detach` (zmx tag 3, cairn equivalent in [[external-protocol]])
and the inferior sees nothing.

### Signal forwarding

Bytes do the work. Ctrl+C arrives as 0x03, forwards as `Input`, and the
PTY line discipline on the **daemon side** translates it to SIGINT for
the inferior. Same for Ctrl+Z (0x1A → SIGTSTP) and Ctrl+\ — except we've
claimed Ctrl+\ for detach.

SIGWINCH is the exception: a host-OS event on the client's process, not
a byte. zmx installs a self-pipe handler (`src/main.zig:2289-2290`
`openSignalPipe` + `installWakeHandler(SIG.WINCH)`) and on signal-pipe
readability re-queries TTY size and sends a `Resize` frame
(`src/main.zig:2355-2359`). Cairn's CLI uses tokio's
`signal::unix::SignalKind::window_change()`.

Signals on the **client process itself** (SIGTERM, SIGHUP) are not
forwarded — they tear down the CLI, which closes the WebSocket, which
the daemon treats as a normal detach. The session keeps running.

### Bootstrap sequence

Tracing `src/main.zig:1941-1992`:

1. `ensureSession()` — spawn the daemon if missing
   ([[daemon-process-model]]).
2. Connect the transport.
3. Capture and modify termios (above).
4. Write `\x1b[2J\x1b[H` to clear the local screen
   (`src/main.zig:1989-1990`) — fresh canvas for snapshot replay.
   Without this, snapshot text overlays whatever was on screen.
5. Send `Init` / cairn's `Attach` with TTY size, then enter the loop.
6. On any exit (detach, `Switch`, EOF), unwind: termios restore, RIS
   write, close transport. zmx uses `defer`; cairn uses a guard `Drop`.

### Output destination when stdout isn't a TTY

If `tcgetattr` fails (`src/main.zig:1958`) we don't go raw, but zmx still
runs the full loop, dumping `Output` bytes — escape sequences and all —
to whatever fd is hooked up. Fine for `script(1)`-style capture; the CLI
should warn on stderr when stdout isn't a TTY, then proceed.

## Web client (ghostty-web) concerns

No termios, no SIGWINCH, no fd. ghostty-web hosts a WASM copy of
libghostty, so unlike the CLI client (where the emulator *is* the local
terminal), snapshot bytes feed an emulator the browser owns; the
rendered cells paint onto a `<canvas>`.

### Resize is explicit, not signal-driven

There is no SIGWINCH in a browser. CSS layout, font changes, and zoom all
change the cell grid silently. ghostty-web computes its cell dimensions
from font metrics and the canvas pixel size, and must send an explicit
`Resize { cols, rows }` frame ([[external-protocol]]) on every change. A
`ResizeObserver` on the canvas plus a font-load listener covers the cases.

Open question: should the browser also report **cell pixel dimensions**
in the `Resize` frame? XTWINOPS queries (`CSI 14 t`, `CSI 16 t`) can ask
for pixel and cell sizes; today the daemon answers from the embedded
emulator's grid, which has no concept of "actual pixels at the renderer".
Surfacing this would let `tput` and friends report accurate values. See
[[resize-semantics]] for the arbitration story.

### No signal forwarding; intercept browser shortcuts

Ctrl+C, Ctrl+Z, etc. still travel as bytes — same as CLI. The challenge
is the **opposite**: the browser eats some keys before JS sees them.
Ctrl+W closes the tab; Ctrl+T opens a new one; Ctrl+N opens a new
window. ghostty-web has to call `event.preventDefault()` on the keys it
wants to forward, and accept that a few (Ctrl+W on most browsers) are
unrecoverably claimed by the user agent. The detach hotkey from the CLI
client (Ctrl+\\) has no place here — closing the tab *is* detach.

### Reconnect-as-attach

Browsers disconnect WebSockets routinely: backgrounded tabs drop,
network blips kill the socket, OS sleep kills the socket. Every
reconnect is a fresh `Hello` → `Welcome` → `Attach` → `Snapshot` cycle
([[external-protocol]]). The browser keeps its WASM emulator alive
across reconnects and feeds it the new snapshot, so the user sees
continuity even though the connection was destroyed and remade. CLI
clients also reconnect on transient failure, but the *user* notices
because their terminal flickers; the browser hides it entirely.

### Backgrounded tabs

Hidden tabs throttle JS timers to ~1 Hz and may stop processing
WebSocket frames promptly, eventually surfacing as a slow consumer the
broadcast channel (`crates/cairn-pty/src/pty/subscription.rs`) drops.
Full treatment in [[backpressure]]; the **client-side** contribution is
to reconnect cleanly on `document.visibilitychange` rather than expect
the socket to survive a hidden hour.

### Clipboard (OSC 52)

A CLI client forwards OSC 52 to the local terminal, which decides
whether to honour it (most prompt the user). In the browser ghostty-web
bridges it into `navigator.clipboard.writeText`, which **requires a
user gesture** and HTTPS — silent paste is impossible. The daemon
doesn't need to know any of this: it forwards the escape bytes as
ordinary `Output` and lets each client's policy decide.

### Origin, CSP, and the trust model

CLI clients connect to `localhost` and authenticate with a token loaded
from the user's filesystem; the local-only path can probably skip TLS
([[authentication]]). Browser clients connect over `wss://` from an
arbitrary origin and must be validated:

- **Origin checks** on the WebSocket upgrade — reject any origin not on
  the configured allowlist. WebSocket has no same-origin policy by
  default; the daemon must enforce it.
- **CSP `connect-src`** on the ghostty-web page itself, set tight enough
  that a compromised third-party script can't open a WebSocket to the
  daemon.
- **Same auth scheme as CLI** (bearer token in subprotocol or first
  `Hello` frame) but assume the browser is a less-trustworthy custodian
  — short-lived tokens, no long-lived secret in `localStorage`.

See [[authentication]] for the token issuance flow and
[[external-protocol]] (open question 1) for the placement debate.

## Where the daemon is identical for both

By design, almost everywhere. Both clients speak the same versioned
msgpack-over-WebSocket-binary protocol; both send `Attach { cols, rows
}` on connect and receive a `Snapshot` if there's prior PTY output
([[terminal-state-and-replay]]); both participate in leader election
the same way ([[client-attach-and-election]]) — the worker doesn't
distinguish CLI from browser; both are subject to the same
broadcast-drop policy ([[backpressure]]) and `Resize` semantics
([[resize-semantics]]).

The split surfaces only in the **control plane**: `Info`, `History`,
`Kill`, `Switch` are CLI-flavoured operations
([[external-protocol]] message table). v1 keeps them off the attach
WebSocket and routes them over HTTP/JSON.

## Open Questions

1. **Browser keyboard mode negotiation.** Should ghostty-web advertise
   Kitty keyboard protocol support in `Hello.capabilities` so the
   embedded emulator generates richer key events? CLI inherits whatever
   the host terminal supports; the browser is a blank slate.
2. **Mobile / touch web clients.** ghostty-web on a phone has no
   keyboard. Do we ship a virtual-keyboard overlay, or punt? Affects
   what `Input` frames a browser can produce.
3. **Local-loopback CLI fast path.** A CLI client on the same host as
   the daemon over `wss://localhost` is paying TLS + msgpack tax for no
   security gain. Worth a Unix-socket variant of the WS protocol, or
   accept the cost for transport uniformity? Cross-ref
   [[internal-communication]].
4. **OSC 52 policy surface.** Per-client allow/deny for clipboard
   writes vs. global daemon config? Browsers already gate via user
   gesture; CLI delegates to the host terminal; neither makes the
   daemon authoritative — but if we ever want audit logging
   ([[observability]]) it has to flow through somewhere.
5. **URL detection / link handling.** Pure client concern, but the
   browser can offer richer "open in new tab" affordances than a CLI
   can. Do we standardise an OSC 8 (hyperlink) policy in
   [[configuration]] so the two clients render links consistently?
6. **Detach-hotkey configurability.** Ctrl+\\ is zmx's choice but
   collides with SIGQUIT on user expectation. Should cairn's CLI make
   this configurable per-client? Browsers don't have the problem
   (closing the tab is detach).
7. **Stderr-as-output for non-TTY stdout.** If the CLI is invoked with
   stdout redirected, should we write escape sequences anyway (zmx's
   behaviour) or strip them with a built-in stripper before writing?
   Affects the "use `cairn attach` in a script" workflow.
