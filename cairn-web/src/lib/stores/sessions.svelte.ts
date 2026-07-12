// Thin Svelte 5 runes wrapper over `SessionListEngine`. Subscribes to the
// connection store so the list is (re-)fetched automatically whenever
// connectivity is (re)established — see the design spec: "on recovery,
// re-fetch the session list."

import type { SessionInfo } from '$lib/protocol';
import { getClient, onConnectionStatusChange } from './connection.svelte';
import { SessionListEngine } from './sessionListEngine';

const engine = new SessionListEngine();

let sessions = $state<SessionInfo[]>([]);
let loading = $state(false);
let error = $state<string | undefined>(undefined);

engine.subscribe(() => {
    sessions = engine.sessions;
    loading = engine.loading;
    error = engine.error;
});

onConnectionStatusChange((status) => {
    if (status.state === 'connected') {
        refreshSessions();
    }
});

export function getSessionList() {
    return {
        get sessions() {
            return sessions;
        },
        get loading() {
            return loading;
        },
        get error() {
            return error;
        },
    };
}

/** Re-fetch the session list. A no-op if the client isn't connected yet; failures are recorded in `error`, not thrown. */
export function refreshSessions(): void {
    const client = getClient();
    if (!client) return;
    engine.refresh(client).catch(() => {
        // Already surfaced via `engine.error` (read through the `error` getter above).
    });
}
