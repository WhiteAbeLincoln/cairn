<!--
  Full manual-endpoint screen for standalone hosting, shown when `/cairn.json`
  discovery finds nothing and no endpoint is persisted (or the user explicitly
  asked to change endpoints via `forgetEndpoint()`). Three independent ways in:

  1. Daemon base URL — the primary path for a standalone-hosted build: fetch
     that host's CORS-open `/cairn.json` and apply the normal
     WS-preferred/WT-fallback selection.
  2. A direct `ws://`/`wss://` URL, bypassing discovery entirely.
  3. A direct WebTransport endpoint (`https://` URL), with an optional
     cert-hash field for a daemon's self-signed certificate.

  Whichever succeeds is persisted to localStorage (`?endpoint=` still takes
  top priority over all of this — see `discoverEndpoint`).
-->
<script lang="ts">
import {
    getManualError,
    submitBaseUrl,
    submitDirectWs,
    submitDirectWt,
} from '$lib/stores/connection.svelte';

let baseUrl = $state('');
let wsUrl = $state('');
let wtUrl = $state('');
let wtCertHash = $state('');
let pending = $state(false);
const error = $derived(getManualError());

async function handleBaseUrl(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    pending = true;
    try {
        await submitBaseUrl(baseUrl);
    } finally {
        pending = false;
    }
}

function handleDirectWs(e: SubmitEvent): void {
    e.preventDefault();
    submitDirectWs(wsUrl);
}

function handleDirectWt(e: SubmitEvent): void {
    e.preventDefault();
    submitDirectWt(wtUrl, wtCertHash);
}
</script>

<div class="manual-endpoint">
    <h1>Connect to a daemon</h1>
    <p class="muted">
        This build isn't served by a daemon (or none was found automatically). Choose how to
        connect:
    </p>

    {#if error}
        <p class="banner-error">{error}</p>
    {/if}

    <section class="option">
        <h2>Daemon URL</h2>
        <p class="muted small">
            The host:port a <code>cairn-daemon</code> is listening on. Its
            <code>/cairn.json</code> is fetched to pick the best transport automatically.
        </p>
        <form onsubmit={handleBaseUrl}>
            <input
                type="text"
                bind:value={baseUrl}
                placeholder="http://localhost:8080"
                aria-label="Daemon base URL"
                autocomplete="off"
            />
            <button type="submit" class="btn btn-primary" disabled={pending}>
                {pending ? 'Connecting…' : 'Connect'}
            </button>
        </form>
    </section>

    <section class="option">
        <h2>Direct WebSocket URL</h2>
        <p class="muted small">Skip discovery and dial a WebSocket endpoint directly.</p>
        <form onsubmit={handleDirectWs}>
            <input
                type="text"
                bind:value={wsUrl}
                placeholder="ws://localhost:8080/ws"
                aria-label="Direct WebSocket URL"
                autocomplete="off"
            />
            <button type="submit" class="btn btn-primary">Connect</button>
        </form>
    </section>

    <section class="option">
        <h2>WebTransport endpoint</h2>
        <p class="muted small">
            For a daemon reachable only over WebTransport. The cert-hash field is only needed for
            the daemon's self-signed certificate (find it in <code>/cairn.json</code> or the
            daemon's runtime dir's <code>cert-hash</code> file); leave it blank for a
            certificate-authority-signed cert.
        </p>
        <form onsubmit={handleDirectWt}>
            <input
                type="text"
                bind:value={wtUrl}
                placeholder="https://localhost:4433"
                aria-label="WebTransport endpoint URL"
                autocomplete="off"
            />
            <input
                type="text"
                bind:value={wtCertHash}
                placeholder="Cert hash (optional, self-signed only)"
                aria-label="WebTransport certificate hash"
                autocomplete="off"
                class="mono"
            />
            <button type="submit" class="btn btn-primary">Connect</button>
        </form>
    </section>
</div>

<style>
    .manual-endpoint {
        max-width: 32rem;
        margin: 2rem auto;
        padding: 0 1rem 2rem;
    }

    h1 {
        text-align: center;
    }

    .muted.small {
        font-size: 0.8125rem;
        margin: 0.25rem 0 0.75rem;
    }

    .option {
        margin-top: 1.75rem;
        padding-top: 1.5rem;
        border-top: 1px solid var(--color-border);
    }

    .option h2 {
        font-size: 0.95rem;
    }

    form {
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
    }

    input {
        width: 100%;
        padding: 0.625rem 0.75rem;
        min-height: 2.5rem;
        background: var(--color-surface);
        border: 1px solid var(--color-border);
        border-radius: var(--radius);
        font-family: var(--font-mono);
    }

    input.mono {
        font-size: 0.8125rem;
    }

    input:focus {
        outline: none;
        border-color: var(--color-accent);
    }
</style>
