import { afterEach, describe, expect, it } from 'vitest';
import type { SessionEvent, SessionInfo } from '$lib/protocol';
import { sessionListEngine, SessionListEngine } from './sessionListEngine';

function info(id: string, createdAtUnixMs: bigint, name = id): SessionInfo {
    return {
        id,
        name,
        pid: 4242,
        cols: 80,
        rows: 24,
        attachedClients: [],
        createdAtUnixMs,
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
}

const a = info('018f9a2b-0000-7000-8000-00000000000a', 1_700_000_000_000n);
const b = info('018f9a2b-0000-7000-8000-00000000000b', 1_700_000_001_000n);
const c = info('018f9a2b-0000-7000-8000-00000000000c', 1_700_000_002_000n);

function snapshot(sessions: SessionInfo[]): SessionEvent {
    return { tag: 'snapshot', val: sessions };
}

function upsert(session: SessionInfo): SessionEvent {
    return { tag: 'upsert', val: session };
}

function removed(id: string): SessionEvent {
    return { tag: 'removed', val: id };
}

describe('SessionListEngine', () => {
    it('starts empty and loading, before any event is applied', () => {
        const engine = new SessionListEngine();
        expect(engine.loading).toBe(true);
        expect(engine.sessions).toEqual([]);
    });

    it('a snapshot replaces the whole set and flips loading false', () => {
        const engine = new SessionListEngine();
        engine.applyEvent(snapshot([a, b]));
        expect(engine.loading).toBe(false);
        expect(engine.sessions).toEqual([a, b]);

        // A later snapshot replaces entirely, dropping anything not in it.
        engine.applyEvent(snapshot([c]));
        expect(engine.sessions).toEqual([c]);
    });

    it('upsert inserts a new id', () => {
        const engine = new SessionListEngine();
        engine.applyEvent(snapshot([a]));
        engine.applyEvent(upsert(b));
        expect(engine.sessions).toEqual([a, b]);
    });

    it('upsert replaces an existing id rather than duplicating it', () => {
        const engine = new SessionListEngine();
        engine.applyEvent(snapshot([a]));
        const renamed = { ...a, name: 'renamed' };
        engine.applyEvent(upsert(renamed));
        expect(engine.sessions).toEqual([renamed]);
    });

    it('removed deletes an existing id', () => {
        const engine = new SessionListEngine();
        engine.applyEvent(snapshot([a, b]));
        engine.applyEvent(removed(a.id));
        expect(engine.sessions).toEqual([b]);
    });

    it('removed for an unknown id is a no-op', () => {
        const engine = new SessionListEngine();
        engine.applyEvent(snapshot([a]));
        engine.applyEvent(removed('018f9a2b-0000-7000-8000-0000000000ff'));
        expect(engine.sessions).toEqual([a]);
    });

    it('orders by createdAtUnixMs ascending regardless of event arrival order', () => {
        const engine = new SessionListEngine();
        // Applied out of chronological order — the getter must still sort.
        engine.applyEvent(snapshot([c]));
        engine.applyEvent(upsert(a));
        engine.applyEvent(upsert(b));
        expect(engine.sessions).toEqual([a, b, c]);
    });

    it('breaks ties on equal createdAtUnixMs by id', () => {
        const engine = new SessionListEngine();
        const sameTime1 = info('018f9a2b-0000-7000-8000-00000000000z', 1_700_000_000_000n);
        const sameTime2 = info('018f9a2b-0000-7000-8000-000000000001', 1_700_000_000_000n);
        engine.applyEvent(snapshot([sameTime1, sameTime2]));
        expect(engine.sessions).toEqual([sameTime2, sameTime1]); // '...001' < '...z'
    });

    it('reset() returns to empty + loading, until the next snapshot', () => {
        const engine = new SessionListEngine();
        engine.applyEvent(snapshot([a]));
        expect(engine.loading).toBe(false);

        engine.reset();
        expect(engine.loading).toBe(true);
        expect(engine.sessions).toEqual([]);

        engine.applyEvent(snapshot([b]));
        expect(engine.loading).toBe(false);
        expect(engine.sessions).toEqual([b]);
    });

    it('notifies listeners once per applied event', () => {
        const engine = new SessionListEngine();
        let notifications = 0;
        engine.subscribe(() => {
            notifications += 1;
        });

        engine.applyEvent(snapshot([a]));
        engine.applyEvent(upsert(b));
        engine.applyEvent(removed(a.id));
        expect(notifications).toBe(3);
    });

    it('notifies on reset() too, so a reactive wrapper sees the loading flip immediately', () => {
        const engine = new SessionListEngine();
        engine.applyEvent(snapshot([a]));
        let notifications = 0;
        engine.subscribe(() => {
            notifications += 1;
        });

        engine.reset();
        expect(notifications).toBe(1);
    });

    it('an unsubscribed listener stops receiving notifications', () => {
        const engine = new SessionListEngine();
        let notifications = 0;
        const unsubscribe = engine.subscribe(() => {
            notifications += 1;
        });

        engine.applyEvent(snapshot([a]));
        unsubscribe();
        engine.applyEvent(upsert(b));
        expect(notifications).toBe(1);
    });
});

describe('sessionListEngine singleton', () => {
    afterEach(() => {
        sessionListEngine.reset();
    });

    it('is a shared instance that folds applied events like any other engine', () => {
        expect(sessionListEngine.loading).toBe(true);
        sessionListEngine.applyEvent(snapshot([a]));
        expect(sessionListEngine.loading).toBe(false);
        expect(sessionListEngine.sessions).toEqual([a]);
    });
});
