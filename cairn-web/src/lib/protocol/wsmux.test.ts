import { describe, expect, it } from 'vitest';
import type { Transport } from './transport';
import { MUX_SUBPROTOCOL, type MuxWebSocket, type WsMuxOptions, wsMuxDialer } from './wsmux';

const HEADER_LEN = 5;
const FLAG_FIN = 1;
const FLAG_RST = 1 << 1;

/** Let queued microtasks/macrotasks (socket callbacks, Chan pumps) run. */
const tick = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

class MockWebSocket implements MuxWebSocket {
    binaryType: 'blob' | 'arraybuffer' = 'blob';
    readyState = 0; // CONNECTING
    bufferedAmount = 0;
    protocol = '';
    sent: (Uint8Array | string)[] = [];
    closed = false;
    onopen: (() => void) | null = null;
    onmessage: ((ev: { data: unknown }) => void) | null = null;
    onerror: (() => void) | null = null;
    onclose: (() => void) | null = null;

    constructor(
        readonly url: string,
        readonly protocols: string[],
    ) {}

    send(data: Uint8Array | string): void {
        this.sent.push(data);
    }

    close(): void {
        this.readyState = 3; // CLOSED
        this.closed = true;
    }

    // ── test controls ──────────────────────────────────────────────────────

    /** Complete the handshake with the given selected subprotocol. */
    open(selected = MUX_SUBPROTOCOL): void {
        this.readyState = 1; // OPEN
        this.protocol = selected;
        this.onopen?.();
    }

    /** Deliver a server frame. */
    receive(id: number, flags: number, payload: Uint8Array = new Uint8Array(0)): void {
        const frame = new Uint8Array(HEADER_LEN + payload.length);
        const view = new DataView(frame.buffer);
        view.setUint32(0, id);
        view.setUint8(4, flags);
        frame.set(payload, HEADER_LEN);
        // Hand over a tight ArrayBuffer, as a browser socket would.
        this.onmessage?.({ data: frame.buffer });
    }

    /** Kill the connection from the network side. */
    drop(): void {
        this.readyState = 3;
        this.onclose?.();
    }
}

/** A dialer wired to mock sockets, plus access to every socket it opened. */
function muxFixture(opts: Omit<WsMuxOptions, 'connect'> = {}) {
    const sockets: MockWebSocket[] = [];
    const dial = wsMuxDialer('ws://daemon.test/ws', {
        ...opts,
        connect: (url, protocols) => {
            const ws = new MockWebSocket(url, protocols);
            sockets.push(ws);
            return ws;
        },
    });
    return { dial, sockets };
}

/** Dial and complete the handshake on the (single) underlying socket. */
async function dialOpen(
    fixture: ReturnType<typeof muxFixture>,
): Promise<{ transport: Transport & { close?: () => void }; ws: MockWebSocket }> {
    const pending = fixture.dial();
    await tick();
    const ws = fixture.sockets.at(-1);
    if (!ws) throw new Error('dial did not open a socket');
    if (ws.readyState === 0) ws.open();
    const transport = await pending;
    return { transport, ws };
}

function sentFrames(ws: MockWebSocket): Array<{ id: number; flags: number; payload: Uint8Array }> {
    return ws.sent.map((data) => {
        if (typeof data === 'string') throw new Error('mux must never send text frames');
        const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
        return {
            id: view.getUint32(0),
            flags: view.getUint8(4),
            payload: data.slice(HEADER_LEN),
        };
    });
}

const bytes = (s: string): Uint8Array => new TextEncoder().encode(s);
const text = (b: Uint8Array): string => new TextDecoder().decode(b);

describe('wsMuxDialer', () => {
    it('offers the subprotocol and shares one socket across dials with increasing ids', async () => {
        const fixture = muxFixture();

        const { transport: t1, ws } = await dialOpen(fixture);
        expect(ws.protocols).toEqual([MUX_SUBPROTOCOL]);
        expect(ws.binaryType).toBe('arraybuffer');

        const t2 = await fixture.dial();
        expect(fixture.sockets).toHaveLength(1); // same socket, no re-dial

        await t1.write(bytes('first'));
        await t2.write(bytes('second'));

        const frames = sentFrames(ws);
        expect(frames).toHaveLength(2);
        expect(frames[0]).toMatchObject({ id: 1, flags: 0 });
        expect(text(frames[0].payload)).toBe('first');
        expect(frames[1]).toMatchObject({ id: 2, flags: 0 });
        expect(text(frames[1].payload)).toBe('second');
    });

    it('demuxes inbound frames to the right channel, FIN ends with EOF', async () => {
        const fixture = muxFixture();
        const { transport: t1, ws } = await dialOpen(fixture);
        const t2 = await fixture.dial();

        ws.receive(2, 0, bytes('for-two'));
        ws.receive(1, FLAG_FIN, bytes('for-one-and-done'));

        expect(text((await t1.read()) ?? new Uint8Array())).toBe('for-one-and-done');
        expect(await t1.read()).toBeUndefined(); // FIN => EOF
        expect(text((await t2.read()) ?? new Uint8Array())).toBe('for-two');

        ws.receive(2, FLAG_FIN);
        expect(await t2.read()).toBeUndefined();
    });

    it('rejects reads on RST without disturbing other channels', async () => {
        const fixture = muxFixture();
        const { transport: t1, ws } = await dialOpen(fixture);
        const t2 = await fixture.dial();

        ws.receive(1, FLAG_RST);
        await expect(t1.read()).rejects.toThrow('channel reset by daemon');

        ws.receive(2, FLAG_FIN, bytes('fine'));
        expect(text((await t2.read()) ?? new Uint8Array())).toBe('fine');
    });

    it('closeWrite sends an empty FIN frame once', async () => {
        const fixture = muxFixture();
        const { transport, ws } = await dialOpen(fixture);

        transport.closeWrite?.();
        transport.closeWrite?.(); // idempotent

        const frames = sentFrames(ws);
        expect(frames).toHaveLength(1);
        expect(frames[0]).toMatchObject({ id: 1, flags: FLAG_FIN });
        expect(frames[0].payload).toHaveLength(0);
    });

    it('close() on an incomplete channel sends RST; after clean completion sends nothing', async () => {
        const fixture = muxFixture();
        const { transport: cancelled, ws } = await dialOpen(fixture);
        const finished = await fixture.dial();

        // Channel 2 completes cleanly: FIN both ways.
        finished.closeWrite?.();
        ws.receive(2, FLAG_FIN);
        await tick();
        (finished as { close?: () => void }).close?.();

        // Channel 1 is cancelled mid-flight.
        (cancelled as { close?: () => void }).close?.();

        const frames = sentFrames(ws);
        // Only channel 2's FIN and channel 1's RST — no RST for channel 2.
        expect(frames.map((f) => [f.id, f.flags])).toEqual([
            [2, FLAG_FIN],
            [1, FLAG_RST],
        ]);
    });

    it('ignores stale frames for unknown channels', async () => {
        const fixture = muxFixture();
        const { transport, ws } = await dialOpen(fixture);

        ws.receive(99, 0, bytes('stale'));
        ws.receive(99, FLAG_RST);

        // Connection is unaffected: the live channel still works.
        ws.receive(1, FLAG_FIN, bytes('alive'));
        expect(text((await transport.read()) ?? new Uint8Array())).toBe('alive');
        expect(ws.closed).toBe(false);
    });

    it('chunks writes larger than the frame payload limit', async () => {
        const fixture = muxFixture();
        const { transport, ws } = await dialOpen(fixture);

        const big = new Uint8Array((1 << 20) + 1);
        await transport.write(big);

        const frames = sentFrames(ws);
        expect(frames).toHaveLength(2);
        expect(frames[0].payload).toHaveLength(1 << 20);
        expect(frames[1].payload).toHaveLength(1);
        expect(frames.every((f) => f.id === 1)).toBe(true);
    });

    it('socket death fails all live channels, fires onDown once, and redials next time', async () => {
        let downs = 0;
        const fixture = muxFixture({ onDown: () => downs++ });
        const { transport: t1, ws } = await dialOpen(fixture);
        const t2 = await fixture.dial();

        const pendingRead = t1.read();
        ws.drop();

        await expect(pendingRead).rejects.toThrow('WebSocket connection closed');
        await expect(t2.read()).rejects.toThrow('WebSocket connection closed');
        await expect(t1.write(bytes('x'))).rejects.toThrow('WebSocket is not open');
        expect(downs).toBe(1);

        // Next dial opens a fresh socket with fresh channel ids.
        const { transport: t3, ws: ws2 } = await dialOpen(fixture);
        expect(fixture.sockets).toHaveLength(2);
        await t3.write(bytes('again'));
        expect(sentFrames(ws2)[0]).toMatchObject({ id: 1, flags: 0 });
    });

    it('rejects the dial when the daemon does not select the subprotocol', async () => {
        const fixture = muxFixture();
        const pending = fixture.dial();
        await tick();
        fixture.sockets[0].open(''); // no echo => browser-equivalent failure
        await expect(pending).rejects.toThrow('did not select cairn-mux-v0');
        expect(fixture.sockets[0].closed).toBe(true);
    });

    it('treats non-binary frames as a protocol error killing the connection', async () => {
        const fixture = muxFixture();
        const { transport, ws } = await dialOpen(fixture);

        ws.onmessage?.({ data: 'text is not allowed' });

        await expect(transport.read()).rejects.toThrow('mux protocol error');
        expect(ws.closed).toBe(true);
    });
});
