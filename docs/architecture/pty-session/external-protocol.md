# External Wire Protocol

The external protocol governs every byte that crosses the daemon's network
boundary toward an attached client. zmx ships this over a Unix domain
socket; cairn ships the same conceptual messages over WebSocket so
ghostty-web can attach alongside CLI clients. PTY lifecycle lives in
[[pty-lifecycle]]; leader election lives in [[client-attach-and-election]].

## zmx baseline

zmx's protocol is defined in `/Users/abe/Projects/zmx/src/ipc.zig`. The wire
unit is a fixed-size header followed by an opaque payload:

```zig
pub const Header = packed struct {
    tag: Tag,   // u8
    len: u32,   // payload length, little-endian (host order on x86_64/arm64)
};
```

`@sizeOf(Header) == 8` is asserted frozen (`ipc.zig:258`) — the
`packed struct{u8, u32}` is a u40 that the ABI pads to 8 bytes.
`expectedLength` at `ipc.zig:69-75` is the parser's single source of
truth, and `SocketBuffer.next()` (`ipc.zig:175-185`) returns
`(Header, payload[len])` over an internal ring buffer fed by `read(2)`.

Payloads are raw byte slices. Most tags carry opaque blobs; `Init` and
`Resize` carry fixed C-ABI structs via `std.mem.asBytes` /
`std.mem.bytesToValue`:

- `Resize { rows: u16, cols: u16 }` — `ipc.zig:38-41`.
- `Info { clients_len, pid, cmd_len, cwd_len, cmd[256], cwd[256], created_at, task_ended_at, task_exit_code }` — `ipc.zig:57-67`,
  asserted at exactly **552 bytes** (`ipc.zig:256`) and frozen at
  `ipc.zig:248-254`: don't widen, don't reorder; new fields go in new tags.

### Tag set

The full enum (`ipc.zig:6-25`) and its frozen integer mapping
(`ipc.zig:261-269`):

| Tag            | Value | Direction       | Payload                                          |
|----------------|-------|-----------------|--------------------------------------------------|
| `Input`        | 0     | client → daemon | raw keystroke bytes for PTY stdin                |
| `Output`       | 1     | daemon → client | raw PTY stdout bytes (and snapshot replay)       |
| `Resize`       | 2     | both directions | `Resize` struct (daemon → client = "tell me your size") |
| `Detach`       | 3     | client → daemon | empty — drop this one client                     |
| `DetachAll`    | 4     | client → daemon | empty — kick every attached client               |
| `Kill`         | 5     | client → daemon | empty — terminate the session + child           |
| `Info`         | 6     | both            | `Info` struct (response only); client sends empty |
| `Init`         | 7     | client → daemon | `Resize` struct — the canonical "attach" message |
| `History`      | 8     | both            | u8 format byte (request); serialized output (response) |
| `Run`          | 9     | client → daemon | command string for task mode                     |
| `Ack`          | 10    | daemon → client | empty — acknowledges `Run`/`Write`              |
| `Switch`       | 11    | both            | session-name string (rebind the active session)  |
| `Write`        | 12    | client → daemon | `[u32 path_len][path][file content]` (`main.zig:2084-2096`) |
| `TaskComplete` | 13    | daemon → client | one byte: exit code (`main.zig:2588`)            |

The key design decision is `ipc.zig:24` — the enum is **non-exhaustive**.
Old daemons receiving newer tags log and ignore (`main.zig:2685-2688`),
so a newer client can negotiate features by sending optional tags without
breaking older peers. The compile-time guard at `ipc.zig:27-31` enforces this.

### Framing semantics

zmx has no request/response correlation — no sequence IDs. Flows are
fire-and-forget:

- **Attach**: client sends `Init(Resize)` (`main.zig:2303`). The daemon
  may reply with one `Output` frame carrying a serialized terminal-state
  snapshot (`main.zig:960`), then streams live `Output`. Snapshot and live
  data share the same tag; the client can't distinguish them except by
  ordering. See [[terminal-state-and-replay]].
- **Stream**: one PTY read = one `Output` frame, no coalescing
  (`main.zig:962-966`).
- **Pseudo-request/response**: `Info`, `History`, `Write`, and `Run`
  use "send tag X, drain until matching reply tag (or `Ack`)" —
  `ipc.zig:236-244`, `main.zig:2101-2118`. No in-flight multiplexing.

## Cairn's design over WebSocket

WebSocket already provides reliable, ordered, length-delimited message
framing over TCP/TLS, so we drop zmx's `Header { tag, len }` layer entirely:
**one WebSocket message = one protocol message**. We use binary frames
exclusively (opcode `0x2`), never text. Browser `WebSocket` APIs expose
binary frames as `ArrayBuffer` / `Blob`; PTY input contains raw bytes
including 0xFF and partial UTF-8 mid-sequence, which would be corrupted by
a text-frame UTF-8 validator.

### Serialization: tagged binary

We adopt **MessagePack** for the body, with a one-byte version prefix.
Rationale vs. alternatives:

- **MessagePack**: schema-less, portable (Rust `rmp-serde`, TS
  `@msgpack/msgpack`), supports raw binary natively (`bin` type — opaque
  PTY bytes), and self-describing field names evolve without wire-position
  coordination.
- **bincode**: smaller/faster but Rust-only; reproducing its tagged-union
  layout in TS by hand is a maintenance hazard.
- **CBOR**: comparable; Rust ecosystem (`ciborium`) is heavier and we
  have no COSE/IoT need.
- **Custom packed structs (zmx-style)**: rejected — `ipc.zig:248-254`
  documents the cost of maintaining hand-frozen `@sizeOf` invariants
  across two languages.
- **JSON**: rejected on the high-rate `Output`/`Input` path; base64
  costs ~33% bandwidth and defeats the point of binary transport.

Frames encode as a msgpack map `{ "t": u8, "p": ... }`. We prefer the
map form over a positional array — three extra bytes per message buys
forward-compatibility for new fields.

### Message set

Mapping the zmx tag set onto cairn plus the additions web clients need.
Tag integers are renumbered — cairn is a clean break:

| Tag             | Direction       | Payload (msgpack fields)                                  | Notes |
|-----------------|-----------------|-----------------------------------------------------------|-------|
| `Hello`         | client → daemon | `{ proto_version, capabilities: [str], client_name }`     | Always first frame. |
| `Welcome`       | daemon → client | `{ proto_version, capabilities, session_id, server_version }` | Reply to `Hello`. |
| `Attach`        | client → daemon | `{ session, cols, rows, resume_from? }`                   | zmx's `Init` + session selection. |
| `Snapshot`      | daemon → client | `{ bytes: bin }`                                          | Opaque VT escape stream from the embedded emulator. One frame per attach. zmx folds this into `Output` (`main.zig:960`); we split it so clients can route it to a fresh emulator instance. See [[terminal-state-and-replay]]. |
| `Output`        | daemon → client | `{ bytes: bin }`                                          | Live PTY bytes. One PTY read = one frame. |
| `Input`         | client → daemon | `{ bytes: bin }`                                          | Raw keystroke bytes. |
| `Resize`        | both            | `{ cols: u16, rows: u16 }`                                | Client → daemon = request; daemon → client = leader handoff prompt. See [[resize-semantics]]. |
| `Detach`        | client → daemon | `{}`                                                      | Graceful detach. |
| `Ping` / `Pong` | both            | `{ nonce: u32 }`                                          | App-layer keepalive — browsers can't send WS-level pings. |
| `Error`         | daemon → client | `{ code: str, message: str, fatal: bool }`                | No zmx equivalent (zmx closes silently). |
| `Bye`           | both            | `{ reason: str }`                                         | Soft close before FIN; lets the peer distinguish intent from network failure. |

Out of scope for the v1 attach WS: `Info`, `History`, `DetachAll`,
`Kill`, `Switch`, `Run`, `Write`, `TaskComplete`. `Info` and `History`
become HTTP endpoints (`GET /sessions/:id`, scrollback dump). The rest
are CLI-only commands routed through a control endpoint, not the attach
WS. See [[web-vs-cli-clients]].

A deliberate narrowing: the WebSocket carries the **attach data plane**
(input, output, resize, lifecycle); listing/killing/file-injection move
to an HTTP/JSON control plane. zmx couldn't do this — Unix sockets aren't
multiplexed at the transport layer. See [[web-vs-cli-clients]].

### Streaming and frame size

One PTY read = one `Output` WebSocket frame, matching zmx
(`main.zig:962-966`). PTY reads are kernel-bounded (~64 KiB on Linux),
well under any WS frame limit. We cap application-level frames at
**1 MiB**; oversized payloads (e.g. `cat`-ing a large file) split across
multiple `Output` frames. Splitting at arbitrary byte boundaries is safe
because the client emulator reassembles partial VT escape sequences
stateful-ly. No reassembly markers needed.

Backpressure handling lives in [[backpressure]]; the wire surface is
either `Error{code:"lagged", fatal:true}` (client reconnects and
re-snapshots) or `Bye` followed by close.

### Versioning

A one-byte **protocol-version prefix** precedes the msgpack body. v1 is
`0x01`. An unknown byte triggers `Error{code:"proto_version", fatal:true}`
and close. Ordinary evolution rides on msgpack's self-describing maps and
the capabilities exchange — `Hello`/`Welcome` carry capability strings
(`"snapshot.v1"`, `"resize"`, `"history.osc133"`, …). Features attach a
capability; clients that don't advertise it never receive its messages.
This is the WebSocket analogue of `ipc.zig:24`'s non-exhaustive enum.

Backwards compatibility is otherwise out of scope for v1 — daemon and
ghostty-web ship together; we'll bump the prefix the first time we need a
hard break.

### Reconnect semantics

WebSockets disconnect routinely (background tabs, network blips, idle
timeouts). The protocol assumes **every reconnect is a full re-attach**:
`Hello`, `Attach`, fresh `Snapshot`, resume streaming. This matches zmx
on every new socket connection (`main.zig:946-970`).

We deliberately do **not** add resume-from-sequence semantics:

1. The snapshot path is already canonical — the embedded ghostty
   emulator's serialized state reflects current screen regardless of
   gap duration.
2. Per-client byte offsets would require unbounded retention per client,
   defeating the broadcast-channel design
   (`crates/cairn-pty/src/pty/subscription.rs:18`).
3. Lossless replay is impossible anyway — the broadcast channel drops
   lagged subscribers (`subscription.rs:13`).

The `Attach.resume_from` field is reserved (omitted in v1) so future
versions can opt into delta replay without a wire break. Leader election
on reattach: [[client-attach-and-election]].

### Binary safety

All payloads are opaque. `Input` and `Output` may contain any byte
0x00–0xFF in any order — standalone 0xFF, embedded NULs, split UTF-8
sequences. We never validate or transform in transit; that's the
emulator's job at one end and the kernel/PTY's job at the other. zmx
takes the same posture — `handleOutput` (`main.zig:962`) just
`appendSlice`s bytes.

## Open Questions

1. **Auth frame placement** — token on the WS subprotocol header, on
   `Hello.auth`, or a separate first frame before `Hello`? See
   [[authentication]]; needs a v1 decision.
2. **Snapshot fragmentation** — large grids with scrollback serialize
   to multi-MiB. Split `Snapshot` across frames with a `final: bool`,
   or raise the 1 MiB cap just for that tag?
3. **`Resize` arbitration** — when a non-leader sends `Resize`, do we
   ack-and-ignore, reply with `Error`, or silently swallow it (zmx's
   choice, `main.zig:1009-1011`)? Affects [[resize-semantics]].
4. **`Ping` cadence** — server-side liveness probe, or rely on TCP
   keepalive + WS close? Interacts with [[backpressure]].
5. **Control plane split** — `Info` / `History` / `Kill` genuinely move
   to HTTP, or do CLI clients need single-transport correlation?
   [[web-vs-cli-clients]].
6. **Schema source-of-truth** — `.proto`-style IDL with codegen, or
   hand-written `serde` + TS interfaces? Affects [[testing]] and
   [[observability]].
7. **MessagePack `ext` types for `Bytes`** — one extra byte per frame
   but unlocks typed-array fast paths on the JS side. Worth it?
