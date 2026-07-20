# Wasm Plugin Runtime And Reference Plugins

## Status

Approved design for Cairn's initial WebAssembly component plugin runtime. The
exact WASI interface versions remain pinned by the implementation plan against
the selected Wasmtime release and guest toolchains.

This design generalizes the handler registry proposed in
`2026-07-18-http-mitm-wasi-middleware-design.md`: HTTP middleware can wrap any
Cairn-hosted HTTP handler, not only intercepted session traffic.

## Summary

Cairn will host WebAssembly components that expose standard WASI entry points or
Cairn-specific entry points. Initial invocation forms are remote CLI commands,
hosted HTTP services, HTTP middleware, ordered session-spec transformations,
and session-exit handlers.

A component's artifact identity, configured installation identity, typed WIT
contracts, and user-facing runtime namespaces are separate. Consequently,
competing Pi and Codex components can both implement the same generic LLM
contract without either owning the name `llm`. Explicit bindings choose which
installation provides a typed import, CLI alias, HTTP route, transformation
alias, or event handler.

The first reference plugins are deliberately small but useful. Together they
exercise standard WASI hosting, Cairn API imports, remote invocation, lifecycle
events, capability enforcement, session spawning, proxying, service mounting,
and ordered composition.

## Goals

- Host components exporting standard `wasi:cli/command`,
  `wasi:http/service`, and `wasi:http/middleware` worlds.
- Host Cairn-aware components that import existing session and proxy
  functionality or export Cairn-specific entry points.
- Invoke daemon-installed CLI components remotely through
  `cairn plugin run <installation>`.
- Mount plugin HTTP services beneath explicitly configured path namespaces.
- Apply one HTTP middleware model to plugin services, intercepted session
  traffic, and explicitly opted-in Cairn routes.
- Compose session-spec transformations in caller-defined launch plans.
- Invoke plugin handlers after matching sessions exit.
- Allow multiple implementations of a generic WIT contract and bind consumers
  to providers explicitly.
- Make capabilities, invocation selection, failure behavior, and composition
  order visible and testable.

## Non-Goals

- LLM harnesses, Nix environments, operating-system sandboxing, ticket
  automation, and secret injection in the first implementation. They inform the
  interfaces but build on the initial runtime.
- Automatically inferring the semantic order of session transformers.
- Automatically selecting among competing providers of a WIT contract.
- Durable event delivery, timers, output-pattern triggers, or workflow
  persistence.
- Friendly top-level commands such as `cairn doctor`. The first CLI surface is
  `cairn plugin run`; aliases and manifest-suggested namespaces follow later.
- A central plugin marketplace, artifact distribution protocol, or trust/signing
  system.
- Implicit middleware access to every session or Cairn-owned HTTP route.

## Invocation Model

How Cairn invokes a plugin is independent from which capabilities the plugin
imports.

### CLI Command

A CLI plugin exports `wasi:cli/command`. The initial invocation syntax is:

```text
cairn plugin run <installation> -- <arguments...>
```

If an installation exposes more than one command, callers select one with
`--export <name>`; omission is valid only when exactly one command is available.
Cairn maps arguments, permitted environment values, streamed stdin, independent
stdout and stderr streams, and the final exit status between the remote client
and daemon-hosted component.

A later phase may bind friendly top-level CLI namespaces to installation
exports. Such aliases are routing configuration rather than plugin or
installation names. Users can resolve alias conflicts without renaming an
artifact or changing its generic typed contracts. Explicit `plugin run`
invocation remains available.

### HTTP Service

An HTTP plugin exports `wasi:http/service`. A binding mounts that export beneath
an explicit path namespace on an existing Cairn HTTP listener. Cairn authenticates
and authorizes the caller before invoking the component. Installing a plugin
does not open a listener or claim its manifest's suggested path automatically.

### HTTP Middleware

An HTTP middleware plugin exports `wasi:http/middleware`. Middleware can modify,
reject, synthesize, or forward a request. It can wrap any handler registered in
Cairn's unified HTTP registry:

- A plugin's HTTP service
- The origin forwarder for intercepted session traffic
- A Cairn-owned route that explicitly opts into plugin middleware
- A fallback handler

This permits one plugin to extend another at the HTTP request/response layer.
For domain-level extension, the extending plugin should instead import the
other plugin's typed WIT contract.

### Session Transformer

A session transformer exports a Cairn-specific interface. It receives a complete
session specification and returns a transformed specification or a structured
error. Environment defaults, Nix command wrapping, sandbox wrapping, and secret
configuration can share this entry point.

Cairn executes transformers as a host-driven sequence, but it does not infer
their order. The caller or configuration supplies an explicit launch plan. Cairn
records each stage's changes, validates constraints and final invariants, and
only then spawns the process.

### Event Handler

The first lifecycle interface invokes matching handlers after a session exits.
The final status and logs are available before invocation. Handlers execute
asynchronously with bounded concurrency. Their failures are observable but
cannot change the recorded session result.

Future lifecycle entry points may include session creation, attach/detach,
output matches, idle events, and timers.

## Identity And Namespace Model

Cairn keeps four concepts separate:

1. **Component identity:** immutable artifact/package identity and version, for
   example `example/pi-agent@0.1.0`.
2. **Installation identity:** a unique local configured instance, for example
   `personal-pi`.
3. **Typed contract binding:** selection of the provider for an imported WIT
   interface such as `cairn:llm/agent`.
4. **Runtime binding:** a user-facing CLI alias, HTTP path, transformation name,
   event subscription, or middleware route binding.

For example:

```text
CLI alias "llm"     -> installation "personal-pi" -> command export
HTTP path "/llm/"  -> installation "personal-pi" -> HTTP service export
WIT import "llm"   -> installation "personal-pi" -> cairn:llm/agent
```

Pi and Codex installations may coexist and provide the same generic interface.
No provider wins because of package name, installation order, or a marketplace
namespace.

## Host Architecture

The daemon owns five distinct registries:

1. **Artifacts:** installed component binaries and immutable metadata.
2. **Installations:** component configuration, capability grants, and lifecycle
   policy.
3. **Exports:** callable entry points provided by each installation.
4. **Bindings:** CLI aliases, HTTP routes and chains, transformation aliases and
   launch profiles, event subscriptions, and typed import providers.
5. **Instances:** active Wasmtime component instances, separate from configured
   installation identity.

Bindings are resolved and validated before invocation. Missing exports,
incompatible interface versions, route conflicts, unsatisfied imports, and
ambiguous aliases are configuration errors rather than deferred guest failures.

The implementation plan must specify instance reuse and concurrency per entry
point using the selected WASI and Wasmtime versions. In particular, HTTP
middleware and services may require shared long-lived state, while concurrent
CLI invocations need independently attributed arguments and stdio. Cairn must
not claim shared in-memory state where the runtime requires separate instances;
host-provided persistent state is the portable fallback.

## Explicit Launch Plans

A launch plan is separate from the canonical session specification:

```text
launch plan:
  base session spec
  transformations:
    - environment-defaults:rust
    - nix:project-shell
    - sandbox:restricted
```

A plan may originate from:

- Repeated ordered CLI options
- A named session template or profile
- A higher-level plugin such as an LLM harness
- Daemon policy requiring specific stages

Bindings use logical transformation aliases rather than artifact identities, so
a provider can be replaced without rewriting every profile.

Transformer metadata may declare requirements such as running after another
kind of transformer or being final. Cairn validates these constraints but never
uses them to invent an order. Contradictory plans fail before process creation.

Security-enforcing transformations require stronger protection than ordinary
command wrappers. A required sandbox stage must be pinned appropriately and
followed by host validation, or eventually be represented as a structured spawn
backend. A later arbitrary transformer must not be able to silently unwrap a
mandatory policy stage.

For each stage, Cairn retains identity, duration, diagnostics, and a structured
or redacted diff. A resolved-spec preview operation runs a chain without
spawning and presents the final command, environment, proxy routes, and
per-stage changes.

## Unified HTTP Handler Chains

All hosted HTTP traffic uses one conceptual handler registry:

```text
authenticate and authorize
  -> route match
  -> middleware A
  -> middleware B
  -> terminal handler
```

The route binding selects both the ordered middleware list and terminal handler.
Rewriting a URI inside middleware does not restart route selection. Multiple
services do not implicitly claim overlapping paths; route ownership and
middleware order are explicit.

Middleware access is an explicit grant. An installed middleware receives neither
session proxy traffic nor plugin service traffic until bound. Bindings also
select fail-closed or bypass behavior. Policy middleware defaults to fail-closed.

This model supports, for example, authorization and audit middleware wrapping an
LLM plugin service, while the same middleware implementation may be separately
bound to selected session proxy routes.

## Manifests, Installations, And Bindings

<!-- Not set on this manifest config format. Will definitely need more bikeshedding -->

A manifest advertises exports and requirements but does not claim global names:

```toml
[component]
id = "example/session-tools"
version = "0.1.0"

[exports.report]
kind = "wasi-cli-command"

[exports.api]
kind = "wasi-http-service"

[exports.defaults]
kind = "cairn-session-transformer"

[exports.exit]
kind = "cairn-session-exited-handler"
```

Suggested aliases and mount paths may improve installation UX, but they have no
routing authority.

An installation supplies configuration and grants:

```toml
[installations.team-tools]
component = "example/session-tools@0.1.0"

[installations.team-tools.config]
name-prefix = "team-"

[installations.team-tools.grants]
sessions = "read"
http-egress = ["hooks.example.com"]
```

Bindings connect installation exports to runtime use:

```toml
[cli.session-report]
target = "team-tools.report"

[http."/tools/sessions/"]
service = "team-tools.api"
middleware = ["auth-policy.handler", "audit.handler"]

[transforms.team-defaults]
target = "team-tools.defaults"

[[events.session-exited]]
target = "team-tools.exit"
selector = "name-prefix:team-"

[launch-profiles.agent]
transforms = [
  "team-defaults",
  "project-nix",
  "restricted-sandbox",
]
```

The syntax is illustrative; implementation may use another configuration format
while preserving the separation and behavior.

## Capability Model

Plugins receive no ambient Cairn authority. Installation grants separately
control:

- Session metadata, creation, input, termination, and log access
- HTTP egress by origin
- Preopened filesystem directories and access modes
- Environment and configuration values
- Clocks and randomness
- Persistent host-provided state
- Proxy traffic and hosted-route bindings
- Typed access to another plugin installation

Cairn authenticates CLI and hosted HTTP callers before guest invocation. Caller,
route, exchange, and source-session context is supplied out of band rather than
through forgeable environment variables or forwarded HTTP headers.

Capability checks occur in host implementations of imported interfaces. Merely
importing an interface or declaring a desired capability in a manifest does not
grant it.

<!--
I want to consider capabilities & grants more.
WIT already provides a pretty clear way to define exactly what
a component is allowed to use. Layering another capability system
on top doesn't seem right. However, we would need to figure out
how to make our WIT interfaces fine-grained enough - e.g.
a component declares that it wants to read pty sessions but not create them.
And there's difficulty with WASI interfaces too - wasi:cli/command pulls in
a lot of privileged stuff, including filesystem and sockets.
-->

<!--
Other ideas around plugin composition and grants:
Could we have a plugin provide some of our built-in functionality,
e.g. the session and proxy WIT apis, and at runtime route to those
when another plugin invokes?

For instance, this would allow a plugin to provide a sandboxed, secure
bash environment - something like https://github.com/mayflower/wasmsh could
be the backend of a replacement session api, allowing built-in commands to be run,
but nothing else.
Obviously, this would break plugins which require true linux semantics or execution
of arbitrary binaries, but could provide more security in other cases.
It could also be used for wasi interfaces. For instance, to create a virtualized
filesystem for another plugin, an alternate log-only wasi:http/client that doesn't
actually perform the request.
-->

## Failure Behavior

- **CLI command:** normal component exit status is returned to the client. Traps,
  resource exhaustion, and host invocation failures are distinct Cairn errors.
- **HTTP service:** traps and malformed responses become controlled server or
  gateway failures, respecting whether a response has already started.
- **HTTP middleware:** each binding selects fail-closed or explicit bypass.
  Bypass continues with the remaining chain and does not restart routing.
- **Session transformer:** any error aborts the launch before process creation.
  The error identifies the stage and includes safe diagnostics and prior diffs.
- **Event handler:** failure is recorded but does not alter the source event.
  Initial delivery is bounded and best-effort; durable retries are deferred.

Installation policy sets time, memory, interruption/fuel, concurrent-call, and
output limits. Guests cannot raise their own limits.

Cancellation propagates from disconnected remote CLI and HTTP clients into the
component invocation. A cancelled request or command is not silently replayed
against a replacement instance.

## Observability

Every invocation records:

- Installation and export identity
- Binding that selected it
- Authenticated principal or source session
- Start time, duration, and outcome
- Trap, cancellation, timeout, or resource-limit category
- Capability denials
- Transformer stage/diff or HTTP exchange ID when applicable

Guest stdout and stderr go to per-installation plugin logs. For remote CLI
commands they are also streamed independently to the invoking client. Cairn
host diagnostics remain separate so guest output is scriptable.

Sensitive configuration and environment values are redacted from invocation
records and transformer diffs.

## Initial Reference Plugins

### Session Defaults

Exports the Cairn session-transform interface. Configuration can add name
prefixes, environment values, timeouts, scrollback defaults, or a default working
directory. It establishes the transformation API without Nix or sandbox
complexity.

### HTTP Policy

Exports standard HTTP middleware. It injects configured request headers and
blocks configured host/path combinations. It can wrap either a session origin
forwarder or another plugin's HTTP service, demonstrating unified chains,
mutation, forwarding, policy failure, and explicit binding.

### Session Report

Exports `wasi:cli/command` and imports read-only Cairn session APIs. Invoked with
`cairn plugin run`, it prints session state, runtime, exit result, and recent
output. It demonstrates remote arguments and stdio plus a standard entry point
with Cairn-specific imports.

### Session API

Exports `wasi:http/service` and imports read-only Cairn session APIs. It serves
selected session metadata as JSON beneath a configured path. HTTP Policy wraps
it in composition tests.

### Exit Webhook

Exports the session-exited handler and optionally imports `wasi:http/client`.
It posts a compact exit summary to an allowed URL. Without egress permission it
can emit the summary to plugin logs, demonstrating capability denial and
host-driven invocation.

### Composition Probe

A synthetic test component provides multiple transformer or middleware exports
that record order. Configuration can make a stage fail, trap, exceed a limit, or
return a semantically invalid session specification or HTTP response. It
exercises diagnostics and policy paths that useful plugins should not
intentionally trigger.

## Candidate Follow-Up Plugins

These remain examples to inform future interfaces, not initial runtime scope:

- **LLM harness:** wraps session spawning and logs to provide structured Pi and
  Codex agent operations and events.
- **Nix environment:** transforms a launch plan using inline packages, inline
  dev-shell configuration, or a `shell.nix`/flake path.
- **Sandbox:** applies sandbox-exec, bubblewrap, container, or future structured
  spawn isolation.
- **Ticket source and ticket agent:** obtains Jira/GitHub/GitLab work items and
  composes with the generic LLM interface to launch matching work.
- **Secret broker middleware:** injects upstream credentials after process-side
  proxying so the child never receives raw secrets.
- **HTTP fixture middleware:** returns configured synthetic responses for
  offline agent and integration testing.
- **Correlation middleware:** adds and records exchange identifiers around proxy
  or plugin-service traffic.
- **Webhook runner service:** exposes an authenticated endpoint that creates a
  session from a fixed launch profile.
- **Transcript archiver:** stores final logs in a granted directory after exit.
- **Retry policy:** creates a replacement session for selected failures with a
  bounded retry count.
- **Environment inspector:** a portable CLI command showing exactly which WASI
  arguments, environment, stdio, clocks, and directories Cairn grants.
- **Resolved-spec preview:** invokes a launch plan without spawning and renders
  its final specification and per-stage diagnostics.
- **Static HTTP service:** a standard-only service serving a granted directory.
- **HTTP service decorator:** adds caching or security headers around another
  plugin's service.
- **Session launcher command:** creates sessions from named launch profiles.

## Testing And Acceptance

### Runtime Contracts

Tests must:

- Install and invoke standard CLI command and HTTP service components.
- Link Cairn-specific imports into components exporting standard WASI entry
  points.
- Reject missing imports, incompatible interface versions, route conflicts, and
  ambiguous aliases before invocation.
- Verify capability denial for sessions, HTTP egress, filesystem access, and
  environment forwarding.
- Verify traps, timeouts, cancellation, output limits, and concurrent invocations
  cannot destabilize the daemon.

### Composition

Tests must:

- Execute an explicitly ordered launch plan and capture each stage's diff.
- Reject invalid ordering constraints and start no process after a transformer
  failure.
- Wrap a plugin HTTP service with two ordered middleware handlers.
- Apply the same middleware implementation to intercepted session traffic.
- Verify forwarding, request/response mutation, synthetic responses,
  fail-closed behavior, and explicit bypass.
- Bind one generic typed import to either of two competing provider
  installations.

### Remote CLI

Tests must:

- Stream stdin to a daemon-hosted command.
- Stream stdout and stderr independently to the remote client.
- Preserve guest exit status while distinguishing host invocation failures.
- Invoke an installation explicitly even without a friendly alias.

### Events

Tests must:

- Invoke an exit handler only after final status and logs are available.
- Apply event selectors correctly.
- Bound event concurrency and record failures without changing session results.

### End-To-End Reference Scenario

One acceptance scenario must:

1. Resolve a launch profile through two ordered transformers.
2. Spawn a session with HTTP proxying enabled.
3. Pass its traffic through HTTP Policy middleware.
4. Query it through Session API wrapped by the same middleware system.
5. Inspect it with the remote Session Report command.
6. Invoke Exit Webhook when it finishes.
7. Produce correlated invocation diagnostics throughout.

This demonstrates all initial entry points without depending on LLM, Nix,
sandbox, ticketing, timers, or durable workflow state.

## Reconciliation Before Implementation

Before implementation planning is finalized:

1. Pin the exact WASI CLI and HTTP world versions supported by the chosen
   Wasmtime component runtime and guest SDKs.
2. Reconcile the unified handler registry here with the implemented HTTP proxy
   and the provisional registry in the HTTP middleware design.
3. Define versioned Cairn WIT packages for launch plans, transformers, events,
   plugin management, typed invocation context, and remote command dispatch.
4. Decide how existing `sessions.create(session-spec)` coexists with launch-plan
   creation without silently applying optional transformations.
5. Specify component instance reuse, concurrent invocation, hot replacement,
   and state guarantees for each entry-point type.
6. Define the concrete configuration format, selector grammar, alias
   normalization, route precedence, and configuration transaction behavior.
7. Define how transformer diffs are computed and redacted, especially for
   secret-bearing environment changes.
8. Ensure mandatory sandbox or policy stages cannot be bypassed by later
   transformations or direct spawn APIs.

## Proposed Sub-Specifications

This design should be implemented through smaller specifications rather than one
monolithic implementation plan. Each sub-spec must pin its own concrete WIT and
Rust APIs, failure semantics, tests, and migration impact. The proposed topics
are listed in approximate dependency order; adjacent topics may be designed in
parallel when their shared contracts have been agreed.

### 1. Component Runtime Foundation

Define Wasmtime and WASI versions, component compilation and loading, engine and
store ownership, async invocation, instance reuse, concurrency, cancellation,
resource limits, trap recovery, and daemon shutdown behavior. Include a minimal
in-process conformance component, but no user-facing plugin entry point.

This is the foundation for every later sub-spec and must explicitly state which
lifecycle guarantees are possible for stateful HTTP components and independently
attributed CLI invocations.

### 2. Plugin Manifests, Installation, And Management

Define artifact identity, installation identity, manifest schema, configuration
validation, install/update/remove operations, local storage layout, and the
plugin-management daemon and CLI APIs. Specify transactional configuration
changes and how suggested aliases differ from active bindings.

Artifact distribution, a marketplace, and trust/signing policy may remain out
of scope, but the model must leave room to add them without changing
installation identity.

### 3. WIT Contracts And Typed Inter-Plugin Linking

Define versioned Cairn WIT packages for invocation context, errors, plugin
metadata, and provider/consumer contracts. Specify how installation imports are
bound to host interfaces or another installation, how multiple providers of the
same contract coexist, and when incompatible or cyclic link graphs are rejected.

This sub-spec should also decide whether existing daemon WIT interfaces are
reused directly, adapted into narrower plugin-facing interfaces, or both.

### 4. Capability And Principal Model

Define grants for Cairn session operations, HTTP egress, filesystem access,
environment values, clocks, randomness, state, proxy traffic, and typed plugin
imports. Specify authenticated principal propagation, session attribution,
configuration secrets, redaction, and host-side authorization checks.

The model must distinguish declaring a requested capability from granting it
and must support grants narrower than an entire imported interface where Cairn
operations have materially different authority.

### 5. Remote WASI CLI Commands

Define daemon protocol additions and CLI behavior for
`cairn plugin run <installation>`, named exports, arguments, selected environment
values, streamed stdin/stdout/stderr, terminal versus non-terminal execution,
exit statuses, cancellation, limits, and host invocation errors. Decide whether
PTY-backed plugin commands are supported or explicitly deferred.

The Session Report and Environment Inspector components should provide the
acceptance fixtures for this vertical slice.

### 6. Unified HTTP Handler Registry And Hosted Services

Reconcile this design with the implemented MITM proxy and define the common
handler abstraction, route ownership and precedence, authentication before
dispatch, plugin HTTP mounts, terminal handlers, streaming bodies, trailers,
cancellation, and service instance lifecycle.

This sub-spec covers `wasi:http/service` hosting and establishes the registry
used by middleware. The Session API and a portable static or echo service should
serve as acceptance fixtures.

### 7. WASI HTTP Middleware Composition

Define the adapter for the selected `wasi:http/middleware` version, ordered
chains around plugin services and proxy origins, immutable invocation context,
`next` continuation behavior, URI rewriting, loop prevention, fail-closed and
bypass policy, hot replacement, and observation boundaries.

This sub-spec amends or supersedes the provisional decisions in
`2026-07-18-http-mitm-wasi-middleware-design.md`. HTTP Policy and Composition
Probe should demonstrate that the same middleware can wrap both a hosted service
and intercepted session traffic.

### 8. Launch Plans And Session Transformers

Define launch-plan records, transformation bindings, ordered invocation,
configuration of individual stages, constraint validation, per-stage diffs and
redaction, resolved-spec preview, final validation, and process creation. Decide
how launch plans coexist with the current `sessions.create(session-spec)` API
and how higher-level plugins request a plan without naming component artifacts.

The security model must prevent mandatory policy or sandbox stages from being
bypassed by later transformations or direct spawn paths. Session Defaults and
Composition Probe provide the initial fixtures.

### 9. Lifecycle Events And Session-Exit Handlers

Define event records, selectors, binding resolution, invocation timing, access
to final status and logs, bounded concurrency, shutdown behavior, and
best-effort failure reporting. Explicitly defer or define the boundary toward
durable delivery, retries, timers, and output-match triggers.

Exit Webhook is the acceptance plugin for this sub-spec.

### 10. Plugin Operations And Observability

Define per-installation logs, invocation records, metrics, capability-denial
audit events, health status, enable/disable behavior, reload and upgrade drain,
trap quarantine or restart policy, and diagnostics exposed through Cairn's CLI
and web APIs.

This sub-spec should consolidate the operational behavior introduced by earlier
vertical slices rather than requiring each entry-point design to invent a
separate logging and lifecycle model.

### 11. Reference Plugin Suite And End-To-End Acceptance

Specify the source layout, guest SDK expectations, build reproducibility, test
fixtures, packaging, and documentation for Session Defaults, HTTP Policy,
Session Report, Session API, Exit Webhook, and Composition Probe. Define the
cross-feature acceptance scenario from this document as an executable system
test.

This final integration sub-spec should reveal gaps between independently built
entry points; it should not introduce new runtime capabilities merely to make
the demonstration richer.
