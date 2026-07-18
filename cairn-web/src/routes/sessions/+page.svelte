<script lang="ts">
import SessionList from '$lib/components/SessionList.svelte';
import { getSessionList } from '$lib/stores/sessions.svelte';

// Purely declarative: the session list is kept live by the daemon's
// watch-sessions push stream (see connection.svelte.ts/sessionListEngine.ts),
// so there's no poll or manual refresh to drive here.
const store = getSessionList();
</script>

<div class="page-header">
    <h1>Sessions</h1>
    <a href="/sessions/new" class="btn btn-primary">+ New session</a>
</div>

<SessionList sessions={store.sessions} loading={store.loading} />

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
