<script lang="ts">
import { page } from '$app/state';
import { forgetEndpoint, getConnectionEndpoint, getIdentity } from '$lib/stores/connection.svelte';
import ConnectionIndicator from './ConnectionIndicator.svelte';

const isActive = $derived(page.url.pathname.startsWith('/sessions'));
// Only worth offering once *some* endpoint has been resolved — otherwise the
// manual-entry screen is already showing.
const hasEndpoint = $derived(!!getConnectionEndpoint());
const identity = $derived(getIdentity());
const showIdentity = $derived(identity != null && identity !== 'anonymous');
</script>

<nav class="nav">
    <a href="/sessions" class="nav-brand">cairn</a>
    <div class="nav-links">
        <a href="/sessions" class:active={isActive}>Sessions</a>
    </div>
    <ConnectionIndicator />
    {#if showIdentity}
        <span class="identity" title={identity}>{identity}</span>
    {/if}
    {#if hasEndpoint}
        <button type="button" class="change-endpoint" onclick={forgetEndpoint}>
            Change endpoint
        </button>
    {/if}
</nav>

<style>
    .nav {
        display: flex;
        align-items: center;
        gap: 1rem;
        padding: 0.75rem 1rem;
        background: var(--color-surface);
        border-bottom: 1px solid var(--color-border);
    }

    .nav-brand {
        font-weight: 700;
        font-size: 1.1rem;
        font-family: var(--font-mono);
        color: var(--color-text);
    }

    .nav-brand:hover {
        text-decoration: none;
    }

    .nav-links {
        display: flex;
        gap: 0.5rem;
        flex: 1;
    }

    .nav-links a {
        padding: 0.375rem 0.75rem;
        border-radius: var(--radius);
        color: var(--color-text-muted);
        font-size: 0.875rem;
    }

    .nav-links a:hover {
        text-decoration: none;
        color: var(--color-text);
    }

    .nav-links a.active {
        color: var(--color-text);
        background: var(--color-surface-hover);
    }

    .identity {
        font-size: 0.75rem;
        color: var(--color-text-muted);
        font-family: var(--font-mono);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        max-width: 12rem;
    }

    @media (max-width: 639px) {
        .identity {
            display: none;
        }
    }

    .change-endpoint {
        background: none;
        border: none;
        padding: 0.375rem 0.5rem;
        font-size: 0.75rem;
        color: var(--color-text-muted);
        cursor: pointer;
        border-radius: var(--radius);
        white-space: nowrap;
    }

    .change-endpoint:hover {
        color: var(--color-text);
        background: var(--color-surface-hover);
    }
</style>
