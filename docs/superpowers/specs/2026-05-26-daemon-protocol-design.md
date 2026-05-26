# Daemon protocol & transports

## Status

Design. Decides the wire protocol, transports, and schema source-of-truth for the cairn daemon binary. Supersedes the speculative protocol/transport design recorded in `docs/architecture/pty-session/external-protocol.md` and `docs/architecture/pty-session/transports.md` (see [Updates to pty-session docs](#updates-to-pty-session-docs)).

## Summary

The cairn daemon exposes its full surface as a set of WIT interfaces served via [wRPC](https://github.com/bytecodealliance/wrpc) over two transports:

- **Unix Domain Socket** for local CLI clients. Peer-credential auth.
- **WebTransport** (HTTP/3 over QUIC) for remote clients — browser and CLI alike. Bearer-token auth.

A single WIT schema is the source of truth for: the daemon's server-side implementation (via `wit-bindgen-wrpc-rust`), the in-process WASM plugin API (via `wit-bindgen-rust`, host functions resolved by wasmtime), and the browser TypeScript client (via a TS canonical-ABI codec cairn builds — wRPC has no TS bindings today).

## Why wRPC

The brainstorming session weighed five candidates:

1. **Split HTTP/REST + WebSocket.** Two wire formats, two error envelopes, two dispatchers, two browser codepaths. Half the CLI commands stream regardless, weakening REST's natural fit.
2. **Hand-rolled msgpack RPC over WS + UDS.** Single wire format but hand-defined framing, hand-defined schema, no plugin alignment.
3. **gRPC / Connect-RPC.** Schema-first but browser support for client→server and bidirectional streaming via Connect-Web is limited — bidi `attach` ends up needing a separate WebSocket anyway. Re-introduces the split.
4. **WIT + hand-rolled framing.** Cleaner than (2) thanks to schema-as-source-of-truth, but we hand-write the wire encoding for WIT values.
5. **wRPC.** WIT-native by design, schema is the contract, plugin API drops out for free, native `stream<T>` / `future<T>` support, transport-agnostic with first-class UDS, TCP, and WebTransport. Maintained by the Bytecode Alliance.

(5) wins on the merits: it eliminates schema duplication between RPC and plugins, gives us a maintained crate for the wire (`wit-bindgen-wrpc-rust`), and aligns with the WASM-component direction described in `DESIGN.md`.

Trade-offs:

- **No upstream WebSocket transport.** WebTransport is the browser carrier. Safari 26.4 ships stable WT, satisfying our last browser holdout.
- **No upstream TypeScript bindings.** We build the TS client ourselves: a canonical-ABI codec plus a small WebTransport orchestrator. Bounded work — the wire format is fully specified in wRPC's `SPEC.md`.

## Transports

### Unix Domain Socket (local CLI)

- **Path**: `$XDG_RUNTIME_DIR/cairn/cairn.sock` on Linux, `$TMPDIR/cairn.sock` on macOS. Socket mode `0o600`, parent dir mode `0o700`.
- **Carrier**: wRPC's UDS transport. Per wRPC's `SPEC.md`, the framed-stream spec applies — one connection per invocation. On UDS this is microseconds, negligible.
- **Auth**: filesystem DAC (the user only sees their own socket) plus `SO_PEERCRED` (Linux) / `getpeereid` (macOS) to record the invoking uid for audit. No bearer token is consulted, matching the contract documented at `crates/cairn-client/src/cli.rs:14-26`.
- **Connection lifecycle**: each CLI process opens a fresh UDS connection per invocation; long-running operations (`attach`) hold the connection until the operation ends.

### WebTransport (remote browser and CLI)

- **Listener**: HTTP/3 endpoint on `127.0.0.1:<port>` by default; configurable to non-loopback for remote browser/mobile/CLI access.
- **Carrier**: wRPC's `transport-web` crate (WebTransport over QUIC via `wtransport`). The browser holds one `Connection` per page-session; the CLI gets one `Connection` per process invocation. Each operation opens a bidirectional stream via `Connection::open_bi` (`crates/transport-web/src/lib.rs:91-101`). No per-invocation handshake — stream open is a single QUIC SETTINGS frame.
- **Auth**: bearer token, sent as the first invocation on the connection (see [Authentication](#authentication)).
- **Connection lifecycle**:
  - Browser: long-lived, one per tab. Reconnect-on-disconnect is application logic in the TS client.
  - Remote CLI: opened per process invocation. Each `cairn <cmd>` pays one QUIC+TLS handshake then runs one or more operations on streams within that connection. A future `cairn-agent` could pool connections across processes to amortize the handshake; out of scope for v0.

A separate plain TCP/TLS transport is not in v0. WebTransport is the single remote carrier. If WT proves unworkable in some deployment (UDP blocked, HTTP/3 disabled), adding wRPC's TCP transport later is additive — no schema change required.

## WIT interface surface

The schema lives in `crates/cairn-protocol/wit/cairn.wit` (new crate). Sketched (final shape iterated during implementation):

```wit
package cairn:daemon@0.1.0;

interface types {
    type session-id = string;     // UUIDv7
    type client-id = string;

    record session-spec {
        name: option<string>,
        command: list<string>,
        env: list<tuple<string, string>>,
        env-inherit: bool,
        workdir: option<string>,
        tty: bool,
        stdin: bool,
        idle-timeout-secs: option<u64>,
        scrollback-lines: u32,
    }

    record session-info {
        id: session-id,
        name: option<string>,
        pid: option<u32>,
        cols: u16,
        rows: u16,
        attached-clients: list<client-id>,
        created-at-unix-ms: u64,
        exit: option<exit-status>,
        spec: session-spec,
    }

    record exit-status {
        code: option<s32>,
        signal: option<u8>,
        unix-ms: u64,
    }

    variant signal {
        named(signal-name),
        numbered(u8),
    }

    enum signal-name {
        hup, int, quit, ill, trap, abrt, bus, fpe, kill,
        usr1, segv, usr2, pipe, alrm, term, chld, cont,
        stop, tstp, ttin, ttou, urg, xcpu, xfsz, vtalrm,
        prof, winch, io, sys,
    }

    variant log-window {
        tail(u32),
        since-unix-ms(u64),
        all,
    }

    record attach-init {
        cols: u16,
        rows: u16,
        no-stdin: bool,
    }

    variant client-event {
        input(list<u8>),
        resize(tuple<u16, u16>),
        detach,
    }

    variant server-event {
        snapshot(list<u8>),
        output(list<u8>),
        exited(exit-status),
        error(error),
    }

    record error {
        code: string,
        message: string,
    }
}

interface sessions {
    use types.{
        session-id, client-id, session-spec, session-info, signal,
        log-window, attach-init, client-event, server-event,
        exit-status, error,
    };

    list-all: func() -> list<session-info>;
    inspect:  func(id: session-id) -> result<session-info, error>;

    create:  func(spec: session-spec) -> result<session-info, error>;
    rename:  func(id: session-id, new-name: string) -> result<_, error>;
    restart: func(id: session-id, force: bool) -> result<_, error>;
    kill:    func(id: session-id, signal: signal) -> result<_, error>;
    kick:    func(id: session-id, client: option<client-id>) -> result<_, error>;

    /// Resolves when the session exits.
    wait:    func(id: session-id) -> future<exit-status>;

    /// Server-streaming. `follow` controls whether the stream ends once
    /// the buffered window is delivered or continues until exit.
    logs:    func(id: session-id, window: log-window, follow: bool)
             -> stream<list<u8>>;

    /// Bidirectional. The client-event stream and server-event stream
    /// are independently ordered.
    attach:  func(id: session-id, init: attach-init,
                  events: stream<client-event>) -> stream<server-event>;

    /// Client-streaming. One chunk per stream element. Empty stream is a no-op.
    send:    func(id: session-id, chunks: stream<list<u8>>)
             -> result<_, error>;
}

interface meta {
    record version-info {
        daemon: string,
        protocol: string,    // "cairn:daemon@0.1.0"
    }

    /// First invocation on every authenticated connection. Calls to any
    /// other interface before this succeeds are rejected.
    authenticate: func(token: string) -> result<_, error>;

    whoami:  func() -> result<string, error>;
    version: func() -> version-info;
}

world daemon {
    export sessions;
    export meta;
}
```

Notes:

- **No WIT `resource` types on the wire.** Sessions are addressed by UUIDv7 string IDs, resolved server-side. Resources don't survive network boundaries cleanly and would constrain the multi-daemon migration path (see `daemon-process-model.md`).
- **Session IDs are always server-assigned.** UUIDv7 gives free chronological sort. The `name` field is the user-facing correlation key for tying a session to an external system (Jira ticket, database row, etc.). Idempotent create-under-retry is intentionally not in v0; if it becomes a real need, it gets a separate `idempotency-key` field with explicit semantics (window, conflict behavior) rather than being folded into the resource id.
- **`attach` is genuinely bidi.** Two independent streams. wRPC's indexing handles them without extra ceremony.
- **`session-spec` mirrors `cli.rs` field-for-field** where reasonable; the CLI argument parser is the spec for which fields exist.

### Mapping CLI commands to WIT operations

| CLI command                                | Operation                                   | Shape                                |
|--------------------------------------------|---------------------------------------------|--------------------------------------|
| `cairn list`                               | `sessions.list-all`                         | unary                                |
| `cairn inspect`                            | `sessions.inspect`                          | unary                                |
| `cairn exec` / `run` with `--detach`       | `sessions.create`                           | unary                                |
| `cairn exec` / `run` attached              | `sessions.create` + `sessions.attach`       | unary then bidi                      |
| `cairn attach`                             | `sessions.attach`                           | bidi                                 |
| `cairn send <input>` / `cairn send` stdin  | `sessions.send`                             | client-streaming (1-element or N)    |
| `cairn kill`                               | `sessions.kill` + optional `sessions.wait`  | unary (+ future if waited)           |
| `cairn rename`                             | `sessions.rename`                           | unary                                |
| `cairn restart`                            | `sessions.restart`                          | unary                                |
| `cairn kick`                               | `sessions.kick`                             | unary                                |
| `cairn logs`                               | `sessions.logs`                             | server-streaming                     |
| `cairn wait`                               | `sessions.wait`                             | future                               |
| `cairn whoami`                             | `meta.whoami`                               | unary                                |
| `cairn version`                            | `meta.version`                              | unary                                |
| `cairn completion`                         | (local-only, no daemon round-trip)          | n/a                                  |

Glob and `--all` expansion (`SessionTargets` in `cli.rs`) is resolved client-side via `sessions.list-all` plus per-target unary calls. Keeps the WIT surface narrower at the cost of one extra round-trip per bulk command. See [Open questions](#open-questions).

## Plugin API

In-process WASM plugins import the same `sessions` and `meta` interfaces as host functions. Wasmtime resolves these calls directly to the daemon's session registry via `wit-bindgen-rust`-generated host trait impls. RPC-back-to-the-host-process is wasteful and avoided.

The daemon's plugin runtime and its wRPC server share a common server-side trait implementation, so adding a new operation means: define it in WIT once, implement it in one Rust trait impl, and both call paths get it.

Out-of-process or remote plugins (not in scope for v0) would use wRPC directly with the same interfaces; nothing changes at the schema level.

## TypeScript client

The browser is a peer with the CLI and exercises the full surface. To make this usable we need a TS library that ships from this repo:

1. **Canonical-ABI codec.** Encode/decode for the subset of WIT value types we use: primitives, `string`, `list<T>`, `option<T>`, `result<T, E>`, `tuple<…>`, `record`, `variant`, `enum`, `stream<T>`, `future<T>`. Reference: wRPC `SPEC.md` and `crates/transport-web/src/lib.rs`; the existing demo at `examples/web/ui/index.html` shows the byte-level shape for one interface. Scope estimate: a few hundred lines plus thorough tests against fixtures generated by the Rust binding.
2. **WebTransport orchestrator.** Manage one `Connection` per page session; open a `BidirectionalStream` per invocation; write the wRPC header (`instance`, `name`) then encoded params; read back the response buffer (and any indexed async streams).
3. **TS type bindings.** Generated from the WIT files via [`jco types`](https://github.com/bytecodealliance/jco) as a build step. jco's TS bindgen lowers `stream<T>` to `ReadableStream<T>` and `future<T>` to `Promise<T>` in function signatures (`crates/js-component-bindgen/src/ts_bindgen.rs:960-970`) — both browser-native. We use jco for type emission only; the runtime (canonical-ABI codec + WebTransport orchestrator above) is ours, so jco's NodeJS-only `preview3-shim` is not on the dependency path.

   Known gap: jco's bindgen panics (`todo!()`) on named type aliases that wrap `stream<T>` or `future<T>` (`ts_bindgen.rs:238-239, 896-897`). Our WIT uses these types inline in function signatures only; revisit if a future schema change introduces a named-stream alias.
4. **Application-layer plumbing.** Reconnect-on-disconnect, page-visibility-driven probes, auth token refresh. These are application concerns sitting above the wRPC layer.

The TS client lives in a sibling crate/directory (final name TBD; likely `web/` or `crates/cairn-web/`). Upstream contribution back to wRPC of either the codec or the TS bindings is desirable but not blocking for cairn v0.

## Authentication

- **UDS**: `SO_PEERCRED` / `getpeereid` plus filesystem permissions (socket `0o600`, dir `0o700`). No application-layer token. Matches the `cli.rs` doc comment.
- **WebTransport**: bearer token loaded from `$XDG_RUNTIME_DIR/cairn/token` (mode `0o600`), regenerated on each daemon start.
  Placement: **first invocation on each connection** — a `meta.authenticate(token)` call that the daemon requires before serving any other interface. Failure closes the underlying connection.

  This is not a preference — it's forced by the browser API. The `WebTransport` constructor in JS only accepts `allowPooling`, `congestionControl`, `requireUnreliable`, and `serverCertificateHashes` (MDN, W3C `webtransport` spec). There is no path to set an `Authorization` header from the browser, exactly as with `new WebSocket(url)`. URL-embedded tokens leak to access logs, browser history, and `Referer`-like fields; cookies would require same-origin setup we don't want for v0. First-message authentication is the only mechanism that works for the browser, and the CLI uses the same path for uniformity even though `wtransport`'s Rust client could in principle set headers.

  Cost accepted: the daemon allocates session-level resources for an unauthenticated peer between connection-open and the first `meta.authenticate` reply. Mitigated by aggressive rate-limiting on unauthenticated connections (see [Open questions](#open-questions)) and a short deadline (~5s) for the first frame.
- **Browser token issuance**: out of scope for v0. v0 expects the user to paste the token into the local web UI on first connect. A discovery+exchange flow (OAuth-style local code) is future work tracked in `authentication.md`.

## Streaming semantics

- **Unary**: params and return per WIT signature. No further handling.
- **Server-streaming** (`logs`): WIT return is `stream<T>`. wRPC's indexing carries the stream over an async sub-channel; client awaits stream close.
- **Future-returning** (`wait`): WIT return is `future<T>`. Same indexing mechanism as a stream of one; the client resolves a single value asynchronously.
- **Client-streaming** (`send`): WIT param is `stream<T>`. Client writes chunks until end-of-stream; server returns one result.
- **Bidirectional** (`attach`): a `stream<T>` param and a `stream<U>` return, independently scheduled. Client→server `client-event` ordering is preserved within its own stream.

PTY output is **not** coalesced on the server: one PTY read → one `server-event::output` element. The wRPC indexing carries this directly. Backpressure policy stays on the daemon side as described in `backpressure.md` (broadcast channel + lag → close).

## Out of scope for v0

- Connection-pooling agent (`cairn-agent`) for amortizing QUIC+TLS handshake on remote CLI.
- Standalone TCP/TLS transport (additive later if WebTransport proves unworkable in some deployment).
- Remote / out-of-process WASM plugin transport.
- mTLS / client-certificate auth.
- Delta-replay on attach (every reattach is a fresh `Snapshot` via `server-event`).
- OAuth-style browser auth issuance.
- Cross-user attach (per-user daemons only).

## Open questions

1. **Rate-limiting unauthenticated connections.** First-message auth means the daemon accepts a WT connection and allocates session state before knowing whether the peer has a valid token. Need a connection-establishment rate limiter and a deadline (~5s) for the first frame, after which the connection is closed. Specifics TBD.
2. **Bulk operations.** Glob/`--all` resolution is client-side in this design (`list` then per-target call). Cleaner WIT surface but extra round-trip; revisit if measured CLI latency makes it painful.
3. **`restart` and session id continuity.** Per `cli.rs`, restart keeps the id. WIT shape supports this; confirm the implementation contract.
4. **`logs` window without follow.** Should the stream close on EOF of the buffered window, or do we need a separate "snapshot-then-close" return shape? Current draft uses `follow: bool` on the param; sufficient but verify.
5. **Capability negotiation.** wRPC dispatches by `instance` + `function name` strings, so adding new functions is additive. Beyond that, do we need a version-handshake step? Probably not — `meta.version` is enough — but flag for confirmation.

## Updates to pty-session docs

Several `docs/architecture/pty-session/` documents pre-date this decision or were investigative notes that no longer reflect the chosen direction. Apply after this spec is approved so the docs tell a consistent story rather than a half-converted one.

1. **`external-protocol.md`** — Major rewrite. Drop the msgpack-over-WebSocket framing entirely. Replace "Cairn's design over WebSocket" with: wRPC + WIT, the schema sketched here, the wire format as defined in wRPC's `SPEC.md`. Keep the zmx baseline section as historical context only.
2. **`transports.md`** — Major rewrite. WebSocket is **not** the v0 primary; WebTransport is, for browsers. UDS is primary for local CLI; TCP/TLS for remote CLI. Reorder the "Transport options" table. The "WebTransport: consider for later" section flips — WebTransport is now v0.
3. **`web-vs-cli-clients.md`** — Update CLI bootstrap (UDS connection, no termios-affecting WS upgrade) and browser connection (WT, not WS). Remove or rewrite WS-specific UA-keyboard/backgrounding sections that apply differently to WT.
4. **`daemon-process-model.md`** — Update the "Listener" subsection (current lines 126-137): the daemon binds two listeners (UDS, WT), not the previously sketched HTTP+WS + UDS pair. Remove "HTTP control plane / WS upgrade on `/sessions/{id}/attach`" — all paths are wRPC invocations.
5. **`authentication.md`** — Update token-frame placement per [Authentication](#authentication). Drop CSWSH / Origin / WS subprotocol material (irrelevant under WT). Add SO_PEERCRED specifics for UDS.
6. **`client-attach-and-election.md`** — Re-anchor the `Attach` flow on the bidi WIT `attach` operation. Leader election mechanics are unaffected at the application layer; references to message tags (`Init`, `Output`, etc.) become WIT `client-event` / `server-event` variants.
7. **`terminal-state-and-replay.md`** — Replace references to the `Snapshot` message tag with `server-event::snapshot`. The serialization strategy itself is unchanged.
8. **`backpressure.md`** — Re-anchor on wRPC's stream-credit / flow-control. The "broadcast channel + lag → close" pattern on the daemon side is unchanged.
9. **`pty-session/README.md`** — Replace item 4 of "What needs to be built" with this spec. Update "Deliberate divergences" #1 — we still diverge from zmx on transport, but the new shape is wRPC + WT/UDS/TCP, not WebSocket.
10. **`observability.md`**, **`testing.md`** — Add wRPC-specific notes (instance/function names appear on trace spans; tests can use `wrpc-test` for in-process round-trips).

These edits are documentation hygiene, not blockers for daemon implementation. They can ride alongside the implementation plan.
