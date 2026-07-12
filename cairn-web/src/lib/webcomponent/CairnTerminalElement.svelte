<!--
  The `<cairn-terminal>` web component: a thin wrapper that compiles
  `Terminal.svelte` (kept unchanged and shared with the SvelteKit app) into a
  standalone custom element via Svelte's `customElement` option. This file is
  the entry point for the separate `build:element` Vite target (see
  `vite.element.config.ts`) — it is never imported by the SvelteKit app.

  Contract (see the design spec's "`<cairn-terminal>` web component" section):
    Attributes: session-id, endpoint, font-size, font-family
    Properties: .client (DaemonClient), .sessionId (string)
    Events: cairn-attached, cairn-detached, cairn-exited
    Modes: shared-client (`.client` set) or standalone (`endpoint` attribute)

  Light DOM (`shadow: 'none'`) rather than a shadow root: `@wterm/dom/css`
  (imported by Terminal.svelte) injects its stylesheet into the *document*,
  not into whatever shadow root happens to be current — a shadow root would
  cut the terminal off from its own required CSS (grid layout, cursor,
  selection). Terminal.svelte's own `<style>` block is still scoped normally
  by Svelte's per-component class hashing regardless of shadow DOM, so this
  doesn't reintroduce style leakage for its own rules — only the two
  deliberately-global `.btn`/`.btn-primary` classes it reuses from the app's
  shared stylesheet need restating here (see `:global(...)` below), since a
  standalone page has no `app.css`.
-->
<svelte:options
    customElement={{
        tag: 'cairn-terminal',
        shadow: 'none',
        props: {
            sessionId: { attribute: 'session-id' },
            endpoint: { attribute: 'endpoint' },
            fontSize: { attribute: 'font-size', type: 'Number' },
            fontFamily: { attribute: 'font-family' },
            client: {},
        },
    }}
/>

<script lang="ts">
import { onDestroy } from 'svelte';
import Terminal from '$lib/components/Terminal.svelte';
import { DaemonClient, wsDialer, wtDialer } from '$lib/protocol';
import type { AttachPhase } from '$lib/terminal/attachController';

interface Props {
    sessionId?: string;
    endpoint?: string;
    fontSize?: number;
    fontFamily?: string;
    /** Shared-client mode: an already-connected `DaemonClient` (e.g. the host app's own). */
    client?: DaemonClient;
}

let { sessionId, endpoint, fontSize, fontFamily, client }: Props = $props();

// The custom element instance, for dispatching `cairn-*` events on it. Valid
// because `svelte.config.js` sets `compilerOptions.customElement: true`
// (required project-wide for this file's own `<svelte:options
// customElement>` tag to take effect at all — see that file's comment) and
// this component declares that tag. `vite build --config vite.element.config.ts`
// compiles this correctly (verified: the output calls `$$props.$$host`), but
// this pinned svelte-check's generated type-checking shim doesn't yet model
// the `$host()` rune's type (it reports the synthetic `$host` binding as used
// before its own declaration) — a tooling gap, not a real error.
// @ts-expect-error -- svelte-check doesn't yet type `$host()`; see above.
const host: HTMLElement = $host();

/**
 * Standalone mode: build our own client from the `endpoint` attribute when no
 * `.client` was supplied. Scheme selects the transport, mirroring the
 * daemon's own `--listen` convention (`ws(s)://` = WebSocket, `https://` =
 * WebTransport). `$derived` only rebuilds this when `client`/`endpoint`
 * actually change, so switching sessions on the same endpoint doesn't churn
 * dialers. Computed as one object (rather than a derived that also writes a
 * separate `$state` as a side effect) so there's a single derivation pass.
 */
const clientResult = $derived.by((): { client?: DaemonClient; error?: string } => {
    if (client) return { client };
    if (!endpoint) return {};
    if (/^wss?:\/\//i.test(endpoint)) return { client: new DaemonClient(wsDialer(endpoint)) };
    if (/^https:\/\//i.test(endpoint)) return { client: new DaemonClient(wtDialer(endpoint)) };
    return { error: `endpoint must be a ws://, wss://, or https:// URL, got: ${endpoint}` };
});
const effectiveClient = $derived(clientResult.client);
const configError = $derived(clientResult.error);

// Re-key the inner Terminal on session or client identity changes, forcing a
// clean unmount/remount (fresh AttachController) rather than trying to nurse
// one instance through an arbitrary reconfiguration.
const instanceKey = $derived(`${sessionId ?? ''}:${client ? 'shared' : (endpoint ?? '')}`);

function emit(type: string, detail: unknown): void {
    host.dispatchEvent(new CustomEvent(type, { detail, bubbles: true, composed: true }));
}

function handlePhase(phase: AttachPhase): void {
    switch (phase.kind) {
        case 'attached':
            emit('cairn-attached', { sessionId });
            break;
        case 'exited':
            emit('cairn-exited', phase.status);
            break;
        case 'error':
            emit('cairn-detached', { sessionId, reason: `error:${phase.code}` });
            break;
        case 'disconnected':
            emit('cairn-detached', {
                sessionId,
                reason: phase.message ? `disconnected:${phase.message}` : 'disconnected',
            });
            break;
    }
}

// Detach on removal from the DOM (navigation away, or the host page tearing
// the element down) — Terminal.svelte's own onMount cleanup already stops the
// AttachController; this just completes the event contract ("Detach
// (navigation, explicit, error)" per the design spec).
onDestroy(() => {
    if (sessionId) emit('cairn-detached', { sessionId, reason: 'unmount' });
});
</script>

{#if !sessionId}
    <p class="cairn-terminal-config-error">
        &lt;cairn-terminal&gt;: missing required <code>session-id</code> attribute
    </p>
{:else if !effectiveClient}
    <p class="cairn-terminal-config-error">
        {configError ?? '<cairn-terminal>: set a .client property or an endpoint attribute'}
    </p>
{:else}
    {#key instanceKey}
        <Terminal
            client={effectiveClient}
            {sessionId}
            {fontSize}
            {fontFamily}
            onPhase={handlePhase}
        />
    {/key}
{/if}

<style>
    /* `<cairn-terminal>` fills whatever box the host page gives it — matching
       Terminal.svelte's own `position:absolute` contract, this element must be
       sized and positioned by its container (e.g. `height: 400px` or a flex
       item). */
    :global(cairn-terminal) {
        display: block;
        position: relative;
        overflow: hidden;
        border-radius: var(--radius, 8px);

        /* Design tokens Terminal.svelte's styles depend on (app.css's
           `:root`, not present on a bare standalone page) — default values so
           the terminal looks right with zero host-page CSS. A host page that
           *does* define these on an ancestor wins, since a rule scoped to the
           element itself only sets the property when nothing more specific
           already applies... but as a plain (non-`:host`) rule this always
           applies once matched; that's fine here since there's no shadow
           boundary to reason about (`shadow: 'none'`) and these are sensible
           values to hard-default a batteries-included widget to regardless. */
        --color-bg: #0b0d10;
        --color-surface: #14171c;
        --color-border: #262b33;
        --color-text: #e6e9ef;
        --color-text-muted: #8891a0;
        --color-accent: #4f8cff;
        --font-mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
        --radius: 8px;
        background: var(--color-bg);
        color: var(--color-text);
        font-family: var(--font-mono);
    }

    .cairn-terminal-config-error {
        padding: 1rem;
        color: var(--color-error, #f87171);
        font-family: var(--font-mono);
        font-size: 0.875rem;
    }

    /* Terminal.svelte's exit/reattach overlays use the app's shared `.btn`
       classes (defined in `app.css`, absent on a standalone page). Restated
       here as `:global()` so the WC bundle is self-contained. */
    :global(cairn-terminal .btn) {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 0.375rem;
        padding: 0.625rem 1rem;
        min-height: 2.5rem;
        border-radius: var(--radius);
        border: 1px solid var(--color-border);
        background: var(--color-surface);
        color: var(--color-text);
        cursor: pointer;
        font-family: inherit;
        font-size: 0.9rem;
        text-decoration: none;
    }

    :global(cairn-terminal .btn:hover) {
        opacity: 0.9;
    }

    :global(cairn-terminal .btn-primary) {
        background: var(--color-accent);
        border-color: var(--color-accent);
        color: #fff;
    }
</style>
