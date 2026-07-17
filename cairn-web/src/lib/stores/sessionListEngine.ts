// The session-list data engine: framework-free so it's directly testable —
// see `sessionListEngine.test.ts`. `sessions.svelte.ts` wraps this in
// `$state` for the UI.
//
// The daemon pushes a `watch-sessions` stream (see `$lib/protocol`): a
// `snapshot` of every session up front, then `upsert`/`removed` deltas as
// sessions change. This engine folds that stream into a `Map` keyed by
// session id and exposes a deterministically sorted view. There's no more
// polling `refresh()` to coalesce — the daemon is the one pushing now, and
// connection-level errors are `ReconnectController`'s to report (see
// `reconnect.ts`), not this engine's.
//
// `connection.svelte.ts` (which owns the `ReconnectController` driving the
// watch stream) and `sessions.svelte.ts` (which reads the resulting list)
// can't import each other without a cycle, so this module also exports a
// process-wide singleton (`sessionListEngine`) that both sides share.

import type { SessionEvent, SessionId, SessionInfo } from '$lib/protocol';

export type SessionListListener = () => void;

export class SessionListEngine {
    readonly #sessions = new Map<SessionId, SessionInfo>();
    #loading = true;
    readonly #listeners = new Set<SessionListListener>();

    /**
     * Sessions ordered by creation time ascending, id as a tiebreak for equal
     * timestamps — deterministic regardless of the order events arrived in
     * (an `upsert` for an old session can arrive after a newer one's).
     */
    get sessions(): SessionInfo[] {
        return [...this.#sessions.values()].sort(compareByCreation);
    }

    /** True until the first `snapshot` is applied (again after `reset()`, until the next one). */
    get loading(): boolean {
        return this.#loading;
    }

    subscribe(listener: SessionListListener): () => void {
        this.#listeners.add(listener);
        return () => this.#listeners.delete(listener);
    }

    /**
     * Fold one event from a `watchSessions()` stream into the engine.
     * `snapshot` replaces the whole set (the stream's first event, and the
     * baseline again after every reconnect); `upsert` inserts-or-replaces by
     * id; `removed` deletes by id — a no-op if the id is already gone (e.g. a
     * `removed` racing a `snapshot` that already dropped it).
     */
    applyEvent(ev: SessionEvent): void {
        switch (ev.tag) {
            case 'snapshot':
                this.#sessions.clear();
                for (const info of ev.val) this.#sessions.set(info.id, info);
                this.#loading = false;
                break;
            case 'upsert':
                this.#sessions.set(ev.val.id, ev.val);
                break;
            case 'removed':
                this.#sessions.delete(ev.val);
                break;
        }
        this.#notify();
    }

    /** Drop all state and go back to `loading`, e.g. when switching daemon endpoints — the old snapshot no longer applies. */
    reset(): void {
        this.#sessions.clear();
        this.#loading = true;
        this.#notify();
    }

    #notify(): void {
        for (const listener of this.#listeners) listener();
    }
}

/** `createdAtUnixMs` is a `bigint` — compare explicitly rather than subtracting. */
function compareByCreation(a: SessionInfo, b: SessionInfo): number {
    if (a.createdAtUnixMs !== b.createdAtUnixMs) {
        return a.createdAtUnixMs < b.createdAtUnixMs ? -1 : 1;
    }
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/** Shared by `connection.svelte.ts` (feeds `watchSessions()` events in) and `sessions.svelte.ts` (reads the list back out). */
export const sessionListEngine: SessionListEngine = new SessionListEngine();
