// The session-list data engine: framework-free so it's directly testable
// (including its interaction with `ReconnectController` — see
// `sessionListEngine.test.ts`). `sessions.svelte.ts` wraps this in `$state`
// for the UI.

import type { DaemonClient, SessionInfo } from '$lib/protocol';

export type SessionListListener = () => void;

export class SessionListEngine {
    #sessions: SessionInfo[] = [];
    #loading = false;
    #error: string | undefined;
    readonly #listeners = new Set<SessionListListener>();

    get sessions(): SessionInfo[] {
        return this.#sessions;
    }

    get loading(): boolean {
        return this.#loading;
    }

    /** The most recent refresh failure, as a display-ready message. `undefined` once a refresh succeeds. */
    get error(): string | undefined {
        return this.#error;
    }

    subscribe(listener: SessionListListener): () => void {
        this.#listeners.add(listener);
        return () => this.#listeners.delete(listener);
    }

    /**
     * Fetch the session list. Sets `error` and *rethrows* on failure — the
     * throw lets a caller (e.g. `ReconnectController`'s probe) also treat this
     * as the connectivity check, so "on recovery, re-fetch the session list"
     * falls out of the reconnect loop without a separate callback.
     */
    async refresh(client: DaemonClient): Promise<void> {
        this.#loading = true;
        this.#notify();
        try {
            this.#sessions = await client.listAll();
            this.#error = undefined;
        } catch (err) {
            this.#error = err instanceof Error ? err.message : String(err);
            throw err;
        } finally {
            this.#loading = false;
            this.#notify();
        }
    }

    #notify(): void {
        for (const listener of this.#listeners) listener();
    }
}
