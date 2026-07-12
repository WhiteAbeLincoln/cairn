<!--
  Minimal manual-endpoint fallback, shown when `/cairn.json` discovery finds
  nothing and no endpoint is persisted. Deliberately thin — a single direct
  ws://|wss:// URL field. The full standalone-hosting screen (daemon base-URL
  bootstrap, cert-hash field) is Task 9.
-->
<script lang="ts">
import { getManualError, submitManualEndpoint } from '$lib/stores/connection.svelte';

let value = $state('');
const error = $derived(getManualError());

function handleSubmit(e: SubmitEvent): void {
    e.preventDefault();
    submitManualEndpoint(value);
}
</script>

<div class="manual-endpoint">
    <h1>Connect to a daemon</h1>
    <p class="muted">
        Couldn't find a daemon endpoint automatically. Enter its WebSocket URL directly.
    </p>
    <form onsubmit={handleSubmit}>
        <input
            type="text"
            bind:value
            placeholder="ws://localhost:8080/ws"
            aria-label="Daemon WebSocket URL"
            autocomplete="off"
        />
        <button type="submit" class="btn btn-primary">Connect</button>
    </form>
    {#if error}
        <p class="banner-error">{error}</p>
    {/if}
</div>

<style>
    .manual-endpoint {
        max-width: 28rem;
        margin: 3rem auto;
        padding: 0 1rem;
        text-align: center;
    }

    form {
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
        margin-top: 1.5rem;
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

    input:focus {
        outline: none;
        border-color: var(--color-accent);
    }
</style>
