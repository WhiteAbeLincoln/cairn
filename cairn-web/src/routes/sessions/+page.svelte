<script lang="ts">
import { onMount } from 'svelte';
import SessionList from '$lib/components/SessionList.svelte';
import { getSessionList, refreshSessions } from '$lib/stores/sessions.svelte';

const store = getSessionList();

// Reconnect-triggered refresh lives in the stores layer (sessions.svelte.ts);
// this poll just keeps the visible list current while the page is mounted.
onMount(() => {
    refreshSessions();
    const interval = setInterval(refreshSessions, 5000);
    return () => clearInterval(interval);
});
</script>

<div class="page-header">
    <h1>Sessions</h1>
    <a href="/sessions/new" class="btn btn-primary">+ New session</a>
</div>

<SessionList sessions={store.sessions} loading={store.loading} error={store.error} />

<style>
    .page-header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 1rem;
        margin-bottom: 1.25rem;
    }

    .page-header h1 {
        font-size: 1.25rem;
    }
</style>
