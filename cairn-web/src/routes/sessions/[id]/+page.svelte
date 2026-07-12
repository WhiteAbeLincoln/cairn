<script lang="ts">
import { untrack } from 'svelte';
import { page } from '$app/state';
import SessionDetail from '$lib/components/SessionDetail.svelte';
import type { SessionInfo } from '$lib/protocol';
import { getClient, getConnectionStatus } from '$lib/stores/connection.svelte';

let session = $state<SessionInfo | undefined>(undefined);
let error = $state<string | undefined>(undefined);

const sessionId = $derived(page.params.id);
const status = $derived(getConnectionStatus());

$effect(() => {
    // Re-run whenever the route param or connection status changes.
    const id = sessionId;
    if (!id) return;
    // Navigated to a different session: drop the previous session's data so the
    // keyed detail view (and its terminal) never renders stale for the new id.
    // Read `session` untracked — this effect *writes* `session` (below), so a
    // tracked read here would retrigger it in a tight inspect() loop.
    const current = untrack(() => session);
    if (current && current.id !== id) {
        session = undefined;
        error = undefined;
    }
    if (status.state !== 'connected') return;
    const client = getClient();
    if (!client) return;

    let cancelled = false;
    client.inspect(id).then(
        (info) => {
            if (!cancelled) {
                session = info;
                error = undefined;
            }
        },
        (err) => {
            if (!cancelled) {
                error = err instanceof Error ? err.message : String(err);
            }
        },
    );
    return () => {
        cancelled = true;
    };
});
</script>

{#if error}
    <div class="error-state">
        <p class="banner-error">Failed to load session: {error}</p>
        <a href="/sessions">&larr; Back to sessions</a>
    </div>
{:else if session}
    {#key session.id}
        <SessionDetail {session} onUpdated={(info) => (session = info)} />
    {/key}
{:else}
    <p class="muted">Loading…</p>
{/if}

<style>
    .error-state {
        text-align: center;
        padding: 2rem 1rem;
    }
</style>
