// Framework-free attach lifecycle. Owns the client-event queue and the
// server-event loop for one attach to one session, translating each into
// callbacks (`AttachSink`) a view layer renders. No Svelte, no wterm, no DOM —
// so the queue open/close semantics, resize debounce, and overlay-state
// derivation are all unit-testable without a browser (Terminal.svelte is the
// thin wrapper that wires this to wterm and the DOM).
//
// Backpressure: the outbound queue is the SDK `Chan` (awaitable, no polling)
// and the server loop `for await`s the SDK async iterable — no spin loops
// anywhere. The socket-level write backpressure lives in the ws transport.

import { Chan } from '$lib/protocol';
import type {
    AttachInit,
    ClientEvent,
    ExitStatus,
    ServerEvent,
    SessionId,
} from '$lib/protocol';

/**
 * The observable state of an attach, derived from the server-event stream and
 * its lifecycle. Drives the overlay shown over the terminal.
 */
export type AttachPhase =
    | { readonly kind: 'connecting' }
    | { readonly kind: 'attached' }
    | { readonly kind: 'exited'; readonly status: ExitStatus }
    // In-band `error` event (`client.kicked`, `client.lagged`, …): the daemon
    // ended our attach deliberately; the view offers a reattach action.
    | { readonly kind: 'error'; readonly code: string; readonly message: string }
    // The stream dropped without an `exited`/`error` and without us tearing it
    // down (daemon restart, network): reconnectable, view offers reattach.
    | { readonly kind: 'disconnected'; readonly message?: string };

/** Callbacks the controller invokes as the attach progresses. */
export interface AttachSink {
    /** The daemon's guaranteed first event — full screen state, the first paint. */
    onSnapshot(bytes: Uint8Array): void;
    /** An incremental output batch. */
    onOutput(bytes: Uint8Array): void;
    /** A phase transition (also readable synchronously via `controller.phase`). */
    onPhase(phase: AttachPhase): void;
}

/**
 * The slice of `DaemonClient` the controller needs. Declared structurally so
 * the real client satisfies it and a fake can stand in for tests.
 */
export interface AttachClient {
    attach(
        id: SessionId,
        init: AttachInit,
        clientEvents: AsyncIterable<ClientEvent>,
    ): AsyncIterable<ServerEvent[]>;
}

export interface AttachControllerOptions {
    /** Coalescing window for resize events. Default 100ms (per the design spec). */
    resizeDebounceMs?: number;
    /** Injectable in place of `setTimeout`, for deterministic tests. */
    setTimer?: (fn: () => void, ms: number) => unknown;
    /** Injectable in place of `clearTimeout`. */
    clearTimer?: (handle: unknown) => void;
}

const DEFAULT_RESIZE_DEBOUNCE_MS = 100;

export class AttachController {
    readonly #client: AttachClient;
    readonly #sink: AttachSink;
    readonly #resizeDebounceMs: number;
    readonly #setTimer: (fn: () => void, ms: number) => unknown;
    readonly #clearTimer: (handle: unknown) => void;

    readonly #events = new Chan<ClientEvent>();
    #phase: AttachPhase = { kind: 'connecting' };
    // `#tornDown`: we initiated teardown; a subsequent stream end is expected.
    #tornDown = false;
    // `#terminal`: a terminal server event (exited / in-band error) already
    // fired, so the stream ending afterwards is expected, not a drop.
    #terminal = false;
    #resizeTimer: unknown = undefined;
    #pendingResize: [number, number] | undefined;
    #loop: Promise<void> | undefined;

    constructor(client: AttachClient, sink: AttachSink, options: AttachControllerOptions = {}) {
        this.#client = client;
        this.#sink = sink;
        this.#resizeDebounceMs = options.resizeDebounceMs ?? DEFAULT_RESIZE_DEBOUNCE_MS;
        this.#setTimer = options.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
        this.#clearTimer =
            options.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
    }

    get phase(): AttachPhase {
        return this.#phase;
    }

    /** Begin the attach. Idempotent — a second call is a no-op. */
    start(id: SessionId, init: AttachInit): void {
        if (this.#loop) return;
        this.#loop = this.#run(id, init);
    }

    /** Queue keystrokes / pasted bytes as an `input` event. */
    write(bytes: Uint8Array): void {
        if (this.#tornDown) return;
        this.#events.push({ tag: 'input', val: bytes });
    }

    /**
     * Request a resize. Debounced: only the latest dimensions within the window
     * are sent, so a drag-resize doesn't flood the PTY with SIGWINCH churn.
     */
    resize(cols: number, rows: number): void {
        if (this.#tornDown) return;
        this.#pendingResize = [cols, rows];
        if (this.#resizeTimer !== undefined) this.#clearTimer(this.#resizeTimer);
        this.#resizeTimer = this.#setTimer(() => {
            this.#resizeTimer = undefined;
            const dims = this.#pendingResize;
            this.#pendingResize = undefined;
            if (dims && !this.#tornDown) this.#events.push({ tag: 'resize', val: dims });
        }, this.#resizeDebounceMs);
    }

    /**
     * Tear down the attach: push a final `detach`, then close the client-event
     * queue. Closing ends the SDK invocation's write side (empty-text EOF),
     * which lets the daemon end the server stream; `DaemonClient.attach` then
     * closes the socket as the cleanup backstop. Detach-then-close is the
     * normal path; the socket close covers a peer that never drains the detach.
     * Synchronous and idempotent — safe to call from a Svelte unmount cleanup.
     */
    stop(): void {
        if (this.#tornDown) return;
        this.#tornDown = true;
        if (this.#resizeTimer !== undefined) {
            this.#clearTimer(this.#resizeTimer);
            this.#resizeTimer = undefined;
        }
        this.#events.push({ tag: 'detach' });
        this.#events.close();
    }

    async #run(id: SessionId, init: AttachInit): Promise<void> {
        try {
            const stream = this.#client.attach(id, init, this.#events);
            for await (const batch of stream) {
                for (const ev of batch) this.#dispatch(ev);
            }
            // Server stream ended cleanly. Expected only if we tore it down or a
            // terminal event already fired; otherwise it dropped underneath us.
            if (!this.#tornDown && !this.#terminal) {
                this.#setPhase({ kind: 'disconnected' });
            }
        } catch (err) {
            if (!this.#tornDown && !this.#terminal) {
                this.#setPhase({
                    kind: 'disconnected',
                    message: err instanceof Error ? err.message : String(err),
                });
            }
        } finally {
            // Complete the outbound writer even when the daemon ended the stream
            // first (e.g. `exited`) and we never called `stop()`. Idempotent.
            this.#events.close();
        }
    }

    #dispatch(ev: ServerEvent): void {
        switch (ev.tag) {
            case 'snapshot':
                this.#sink.onSnapshot(ev.val);
                if (this.#phase.kind === 'connecting') this.#setPhase({ kind: 'attached' });
                break;
            case 'output':
                this.#sink.onOutput(ev.val);
                break;
            case 'exited':
                this.#terminal = true;
                this.#setPhase({ kind: 'exited', status: ev.val });
                break;
            case 'error':
                this.#terminal = true;
                this.#setPhase({ kind: 'error', code: ev.val.code, message: ev.val.message });
                break;
        }
    }

    #setPhase(phase: AttachPhase): void {
        this.#phase = phase;
        this.#sink.onPhase(phase);
    }
}
