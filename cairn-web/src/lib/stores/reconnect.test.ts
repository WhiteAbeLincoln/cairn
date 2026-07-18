import { describe, expect, it, vi } from 'vitest';
import type { ConnectionStatus } from './reconnect';
import { backoffDelay, ReconnectController } from './reconnect';

describe('backoffDelay', () => {
    it('grows exponentially with attempt, given deterministic jitter', () => {
        const jitter = () => 1; // full jitter at its maximum -> exact exponential value
        expect(backoffDelay(0, { baseMs: 100, maxMs: 10_000, jitter })).toBe(100);
        expect(backoffDelay(1, { baseMs: 100, maxMs: 10_000, jitter })).toBe(200);
        expect(backoffDelay(2, { baseMs: 100, maxMs: 10_000, jitter })).toBe(400);
        expect(backoffDelay(3, { baseMs: 100, maxMs: 10_000, jitter })).toBe(800);
    });

    it('caps at maxMs regardless of how large the attempt grows', () => {
        const jitter = () => 1;
        expect(backoffDelay(10, { baseMs: 250, maxMs: 10_000, jitter })).toBe(10_000);
        expect(backoffDelay(100, { baseMs: 250, maxMs: 10_000, jitter })).toBe(10_000);
    });

    it('scales the jittered fraction within [0, cap)', () => {
        expect(backoffDelay(2, { baseMs: 100, maxMs: 10_000, jitter: () => 0 })).toBe(0);
        expect(backoffDelay(2, { baseMs: 100, maxMs: 10_000, jitter: () => 0.5 })).toBe(200);
    });

    it('defaults to a 250ms base and a 10s cap', () => {
        expect(backoffDelay(0, { jitter: () => 1 })).toBe(250);
        expect(backoffDelay(20, { jitter: () => 1 })).toBe(10_000);
    });
});

/** A fake scheduler: records every (callback, delay) pair instead of using real timers. */
function fakeScheduler() {
    const scheduled: Array<{ fn: () => void; ms: number }> = [];
    return {
        scheduled,
        schedule: (fn: () => void, ms: number) => {
            const handle = { fn, ms };
            scheduled.push(handle);
            return handle;
        },
        clearSchedule: (handle: unknown) => {
            const idx = scheduled.indexOf(handle as { fn: () => void; ms: number });
            if (idx >= 0) scheduled.splice(idx, 1);
        },
        /** Run the oldest still-pending scheduled callback, as if its delay elapsed. */
        fireNext: async () => {
            const next = scheduled.shift();
            if (!next) throw new Error('no scheduled callback to fire');
            next.fn();
            await flush();
        },
    };
}

/** Let pending microtasks (promise chains inside the controller) settle. */
function flush(): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, 0));
}

/** A `run` that never settles on its own — models a healthy, still-open watch stream. Only external abort ends it. */
function openEndedRun(onUp: () => void, signal: AbortSignal): Promise<void> {
    onUp();
    return new Promise<void>((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true });
    });
}

describe('ReconnectController', () => {
    it('start() goes connecting, then run() rejecting before onUp() goes reconnecting at attempt 1', async () => {
        const run = vi.fn(async () => {
            throw new Error('boom-1');
        });
        const statuses: ConnectionStatus[] = [];
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, maxMs: 10_000, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        controller.onStatusChange((s) => statuses.push(s));

        controller.start();
        await flush();

        expect(controller.status).toMatchObject({
            state: 'reconnecting',
            attempt: 1,
            retryInMs: 100,
        });
        expect((controller.status as { error: Error }).error.message).toBe('boom-1');
        expect(statuses.map((s) => s.state)).toEqual(['connecting', 'reconnecting']);
    });

    it('backoff grows across consecutive failures, then onUp() goes connected', async () => {
        let call = 0;
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => {
            call += 1;
            if (call <= 3) return Promise.reject(new Error(`boom-${call}`));
            return openEndedRun(onUp, signal); // stays healthy — proves "connected" sticks
        });
        const statuses: ConnectionStatus[] = [];
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, maxMs: 10_000, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        controller.onStatusChange((s) => statuses.push(s));

        controller.start();
        await flush();
        expect(controller.status).toMatchObject({ attempt: 1, retryInMs: 100 });

        await scheduler.fireNext(); // retry #1 -> fails again
        expect(controller.status).toMatchObject({ attempt: 2, retryInMs: 200 });

        await scheduler.fireNext(); // retry #2 -> fails again
        expect(controller.status).toMatchObject({ attempt: 3, retryInMs: 400 });

        await scheduler.fireNext(); // retry #3 -> onUp() this time
        expect(controller.status).toEqual({ state: 'connected' });

        expect(statuses.map((s) => s.state)).toEqual([
            'connecting',
            'reconnecting',
            'reconnecting',
            'reconnecting',
            'connected',
        ]);
        expect(run).toHaveBeenCalledTimes(4);
        controller.stop(); // tear down the still-open run() from the final attempt
    });

    it('a run() that resolves after onUp() (the stream ending cleanly) still counts as down and schedules a retry', async () => {
        const run = vi.fn(async (onUp: () => void) => {
            onUp(); // goes live...
            // ...then the stream ends on its own (daemon closed it, EOF, etc).
        });
        const statuses: ConnectionStatus[] = [];
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        controller.onStatusChange((s) => statuses.push(s));

        controller.start();
        await flush();
        expect(statuses.map((s) => s.state)).toEqual(['connecting', 'connected', 'reconnecting']);
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });
        expect(run).toHaveBeenCalledTimes(1);

        await scheduler.fireNext(); // scheduled retry calls run() again
        expect(run).toHaveBeenCalledTimes(2);
    });

    it('recovering resets the attempt counter, so a later drop starts back at attempt 1', async () => {
        let call = 0;
        // #1 fails outright; #2 goes live and stays open until externally rejected (simulating a later drop).
        const pending: Array<(err: Error) => void> = [];
        const run = vi.fn((onUp: () => void) => {
            call += 1;
            if (call === 1) return Promise.reject(new Error('down'));
            onUp();
            return new Promise<void>((_resolve, reject) => pending.push(reject));
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });

        await scheduler.fireNext(); // retry -> goes live
        expect(controller.status).toEqual({ state: 'connected' });

        pending[0]?.(new Error('dropped')); // simulate the now-healthy stream dying later
        await flush();
        expect(controller.status).toMatchObject({
            state: 'reconnecting',
            attempt: 1, // not 2 — the counter was reset by the intervening onUp()
            retryInMs: 100,
        });
    });

    it('stop() cancels the pending retry timer and a fresh start() still runs normally', async () => {
        const run = vi.fn(async () => {
            throw new Error('down');
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(run).toHaveBeenCalledTimes(1);

        controller.stop();
        expect(scheduler.scheduled).toEqual([]); // stop() clears the pending retry

        controller.start();
        await flush();
        expect(run).toHaveBeenCalledTimes(2); // a fresh start() still runs normally
    });

    it('a bare start() clears a pending retry timer from a previous down(), so it cannot later supersede the new attempt', async () => {
        let call = 0;
        // #1 fails outright, scheduling a retry timer. #2+ (triggered by the
        // bare start() below, not by that timer) goes live and stays open —
        // this is the state the stale timer would tear down if left uncleared.
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => {
            call += 1;
            if (call === 1) return Promise.reject(new Error('boom'));
            return openEndedRun(onUp, signal);
        });
        const statuses: string[] = [];
        const scheduler = fakeScheduler();
        const clearSchedule = vi.fn(scheduler.clearSchedule);
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule,
        });
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start();
        await flush();
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });
        expect(scheduler.scheduled).toHaveLength(1);
        const staleTimer = scheduler.scheduled[0];

        // A bare start() — e.g. the endpoint store re-dialing directly —
        // before the scheduled retry ever fires.
        controller.start();
        await flush();
        expect(controller.status).toEqual({ state: 'connected' });

        expect(clearSchedule).toHaveBeenCalledWith(staleTimer);
        expect(scheduler.scheduled).not.toContain(staleTimer);

        // Fire the stale callback directly, simulating the one thing a fake
        // timer can do that a real cleared `setTimeout` cannot: prove that
        // even if it somehow ran, it has no observable effect on the new,
        // now-connected attempt (no extra status transitions).
        staleTimer.fn();
        await flush();

        expect(statuses).toEqual(['connecting', 'reconnecting', 'connecting', 'connected']);
        controller.stop();
    });

    it('stop() aborts the in-flight run() signal', async () => {
        let capturedSignal: AbortSignal | undefined;
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => {
            capturedSignal = signal;
            return openEndedRun(onUp, signal);
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(capturedSignal?.aborted).toBe(false);

        controller.stop();
        expect(capturedSignal?.aborted).toBe(true);
    });

    it('each run() attempt gets its own fresh AbortSignal', async () => {
        const signals: AbortSignal[] = [];
        // Stays in flight (like a real watch stream) until aborted — a run
        // that settles on its own never needs its signal aborted, so that
        // wouldn't exercise anything here.
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => {
            signals.push(signal);
            return openEndedRun(onUp, signal);
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        controller.stop(); // aborts signals[0]
        controller.start();
        await flush();

        expect(signals).toHaveLength(2);
        expect(signals[0]).not.toBe(signals[1]);
        expect(signals[0]?.aborted).toBe(true); // aborted by the intervening stop()
        expect(signals[1]?.aborted).toBe(false); // the new attempt's signal is untouched

        controller.stop(); // tear down the still-open second attempt
    });

    it('double start() with no intervening stop() still aborts the superseded attempt (no orphaned AbortController)', async () => {
        const signals: AbortSignal[] = [];
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => {
            signals.push(signal);
            return openEndedRun(onUp, signal);
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start(); // attempt #1 in flight, never stopped
        await flush();
        expect(signals).toHaveLength(1);
        expect(signals[0]?.aborted).toBe(false);

        controller.start(); // called again while #1 is still live — must not orphan it
        await flush();

        expect(signals).toHaveLength(2);
        expect(signals[0]?.aborted).toBe(true); // superseded attempt #1 was aborted, not leaked
        expect(signals[1]?.aborted).toBe(false); // attempt #2 is untouched

        controller.stop();
    });

    it('a late settle of the aborted run() produces no status transition and schedules no retry', async () => {
        let rejectRun!: (err: Error) => void;
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => {
            onUp();
            return new Promise<void>((_resolve, reject) => {
                rejectRun = reject;
                // A well-behaved run would reject promptly on abort; this one
                // is deliberately slow, to prove the controller — not just
                // the run — is what suppresses the stale transition.
                void signal;
            });
        });
        const statuses: string[] = [];
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start();
        await flush();
        expect(statuses).toEqual(['connecting', 'connected']);

        controller.stop();
        rejectRun(new Error('late'));
        await flush();

        expect(statuses).toEqual(['connecting', 'connected']); // no reconnecting after stop()
        expect(scheduler.scheduled).toEqual([]); // and no retry was scheduled either
    });

    /**
     * A `run()` whose promise is captured and settled manually (deferred
     * pattern), so the test controls exactly when the settle lands relative
     * to a stop()+start() that happens while it's still pending — unlike the
     * other tests above, where the fake `run` rejects synchronously and so
     * can never actually straddle a stop()/start() boundary.
     */
    function deferredRun() {
        const calls: Array<{
            onUp: () => void;
            signal: AbortSignal;
            resolve: () => void;
            reject: (err: Error) => void;
        }> = [];
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => {
            return new Promise<void>((resolve, reject) => {
                calls.push({ onUp, signal, resolve, reject });
            });
        });
        return { run, calls };
    }

    it('a stale generation rejecting after stop()+start() does not touch the new generation status/attempt/timer', async () => {
        const { run, calls } = deferredRun();
        const scheduler = fakeScheduler();
        const statuses: string[] = [];
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start(); // generation 1, attempt #1 pending
        await flush();
        expect(calls).toHaveLength(1);

        controller.stop(); // generation -> 2; aborts #1's signal, but our fake run ignores that
        controller.start(); // generation -> 3, attempt #2 pending
        await flush();
        expect(calls).toHaveLength(2);

        calls[1].onUp(); // the CURRENT attempt goes live
        expect(controller.status).toEqual({ state: 'connected' });

        // The STALE attempt #1 finally settles — a real stream's teardown
        // landing a tick or more after abort(), well after start() already
        // moved on.
        calls[0].reject(new Error('stale-drop'));
        await flush();

        expect(controller.status).toEqual({ state: 'connected' }); // untouched by the stale settle
        expect(statuses).toEqual(['connecting', 'connecting', 'connected']); // no spurious reconnecting inserted
        expect(scheduler.scheduled).toEqual([]); // no bogus retry timer from the stale generation
    });

    it("a stale generation's onUp() after stop()+start() does not flip status or reset the attempt counter", async () => {
        const { run, calls } = deferredRun();
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start(); // generation 1, attempt #1 pending
        await flush();
        controller.stop(); // generation -> 2
        controller.start(); // generation -> 3, attempt #2 pending
        await flush();
        expect(calls).toHaveLength(2);

        // Put the CURRENT generation into reconnecting/attempt-1, so a
        // "reset to attempt 0" from the stale onUp() would be observable.
        calls[1].reject(new Error('down'));
        await flush();
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });

        // The STALE attempt #1 calls onUp() — must be inert.
        calls[0].onUp();
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });
    });

    it('does not re-notify subscribers while a single run() stays healthy', async () => {
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => openEndedRun(onUp, signal));
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        const statuses: string[] = [];
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start();
        await flush();
        expect(statuses).toEqual(['connecting', 'connected']);
        expect(run).toHaveBeenCalledTimes(1); // one call supervises the whole healthy period, no re-probing

        controller.stop();
    });
});

describe('ReconnectController establishment timeout (upTimeoutMs)', () => {
    /** A run that stays pending and never reaches `onUp()` — models a zombie
     * socket that connects but never delivers the first snapshot. */
    function neverUpRun(signal: AbortSignal): Promise<void> {
        return new Promise<void>((_resolve, reject) => {
            signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true });
        });
    }

    it('aborts a run that never reaches onUp() and goes reconnecting with a timeout error', async () => {
        let signal: AbortSignal | undefined;
        const run = vi.fn((_onUp: () => void, s: AbortSignal) => {
            signal = s;
            return neverUpRun(s);
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            upTimeoutMs: 5000,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(controller.status).toEqual({ state: 'connecting' });
        expect(scheduler.scheduled.map((s) => s.ms)).toEqual([5000]); // the up-guard, armed

        await scheduler.fireNext(); // up-guard fires
        expect(signal?.aborted).toBe(true); // the stale stream is torn down
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });
        expect((controller.status as { error: Error }).error.message).toContain('5000ms');
    });

    it('declares down even when the aborted run never settles, and its late settle stays inert', async () => {
        // A run that ignores abort entirely — the worst-case wedge. The guard
        // itself must produce the reconnecting transition; waiting for the
        // run to settle would wait forever.
        let rejectRun: ((err: Error) => void) | undefined;
        const run = vi.fn(
            (_onUp: () => void, _s: AbortSignal) =>
                new Promise<void>((_resolve, reject) => {
                    rejectRun = reject;
                }),
        );
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            upTimeoutMs: 5000,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        const statuses: string[] = [];
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start();
        await flush();
        await scheduler.fireNext(); // up-guard fires with the run still pending
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });

        // The wedged run finally rejects, long after the guard declared down:
        // no second reconnecting notification, no double-scheduled retry.
        rejectRun?.(new Error('late wedge death'));
        await flush();
        expect(statuses).toEqual(['connecting', 'reconnecting']);
        expect(scheduler.scheduled).toHaveLength(1); // exactly one retry timer
    });

    it('clears the up-guard once onUp() arrives, so it never fires', async () => {
        const run = vi.fn((onUp: () => void, signal: AbortSignal) => openEndedRun(onUp, signal));
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            run,
            upTimeoutMs: 5000,
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(controller.status).toEqual({ state: 'connected' });
        expect(scheduler.scheduled).toHaveLength(0); // guard cleared, nothing pending

        controller.stop();
    });
});

describe('ReconnectController watchdog', () => {
    function fixture(probe: () => Promise<void>) {
        const scheduler = fakeScheduler();
        let signal: AbortSignal | undefined;
        const run = vi.fn((onUp: () => void, s: AbortSignal) => {
            signal = s;
            return openEndedRun(onUp, s);
        });
        const controller = new ReconnectController({
            run,
            upTimeoutMs: 5000,
            watchdog: { probe, intervalMs: 30_000, timeoutMs: 10_000 },
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        return { scheduler, run, controller, signal: () => signal };
    }

    it('arms on onUp(), and successful probes re-arm without status noise', async () => {
        const probe = vi.fn(async () => {});
        const { scheduler, controller } = fixture(probe);
        const statuses: string[] = [];
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start();
        await flush();
        expect(controller.status).toEqual({ state: 'connected' });
        expect(scheduler.scheduled.map((s) => s.ms)).toEqual([30_000]); // the watchdog tick

        await scheduler.fireNext(); // tick -> probe resolves (its 10s timeout is cleared)
        expect(probe).toHaveBeenCalledTimes(1);
        expect(scheduler.scheduled.map((s) => s.ms)).toEqual([30_000]); // re-armed

        await scheduler.fireNext();
        expect(probe).toHaveBeenCalledTimes(2);
        expect(statuses).toEqual(['connecting', 'connected']); // probes are not news

        controller.stop();
    });

    it('a failing probe aborts the run and goes reconnecting with the probe failure as cause', async () => {
        const probe = vi.fn(async () => {
            throw new Error('probe-boom');
        });
        const { scheduler, controller, signal } = fixture(probe);

        controller.start();
        await flush();
        await scheduler.fireNext(); // tick -> probe rejects
        expect(signal()?.aborted).toBe(true);
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });
        expect((controller.status as { error: Error }).error.message).toContain('probe-boom');
    });

    it('a probe that never settles times out and aborts the run', async () => {
        const probe = vi.fn(() => new Promise<void>(() => {}));
        const { scheduler, controller, signal } = fixture(probe);

        controller.start();
        await flush();
        await scheduler.fireNext(); // tick: probe starts, its timeout timer is armed
        expect(scheduler.scheduled.map((s) => s.ms)).toEqual([10_000]);

        await scheduler.fireNext(); // the probe timeout fires
        expect(signal()?.aborted).toBe(true);
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });
        expect((controller.status as { error: Error }).error.message).toContain('10000ms');
    });

    it('the run settling on its own clears the pending watchdog tick', async () => {
        const probe = vi.fn(async () => {});
        const scheduler = fakeScheduler();
        let die: ((err: Error) => void) | undefined;
        const run = (onUp: () => void, _s: AbortSignal): Promise<void> => {
            onUp();
            return new Promise<void>((_resolve, reject) => {
                die = reject;
            });
        };
        const controller = new ReconnectController({
            run,
            watchdog: { probe, intervalMs: 30_000, timeoutMs: 10_000 },
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(scheduler.scheduled.map((s) => s.ms)).toEqual([30_000]); // watchdog armed

        die?.(new Error('stream died'));
        await flush();
        expect(controller.status).toMatchObject({ state: 'reconnecting' });
        // Only the retry timer remains — the orphaned watchdog tick was
        // cleared, so it can never probe a connection already declared down.
        expect(scheduler.scheduled.map((s) => s.ms)).toEqual([100]);
        expect(probe).not.toHaveBeenCalled();
    });

    it('stop() while connected leaves the pending watchdog tick inert', async () => {
        const probe = vi.fn(async () => {});
        const { scheduler, controller } = fixture(probe);

        controller.start();
        await flush();
        expect(scheduler.scheduled.map((s) => s.ms)).toEqual([30_000]);

        controller.stop();
        // The tick may still be queued (stop() only guarantees inertness, not
        // cancellation) — firing it must not probe or change status.
        while (scheduler.scheduled.length > 0) {
            await scheduler.fireNext();
        }
        expect(probe).not.toHaveBeenCalled();
        expect(controller.status).toEqual({ state: 'connected' }); // untouched since stop()
    });
});
