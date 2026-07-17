<!-- Always-visible connection status dot + label; compact on mobile (dot only). -->
<script lang="ts">
import { getConnectionEndpoint, getConnectionStatus } from '$lib/stores/connection.svelte';

const status = $derived(getConnectionStatus());
const endpoint = $derived(getConnectionEndpoint());

let showDetails = $state(false);

const label = $derived.by(() => {
    switch (status.state) {
        case 'connected':
            return 'Connected';
        case 'connecting':
            return 'Connecting…';
        case 'reconnecting':
            return `Reconnecting… (attempt ${status.attempt})`;
    }
});

const dotClass = $derived(
    status.state === 'connected'
        ? 'connected'
        : status.state === 'connecting'
          ? 'connecting'
          : 'reconnecting',
);
</script>

<div class="indicator-wrapper">
    <button
        type="button"
        class="indicator"
        class:clickable={status.state === 'reconnecting'}
        title={label}
        onclick={() => {
            if (status.state === 'reconnecting') showDetails = !showDetails;
        }}
    >
        <span class="dot {dotClass}"></span>
        <span class="label">{label}</span>
    </button>

    {#if status.state === 'reconnecting' && showDetails}
        <div class="popover">
            <div class="popover-title">Connection trouble</div>
            <code class="popover-message">{status.error.message}</code>
            {#if endpoint}
                <div class="popover-detail">Trying to reach <code>{endpoint.url}</code></div>
            {/if}
            <div class="popover-detail">Next retry in ~{Math.round(status.retryInMs / 1000)}s</div>
        </div>
    {/if}
</div>

<style>
    .indicator-wrapper {
        position: relative;
    }

    .indicator {
        display: flex;
        align-items: center;
        gap: 0.375rem;
        font-size: 0.75rem;
        color: var(--color-text-muted);
        background: none;
        border: none;
        padding: 0.25rem 0.375rem;
        border-radius: var(--radius);
    }

    .indicator.clickable {
        cursor: pointer;
    }

    .indicator.clickable:hover {
        background: var(--color-surface-hover);
    }

    .dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        flex-shrink: 0;
    }
    .dot.connected {
        background: var(--color-success);
    }
    .dot.connecting {
        background: var(--color-warning);
    }
    .dot.reconnecting {
        background: var(--color-error);
    }

    .label {
        display: none;
    }

    @media (min-width: 640px) {
        .label {
            display: inline;
        }
    }

    .popover {
        position: absolute;
        top: calc(100% + 0.5rem);
        right: 0;
        width: min(20rem, calc(100vw - 2rem));
        padding: 0.75rem;
        background: var(--color-surface);
        border: 1px solid var(--color-error);
        border-radius: var(--radius);
        z-index: 100;
        font-size: 0.8rem;
        line-height: 1.5;
        display: flex;
        flex-direction: column;
        gap: 0.375rem;
    }

    .popover-title {
        font-weight: 600;
        color: var(--color-error);
    }

    .popover-message {
        display: block;
        padding: 0.375rem 0.5rem;
        background: var(--color-bg);
        border-radius: 3px;
        font-size: 0.75rem;
        word-break: break-all;
    }

    .popover-detail {
        color: var(--color-text-muted);
    }

    .popover-detail code {
        color: var(--color-text);
    }
</style>
