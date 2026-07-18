// The muxed-transport gate: a DaemonClient with `control = wsMuxDialer` (one
// persistent `cairn-mux-v0` WebSocket carrying every unary call and `wait`)
// and `streams = wsDialer` (one-shot sockets for attach/logs/send), driven
// against a REAL cairn-daemon. This is the end-to-end proof for the mux work:
// node's `WebSocket` negotiates the subprotocol, many concurrent invocations
// interleave on one socket, and the two-dialer routing holds up on the wire.
//
// Socket usage is asserted directly: the `connect` hook of `wsMuxDialer`
// counts real sockets, so "everything rode one connection" is observed, not
// assumed. (Node ≥ 22's undici `WebSocket` handles `new WebSocket(url,
// ['cairn-mux-v0'])` subprotocol negotiation natively — nothing special is
// needed in the harness.)

import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { DaemonHarness, dec, enc, reapSessions, spec } from './harness';
import {
    DaemonClient,
    type MuxWebSocket,
    type SessionEvent,
    wsDialer,
    wsMuxDialer,
} from '../../src/lib/protocol';

let harness: DaemonHarness;
/** Client under test: muxed control, one-shot streams. */
let muxed: DaemonClient;
/** Real sockets opened by the mux dialer — the single-socket assertion. */
let socketsOpened = 0;

beforeAll(async () => {
    harness = await DaemonHarness.start();
    const control = wsMuxDialer(harness.wsUrl, {
        connect: (url, protocols) => {
            socketsOpened += 1;
            // Same cast as wsmux.ts's browserSocket: the runtime WebSocket
            // satisfies MuxWebSocket, but its DOM handler property types
            // fail the strictly contravariant structural check.
            return new WebSocket(url, protocols) as unknown as MuxWebSocket;
        },
    });
    muxed = new DaemonClient(control, wsDialer(harness.wsUrl));
});

afterAll(async () => {
    await harness?.stop();
});

// Keep the daemon quiet between tests (same discipline as interop.test.ts).
// Cleanup goes through the harness's own one-shot client so it can never
// perturb the mux socket count.
afterEach(async () => {
    await reapSessions(harness.client);
});

describe('muxed control connection', () => {
    it('concurrent unary calls interleave on one socket with correct results', async () => {
        // Fired together: the mux must carry these as concurrent channels on
        // one connection, not serialize dials.
        const [info, list, who] = await Promise.all([
            muxed.version(),
            muxed.listAll(),
            muxed.whoami(),
        ]);
        expect(info.protocol).toBe('cairn:daemon@0.1.0');
        expect(info.daemon).toMatch(/^cairn-daemon\//);
        expect(list).toEqual([]); // fresh daemon: an empty list round-trips
        expect(who).toBe('anonymous');
        expect(socketsOpened).toBe(1);
    });

    it('sequential calls after the burst reuse the cached socket', async () => {
        const first = await muxed.version();
        const second = await muxed.version();
        expect(second.daemon).toBe(first.daemon);
        // Still the one socket from the very first dial — no per-call redials.
        expect(socketsOpened).toBe(1);
    });

    it('a pending wait does not block other calls on the shared socket', async () => {
        const created = await muxed.create(spec({ command: ['cat'] }));
        // `wait` routes over control, so its future holds a mux channel open
        // for the session's whole lifetime…
        const exitP = muxed.wait(created.id);
        // …while other channels on the same socket keep completing.
        const info = await muxed.version();
        expect(info.protocol).toBe('cairn:daemon@0.1.0');

        await muxed.kill(created.id, { tag: 'named', val: 'kill' }, undefined);
        const exit = await exitP;
        expect(exit.unixMs).toBeGreaterThan(0n);
        expect(exit.code ?? exit.signal).toBeDefined();
        expect(socketsOpened).toBe(1);
    });
});

describe('two-dialer routing', () => {
    it('streams ride one-shot sockets while control stays muxed, in one client', async () => {
        // Lifecycle over control (mux): create → send/logs over streams
        // (one-shot) → kill + wait over control. All through the same client.
        const created = await muxed.create(spec({ name: 'mux-routing', command: ['cat'] }));
        expect(created.name).toBe('mux-routing');

        const marker = 'mux-routing-77';
        async function* input(): AsyncIterable<Uint8Array> {
            yield enc(`${marker}\n`);
        }
        await muxed.send(created.id, input());

        // `cat` echoes injected stdin; poll the rendered log until it lands.
        await expect
            .poll(
                async () => {
                    let text = '';
                    for await (const batch of muxed.logs(created.id, { tag: 'all' }, false)) {
                        for (const rec of batch) text += dec(rec);
                    }
                    return text;
                },
                { timeout: 5_000, interval: 100 },
            )
            .toContain(marker);

        await muxed.kill(created.id, { tag: 'named', val: 'term' }, undefined);
        const exit = await muxed.wait(created.id);
        expect(exit.unixMs).toBeGreaterThan(0n);

        // send + every logs poll dialed fresh one-shot sockets; none of that
        // traffic touched (or grew) the mux connection.
        expect(socketsOpened).toBe(1);
    });
});

describe('watch-sessions on the muxed control connection', () => {
    it('holds a live push stream on one mux channel; abort RSTs the channel, not the socket', async () => {
        const controller = new AbortController();
        const events: SessionEvent[] = [];
        const watcher = (async () => {
            for await (const ev of muxed.watchSessions(controller.signal)) {
                events.push(ev);
            }
        })();

        // The stream opens with a snapshot…
        await expect.poll(() => events.length, { timeout: 5_000 }).toBeGreaterThan(0);
        expect(events[0].tag).toBe('snapshot');

        // …and while its channel stays open, unary calls (create) proceed on
        // sibling channels of the SAME socket, and the daemon pushes the
        // resulting upsert back over the held-open stream.
        const created = await muxed.create(spec({ name: 'mux-watch', command: ['cat'] }));
        await expect
            .poll(
                () => events.some((e) => e.tag === 'upsert' && e.val.name === 'mux-watch'),
                { timeout: 5_000 },
            )
            .toBe(true);

        // Exit lands as an upsert carrying the exit status (registry exit
        // watcher → bus → this subscriber), still on the one socket.
        await muxed.kill(created.id, { tag: 'named', val: 'kill' }, undefined);
        await expect
            .poll(
                () =>
                    events.some(
                        (e) => e.tag === 'upsert' && e.val.id === created.id && e.val.exit !== undefined,
                    ),
                { timeout: 5_000 },
            )
            .toBe(true);

        // Cancelling the subscription tears down only its channel: the
        // iterator ends cleanly and the shared socket keeps serving.
        controller.abort();
        await watcher;
        const info = await muxed.version();
        expect(info.protocol).toBe('cairn:daemon@0.1.0');
        expect(socketsOpened).toBe(1);
    });
});
