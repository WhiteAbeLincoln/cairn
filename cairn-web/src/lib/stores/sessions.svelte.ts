// Thin Svelte 5 runes wrapper over the shared `sessionListEngine` singleton.
// The engine is fed by `connection.svelte.ts`'s `ReconnectController` driving
// the daemon's `watch-sessions` push stream — there's no more polling or
// reconnect-triggered refresh to wire up here; this module's only job is to
// mirror the singleton's `sessions`/`loading` into `$state` for the UI.

import type { SessionInfo } from '$lib/protocol';
import { sessionListEngine } from './sessionListEngine';

let sessions = $state<SessionInfo[]>(sessionListEngine.sessions);
let loading = $state(sessionListEngine.loading);

sessionListEngine.subscribe(() => {
    sessions = sessionListEngine.sessions;
    loading = sessionListEngine.loading;
});

export function getSessionList() {
    return {
        get sessions() {
            return sessions;
        },
        get loading() {
            return loading;
        },
    };
}
