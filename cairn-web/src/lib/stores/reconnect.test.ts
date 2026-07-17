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

describe('ReconnectController', () => {
    it('transitions connecting -> reconnecting (backoff growing) -> connected on eventual success', async () => {
        let call = 0;
        const probe = vi.fn(async () => {
            call += 1;
            if (call <= 3) throw new Error(`boom-${call}`);
        });
        const statuses: ConnectionStatus[] = [];
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            probe,
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

        await scheduler.fireNext(); // retry #1 -> fails again
        expect(controller.status).toMatchObject({
            state: 'reconnecting',
            attempt: 2,
            retryInMs: 200,
        });

        await scheduler.fireNext(); // retry #2 -> fails again
        expect(controller.status).toMatchObject({
            state: 'reconnecting',
            attempt: 3,
            retryInMs: 400,
        });

        await scheduler.fireNext(); // retry #3 -> succeeds
        expect(controller.status).toEqual({ state: 'connected' });

        expect(statuses.map((s) => s.state)).toEqual([
            'connecting',
            'reconnecting',
            'reconnecting',
            'reconnecting',
            'connected',
        ]);
        expect(probe).toHaveBeenCalledTimes(4);
    });

    it('resets the attempt counter after recovering, so a later failure starts back at attempt 1', async () => {
        let shouldFail = true;
        const probe = vi.fn(async () => {
            if (shouldFail) throw new Error('down');
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            probe,
            steadyIntervalMs: 5_000,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(controller.status).toMatchObject({ state: 'reconnecting', attempt: 1 });

        shouldFail = false;
        await scheduler.fireNext(); // recovers
        expect(controller.status).toEqual({ state: 'connected' });
        expect(scheduler.scheduled).toEqual([{ fn: expect.any(Function), ms: 5_000 }]);

        shouldFail = true;
        await scheduler.fireNext(); // steady re-probe fails
        expect(controller.status).toMatchObject({
            state: 'reconnecting',
            attempt: 1,
            retryInMs: 100,
        });
    });

    it('does not re-notify subscribers on successful steady-state probes while connected', async () => {
        const probe = vi.fn(async () => {});
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            probe,
            steadyIntervalMs: 15_000,
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });
        const statuses: string[] = [];
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start();
        await flush();
        expect(statuses).toEqual(['connecting', 'connected']);

        await scheduler.fireNext(); // steady-state probe #1 succeeds
        await scheduler.fireNext(); // steady-state probe #2 succeeds
        expect(probe).toHaveBeenCalledTimes(3);
        // Still probing on the steady interval, but no new notifications:
        // "connected -> connected" is not a transition.
        expect(statuses).toEqual(['connecting', 'connected']);
        expect(controller.status).toEqual({ state: 'connected' });
    });

    it('stop() prevents any further scheduled probe from running', async () => {
        const probe = vi.fn(async () => {
            throw new Error('down');
        });
        const scheduler = fakeScheduler();
        const controller = new ReconnectController({
            probe,
            backoff: { baseMs: 100, jitter: () => 1 },
            schedule: scheduler.schedule,
            clearSchedule: scheduler.clearSchedule,
        });

        controller.start();
        await flush();
        expect(probe).toHaveBeenCalledTimes(1);

        controller.stop();
        expect(scheduler.scheduled).toEqual([]); // stop() clears the pending retry

        controller.start();
        await flush();
        expect(probe).toHaveBeenCalledTimes(2); // a fresh start() still probes normally
    });
});
