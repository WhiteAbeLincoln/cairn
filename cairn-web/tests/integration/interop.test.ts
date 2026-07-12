// The wire-interop gate: the browser protocol stack (DaemonClient + wsDialer +
// the vendored wRPC JS SDK) driven against a REAL cairn-daemon over ws://.
//
// This is the milestone the first web-UI attempt never reached — proof that the
// JS side and the Rust daemon agree on the wire before any UI exists. Every one
// of the 14 WIT functions is exercised end-to-end against live PTY sessions,
// error paths included, plus a CPU-regression guard for the ~600% busy-spin the
// v1 client hit while attached to a quiet session.

import { spawn as spawnProcess } from 'node:child_process';
import { Chan } from '@bytecodealliance/wrpc';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { DaemonHarness, parseCpuTime, sampleCpuFraction } from './harness';
import {
    CairnError,
    type ClientEvent,
    type ServerEvent,
    type SessionSpec,
} from '../../src/lib/protocol';

const enc = (s: string): Uint8Array => new TextEncoder().encode(s);
const dec = (b: Uint8Array): string => new TextDecoder().decode(b);

let harness: DaemonHarness;

beforeAll(async () => {
    harness = await DaemonHarness.start();
});

afterAll(async () => {
    await harness?.stop();
});

// Keep the daemon quiet between tests: SIGKILL every session so a leftover
// interactive session can never perturb the CPU-regression window.
afterEach(async () => {
    const sessions = await harness.client.listAll();
    await Promise.all(
        sessions.map((s) =>
            harness.client.kill(s.id, { tag: 'named', val: 'kill' }).catch(() => {}),
        ),
    );
});

/** A session spec with interactive defaults; override per test. */
function spec(overrides: Partial<SessionSpec> & { command: string[] }): SessionSpec {
    return {
        name: undefined,
        env: [],
        envInherit: true,
        workdir: undefined,
        tty: true,
        stdin: true,
        idleTimeoutSecs: undefined,
        scrollbackLines: 1000,
        ...overrides,
    };
}

/** `it.next()` with a deadline so a wire hang fails the test instead of it. */
function nextWithin<T>(
    it: AsyncIterator<T>,
    ms: number,
    label: string,
): Promise<IteratorResult<T>> {
    let timer: ReturnType<typeof setTimeout>;
    const timeout = new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`timed out waiting for ${label} after ${ms}ms`)), ms);
    });
    return Promise.race([
        it.next().then((r) => {
            clearTimeout(timer);
            return r;
        }),
        timeout,
    ]);
}

describe('meta', () => {
    it('version reports the protocol id and a daemon build string', async () => {
        const info = await harness.client.version();
        expect(info.protocol).toBe('cairn:daemon@0.1.0');
        expect(info.daemon).toMatch(/^cairn-daemon\//);
    });

    it('whoami identifies a loopback ws client as anonymous', async () => {
        // A ws:// peer on loopback with no auth chain resolves to the anonymous
        // identity (contrast with UDS, which is a Unix identity).
        expect(await harness.client.whoami()).toBe('anonymous');
    });

    it('authenticate rejects first-message auth over ws with a coded error', async () => {
        // Only Unix (UDS) identities short-circuit authenticate; a ws client is
        // anonymous, so the daemon reports the feature is unimplemented.
        const rejection = harness.client.authenticate('any-token');
        await expect(rejection).rejects.toBeInstanceOf(CairnError);
        await expect(rejection).rejects.toMatchObject({ code: 'unimplemented' });
    });
});

describe('sessions — unary lifecycle', () => {
    it('create → list → inspect → rename → restart → kick → kill → wait', async () => {
        const created = await harness.client.create(
            spec({ name: 'lifecycle', command: ['cat'] }),
        );
        expect(created.id).toMatch(/[0-9a-f-]{36}/);
        expect(created.name).toBe('lifecycle');
        // The daemon leaves `pid` unset in v0 (registry.rs: "None for v0"); the
        // point here is that the absent `option<u32>` round-trips as undefined.
        expect(created.pid).toBeUndefined();
        expect(created.exit).toBeUndefined();
        expect(created.spec.command).toEqual(['cat']);

        const list = await harness.client.listAll();
        expect(list.map((s) => s.id)).toContain(created.id);

        const inspected = await harness.client.inspect(created.id);
        expect(inspected.id).toBe(created.id);
        expect(inspected.name).toBe('lifecycle');

        await harness.client.rename(created.id, 'lifecycle-renamed');
        expect((await harness.client.inspect(created.id)).name).toBe('lifecycle-renamed');

        // A live session refuses a non-forced restart, then accepts a forced one.
        const unforced = harness.client.restart(created.id, false);
        await expect(unforced).rejects.toMatchObject({ code: 'session.running' });
        await harness.client.restart(created.id, true);
        // Still resolvable (a fresh child under the same id) after the restart.
        expect((await harness.client.inspect(created.id)).id).toBe(created.id);

        // No client is attached, so kick is a well-defined no-op success.
        await expect(harness.client.kick(created.id)).resolves.toBeUndefined();

        await harness.client.kill(created.id, { tag: 'named', val: 'term' }, undefined);
        const exit = await harness.client.wait(created.id);
        // SIGTERM'd: either a signal or a code, and always a stamped exit time.
        expect(exit.unixMs).toBeGreaterThan(0n);
        expect(exit.code ?? exit.signal).toBeDefined();
    });

    it('wait resolves with the exact exit code of a self-exiting process', async () => {
        const created = await harness.client.create(
            spec({ command: ['sh', '-c', 'exit 3'], tty: false, stdin: false }),
        );
        const exit = await harness.client.wait(created.id);
        expect(exit.code).toBe(3);
        expect(exit.signal).toBeUndefined();
    });

    it('inspect of an unknown id rejects with the daemon not-found code', async () => {
        const rejection = harness.client.inspect('00000000-0000-7000-8000-000000000000');
        await expect(rejection).rejects.toBeInstanceOf(CairnError);
        await expect(rejection).rejects.toMatchObject({ code: 'session.not_found' });
    });
});

describe('sessions — send', () => {
    it('injects bytes into a session that surface in its output', async () => {
        const created = await harness.client.create(spec({ command: ['cat'] }));
        const marker = 'send-marker-42';

        async function* input(): AsyncIterable<Uint8Array> {
            yield enc(`${marker}\n`);
        }
        await harness.client.send(created.id, input());

        // `cat` echoes injected stdin; poll the rendered log until it lands.
        await expect
            .poll(
                async () => {
                    let text = '';
                    for await (const batch of harness.client.logs(
                        created.id,
                        { tag: 'all' },
                        false,
                    )) {
                        for (const rec of batch) text += dec(rec);
                    }
                    return text;
                },
                { timeout: 5_000, interval: 100 },
            )
            .toContain(marker);
    });
});

describe('sessions — logs', () => {
    it('tail honors the window and all returns the full output', async () => {
        const created = await harness.client.create(
            spec({
                command: ['sh', '-c', 'printf "alpha\\nbravo\\ncharlie\\n"; sleep 0.2'],
            }),
        );
        await harness.client.wait(created.id);

        const all = await collectLogs(created.id, { tag: 'all' });
        expect(all).toContain('alpha');
        expect(all).toContain('charlie');

        // A zero-line tail is a defined empty window: proves the daemon honors
        // the window argument end-to-end rather than always replaying the buffer.
        const none = await collectLogs(created.id, { tag: 'tail', val: 0 });
        expect(none).not.toContain('charlie');

        const tailSome = await collectLogs(created.id, { tag: 'tail', val: 100 });
        expect(tailSome).toContain('charlie');
    });

    it('follow delivers live output after the snapshot and ends when the session exits', async () => {
        const created = await harness.client.create(spec({ command: ['cat'] }));
        const stream = harness.client.logs(created.id, { tag: 'all' }, true);
        const it = stream[Symbol.asyncIterator]();

        // First batch is the (initially empty) snapshot; consuming it guarantees
        // the follow subscription is live before we inject.
        await nextWithin(it, 5_000, 'logs snapshot');

        const marker = 'live-follow-99';
        async function* input(): AsyncIterable<Uint8Array> {
            yield enc(`${marker}\n`);
        }
        await harness.client.send(created.id, input());

        let seen = '';
        while (!seen.includes(marker)) {
            const next = await nextWithin(it, 5_000, 'live log output');
            if (next.done) throw new Error('follow stream ended before the marker arrived');
            for (const rec of next.value) seen += dec(rec);
        }
        expect(seen).toContain(marker);

        // Killing the session closes the broadcast, which must end the follow.
        await harness.client.kill(created.id, { tag: 'named', val: 'kill' }, undefined);
        let ended = false;
        for (let i = 0; i < 50 && !ended; i++) {
            const next = await nextWithin(it, 5_000, 'follow stream end');
            ended = next.done === true;
        }
        expect(ended).toBe(true);
    });
});

describe('sessions — attach', () => {
    it('snapshot first → input echoed → resize applied → detach ends the stream', async () => {
        const created = await harness.client.create(spec({ command: ['cat'] }));
        const clientEvents = new Chan<ClientEvent>();
        const stream = harness.client.attach(
            created.id,
            { cols: 80, rows: 24, noStdin: false },
            clientEvents,
        );
        const it = stream[Symbol.asyncIterator]();

        const first = await nextWithin(it, 5_000, 'attach snapshot');
        expect(first.done).toBe(false);
        const firstBatch = first.value as ServerEvent[];
        expect(firstBatch[0].tag).toBe('snapshot');

        // Typed input round-trips: the tty line discipline (and cat) echo it back.
        const marker = 'attach-echo-7';
        clientEvents.push({ tag: 'input', val: enc(`${marker}\n`) });
        let echoed = '';
        while (!echoed.includes(marker)) {
            const next = await nextWithin(it, 5_000, 'echoed output');
            if (next.done) throw new Error('attach stream ended before the echo arrived');
            for (const ev of next.value as ServerEvent[]) {
                if (ev.tag === 'output' || ev.tag === 'snapshot') echoed += dec(ev.val);
            }
        }
        expect(echoed).toContain(marker);

        // The first interactive attacher is the leader, so its resize propagates
        // to the PTY — observable through inspect.
        clientEvents.push({ tag: 'resize', val: [100, 40] });
        await expect
            .poll(async () => (await harness.client.inspect(created.id)).cols, {
                timeout: 5_000,
                interval: 100,
            })
            .toBe(100);
        expect((await harness.client.inspect(created.id)).rows).toBe(40);

        // Detach must end the server stream cleanly (done, no error).
        clientEvents.push({ tag: 'detach' });
        clientEvents.close();
        let done = false;
        for (let i = 0; i < 50 && !done; i++) {
            const next = await nextWithin(it, 5_000, 'attach stream end');
            done = next.done === true;
        }
        expect(done).toBe(true);
    });

    it('attach to an unknown id yields a single not-found error event', async () => {
        const clientEvents = new Chan<ClientEvent>();
        const stream = harness.client.attach(
            '00000000-0000-7000-8000-000000000000',
            { cols: 80, rows: 24, noStdin: false },
            clientEvents,
        );
        const it = stream[Symbol.asyncIterator]();
        const first = await nextWithin(it, 5_000, 'attach error event');
        const batch = first.value as ServerEvent[];
        expect(batch[0].tag).toBe('error');
        const ev = batch[0];
        if (ev.tag !== 'error') throw new Error('expected an error event');
        expect(ev.val).toBeInstanceOf(CairnError);
        expect(ev.val.code).toBe('session.not_found');
        clientEvents.close();
    });

    it('kick evicts an attached client with the client.kicked code', async () => {
        const created = await harness.client.create(spec({ command: ['cat'] }));
        const clientEvents = new Chan<ClientEvent>();
        const stream = harness.client.attach(
            created.id,
            { cols: 80, rows: 24, noStdin: false },
            clientEvents,
        );
        const it = stream[Symbol.asyncIterator]();
        await nextWithin(it, 5_000, 'attach snapshot'); // ensure the client is registered

        await harness.client.kick(created.id);

        // Read forward until the kick error surfaces (any interleaved output first).
        let kicked: CairnError | undefined;
        for (let i = 0; i < 50 && !kicked; i++) {
            const next = await nextWithin(it, 5_000, 'kick error event');
            if (next.done) break;
            for (const ev of next.value as ServerEvent[]) {
                if (ev.tag === 'error') kicked = ev.val;
            }
        }
        expect(kicked).toBeInstanceOf(CairnError);
        expect(kicked?.code).toBe('client.kicked');
        clientEvents.close();
    });
});

describe('CPU regression', () => {
    // Non-vacuity guards: the idle assertion below is only meaningful if the
    // sampler can actually observe CPU burn. A sampler stuck at 0 (ps format
    // change, parse regression, wrong pid) would pass `0 < 0.1` forever, so
    // both failure modes are pinned here in the committed suite.

    it('parseCpuTime decodes every ps TIME format', () => {
        expect(parseCpuTime('0:00.00')).toBe(0);
        expect(parseCpuTime('0:02.50')).toBe(2.5);
        expect(parseCpuTime('12:34.56')).toBeCloseTo(754.56);
        expect(parseCpuTime('1:02:03.00')).toBe(3723);
        expect(parseCpuTime('1-02:03:04.00')).toBe(93_784);
    });

    it('sampler reports a busy process as near one full core', async () => {
        // A single-threaded spin loop must read close to 1.0; if the sampler
        // were silently returning 0, this fails and exposes the vacuity.
        const busy = spawnProcess(process.execPath, [
            '-e',
            'const s = Date.now(); while (Date.now() - s < 6000) { Math.sqrt(Math.random()); }',
        ]);
        try {
            const pid = busy.pid;
            if (pid === undefined) throw new Error('busy process has no pid');
            const sample = await sampleCpuFraction(pid, 2_000);
            console.log(
                `[cpu] busy control: ${(sample.fraction * 100).toFixed(2)}% of one core ` +
                    `(${sample.cpuDelta.toFixed(2)}s CPU over ${sample.wallSeconds.toFixed(2)}s wall)`,
            );
            expect(sample.fraction).toBeGreaterThan(0.5);
        } finally {
            busy.kill('SIGKILL');
        }
    });

    it('daemon stays near-idle while a client is attached to a quiet session', async () => {
        // The v1 web client pinned the daemon at ~600% CPU while attached to a
        // quiet session. Hold an idle attach open and assert the daemon accrues
        // essentially no CPU across the window.
        const created = await harness.client.create(spec({ command: ['cat'] }));
        const clientEvents = new Chan<ClientEvent>();
        const stream = harness.client.attach(
            created.id,
            { cols: 80, rows: 24, noStdin: false },
            clientEvents,
        );
        const it = stream[Symbol.asyncIterator]();

        // Consume the snapshot first so the attach is fully established (client
        // registered, subscription live) before the sampling window opens.
        const first = await nextWithin(it, 5_000, 'attach snapshot');
        expect(first.done).toBe(false);
        expect((first.value as ServerEvent[])[0].tag).toBe('snapshot');

        // Then drain the stream in the background so the attach stays alive and
        // any server chatter is consumed; the session is otherwise silent.
        let draining = true;
        const drain = (async () => {
            try {
                while (draining) {
                    const next = await it.next();
                    if (next.done) break;
                }
            } catch {
                // Stream torn down at cleanup; not a failure.
            }
        })();

        const sample = await sampleCpuFraction(harness.pid, 6_000);

        // Assertion is deliberately generous (0.10 == 10% of one core) so CI
        // scheduling noise can't flake it, yet a busy-spin regression (measured
        // in whole cores) fails by a wide margin. Report the number regardless.
        console.log(
            `[cpu] idle daemon: ${(sample.fraction * 100).toFixed(2)}% of one core ` +
                `(${sample.cpuDelta.toFixed(2)}s CPU over ${sample.wallSeconds.toFixed(2)}s wall)`,
        );
        expect(sample.fraction).toBeLessThan(0.1);

        draining = false;
        clientEvents.push({ tag: 'detach' });
        clientEvents.close();
        await drain;
    });
});

/** Collect a non-follow `logs` stream fully and decode it to a string. */
async function collectLogs(
    id: string,
    window: { tag: 'tail'; val: number } | { tag: 'all' },
): Promise<string> {
    let text = '';
    for await (const batch of harness.client.logs(id, window, false)) {
        for (const rec of batch) text += dec(rec);
    }
    return text;
}
