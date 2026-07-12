// Reconnect / backoff state machine. Lives in the stores layer per the design
// spec ("Reconnect lives in the stores layer, not `DaemonClient`"): dial
// failures flip connection state; jittered exponential backoff capped at 10s;
// once healthy, a steady-interval re-probe keeps watching for a dropped
// connection. Framework-free (no Svelte, no timers imported directly — both
// injectable) so the state machine is unit-testable without a browser or
// fake-timer gymnastics.

export type ConnectionStatus =
    | { readonly state: 'connecting' }
    | { readonly state: 'connected' }
    | {
          readonly state: 'reconnecting';
          readonly attempt: number;
          readonly retryInMs: number;
          readonly error: Error;
      };

export interface BackoffOptions {
    /** Delay before the first retry, doubled each subsequent attempt. Default 250ms. */
    baseMs?: number;
    /** Hard cap on the delay, regardless of attempt count. Default 10s per the design spec. */
    maxMs?: number;
    /** Source of randomness in [0, 1); overridable for deterministic tests. Default `Math.random`. */
    jitter?: () => number;
}

/**
 * Full-jitter exponential backoff: `random_between(0, min(maxMs, baseMs * 2^attempt))`.
 * `attempt` is 0-indexed (the delay before the *first* retry uses `attempt: 0`).
 */
export function backoffDelay(attempt: number, opts: BackoffOptions = {}): number {
    const base = opts.baseMs ?? 250;
    const max = opts.maxMs ?? 10_000;
    const jitter = opts.jitter ?? Math.random;
    const cap = Math.min(max, base * 2 ** attempt);
    return Math.floor(jitter() * cap);
}

export interface ReconnectControllerOptions {
    /** The connectivity check to repeat. Resolve = healthy, reject = unhealthy. */
    probe: () => Promise<void>;
    /** Delay between successful probes while connected (health re-check cadence). Default 15s. */
    steadyIntervalMs?: number;
    backoff?: BackoffOptions;
    /** Injectable in place of `setTimeout`, for deterministic tests. */
    schedule?: (fn: () => void, ms: number) => unknown;
    /** Injectable in place of `clearTimeout`. */
    clearSchedule?: (handle: unknown) => void;
}

/**
 * Drives a probe function through connecting -> connected -> reconnecting (on
 * failure, with backoff) -> connected (on recovery) -> ... forever, notifying
 * subscribers of every state transition. There is no terminal "gave up"
 * state: the daemon is expected to come back eventually, and backoff staying
 * capped at `maxMs` keeps retries cheap in the meantime.
 */
export class ReconnectController {
    readonly #opts: ReconnectControllerOptions;
    #status: ConnectionStatus = { state: 'connecting' };
    #attempt = 0;
    #timer: unknown;
    #stopped = true;
    readonly #listeners = new Set<(status: ConnectionStatus) => void>();

    constructor(opts: ReconnectControllerOptions) {
        this.#opts = opts;
    }

    get status(): ConnectionStatus {
        return this.#status;
    }

    /** Subscribe to status changes; returns an unsubscribe function. */
    onStatusChange(listener: (status: ConnectionStatus) => void): () => void {
        this.#listeners.add(listener);
        return () => this.#listeners.delete(listener);
    }

    start(): void {
        this.#stopped = false;
        this.#setStatus({ state: 'connecting' });
        void this.#runNow();
    }

    stop(): void {
        this.#stopped = true;
        if (this.#timer !== undefined) {
            const clear =
                this.#opts.clearSchedule ??
                ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
            clear(this.#timer);
            this.#timer = undefined;
        }
    }

    async #runNow(): Promise<void> {
        try {
            await this.#opts.probe();
            if (this.#stopped) return;
            this.#attempt = 0;
            // Only notify on the connecting/reconnecting -> connected
            // *transition* — a successful steady-state re-probe is not news,
            // and re-notifying would make "refresh on reconnect" subscribers
            // re-fetch every probe interval.
            if (this.#status.state !== 'connected') {
                this.#setStatus({ state: 'connected' });
            }
            this.#scheduleNext(this.#opts.steadyIntervalMs ?? 15_000);
        } catch (err) {
            if (this.#stopped) return;
            const error = err instanceof Error ? err : new Error(String(err));
            const delay = backoffDelay(this.#attempt, this.#opts.backoff);
            this.#attempt += 1;
            this.#setStatus({
                state: 'reconnecting',
                attempt: this.#attempt,
                retryInMs: delay,
                error,
            });
            this.#scheduleNext(delay);
        }
    }

    #scheduleNext(ms: number): void {
        const schedule = this.#opts.schedule ?? ((fn, d) => setTimeout(fn, d));
        this.#timer = schedule(() => {
            void this.#runNow();
        }, ms);
    }

    #setStatus(status: ConnectionStatus): void {
        this.#status = status;
        for (const listener of this.#listeners) listener(status);
    }
}
