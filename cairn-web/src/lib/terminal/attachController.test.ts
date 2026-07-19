import { Chan } from '@bytecodealliance/wrpc';
import { describe, expect, it } from 'vitest';
import {
    type AttachInit,
    CairnError,
    type ClientEvent,
    type ExitStatus,
    type ServerEvent,
    type SessionId,
} from '$lib/protocol';
import {
    AttachController,
    type AttachControllerOptions,
    type AttachPhase,
    type AttachSink,
} from './attachController';

const enc = (s: string): Uint8Array => new TextEncoder().encode(s);
const INIT: AttachInit = { cols: 80, rows: 24, noStdin: false };

/**
 * A stand-in for `DaemonClient`: captures the client-event stream the
 * controller passes in (so a test can read what was queued) and returns a
 * `Chan` the test drives as the server-event side.
 */
class FakeClient {
    captured: AsyncIterable<ClientEvent> | undefined;
    init: AttachInit | undefined;
    readonly server = new Chan<ServerEvent[]>();

    attach(
        _id: SessionId,
        init: AttachInit,
        clientEvents: AsyncIterable<ClientEvent>,
    ): AsyncIterable<ServerEvent[]> {
        this.captured = clientEvents;
        this.init = init;
        return this.server;
    }
}

/** A manual timer queue so the resize debounce is exercised without wall time. */
class FakeTimers {
    #handles = new Map<number, () => void>();
    #next = 1;
    readonly set = (fn: () => void, _ms: number): unknown => {
        const id = this.#next++;
        this.#handles.set(id, fn);
        return id;
    };
    readonly clear = (handle: unknown): void => {
        this.#handles.delete(handle as number);
    };
    pending(): number {
        return this.#handles.size;
    }
    runAll(): void {
        const fns = [...this.#handles.values()];
        this.#handles.clear();
        for (const fn of fns) fn();
    }
}

function recordingSink(phases: AttachPhase[] = [], output: Uint8Array[] = []): AttachSink {
    return {
        onSnapshot: (b) => output.push(b),
        onOutput: (b) => output.push(b),
        onPhase: (p) => phases.push(p),
    };
}

/** Yield to the macrotask queue so the controller's server loop drains. */
const tick = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

function makeController(
    fake: FakeClient,
    sink: AttachSink,
    opts: AttachControllerOptions = {},
): AttachController {
    return new AttachController(fake, sink, opts);
}

describe('AttachController — client-event queue', () => {
    it('stop() pushes a final detach then closes the queue', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);
        expect(fake.captured).toBeDefined();
        expect(fake.init).toEqual(INIT);

        c.stop();

        const it = (fake.captured as AsyncIterable<ClientEvent>)[Symbol.asyncIterator]();
        expect(await it.next()).toEqual({ value: { tag: 'detach' }, done: false });
        expect(await it.next()).toEqual({ value: undefined, done: true });
    });

    it('queues input in order and drops writes after teardown', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);

        c.write(enc('a'));
        c.write(enc('b'));
        c.stop();
        c.write(enc('after-teardown')); // no-op: the queue is closed

        const it = (fake.captured as AsyncIterable<ClientEvent>)[Symbol.asyncIterator]();
        expect((await it.next()).value).toEqual({ tag: 'input', val: enc('a') });
        expect((await it.next()).value).toEqual({ tag: 'input', val: enc('b') });
        expect((await it.next()).value).toEqual({ tag: 'detach' });
        expect((await it.next()).done).toBe(true);
    });

    it('stop() is idempotent — a second call does not push another detach', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);
        c.stop();
        c.stop();

        const it = (fake.captured as AsyncIterable<ClientEvent>)[Symbol.asyncIterator]();
        expect((await it.next()).value).toEqual({ tag: 'detach' });
        expect((await it.next()).done).toBe(true);
    });
});

describe('AttachController — resize debounce', () => {
    it('coalesces rapid resizes into one event carrying the latest dims', async () => {
        const timers = new FakeTimers();
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink(), {
            resizeDebounceMs: 100,
            setTimer: timers.set,
            clearTimer: timers.clear,
        });
        c.start('sess', INIT);

        c.resize(100, 40);
        c.resize(120, 50);
        c.resize(90, 30);
        // Each call clears the prior pending timer, so only one stays armed.
        expect(timers.pending()).toBe(1);

        timers.runAll();
        c.stop();

        const it = (fake.captured as AsyncIterable<ClientEvent>)[Symbol.asyncIterator]();
        expect((await it.next()).value).toEqual({ tag: 'resize', val: [90, 30] });
        expect((await it.next()).value).toEqual({ tag: 'detach' });
        expect((await it.next()).done).toBe(true);
    });

    it('does not emit a resize once torn down, even if a timer was armed', async () => {
        const timers = new FakeTimers();
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink(), {
            resizeDebounceMs: 100,
            setTimer: timers.set,
            clearTimer: timers.clear,
        });
        c.start('sess', INIT);

        c.resize(100, 40);
        c.stop(); // cancels the pending resize timer
        expect(timers.pending()).toBe(0);
        timers.runAll(); // nothing to run

        const it = (fake.captured as AsyncIterable<ClientEvent>)[Symbol.asyncIterator]();
        // Straight to detach — no resize slipped through after teardown.
        expect((await it.next()).value).toEqual({ tag: 'detach' });
        expect((await it.next()).done).toBe(true);
    });
});

describe('AttachController — phase / overlay derivation', () => {
    it('starts connecting and reaches attached on the first snapshot', async () => {
        const fake = new FakeClient();
        const phases: AttachPhase[] = [];
        const c = makeController(fake, recordingSink(phases));
        expect(c.phase).toEqual({ kind: 'connecting' });

        c.start('sess', INIT);
        fake.server.push([{ tag: 'snapshot', val: enc('screen') }]);
        await tick();

        expect(c.phase).toEqual({ kind: 'attached' });
        expect(phases).toEqual([{ kind: 'attached' }]);
    });

    it('renders snapshot then output through the sink', async () => {
        const fake = new FakeClient();
        const output: Uint8Array[] = [];
        const c = makeController(fake, recordingSink([], output));
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('initial') }]);
        fake.server.push([{ tag: 'output', val: enc('-more') }]);
        await tick();

        expect(output.map((b) => new TextDecoder().decode(b))).toEqual(['initial', '-more']);
    });

    it('surfaces exited with the daemon-reported status', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('') }]);
        await tick();
        const status: ExitStatus = { code: 3, unixMs: 123n };
        fake.server.push([{ tag: 'exited', val: status }]);
        await tick();

        expect(c.phase).toEqual({ kind: 'exited', status });
    });

    it('surfaces an in-band error (kicked) as a reattachable error phase', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('') }]);
        await tick();
        fake.server.push([{ tag: 'error', val: new CairnError('client.kicked', 'evicted') }]);
        await tick();

        expect(c.phase).toEqual({ kind: 'error', code: 'client.kicked', message: 'evicted' });
    });

    it('treats a stream that ends without exit/detach as a disconnected drop', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('') }]);
        await tick();
        fake.server.close(); // clean end, but no `exited` and we did not stop()
        await tick();

        expect(c.phase).toEqual({ kind: 'disconnected' });
    });

    it('treats a stream error as a disconnected drop carrying the message', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('') }]);
        await tick();
        fake.server.close(new Error('socket closed'));
        await tick();

        expect(c.phase.kind).toBe('disconnected');
        expect((c.phase as { message?: string }).message).toContain('socket closed');
    });

    it('does NOT report disconnected when the stream ends after our own teardown', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('') }]);
        await tick();
        c.stop();
        fake.server.close(); // the daemon ending its side in response to detach
        await tick();

        expect(c.phase).toEqual({ kind: 'attached' });
    });

    it('does NOT report disconnected when the stream ends after exited', async () => {
        const fake = new FakeClient();
        const c = makeController(fake, recordingSink());
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('') }]);
        const status: ExitStatus = { code: 0, unixMs: 1n };
        fake.server.push([{ tag: 'exited', val: status }]);
        await tick();
        fake.server.close();
        await tick();

        expect(c.phase).toEqual({ kind: 'exited', status });
    });

    it('treats a sink throw (e.g. WASM trap) as a disconnected drop', async () => {
        const fake = new FakeClient();
        const phases: AttachPhase[] = [];
        const sink: AttachSink = {
            onSnapshot: () => {
                throw new WebAssembly.RuntimeError('unreachable');
            },
            onOutput: () => {},
            onPhase: (p) => phases.push(p),
        };
        const c = makeController(fake, sink);
        c.start('sess', INIT);

        fake.server.push([{ tag: 'snapshot', val: enc('screen') }]);
        await tick();

        expect(c.phase.kind).toBe('disconnected');
        expect((c.phase as { message?: string }).message).toContain('unreachable');
    });
});
