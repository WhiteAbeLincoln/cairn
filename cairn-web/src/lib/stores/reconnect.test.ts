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
        const run = vi.fn(async (onUp: () => void, signal: AbortSignal) => {
            signals.push(signal);
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
        controller.stop();
        controller.start();
        await flush();

        expect(signals).toHaveLength(2);
        expect(signals[0]).not.toBe(signals[1]);
        expect(signals[0]?.aborted).toBe(true); // aborted by the intervening stop()
        expect(signals[1]?.aborted).toBe(false); // the new attempt's signal is untouched
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
