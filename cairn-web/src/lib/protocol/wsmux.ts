import type { Transport } from '@bytecodealliance/wrpc';
import { Chan } from '@bytecodealliance/wrpc';
import type { Dialer } from './transport';
import { WS_OPEN, type WsWire, writeWithBackpressure } from './ws';

/**
 * The `cairn-mux-v0` subprotocol: one persistent WebSocket carrying many
 * concurrent wRPC invocations, each on its own logical channel. Every binary
 * WS message is one frame, `[channel_id: u32 BE][flags: u8][payload]`; only
 * this client opens channels (ids assigned at first send, strictly
 * increasing from 1 in send order), each carrying exactly one invocation.
 * FIN half-closes a direction, RST aborts a channel without touching the
 * socket. See the design spec
 * (`docs/superpowers/specs/2026-07-17-ws-mux-design.md`) and the daemon's
 * mirror implementation (`cairn-daemon/src/serve/transport/ws_mux.rs`).
 */
export const MUX_SUBPROTOCOL = 'cairn-mux-v0';

const HEADER_LEN = 5;
const FLAG_FIN = 1;
const FLAG_RST = 1 << 1;
/** Maximum frame payload; larger writes are chunked. */
const MAX_PAYLOAD = 1 << 20;

/**
 * The WebSocket surface the mux needs — structural (rather than the `WebSocket`
 * class) so tests and non-browser hosts can inject their own implementation.
 */
export interface MuxWebSocket extends WsWire {
    binaryType: 'blob' | 'arraybuffer';
    readonly protocol: string;
    onopen: (() => void) | null;
    onmessage: ((ev: { data: unknown }) => void) | null;
    onerror: (() => void) | null;
    onclose: (() => void) | null;
    close(): void;
}

export interface WsMuxOptions {
    /**
     * Fired once per established-socket death (not for dials that never
     * connect — those reject the dial itself). Lets the connection layer
     * flip to "reconnecting" immediately instead of waiting for a probe.
     */
    onDown?: (err: Error) => void;
    /** Socket constructor override for tests; defaults to `new WebSocket(...)`. */
    connect?: (url: string, protocols: string[]) => MuxWebSocket;
}

/** A {@link Dialer} that can be retired when its connection is replaced. */
export type CloseableDialer = Dialer & {
    /**
     * Permanently retire the dialer: close the cached socket (failing every
     * live channel), and reject any future dials. Without this, replacing a
     * connection (endpoint switch, forget) leaks the old socket on both ends
     * — the daemon's keepalive pings hold it open indefinitely.
     */
    close(): void;
};

/** The DOM WebSocket satisfies {@link MuxWebSocket} at runtime; its handler
 * properties are declared with DOM event parameter types that fail the
 * strictly contravariant structural check against our narrower signatures,
 * hence the cast. */
function browserSocket(url: string, protocols: string[]): MuxWebSocket {
    return new WebSocket(url, protocols) as unknown as MuxWebSocket;
}

/**
 * A muxed-WebSocket {@link Dialer}: one lazily-opened, cached socket
 * negotiated to `cairn-mux-v0`; each dial returns a {@link Transport}
 * carrying one wRPC invocation on its own channel. If the socket dies, every
 * live channel fails together (the socket is the health signal), the socket
 * is forgotten, and the next dial redials.
 */
export function wsMuxDialer(url: string, opts: WsMuxOptions = {}): CloseableDialer {
    let current: Promise<MuxConn> | undefined;
    let closed = false;

    const open = (): Promise<MuxConn> => {
        if (closed) return Promise.reject(new Error('mux dialer closed'));
        if (current) return current;
        const attempt = MuxConn.connect(url, opts, () => {
            // Established socket died: forget it so the next dial redials.
            if (current === attempt) current = undefined;
        });
        current = attempt;
        // A dial that never connects is forgotten too (wtDialer pattern).
        attempt.catch(() => {
            if (current === attempt) current = undefined;
        });
        return attempt;
    };

    const dial = async () => (await open()).openChannel();
    return Object.assign(dial, {
        close(): void {
            closed = true;
            const retired = current;
            current = undefined;
            retired?.then(
                (conn) => conn.close(),
                () => {}, // a dial that never connected has nothing to close
            );
        },
    });
}

/** A {@link Transport} that also exposes an explicit close (RST if incomplete). */
interface ChannelTransport extends Transport {
    close(): void;
}

interface ChannelState {
    inbound: Chan<Uint8Array>;
    remoteFin: boolean;
    localFin: boolean;
}

class MuxConn {
    readonly #ws: MuxWebSocket;
    readonly #channels = new Map<number, ChannelState>();
    /** Channels dialed but not yet on the wire (no id assigned) — tracked so
     * connection teardown fails their pending reads too. */
    readonly #pending = new Set<ChannelState>();
    #nextId = 1;
    #down: Error | undefined;

    private constructor(ws: MuxWebSocket) {
        this.#ws = ws;
    }

    static connect(
        url: string,
        opts: WsMuxOptions,
        onForget: () => void,
    ): Promise<MuxConn> {
        return new Promise((resolve, reject) => {
            const make = opts.connect ?? browserSocket;
            const ws = make(url, [MUX_SUBPROTOCOL]);
            ws.binaryType = 'arraybuffer';
            const conn = new MuxConn(ws);
            let opened = false;

            ws.onopen = () => {
                // A daemon that did not select the subprotocol is not
                // speaking mux; there is deliberately no fallback (nothing
                // published depends on one — design spec).
                if (ws.protocol !== MUX_SUBPROTOCOL) {
                    ws.close();
                    reject(
                        new Error(
                            `daemon did not select ${MUX_SUBPROTOCOL} (got ${JSON.stringify(ws.protocol)})`,
                        ),
                    );
                    return;
                }
                opened = true;
                resolve(conn);
            };

            ws.onmessage = (ev) => conn.#onMessage(ev.data);

            let notified = false;
            const fail = (reason: string) => {
                if (!opened) {
                    reject(new Error(reason));
                    return;
                }
                if (notified) return;
                notified = true;
                // A protocol-error teardown may have already recorded the
                // real cause; don't mask it with the generic close reason.
                const err = conn.#down ?? new Error(reason);
                conn.#teardown(err);
                onForget();
                opts.onDown?.(err);
            };
            ws.onerror = () => fail(`WebSocket error for ${url}`);
            ws.onclose = () => fail('WebSocket connection closed');
        });
    }

    /** Open a channel and return its per-invocation transport.
     *
     * The channel id is allocated at the FIRST outbound frame, not here: the
     * daemon opens channels in id order and silently ignores frames for a
     * never-opened id at or below the highest seen (the stale rule), so ids
     * must be strictly increasing in *send* order. Allocating at dial time
     * would make that an accident of scheduling — any caller awaiting
     * between dial and first write could reorder the opens and hang an
     * invocation on both sides. */
    openChannel(): ChannelTransport {
        if (this.#down) throw this.#down;
        const state: ChannelState = {
            inbound: new Chan<Uint8Array>(),
            remoteFin: false,
            localFin: false,
        };
        this.#pending.add(state);
        let id: number | undefined;

        // First outbound frame: put the channel on the wire under the next id.
        const ensureId = (): number => {
            if (id === undefined) {
                id = this.#nextId++;
                this.#pending.delete(state);
                this.#channels.set(id, state);
            }
            return id;
        };

        return {
            read: async (): Promise<Uint8Array | undefined> => {
                const { value, done } = await state.inbound.next();
                return done ? undefined : value;
            },
            write: async (bytes: Uint8Array): Promise<void> => {
                if (this.#down) throw this.#down;
                const id = ensureId();
                // Chunk oversized writes; each frame is one WS message.
                let at = 0;
                do {
                    const chunk = bytes.subarray(at, at + MAX_PAYLOAD);
                    await writeWithBackpressure(this.#ws, buildFrame(id, 0, chunk));
                    at += MAX_PAYLOAD;
                } while (at < bytes.length);
            },
            closeWrite: (): void => {
                if (state.localFin || this.#ws.readyState !== WS_OPEN) return;
                state.localFin = true;
                const id = ensureId();
                this.#ws.send(buildFrame(id, FLAG_FIN, EMPTY));
                this.#forgetIfComplete(id, state);
            },
            close: (): void => {
                if (id === undefined) {
                    // Never reached the wire: the daemon has no channel to
                    // reset. Just discard the local state.
                    this.#pending.delete(state);
                    state.inbound.close();
                    return;
                }
                const live = this.#channels.get(id);
                if (live !== state) return; // already complete or torn down
                this.#channels.delete(id);
                state.inbound.close();
                // Cancelling an incomplete invocation: tell the daemon so it
                // stops serving the channel. A completed one needs nothing.
                if (!(state.localFin && state.remoteFin) && this.#ws.readyState === WS_OPEN) {
                    this.#ws.send(buildFrame(id, FLAG_RST, EMPTY));
                }
            },
        };
    }

    /** Deliberate retirement (endpoint switch / dialer close): fail every
     * channel and close the socket. */
    close(): void {
        this.#teardown(new Error('mux dialer closed'));
        this.#ws.close();
    }

    #onMessage(data: unknown): void {
        if (typeof data === 'string' || !(data instanceof ArrayBuffer)) {
            // Text (or unknown) frames do not exist in muxed mode.
            this.#protocolError('unexpected non-binary WebSocket frame');
            return;
        }
        if (data.byteLength < HEADER_LEN) {
            this.#protocolError(`frame shorter than header (${data.byteLength} bytes)`);
            return;
        }
        const view = new DataView(data);
        const id = view.getUint32(0);
        const flags = view.getUint8(4);
        const payload = new Uint8Array(data, HEADER_LEN);

        const state = this.#channels.get(id);
        if (!state) return; // stale: RST raced data, or channel long gone

        if (flags & FLAG_RST) {
            this.#channels.delete(id);
            state.inbound.close(new Error('channel reset by daemon'));
            return;
        }
        if (payload.length > 0) {
            state.inbound.push(payload);
        }
        if (flags & FLAG_FIN) {
            state.remoteFin = true;
            state.inbound.close();
            this.#forgetIfComplete(id, state);
        }
    }

    #forgetIfComplete(id: number, state: ChannelState): void {
        if (state.localFin && state.remoteFin && this.#channels.get(id) === state) {
            this.#channels.delete(id);
        }
    }

    #protocolError(reason: string): void {
        this.#teardown(new Error(`mux protocol error: ${reason}`));
        this.#ws.close();
    }

    /** Connection died: fail every live channel together — including dialed
     * channels that never reached the wire. Idempotent. */
    #teardown(err: Error): void {
        if (this.#down) return;
        this.#down = err;
        for (const state of this.#channels.values()) {
            state.inbound.close(err);
        }
        this.#channels.clear();
        for (const state of this.#pending) {
            state.inbound.close(err);
        }
        this.#pending.clear();
    }
}

const EMPTY = new Uint8Array(0);

function buildFrame(id: number, flags: number, payload: Uint8Array): Uint8Array {
    const frame = new Uint8Array(HEADER_LEN + payload.length);
    const view = new DataView(frame.buffer);
    view.setUint32(0, id);
    view.setUint8(4, flags);
    frame.set(payload, HEADER_LEN);
    return frame;
}
