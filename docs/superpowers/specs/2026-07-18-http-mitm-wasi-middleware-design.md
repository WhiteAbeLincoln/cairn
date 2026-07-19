# WASI HTTP Middleware For Intercepted Traffic And Plugin APIs

> Cairn design record. This is a forward-looking extension to
> `2026-07-18-http-mitm-proxy-design.md`, not part of the initial HTTP MITM
> proxy implementation. Amend it against the implemented proxy and plugin
> runtime before implementation.

## Status And Dependencies

The first HTTP MITM proxy phase deliberately implements a per-session proxy,
one attached wRPC interceptor, and independent passive watchers. That work can
ship without a plugin runtime.

This extension is implemented only after both of these exist:

1. The HTTP MITM proxy has stable request, response, routing, streaming,
   cancellation, and observation behavior.
2. Cairn has a component-model plugin runtime capable of hosting async
   `wasi:http` components.

The semantic guest ABI targeted here is `wasi:http/middleware@0.3.0` as it
exists in the current WASI HTTP proposal. The exact version is provisional:
the implementation must use the version supported by Cairn's selected
component runtime and toolchain. Version adaptation belongs at the plugin-host
boundary, not in the proxy engine.

## Summary

Cairn will treat an HTTP plugin as a reusable middleware service rather than an
interceptor owned by one session. Ordered route bindings select a chain of
registered handlers. A handler can return its own response or invoke its
imported next handler to forward the request through the rest of the chain and
eventually to the origin.

The same plugin's exported `wasi:http/handler` is also mountable beneath the
daemon's authenticated HTTP server. A stateful plugin can therefore intercept
traffic from spawned processes and serve an API used by a Cairn UI or client.
One long-lived plugin instance may serve many sessions; Cairn supplies
invocation context out of band so the plugin can attribute each proxy request
without adding headers that might leak upstream.

Passive observation remains a separate non-blocking facility. Middleware is
on the request's critical path and may enforce policy; watchers must never
delay that path.

## Why The Standard Middleware World Fits

The WASI HTTP `service` world exports `handler` and imports `client`. The
`middleware` world includes `service` and additionally imports `handler`. The
imported handler is the next link in a request/response chain:

```text
spawned process
    -> session MITM listener
    -> route match
    -> middleware A
    -> middleware B
    -> terminal origin handler
    -> response in reverse order
```

A middleware forwards by calling its imported handler. It handles locally by
returning a response without calling that import. It may modify the request
before forwarding and may modify the returned response. Streaming request and
response resources preserve backpressure and long-lived SSE bodies without a
Cairn-specific body protocol.

The separately imported `wasi:http/client` is for new arbitrary outbound
requests, subject to the plugin's network capabilities. It is not the normal
way to continue an intercepted request: using the imported handler preserves
the selected middleware chain, attribution, cancellation, and loop detection.

Reference: <https://github.com/WebAssembly/WASI/blob/main/proposals/http/wit/worlds.wit>

## Cairn Plugin World

Cairn-aware middleware includes the standard world and imports one extension:

```wit
package cairn:http-plugin@0.1.0;

interface context {
    type session-id = string;
    type exchange-id = u64;

    record proxy-source {
        session: session-id,
        exchange: exchange-id,
        original-uri: string,
    }

    record plugin-route-source {
        mount-path: string,
        principal: option<string>,
    }

    variant request-source {
        proxy(proxy-source),
        plugin-route(plugin-route-source),
        cairn-route,
    }

    current: func() -> request-source;
}

world http-middleware-plugin {
    include wasi:http/middleware@0.3.0;
    import context;
}
```

The concrete context types must be aligned with the eventual plugin identity
and authorization model. The required behavior is stable even if the records
change:

- `proxy` identifies the source session and exchange and preserves the URI as
  accepted by the proxy before middleware mutation.
- `plugin-route` identifies the authenticated API mount and caller.
- `cairn-route` is reserved for explicitly configured middleware on Cairn's
  own routes; plugins do not receive those requests by default.

Context is immutable host task state associated with the handler invocation.
It is never represented as an HTTP header. Calls to the imported next handler
remain on the same component-model task and retain the same context, following
WASI HTTP's request-attribution model.

A component targeting unextended `wasi:http/middleware` remains usable as
generic middleware. It simply cannot branch on Cairn session or API-mount
metadata.

## Handler Registry And Route Bindings

The plugin phase introduces a daemon-owned handler registry:

```text
handler id -> implementation
              - WASI plugin instance
              - wRPC interceptor adapter
              - native Cairn middleware

session route -> ordered list of handler ids
API mount     -> ordered list of handler ids
```

Route matching still uses the method, exact host, and path-prefix semantics
from the base MITM design. Matching occurs once when a request enters the
proxy. Rewriting a URI inside middleware does not restart route selection or
recursively select the same chain.

The final handler for an intercepted request is the origin forwarder. It sends
the request resulting from all middleware mutations, so a plugin can redirect
to another upstream by changing the URI before it invokes `next`. The immutable
context retains the original URI for attribution and auditing.

The final handler for a plugin API mount is a 404 response. Forwarding from one
API middleware therefore reaches the next configured middleware, not the
original authority from the browser request. Cairn-owned routes use the Cairn
router as their explicit terminal handler.

The current per-session wRPC interceptor is initially represented by an
implicit session-local handler. After the registry exists, a named wRPC
handler may register once and be bound to several sessions. Its adapter maps
the standard streaming handler call onto the correlated event/action protocol
implemented by the base proxy. The proxy engine must not contain separate
routing implementations for wRPC and WASI.

The correlated event/action protocol is interim. It is a bespoke,
client-initiated shape — the client opens the stream and the daemon pushes
requests as events while the client replies with actions — and it cannot carry
HTTP trailers (see the base proxy design's Exclusions). The preferred end state
is a single handler contract shared by every implementation: the hosted WASI
guest uses `wasi:http` natively; a remote wRPC client (a remote CLI, or any
process acting as a live interceptor without shipping a component) uses a
value-typed projection of the same request/response-with-trailers semantics —
the `wrpc:http` pattern, i.e. `stream<u8>` bodies and a trailers `future` in
place of wasi:http's host-owned resources, never raw resources over the wire;
and native middleware calls the abstraction directly. Converging on one contract
also closes the wRPC interceptor's trailer gap. Its feasibility hinges on
whether the wRPC transport supports the daemon invoking a handler *exported* by
a connected client (reverse invocation); if it does not, the fallback is a
client-initiated stream whose framing is reshaped to mirror the handler
request/response (including trailers) rather than the current action set. This
handler path is only for on-critical-path interception: the web UI and a remote
CLI acting as passive observers use the observation facility instead, and in the
plugin model most browser interaction is with a plugin's authenticated API mount
rather than a raw interceptor.

The public WIT shape for naming handlers and binding chains is intentionally
deferred until the implemented session spec and plugin registry can be
examined together. It must preserve these behavioral requirements:

- The complete handler chain is resolvable before the child process starts.
- A missing required handler rejects session creation rather than creating a
  process whose first matching request races plugin startup.
- Multiple session route sets may reference the same handler.
- Bindings are explicit grants; installing a plugin does not let it inspect
  every session.
- A binding records its failure policy. Policy middleware defaults to fail
  closed; an explicitly observational middleware may be configured to bypass
  on failure.

## Serving A Plugin API

A plugin manifest may declare authenticated HTTP mounts below its namespace,
for example `/plugins/claude/`. The daemon invokes the same exported handler
used for intercepted traffic, but supplies `plugin-route` context and the API
mount's terminal chain.

The host authenticates and authorizes the request before invoking the guest.
The plugin receives a normalized principal in context; it does not implement
Cairn transport authentication. Same-origin mounts on Cairn's HTTP server let
the web application call plugin APIs without a separate listener or CORS
configuration.

Whether the guest sees the full mounted path or a mount-relative path must be
chosen when the plugin router exists and then applied consistently. The host
must expose the original mount in context either way.

If the daemon has no browser-reachable HTTP listener, installing a plugin does
not implicitly open one. Proxy interception continues to work, while its API
mount is reachable only through explicitly configured Cairn HTTP transports.

## Instance Lifecycle And Shared State

WASI HTTP permits a host to reuse an instance zero or more times and to invoke
one instance concurrently. Cairn defines a stronger lifecycle for stateful
plugins:

- The default scope is one long-lived instance per installed plugin
  configuration, shared by every bound session and API mount.
- A plugin may request per-session scope for isolation. API requests targeting
  such a plugin must identify the session so the host can select its instance.
- Cairn may invoke a shared instance concurrently. A plugin can apply
  component-model backpressure, but a configuration incapable of at least two
  concurrent calls cannot support a long-lived SSE response plus its control
  API and must fail installation or health validation.
- Cairn does not silently use an instance pool for a stateful plugin. Pooling
  would split guest memory and break cross-request coordination unless the
  plugin stores all shared state in host-provided services.
- On a trap, affected HTTP exchanges fail according to their route policy and
  open streams close. The host may instantiate a replacement, but guest memory
  is not implicitly recovered.
- Hot replacement sends new calls to the replacement while existing streaming
  calls drain on the old instance up to a configured deadline.

This lifecycle is Cairn policy layered on the standard WASI HTTP ABI. A generic
middleware that assumes no instance persistence remains valid; a Cairn plugin
that intentionally shares live state relies on the documented stronger host
guarantee.

## Claude Remote-Control Plugin

A Claude plugin demonstrates why interception and API serving must share one
handler instance. Its instance owns a map keyed by Cairn session ID containing
bridge environments, work queues, session tokens, SSE subscribers, and UI
clients.

For each Claude process:

1. Session routes select the plugin only for the bridge registration,
   session, and worker paths. OAuth, Messages API inference, feature flags not
   intentionally overridden, and telemetry continue directly to Anthropic.
2. The proxy invokes the plugin with `proxy` context. The plugin returns local
   registration and bridge responses or opens a streaming SSE response.
3. The plugin may call its imported next handler for paths or conditions that
   should still reach the original service.
4. Browser requests under `/plugins/claude/` invoke the same instance with
   `plugin-route` context. They enqueue user work, answer permission prompts,
   and subscribe to events stored under the selected Cairn session ID.
5. The plugin can return an API base URL pointing at its Cairn mount, or keep
   subsequent data-plane calls on the intercepted authority. Both paths reach
   the same stateful service.

Several Claude processes can bind the same installed handler. Exchange IDs
only correlate HTTP calls; the Cairn session ID is the tenant boundary. Any
vendor protocol IDs supplied by Claude remain plugin data and are not used as
Cairn authorization identities.

WebSocket interception remains a separate extension. The current Claude v2
SSE transport is sufficient to validate this architecture first.

## Observation Is Not Middleware

The base proxy's capture and replay store remains outside the middleware
chain. It records the request and response actually seen at the network
boundaries, including requests handled entirely by a plugin.

- Passive watchers consume bounded snapshots and live events without exerting
  backpressure on proxy traffic.
- A logging plugin should use the observation API unless it needs exact
  unbounded body streams.
- A plugin that blocks, rewrites, redirects, or requires complete streaming
  bodies is policy middleware and accepts critical-path backpressure.
- Future pre/post-middleware capture points may be added, but the default audit
  record is the client-visible request and response plus immutable original
  destination metadata.

## Security And Capability Boundaries

- A plugin receives intercepted traffic only from explicit session bindings
  and receives API traffic only from declared mounts.
- Cairn authorization runs before plugin API dispatch. Session-scoped API
  operations must additionally check that the principal may access that
  session.
- The imported next handler is a continuation capability for the selected
  chain, not general network access.
- The imported `wasi:http/client` is granted separately and may have an origin
  allowlist. Requests made through it do not re-enter the current session's
  middleware chain unless a future configuration explicitly asks for that.
- Middleware context never becomes a forwarded header. The proxy strips any
  externally supplied headers using Cairn's reserved namespace before a future
  header-based compatibility adapter can be considered.
- A per-request chain cursor and hop limit detect recursive host wiring. A
  plugin cannot select itself again by rewriting the request URI.
- Interceptors necessarily receive raw credentials and bodies for matched
  traffic. Plugin installation and route binding are privileged operations and
  must be visible in Cairn's audit log.

## Failure, Cancellation, And Backpressure

Downstream disconnect propagates cancellation through the middleware task,
the guest response body, and any pending next-handler call. Host adapters must
not replay a cancelled request into a replacement instance.

A handler that returns before producing the response required by the selected
WASI HTTP version is treated as a handler failure. Traps, malformed responses,
and next-handler protocol failures use the binding's fail-closed or explicit
bypass policy. Bypass invokes the remaining chain; it never restarts matching.

Body and concurrency limits are enforced at the proxy boundary before guest
allocation where possible. Guest backpressure may reduce per-instance
concurrency, but it cannot consume unbounded daemon tasks or queue memory.
Long-lived SSE responses have no ordinary body-completion timeout; installation
health, response-head deadlines, disconnect cancellation, and shutdown drain
deadlines still apply.

## Reconciliation Before Implementation

When the base proxy and plugin system are available, amend this record before
writing the adapter:

1. Replace conceptual handler IDs, route bindings, and context fields with the
   actual registry, session-spec, identity, and authorization types.
2. Decide whether the existing `http-route` WIT record can evolve or whether a
   new binding interface/version is required. Do not silently change its wire
   layout.
3. Pin the WASI HTTP version supported end to end by Wasmtime, component
   tooling, and chosen guest SDKs; document any 0.2/0.3 adapter limitations.
4. Verify that the runtime supports concurrent calls and streaming bodies on a
   reused instance. If it does not, require host-provided shared state rather
   than pretending an instance pool is a singleton.
5. Map proxy cancellation and Hudsucker body errors to the selected WASI HTTP
   error codes and confirm that origin forwarding preserves trailers and HTTP/2
   semantics.
6. Re-run the Claude remote-control proof of concept against the then-current
   CLI and record its exact REST/SSE routes and API-base behavior.
7. Define manifest syntax, mount-path normalization, handler health checks,
   reload drain deadlines, and capability grants in the plugin-system spec.
8. Decide whether the wRPC interceptor keeps its correlated event/action wire
   protocol or is re-expressed as a handler-shaped interface (a `wrpc:http`-style
   value projection of request/response with streaming bodies and trailers) so
   remote wRPC clients and the WASI guest share one handler contract. Confirm
   whether the wRPC transport supports the daemon invoking a client-exported
   handler (reverse invocation); if not, choose the client-initiated fallback
   framing. This decision also determines whether the wRPC path gains HTTP
   trailer support (see item 5).

## Testing And Acceptance

Behavioral tests must cover:

- A generic WASI middleware forwarding through its imported next handler.
- Request mutation before origin forwarding and response mutation on return.
- A terminal synthetic response whose body streams before completion.
- One plugin instance handling an intercepted SSE request and concurrent API
  calls while sharing state.
- Several sessions routed to the same plugin with strict session attribution.
- Per-session instances selected correctly by session-scoped API requests.
- Ordered multi-plugin chains, bypass policy, fail-closed policy, traps,
  response-head timeout, downstream cancellation, and hot-reload draining.
- Plugin API authentication and authorization before guest invocation.
- `wasi:http/client` egress restrictions and non-recursive next-handler wiring.
- Observation of plugin-generated and origin-generated responses without a
  slow watcher delaying either path.
- End-to-end Claude v2 bridge registration, work delivery, SSE event streaming,
  and UI-originated control traffic through a Cairn-mounted plugin API.

Tests assert network-visible responses, state isolation, cancellation, and
authorization decisions. Compile-only assertions about generated bindings or
world shape are not acceptance tests.

## Out Of Scope

- Implementing the plugin runtime as part of the initial MITM proxy feature.
- Treating WASI middleware as a passive, loss-tolerant audit subscription.
- Automatically granting an installed plugin access to existing sessions.
- Opening an unauthenticated HTTP listener for plugin APIs.
- Durable plugin state, state migration, or recovery of guest memory after a
  trap; those belong to the plugin-system persistence design.
- WebSocket frame handling, HTTP/3 interception, transparent packet capture,
  and non-HTTP protocols.
- Free-form dynamic chain rewrites by guest code. Route and chain ownership
  remains with the Cairn host.
