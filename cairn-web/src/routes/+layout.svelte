<script lang="ts">
import { onMount } from 'svelte';
import ManualEndpoint from '$lib/components/ManualEndpoint.svelte';
import Nav from '$lib/components/Nav.svelte';
import { getNeedsManualEndpoint, initConnection } from '$lib/stores/connection.svelte';
import '../app.css';

const { children } = $props();
const needsManualEndpoint = $derived(getNeedsManualEndpoint());

onMount(() => {
    // initConnection never rejects (failures surface as the manual-endpoint
    // screen), so no .catch() is needed on this intentionally-unawaited call.
    void initConnection(window.location.href);
});
</script>

<div class="app-shell">
    <Nav />
    <main class="app-main">
        {#if needsManualEndpoint}
            <ManualEndpoint />
        {:else}
            {@render children()}
        {/if}
    </main>
</div>

<style>
    .app-shell {
        display: flex;
        flex-direction: column;
        min-height: 100vh;
        min-height: 100dvh;
    }

    .app-main {
        flex: 1;
        min-height: 0;
        padding: 1rem;
        /* A flex column so a page can opt into filling the viewport (the
           terminal detail view uses `flex: 1`); block pages stack as before. */
        display: flex;
        flex-direction: column;
    }

    @media (min-width: 640px) {
        .app-main {
            padding: 1.5rem 2rem;
        }
    }
</style>
