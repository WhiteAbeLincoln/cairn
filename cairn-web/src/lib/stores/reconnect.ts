// Reconnect / backoff state machine. Lives in the stores layer per the design
// spec ("Reconnect lives in the stores layer, not `DaemonClient`"): dial
// failures flip connection state; jittered exponential backoff capped at 10s.
// `run` wraps a long-lived server-push stream (the daemon's `watch-sessions`)
// rather than a one-shot probe — it stays pending for as long as the stream
// is healthy, and its eventual settling (resolve or reject) is itself the
// "connection dropped" signal. Because "pending" is also what a silently dead
// connection looks like, two timers backstop the stream: an establishment
// deadline (`upTimeoutMs`) and an optional steady-state `watchdog` probe —
// both declare the attempt down and abort it rather than waiting on a settle
// that may never come. Framework-free (no Svelte, no timers imported
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
    /**
     * How long a run may take to reach `onUp()` before the attempt is
     * declared down and retried. Guards against zombie sockets that connect
     * but never deliver the first snapshot: without a deadline such a run
     * stays pending forever and the controller wedges in `connecting`.
     * Default 10s.
     */
    upTimeoutMs?: number;
    /**
     * Steady-state liveness watchdog, armed while a run is up. A quietly
     * *pending* stream is indistinguishable from a silently dead path (NAT
     * drop, network switch — the browser may not notice for minutes), so a
     * cheap probe converts that silence into a verdict; see the mux design
     * spec's Liveness section. Omit to rely on stream death alone.
     */
    watchdog?: WatchdogOptions;
    backoff?: BackoffOptions;
    /** Injectable in place of `setTimeout`, for deterministic tests. */
    schedule?: (fn: () => void, ms: number) => unknown;
    /** Injectable in place of `clearTimeout`. */
    clearSchedule?: (handle: unknown) => void;
}

export interface WatchdogOptions {
    /** The connectivity check. Resolve = healthy; reject = the run is aborted and retried. */
    probe: () => Promise<void>;
    /** Delay between successful probes. Default 30s. */
    intervalMs?: number;
    /** How long a probe may run before it counts as failed (a black-holed path never rejects on its own). Default 10s. */
    timeoutMs?: number;
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
    /**
     * Monotonic generation counter. `start()`, `stop()`, and every retry
     * bump it; each `#runNow()` call captures the value current at its
     * launch. `run()` does not necessarily reject synchronously on abort —
     * the rejection (or a stray `onUp()`) can land a microtask or more
     * later, potentially after `stop()` and a subsequent `start()` have
     * already moved on to a new attempt. Gating every continuation point on
     * `gen === this.#generation` (rather than a single shared `#stopped`
     * boolean) is what makes such a stale settle inert instead of corrupting
     * the new attempt's status/attempt-count/timer.
     */
    #generation = 0;
    /** The in-flight run's abort controller. Set if and only if a run is currently in flight. */
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

    /**
     * A bare `start()` (no intervening `stop()`) can still race a pending
     * backoff retry timer left over from an earlier `#down()` — e.g. the
     * endpoint store re-dialing directly after a failure, without going
     * through `stop()` first. Left uncancelled, that timer fires later,
     * mints a fresh generation, and supersedes (aborts) the very attempt
     * this `start()` just launched. Clear it up front so it can never fire.
     */
    start(): void {
        this.#clearTimer();
        this.#setStatus({ state: 'connecting' });
        const gen = ++this.#generation;
        void this.#runNow(gen);
    }

    /**
     * Stop supervising: cancels the pending retry timer (if any), aborts the
     * in-flight `run`'s signal (so a switched-away-from endpoint's stream is
     * torn down rather than left running), and bumps the generation counter
     * so that run's continuation — even if it settles well after this call
     * returns — can no longer apply any effect.
     */
    stop(): void {
        this.#generation += 1;
        this.#clearTimer();
        this.#abort?.abort();
        this.#abort = undefined;
    }

    /** Cancel and clear the pending retry timer, if any. Shared by `start()` and `stop()`. */
    #clearTimer(): void {
        if (this.#timer !== undefined) {
            this.#clearFn()(this.#timer);
            this.#timer = undefined;
        }
    }

    #scheduleFn(): (fn: () => void, ms: number) => unknown {
        return this.#opts.schedule ?? ((fn, d) => setTimeout(fn, d));
    }

    #clearFn(): (handle: unknown) => void {
        return (
            this.#opts.clearSchedule ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>))
        );
    }

    async #runNow(gen: number): Promise<void> {
        // A bare start() while a previous attempt is still in flight (no
        // intervening stop()) must not orphan that attempt's controller.
        this.#abort?.abort();
        const abort = new AbortController();
        this.#abort = abort;

        const schedule = this.#scheduleFn();
        const clear = this.#clearFn();

        // The guard slot holds whichever attempt-scoped timer is live: the
        // up-guard until `onUp()`, then the watchdog tick. Never both.
        let guardTimer: unknown;
        const clearGuard = (): void => {
            if (guardTimer !== undefined) {
                clear(guardTimer);
                guardTimer = undefined;
            }
        };

        // Every way this attempt can end funnels through here exactly once:
        // normal settle (resolve or reject), up-guard expiry, watchdog probe
        // failure. The first caller wins; later calls — e.g. a wedged run
        // finally rejecting after the up-guard already declared down — are
        // inert, as is anything after a stale generation. Declaring down here
        // rather than waiting for the run to settle is deliberate: a run
        // wedged on a zombie socket may never settle, even when aborted. The
        // abort() is repeat-safe and matters on the timeout paths: it tears
        // the stream down so a zombie can't keep feeding events after the
        // controller has moved on.
        let finished = false;
        const finish = (error: Error): void => {
            // The guard timer is attempt-local, so clean it even when the
            // settle is stale — a superseded attempt must not leave its
            // (inert, but armed) timer behind.
            clearGuard();
            if (finished || gen !== this.#generation) return;
            finished = true;
            this.#abort = undefined;
            abort.abort();
            this.#down(error);
        };

        const upTimeoutMs = this.#opts.upTimeoutMs ?? 10_000;
        guardTimer = schedule(() => {
            guardTimer = undefined;
            finish(new Error(`watch stream not live after ${upTimeoutMs}ms`));
        }, upTimeoutMs);

        const watchdog = this.#opts.watchdog;
        const armWatchdog = (): void => {
            if (!watchdog) return;
            guardTimer = schedule(() => {
                guardTimer = undefined;
                void watchdogTick(watchdog);
            }, watchdog.intervalMs ?? 30_000);
        };
        const watchdogTick = async (dog: WatchdogOptions): Promise<void> => {
            if (finished || gen !== this.#generation) return;
            try {
                await withTimeout(dog.probe(), dog.timeoutMs ?? 10_000, schedule, clear);
                // Re-check: the run may have settled while the probe was in
                // flight — its finish() cleared the guard slot, and re-arming
                // would resurrect a watchdog for a connection already down.
                if (!finished && gen === this.#generation) armWatchdog();
            } catch (err) {
                finish(
                    new Error(
                        `watchdog probe failed: ${err instanceof Error ? err.message : String(err)}`,
                    ),
                );
            }
        };

        let up = false;
        const onUp = (): void => {
            if (finished || gen !== this.#generation || up) return; // stale — superseded or already handled
            up = true;
            clearGuard(); // the establishment deadline is met...
            armWatchdog(); // ...and steady-state supervision takes over
            this.#attempt = 0;
            // Only notify on the connecting/reconnecting -> connected
            // *transition* — re-notifying on a call after we're already
            // connected (there shouldn't be one) is not news.
            if (this.#status.state !== 'connected') {
                this.#setStatus({ state: 'connected' });
            }
        };
        try {
            await this.#opts.run(onUp, abort.signal);
            finish(new Error('watch stream ended'));
        } catch (err) {
            finish(err instanceof Error ? err : new Error(String(err)));
        }
    }

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
        this.#timer = this.#scheduleFn()(() => {
            const gen = ++this.#generation;
            void this.#runNow(gen);
        }, ms);
    }

    #setStatus(status: ConnectionStatus): void {
        this.#status = status;
        for (const listener of this.#listeners) listener(status);
    }
}

/**
 * Race a promise against a deadline armed via the injectable scheduler. The
 * deadline timer is cleared once the race settles either way; a promise that
 * settles *after* losing the race is simply ignored (`race` keeps handlers on
 * both, so a late rejection is not an unhandled rejection).
 */
function withTimeout(
    promise: Promise<void>,
    timeoutMs: number,
    schedule: (fn: () => void, ms: number) => unknown,
    clear: (handle: unknown) => void,
): Promise<void> {
    let handle: unknown;
    return Promise.race([
        promise,
        new Promise<never>((_, reject) => {
            handle = schedule(
                () => reject(new Error(`probe timed out after ${timeoutMs}ms`)),
                timeoutMs,
            );
        }),
    ]).finally(() => clear(handle));
}
