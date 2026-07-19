# HTTP MITM Proxy Support

> Cairn design record. This document is stored alongside the existing design
> material for discoverability; it is not a Superpowers-generated spec.

## Summary

Cairn will provide per-session HTTP and HTTPS MITM proxying for spawned
processes. Sessions opt in through `session-spec`; unmatched requests continue
to their original host, while requests matching ordered method/host/path rules
are delegated to one connected interceptor. Multiple passive watchers can
observe the same traffic without participating in routing.

The daemon uses one internal proxy backend for external wRPC interceptors,
audit watchers, and a future WASI guest-plugin adapter. This phase adds no CLI
flags or web UI.

The WIT package remains `cairn:daemon@0.1.0`. Adding the optional proxy field to
`session-spec` changes its canonical record layout, so clients must upgrade in
lockstep.

## WIT Contract

`session-spec` gains `http-proxy: option<http-proxy-spec>`. A proxy spec contains
`routes: list<http-route>`; an empty list enables observation without delegating
requests. Route entries are ORed and their fields are ANDed:

- `methods: list<string>` uses normalized uppercase methods; empty means any.
- `host: option<string>` is an exact, case-insensitive hostname without a port.
- `path-prefix: option<string>` matches the raw percent-encoded path without its
  query string.
- A route with no constraints matches every request.

The daemon exports a new `http-proxy` interface:

```wit
intercept: func(ctx: option<call-context>, id: session-id,
                actions: stream<interceptor-action>)
           -> stream<interceptor-event>;

watch: func(ctx: option<call-context>, id: session-id)
       -> stream<observation-event>;
```

HTTP methods, versions, full URIs, and status codes are explicit. Headers are
ordered `list<tuple<string, list<u8>>>` values so duplicates and non-UTF-8 values
survive. Exchange IDs correlate concurrent work.

An intercepted request carries a complete, bounded request body. The
interceptor replies with `forward`, or a sequence of `response-start`, zero or
more `response-body` actions, and `response-end`; it may also explicitly
`fail`. Synthetic response bodies are streamed with backpressure and may remain
open for SSE. A `cancelled` event tells the interceptor that the downstream
process disconnected.

Observation is lifecycle-based: initial/resynchronization snapshots plus live
request start/body/end, response start/body/end, completion, and failure
events. Session-level watcher setup failures use a separate `error` event.
Watcher lag never blocks proxy traffic; it causes a fresh snapshot.

## Backend And Lifecycle

A new `cairn-http-proxy` crate owns the protocol-independent HTTP models, route
matching, MITM listener, bounded capture/replay store, and interceptor trait.
The daemon owns the WIT adapter. A future guest-plugin adapter will implement
the same trait without changing listener or forwarding behavior.

Each proxy-enabled session binds a private `127.0.0.1:0` listener before its
child starts. The child receives uppercase and lowercase HTTP/HTTPS proxy
variables. One daemon-scoped CA signs per-host leaf certificates; its private
key stays in memory. Public CA/bundle files live in a mode-0700 per-daemon
runtime directory and are removed during shutdown.

The child also receives `NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE`,
`REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`, and `GIT_SSL_CAINFO`. The general bundle
contains native roots, Cairn's CA, and readable pre-existing configured bundles.
Proxy-owned values override inherited/session values; `NO_PROXY` is preserved.

The stable session entry owns the proxy handle. Restarts reuse the listener,
CA, routes, interceptor, and observation history. Process exit retains bounded
history; daemon shutdown cooperatively stops proxy tasks after draining
sessions.

One interceptor and multiple observers may attach to a session. Unmatched
traffic never waits for the interceptor. Early matched requests wait briefly
for the interceptor; disconnect fails current exchanges and releases the slot,
without replaying requests to a replacement.

Fail-closed responses are:

- `413` for an oversized matched request.
- `502` for an absent/disconnected/invalid interceptor or upstream failure.
- `504` when the interceptor does not choose forward or start a response in
  time.
- `503` when the active-exchange cap is exhausted.

Listener or CA initialization failures reject session creation with
`session.proxy_failed` before spawning the child.

## Resource Defaults

- Matched request body: 8 MiB maximum.
- Observation capture: 1 MiB in each direction, with total byte counts and
  truncation flags.
- Replay: 256 exchanges and 32 MiB per session, evicting the oldest completed
  exchanges first while retaining active exchanges.
- Active exchanges: 256 per session.
- Interceptor connection wait: 5 seconds.
- Forward/response-start decision wait: 30 seconds.
- Synthetic response bodies: no total or idle timeout, preserving SSE.
- Watchers receive raw authenticated headers and captured body bytes.

## Testing And Acceptance

Behavioral tests cover route matching and normalization; plaintext origin
forwarding; HTTPS interception and generated-CA trust; injected environment
variables; synthetic and forwarded responses; concurrent exchange correlation;
SSE delivery before response completion; startup queuing; duplicate
interceptors; timeout/failure statuses; cancellation; resource limits; replay,
truncation, eviction, and lag resynchronization; restart reuse; and cooperative
shutdown cleanup.

Protocol round trips and cross-language goldens cover the new WIT values. Final
verification runs `cargo nextest run --workspace`,
`cargo clippy --workspace --all-targets`, and `cargo fmt --check`.

## Exclusions

This is an explicit proxy, not transparent packet capture. Processes that
ignore proxy variables, certificate pinning, native trust-store installation,
raw TCP/UDP, DNS auditing, HTTP/3, and WebSocket frame interception are out of
scope. HTTP/1.0, HTTP/1.1, HTTP/2, and SSE are supported. Unmatched upgrades may
pass through; matched upgrades fail closed as unsupported.

The interceptor cannot modify an origin response after choosing `forward`.
Passive watchers still observe forwarded responses. The current Claude Code v2
remote-control SSE path is supported; its v1 WebSocket path requires the later
WebSocket extension.
