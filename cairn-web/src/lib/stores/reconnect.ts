// Reconnect / backoff state machine. Lives in the stores layer per the design
// spec ("Reconnect lives in the stores layer, not `DaemonClient`"): dial
// failures flip connection state; jittered exponential backoff capped at 10s;
// once healthy, a steady-interval re-probe keeps watching for a dropped
// connection, and transports that notice a death sooner (e.g. the muxed
// WebSocket's onDown) can `kick()` an immediate re-probe instead of waiting
// out the interval. Framework-free (no Svelte, no timers imported directly — both
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
    /**
     * How long a single probe may run before it counts as failed. Guards
     * against zombie sockets that accept writes but never deliver reads: a
     * probe that never settles would otherwise wedge the controller forever
     * (no reschedule, `kick()` no-oping on the in-flight probe). Default 10s.
     */
    probeTimeoutMs?: number;
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
    #probing = false;
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
        this.#clearTimer();
    }

    /**
     * Probe immediately instead of waiting out the current timer — used when a
     * transport notices the connection died (e.g. the muxed WebSocket's
     * `onDown`). Status flips to `connecting` right away (notifying listeners:
     * the connection is suspect), so even a first-try successful re-probe
     * produces a connecting -> connected transition and "refresh on reconnect"
     * subscribers re-fetch. No-op while stopped, and no-op while a probe is
     * already in flight — no status change either (that probe's outcome is
     * about to reschedule anyway; kicking mid-probe must never cause a
     * concurrent double-probe).
     */
    kick(): void {
        if (this.#stopped || this.#probing) return;
        this.#clearTimer();
        this.#setStatus({ state: 'connecting' });
        void this.#runNow();
    }

    async #runNow(): Promise<void> {
        this.#probing = true;
        try {
            await this.#runProbe();
        } finally {
            this.#probing = false;
        }
    }

    async #runProbe(): Promise<void> {
        try {
            await this.#probeWithTimeout();
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

    /**
     * Race the probe against `probeTimeoutMs`, armed via the injectable
     * scheduler. A timed-out probe rejects (flowing into the normal backoff
     * path); if the probe settles first, the timeout timer is cleared. A probe
     * that settles *after* its timeout already fired is simply ignored — the
     * race has settled, and the backoff path has already rescheduled.
     */
    async #probeWithTimeout(): Promise<void> {
        const timeoutMs = this.#opts.probeTimeoutMs ?? 10_000;
        const schedule = this.#opts.schedule ?? ((fn, d) => setTimeout(fn, d));
        let timeoutHandle: unknown;
        try {
            await Promise.race([
                new Promise<never>((_, reject) => {
                    timeoutHandle = schedule(
                        () => reject(new Error(`probe timed out after ${timeoutMs}ms`)),
                        timeoutMs,
                    );
                }),
                this.#opts.probe(),
            ]);
        } finally {
            const clear =
                this.#opts.clearSchedule ??
                ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
            clear(timeoutHandle);
        }
    }

    #clearTimer(): void {
        if (this.#timer !== undefined) {
            const clear =
                this.#opts.clearSchedule ??
                ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
            clear(this.#timer);
            this.#timer = undefined;
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
