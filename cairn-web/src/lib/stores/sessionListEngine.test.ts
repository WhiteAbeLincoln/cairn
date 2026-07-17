import { describe, expect, it, vi } from 'vitest';
import type { DaemonClient, SessionInfo } from '$lib/protocol';
import { ReconnectController } from './reconnect';
import { SessionListEngine } from './sessionListEngine';

const sampleInfo: SessionInfo = {
    id: '018f9a2b-0000-7000-8000-000000000001',
    name: 'demo',
    pid: 4242,
    cols: 80,
    rows: 24,
    attachedClients: [],
    createdAtUnixMs: 1_700_000_000_000n,
    exit: undefined,
    spec: {
        command: ['bash'],
        env: [],
        envInherit: true,
        tty: true,
        stdin: true,
        scrollbackLines: 1000,
    },
};

function fakeClient(listAll: () => Promise<SessionInfo[]>): DaemonClient {
    return { listAll } as unknown as DaemonClient;
}

describe('SessionListEngine', () => {
    it('sets loading during the fetch and populates sessions on success', async () => {
        const engine = new SessionListEngine();
        const seen: boolean[] = [];
        engine.subscribe(() => seen.push(engine.loading));

        await engine.refresh(fakeClient(async () => [sampleInfo]));

        expect(engine.sessions).toEqual([sampleInfo]);
        expect(engine.error).toBeUndefined();
        expect(engine.loading).toBe(false);
        expect(seen).toEqual([true, false]); // notified on start (loading) and finish
    });

    it('records a display error and rethrows on failure', async () => {
        const engine = new SessionListEngine();
        const client = fakeClient(async () => {
            throw new Error('daemon unreachable');
        });

        await expect(engine.refresh(client)).rejects.toThrow('daemon unreachable');
        expect(engine.error).toBe('daemon unreachable');
        expect(engine.sessions).toEqual([]); // no stale data replaced by a failed refresh
    });

    it('clears a previous error once a later refresh succeeds', async () => {
        const engine = new SessionListEngine();
        let fail = true;
        const client = fakeClient(async () => {
            if (fail) throw new Error('down');
            return [sampleInfo];
        });

        await expect(engine.refresh(client)).rejects.toThrow();
        expect(engine.error).toBe('down');

        fail = false;
        await engine.refresh(client);
        expect(engine.error).toBeUndefined();
        expect(engine.sessions).toEqual([sampleInfo]);
    });

    it('coalesces overlapping refreshes onto one in-flight fetch (no stale-overwrites-fresh race)', async () => {
        const engine = new SessionListEngine();
        let resolveList!: (v: SessionInfo[]) => void;
        const listAll = vi.fn(
            () =>
                new Promise<SessionInfo[]>((resolve) => {
                    resolveList = resolve;
                }),
        );
        const client = fakeClient(listAll);

        const first = engine.refresh(client);
        const second = engine.refresh(client); // overlaps: must join, not re-fetch
        expect(listAll).toHaveBeenCalledTimes(1);

        resolveList([sampleInfo]);
        await Promise.all([first, second]);
        expect(engine.sessions).toEqual([sampleInfo]);

        // Once settled, a new refresh really does fetch again.
        const third = engine.refresh(client);
        expect(listAll).toHaveBeenCalledTimes(2);
        resolveList([]);
        await third;
        expect(engine.sessions).toEqual([]);
    });
});

describe('reconnect drives a session-list refresh (store refresh on reconnect)', () => {
    it('re-fetches the session list on every probe, so recovery implies a fresh list', async () => {
        const engine = new SessionListEngine();
        let call = 0;
        const client = fakeClient(async () => {
            call += 1;
            if (call <= 2) throw new Error(`unreachable-${call}`);
            return [sampleInfo];
        });

        const scheduled: Array<() => void> = [];
        const controller = new ReconnectController({
            probe: () => engine.refresh(client),
            backoff: { baseMs: 10, jitter: () => 0 },
            schedule: (fn) => {
                scheduled.push(fn);
                return fn;
            },
            clearSchedule: () => {},
        });

        controller.start();
        await tick();
        expect(controller.status.state).toBe('reconnecting');
        expect(engine.error).toBe('unreachable-1');
        expect(engine.sessions).toEqual([]);

        scheduled.shift()?.(); // retry #1 -> still fails
        await tick();
        expect(controller.status.state).toBe('reconnecting');
        expect(engine.error).toBe('unreachable-2');

        scheduled.shift()?.(); // retry #2 -> succeeds
        await tick();
        expect(controller.status).toEqual({ state: 'connected' });
        expect(engine.error).toBeUndefined();
        expect(engine.sessions).toEqual([sampleInfo]);
    });

    it('does not swallow the underlying error — SessionListEngine.error mirrors ReconnectController.status', async () => {
        const engine = new SessionListEngine();
        const client = fakeClient(async () => {
            throw new Error('boom');
        });
        const statuses: string[] = [];
        const controller = new ReconnectController({
            probe: () => engine.refresh(client),
            backoff: { baseMs: 10, jitter: () => 0 },
            schedule: () => undefined,
            clearSchedule: () => {},
        });
        controller.onStatusChange((s) => statuses.push(s.state));

        controller.start();
        await tick();

        expect(statuses).toEqual(['connecting', 'reconnecting']);
        expect(engine.error).toBe('boom');
    });
});

/** Let the microtask queue (async refresh/probe chains) settle. */
function tick(): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, 0));
}
