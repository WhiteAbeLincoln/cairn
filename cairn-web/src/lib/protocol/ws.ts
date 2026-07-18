import type { Transport } from '@bytecodealliance/wrpc';
import { Chan } from '@bytecodealliance/wrpc';
import type { Dialer } from './transport';

// Once this many bytes are buffered in the socket, `write` waits for the buffer
// to drain before resolving, so a fast producer cannot outrun a slow socket.
// (The v1 web client's ~600% daemon CPU spike is treated as a regression class:
// apply real backpressure, never spin.)
const WRITE_HIGH_WATER = 1 << 20; // 1 MiB
const DRAIN_POLL_MS = 8;

/** A {@link Transport} that also exposes an explicit full close. */
interface ClosableTransport extends Transport {
    close(): void;
}

/**
 * A WebSocket-backed {@link Dialer}. Each dial opens one `WebSocket` carrying a
 * single wRPC invocation. Binary frames carry wire data; an empty *text* frame
 * is the EOF sentinel in both directions (the `wrpc-websockets` convention).
 */
export function wsDialer(url: string): Dialer {
    return () => connect(url);
}

function connect(url: string): Promise<ClosableTransport> {
    return new Promise((resolve, reject) => {
        const ws = new WebSocket(url);
        ws.binaryType = 'arraybuffer';
        const inbound = new Chan<Uint8Array>();
        let opened = false;

        ws.onopen = () => {
            opened = true;
            resolve(makeTransport(ws, inbound));
        };

        ws.onmessage = (ev: MessageEvent) => {
            const data = ev.data;
            if (typeof data === 'string') {
                // Empty text frame = EOF sentinel; anything else is a protocol error.
                if (data.length === 0) {
                    inbound.close();
                } else {
                    inbound.close(new Error('unexpected non-empty WebSocket text frame'));
                }
                return;
            }
            inbound.push(new Uint8Array(data as ArrayBuffer));
        };

        ws.onerror = () => {
            const err = new Error(`WebSocket error for ${url}`);
            if (opened) {
                inbound.close(err);
            } else {
                reject(err);
            }
        };

        ws.onclose = (ev: CloseEvent) => {
            if (!opened) {
                reject(new Error(`WebSocket closed before opening (code ${ev.code})`));
                return;
            }
            // A clean close after the EOF sentinel is a no-op (Chan.close is
            // idempotent). An early close ends the read side, letting the
            // decoder surface an unexpected-EOF error to whoever is reading.
            inbound.close();
        };
    });
}

function makeTransport(ws: WebSocket, inbound: Chan<Uint8Array>): ClosableTransport {
    return {
        async read(): Promise<Uint8Array | undefined> {
            const { value, done } = await inbound.next();
            return done ? undefined : value;
        },
        write(bytes: Uint8Array): Promise<void> {
            return writeWithBackpressure(ws, bytes);
        },
        closeWrite(): void {
            // Half-close: the empty text frame tells the peer our write side ended.
            if (ws.readyState === WebSocket.OPEN) ws.send('');
        },
        close(): void {
            if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
                ws.close();
            }
        },
    };
}

/** `WebSocket.readyState` value for an open socket (RFC 6455 numbering). */
export const WS_OPEN = 1;

/**
 * The slice of the WebSocket surface the write path needs — structural so
 * `wsMuxDialer` (and test fakes) can share the backpressure logic.
 */
export interface WsWire {
    readonly readyState: number;
    readonly bufferedAmount: number;
    send(data: Uint8Array | string): void;
}

/** Send with backpressure; shared by {@link wsDialer} and `wsMuxDialer`. */
export async function writeWithBackpressure(ws: WsWire, bytes: Uint8Array): Promise<void> {
    if (ws.readyState !== WS_OPEN) {
        throw new Error('WebSocket is not open');
    }
    ws.send(bytes);
    // No `drain` event exists for WebSocket; poll bufferedAmount, but only while
    // it is actually above the high-water mark (idle otherwise, so no busy spin).
    while (ws.bufferedAmount > WRITE_HIGH_WATER && ws.readyState === WS_OPEN) {
        await delay(DRAIN_POLL_MS);
    }
}

function delay(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
}
