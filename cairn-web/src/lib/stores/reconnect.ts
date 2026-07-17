// Reconnect / backoff state machine. Lives in the stores layer per the design
// spec ("Reconnect lives in the stores layer, not `DaemonClient`"): dial
// failures flip connection state; jittered exponential backoff capped at 10s.
// `run` wraps a long-lived server-push stream (the daemon's `watch-sessions`)
// rather than a one-shot probe — it stays pending for as long as the stream
// is healthy, and its eventual settling (resolve or reject) is itself the
// "connection dropped" signal. Framework-free (no Svelte, no timers imported
// directly — both injectable) so the state machine is unit-testable without a
// browser or fake-timer gymnastics.

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
    /**
     * One connection attempt: establish the watch stream, call `onUp()` when
     * live (first event received), and stay pending while healthy. Returning
     * OR throwing both mean "down" — the controller schedules a retry.
     * `signal` aborts when the controller is stopped: the run must stop
     * promptly and must not apply further effects after abort.
     */
    run: (onUp: () => void, signal: AbortSignal) => Promise<void>;
    backoff?: BackoffOptions;
    /** Injectable in place of `setTimeout`, for deterministic tests. */
    schedule?: (fn: () => void, ms: number) => unknown;
    /** Injectable in place of `clearTimeout`. */
    clearSchedule?: (handle: unknown) => void;
}

/**
 * Supervises a long-lived `run` through connecting -> connected -> (on `run`
 * settling) reconnecting, with backoff, -> connected again (on recovery) ->
 * ... forever, notifying subscribers of every state transition. There is no
 * terminal "gave up" state: the daemon is expected to come back eventually,
 * and backoff staying capped at `maxMs` keeps retries cheap in the meantime.
 */
export class ReconnectController {
    readonly #opts: ReconnectControllerOptions;
    #status: ConnectionStatus = { state: 'connecting' };
    #attempt = 0;
    #timer: unknown;
    #stopped = true;
    /** The in-flight run's abort controller, so `stop()` can cancel it directly. */
    #abort: AbortController | undefined;
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

    /**
     * Stop supervising: cancels the pending retry timer (if any) and aborts
     * the in-flight `run`'s signal, so a switched-away-from endpoint's stream
     * is torn down rather than left running. Also suppresses any status
     * transition from that in-flight `run` if it settles after this call.
     */
    stop(): void {
        this.#stopped = true;
        if (this.#timer !== undefined) {
            const clear =
                this.#opts.clearSchedule ??
                ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
            clear(this.#timer);
            this.#timer = undefined;
        }
        this.#abort?.abort();
        this.#abort = undefined;
    }

    async #runNow(): Promise<void> {
        const abort = new AbortController();
        this.#abort = abort;
        try {
            await this.#opts.run(this.#onUp, abort.signal);
            if (this.#stopped) return;
            this.#down(new Error('watch stream ended'));
        } catch (err) {
            if (this.#stopped) return;
            this.#down(err instanceof Error ? err : new Error(String(err)));
        }
    }

    /** Called by `run` on its first event. Transition-only notify — a call after we're already `connected` (there shouldn't be one) is not news. */
    readonly #onUp = (): void => {
        if (this.#stopped) return;
        this.#attempt = 0;
        if (this.#status.state !== 'connected') {
            this.#setStatus({ state: 'connected' });
        }
    };

    /** `run` settled (resolved or rejected) — the connection is down. Move to `reconnecting` and schedule a retry with backoff. */
    #down(error: Error): void {
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
