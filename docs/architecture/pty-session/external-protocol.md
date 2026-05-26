# External Wire Protocol

The external protocol governs every byte that crosses the daemon's network
boundary toward an attached client. PTY lifecycle lives in [[pty-lifecycle]];
leader election lives in [[client-attach-and-election]]; transport selection
lives in [[transports]].

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
`expectedLength` at `ipc.zig:69-75` is the parser's single source of truth,
and `SocketBuffer.next()` (`ipc.zig:175-185`) returns `(Header, payload[len])`
over an internal ring buffer fed by `read(2)`.

Payloads are raw byte slices. Most tags carry opaque blobs; `Init` and
`Resize` carry fixed C-ABI structs via `std.mem.asBytes` /
`std.mem.bytesToValue`:

- `Resize { rows: u16, cols: u16 }` — `ipc.zig:38-41`.
- `Info { clients_len, pid, cmd_len, cwd_len, cmd[256], cwd[256], created_at, task_ended_at, task_exit_code }` — `ipc.zig:57-67`,
  asserted at exactly **552 bytes** (`ipc.zig:256`) and frozen at
  `ipc.zig:248-254`: don't widen, don't reorder; new fields go in new tags.

zmx has no request/response correlation — no sequence IDs. `Info`, `History`,
`Write`, and `Run` use "send tag X, drain until matching reply tag (or `Ack`)"
— `ipc.zig:236-244`, `main.zig:2101-2118`. No in-flight multiplexing.

The key design tension is `ipc.zig:248-254`: maintaining hand-frozen `@sizeOf`
invariants across two language runtimes is expensive. Cairn avoids this
entirely by choosing a schema-first RPC approach (below).

## Cairn's design: wRPC over WIT

Cairn exposes its full surface as a set of WIT interfaces served via
[wRPC](https://github.com/bytecodealliance/wrpc) (Bytecode Alliance). wRPC
replaces both zmx's bespoke tag-framing layer and the previously sketched
WebSocket+msgpack approach. There is no HTTP/REST control plane split: all
operations — including streaming — ride wRPC.

The decision is documented in full in
`docs/superpowers/specs/2026-05-26-daemon-protocol-design.md`. The short form:
five candidates were evaluated; wRPC wins because it is WIT-native, supplies
maintained Rust crates for both the wire and the codegen, makes the plugin API
drop out for free, and has first-class `stream<T>` / `future<T>` support
without bespoke framing.

### Versions in use

- `wit-bindgen-wrpc` v0.10 — codegen macro, lives in `cairn-protocol`'s
  `[dependencies]`.
- `wrpc-transport` v0.28 — runtime; the `net` feature enables the UDS variant
  (`wrpc-transport::unix`).
- `wrpc-transport-web` — WebTransport carrier for browser and remote CLI
  (planned; not yet a `Cargo.toml` dependency at time of writing).

### Transports

Two transports are served simultaneously by the daemon. See [[transports]] for
path conventions and connection lifecycle details.

- **Unix Domain Socket** — local CLI clients. Path
  `$XDG_RUNTIME_DIR/cairn/cairn.sock` (Linux) or `$TMPDIR/cairn.sock`
  (macOS). Socket mode `0o600`, parent dir `0o700`. Auth via filesystem DAC +
  `SO_PEERCRED` / `getpeereid`; no bearer token.
- **WebTransport** (HTTP/3 over QUIC) — browser and remote CLI. Each
  operation opens one QUIC bidirectional stream within the connection; QUIC's
  own stream multiplexing carries concurrent calls without additional framing
  ceremony. Auth via first-message `meta.authenticate` (see
  [Authentication](#authentication)).

For non-multiplexed transports (UDS, TCP) wRPC applies its framed-stream
spec (`SPEC.md` in the wRPC repo): a version byte `0x00` followed by a header
encoded in the Component Model canonical ABI:

```wit
record header { instance: string, name: string }
```

then one or more `frame { path: list<u32>, data: list<u8> }` records carrying
the indexed async sub-channels for streams and futures. Cairn adds no framing
on top of this; the wRPC spec is the complete wire-format reference.

### Schema: `cairn:daemon@0.1.0`

The schema lives in `crates/cairn-protocol/wit/cairn.wit`. Three interfaces,
two worlds:

**`cairn:daemon/types`** — shared value types. Notable entries:
- `session-id = string` (UUIDv7, server-assigned — chronological sort for free)
- `client-id = string`
- `session-spec`, `session-info`, `exit-status` — session lifecycle records
- `signal` variant (`named(signal-name)` | `numbered(u8)`) and the full
  `signal-name` enum (30 variants: `hup` through `sys`)
- `log-window` variant (`tail(u32)` | `since-unix-ms(u64)` | `all`)
- `attach-init { cols, rows, no-stdin }` — parameters for an attach call
- `client-event` variant: `input(list<u8>)`, `resize(tuple<u16, u16>)`,
  `detach`
- `server-event` variant: `snapshot(list<u8>)`, `output(list<u8>)`,
  `exited(exit-status)`, `error(error)`
- `error { code: string, message: string }`

**`cairn:daemon/sessions`** — 11 operations:

| Operation   | Shape              | Notes |
|-------------|--------------------|-------|
| `list-all`  | unary              | Returns `list<session-info>` |
| `inspect`   | unary              | `result<session-info, error>` |
| `create`    | unary              | `result<session-info, error>` |
| `rename`    | unary              | `result<_, error>` |
| `restart`   | unary              | `result<_, error>`; keeps session id |
| `kill`      | unary              | `result<_, error>`; takes `signal` |
| `kick`      | unary              | `result<_, error>`; `option<client-id>` = all if `none` |
| `wait`      | future-returning   | `future<exit-status>` |
| `logs`      | server-streaming   | `stream<list<u8>>`, `follow: bool` controls whether stream closes after buffered window |
| `attach`    | bidirectional      | `stream<client-event>` in, `stream<server-event>` out |
| `send`      | client-streaming   | `stream<list<u8>>` in, `result<_, error>` out |

**`cairn:daemon/meta`** — 3 operations:

| Operation      | Shape  | Notes |
|----------------|--------|-------|
| `authenticate` | unary  | `result<_, error>`; required first call on WT connections |
| `whoami`       | unary  | `result<string, error>` |
| `version`      | unary  | `version-info { daemon, protocol }` |

**Worlds:**
- `daemon` — exports both interfaces; drives the server-side
  `Handler` trait codegen via `wit_bindgen_wrpc::generate!`.
- `daemon-client` — imports both interfaces; drives client-side free-function
  codegen. A separate world is required because `wit-bindgen-wrpc` only emits
  client invocation functions for `import`ed interfaces. Client code reaches
  these at `cairn_protocol::client::cairn::daemon::{sessions,meta}::*`.

### Codegen

`crates/cairn-protocol/src/lib.rs` drives two `wit_bindgen_wrpc::generate!`
invocations — one for `daemon` (server-side traits), one scoped to
`pub mod client` for `daemon-client` (client-side free functions). The crate
ships three behavioral round-trip tests in `tests/round_trip.rs` that prove
the generated bindings can serve and consume representative messages over a
real in-process UDS socket:

- `meta_version_round_trips_record_fields` — `meta.version` unary call with
  both `VersionInfo` fields asserted.
- `sessions_list_all_round_trips_two_entries_with_optional_fields` — verifies
  the full `SessionInfo` / `SessionSpec` type graph across the wire including
  `option<T>`, `list<string>`, and `list<tuple<string,string>>`.
- `meta_authenticate_round_trips_error_variant` — exercises both the `Ok`
  and the `Err(error)` branch of `meta.authenticate`.

### Authentication

- **UDS**: no application-layer token. `SO_PEERCRED` (Linux) /
  `getpeereid` (macOS) records the invoking uid for audit; filesystem
  permissions enforce access. See [[daemon-process-model]] for socket
  creation.
- **WebTransport**: bearer token loaded from
  `$XDG_RUNTIME_DIR/cairn/token` (mode `0o600`), regenerated on each daemon
  start. The token is sent as the **first invocation on each WT connection**:
  `meta.authenticate(token)`. Any call to `sessions.*` or `meta.whoami` /
  `meta.version` before `authenticate` succeeds is rejected.

  This is not a preference — it is forced by the browser API. `new
  WebTransport(url)` accepts no `Authorization` header (W3C WebTransport
  spec; same restriction as `new WebSocket(url)`). URL-embedded tokens leak
  to logs and browser history. First-message auth is the only mechanism
  that works in the browser; the CLI uses the same path for uniformity.

  The daemon accepts a WT connection and allocates session-level resources
  before knowing whether the peer has a valid token. Mitigated by a
  connection-establishment rate limiter and a short deadline (~5 s) for
  the first frame. See [[daemon-process-model]].

### Streaming semantics

wRPC's indexing carries async sub-channels over the same underlying
connection without extra ceremony. Concrete shapes used by cairn:

- **Unary**: synchronous params and return; no indexing needed.
- **Server-streaming** (`logs`): return is `stream<T>`; wRPC carries it over
  an indexed async sub-channel; client awaits stream close.
- **Future-returning** (`wait`): return is `future<T>`; same indexing
  mechanism as a one-element stream; client resolves a single value
  asynchronously.
- **Client-streaming** (`send`): a `stream<T>` param; client writes chunks
  until end-of-stream; server returns one result.
- **Bidirectional** (`attach`): a `stream<client-event>` param and a
  `stream<server-event>` return, independently scheduled. Ordering is
  preserved within each stream. wRPC's indexing distinguishes the two
  directions without ceremony.

PTY output is not coalesced on the server: one PTY read produces one
`server-event::output` element in the attach stream. This matches zmx
(`main.zig:962-966`). Backpressure lives in [[backpressure]]; the
broadcast channel drops lagged subscribers (`crates/cairn-pty/src/pty/subscription.rs:13`),
which manifests at the wRPC layer as stream close.

### Binary safety

All `list<u8>` payloads are opaque. `client-event::input` and
`server-event::output` may contain any byte 0x00–0xFF in any order —
standalone 0xFF, embedded NULs, split UTF-8 sequences mid-escape. The
Component Model canonical ABI transmits `list<u8>` as a length-prefixed blob;
cairn never inspects or transforms bytes in transit. zmx takes the same
posture — `handleOutput` (`main.zig:962`) just `appendSlice`s bytes.

### Reconnect semantics

WebTransport connections disconnect routinely (background tabs, network
blips, QUIC idle timeouts). The protocol treats **every reconnect as a full
re-attach**: open a new WT connection, call `meta.authenticate`, call
`sessions.attach` with a fresh `attach-init`, receive a `server-event::snapshot`
from the server-side emulator, then resume streaming. This matches zmx on
every new socket connection (`main.zig:946-970`).

Per-client byte-offset resume is deliberately absent:

1. The snapshot path is already canonical — the embedded ghostty emulator's
   serialized state reflects current screen regardless of gap duration.
2. Per-client offsets would require unbounded retention per client, defeating
   the broadcast-channel design
   (`crates/cairn-pty/src/pty/subscription.rs:18`).
3. Lossless replay is impossible anyway — the broadcast channel drops lagged
   subscribers (`subscription.rs:13`).

Leader election on reattach: [[client-attach-and-election]].

### TypeScript client

The browser exercises the full wRPC surface. Because wRPC has no upstream TS
bindings, cairn ships its own:

1. **Canonical-ABI codec** — encode/decode for the WIT value types we use
   (`string`, `list<T>`, `option<T>`, `result<T,E>`, `tuple<…>`, `record`,
   `variant`, `enum`, `stream<T>`, `future<T>`). Wire format is fully
   specified in wRPC `SPEC.md`.
2. **WebTransport orchestrator** — one `Connection` per page session; one
   `BidirectionalStream` per invocation; write the wRPC header
   (`instance`, `name`) then encoded params; read back response and indexed
   async streams.
3. **TS type bindings** — generated from `cairn.wit` via `jco types` as a
   build step. `jco` lowers `stream<T>` to `ReadableStream<T>` and
   `future<T>` to `Promise<T>` in function signatures (browser-native). Only
   type emission; jco's NodeJS-only preview3 shim is not on the dependency
   path.

Known gap: jco's bindgen panics (`todo!()`) on named type aliases wrapping
`stream<T>` or `future<T>` (`ts_bindgen.rs:238-239, 896-897`). The current
WIT uses these types inline in function signatures only; revisit if a future
schema change introduces a named-stream alias.

## Open Questions

1. **Snapshot fragmentation** — large grids with scrollback serialize to
   multi-MiB byte payloads inside `server-event::snapshot(list<u8>)`. Under
   wRPC the `list<u8>` is a single encoded value; very large payloads may
   exceed stream-buffer limits in the transport. Options: split the initial
   snapshot delivery across multiple `output` events followed by a
   `snapshot-complete` sentinel, or encode the snapshot as a
   `stream<list<u8>>` sub-channel. Affects [[terminal-state-and-replay]].
2. **`Resize` arbitration** — when a non-leader attached client sends
   `client-event::resize`, the current policy is unspecified. zmx silently
   ignores non-leader resizes (`main.zig:1009-1011`). We could ack-and-ignore,
   emit a `server-event::error`, or close the stream. Needs a v1 decision
   that affects [[client-attach-and-election]] and [[resize-semantics]].
3. **Ping / liveness under WebTransport** — QUIC has its own keepalive
   (`keep_alive_interval` in `wtransport`). Whether application-layer
   heartbeats are needed in addition (e.g. for page-visibility-driven probes
   or to distinguish daemon crash from idle) is an open question for the WT
   connection lifecycle. See [[transports]].
4. **Rate-limiting unauthenticated connections** — the daemon allocates a WT
   session between connection-open and the first `meta.authenticate` reply.
   The connection-establishment rate limiter and ~5 s first-frame deadline
   need concrete implementation decisions. See [[daemon-process-model]].
5. **Capability negotiation** — wRPC dispatches by `instance` + `function
   name` strings, so adding new operations is additive without a version bump.
   Whether `meta.version`'s `protocol` string (currently
   `"cairn:daemon@0.1.0"`) is sufficient for client compatibility checks, or
   whether an explicit capability-set exchange is needed, is unresolved.
