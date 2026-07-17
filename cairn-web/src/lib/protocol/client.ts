import type { Type, Value } from '@bytecodealliance/wrpc';
import { invoke, t } from '@bytecodealliance/wrpc';
import type { Dialer, Transport } from './transport';
import {
    type AttachInit,
    CairnError,
    type ClientEvent,
    type ExitStatus,
    type LogWindow,
    type ServerEvent,
    type SessionId,
    type SessionInfo,
    type SessionSpec,
    type Signal,
    type VersionInfo,
} from './types';
import * as wit from './wit';

/**
 * A typed client for the `cairn:daemon@0.1.0` wRPC interface.
 *
 * Stateless per call: every method dials a fresh {@link Transport} via the
 * injected {@link Dialer}, runs exactly one wRPC invocation over it, and closes
 * it when the call (or its stream) completes. Unary methods return promises
 * that reject with a {@link CairnError} when the daemon returns `result::err`;
 * streaming methods (`attach`, `logs`) return async iterables and `wait`
 * returns a promise resolved by the daemon's `future`.
 *
 * The `ctx` (`call-context`) parameter of every WIT method is sent as `none`
 * for now; trace propagation can be threaded through later without changing
 * these signatures.
 */
export class DaemonClient {
    readonly #dial: Dialer;

    constructor(dialer: Dialer) {
        this.#dial = dialer;
    }

    // --- sessions -----------------------------------------------------------

    async listAll(): Promise<SessionInfo[]> {
        const [list] = await this.#unary(
            wit.SESSIONS_INSTANCE,
            'list-all',
            [t.option(wit.callContext)],
            [NO_CTX],
            [t.list(wit.sessionInfo)],
        );
        return list as unknown as SessionInfo[];
    }

    async inspect(id: SessionId): Promise<SessionInfo> {
        const [res] = await this.#unary(
            wit.SESSIONS_INSTANCE,
            'inspect',
            [t.option(wit.callContext), t.string],
            [NO_CTX, id],
            [t.result(wit.sessionInfo, wit.error)],
        );
        return unwrapResult<SessionInfo>(res);
    }

    async create(spec: SessionSpec): Promise<SessionInfo> {
        const [res] = await this.#unary(
            wit.SESSIONS_INSTANCE,
            'create',
            [t.option(wit.callContext), wit.sessionSpec],
            [NO_CTX, spec],
            [t.result(wit.sessionInfo, wit.error)],
        );
        return unwrapResult<SessionInfo>(res);
    }

    async rename(id: SessionId, newName: string): Promise<void> {
        const [res] = await this.#unary(
            wit.SESSIONS_INSTANCE,
            'rename',
            [t.option(wit.callContext), t.string, t.string],
            [NO_CTX, id, newName],
            [t.result(null, wit.error)],
        );
        unwrapResult<void>(res);
    }

    async restart(id: SessionId, force: boolean): Promise<void> {
        const [res] = await this.#unary(
            wit.SESSIONS_INSTANCE,
            'restart',
            [t.option(wit.callContext), t.string, t.bool],
            [NO_CTX, id, force],
            [t.result(null, wit.error)],
        );
        unwrapResult<void>(res);
    }

    async kill(id: SessionId, sig: Signal, graceMs?: number): Promise<void> {
        const [res] = await this.#unary(
            wit.SESSIONS_INSTANCE,
            'kill',
            [t.option(wit.callContext), t.string, wit.signal, t.option(t.u32)],
            [NO_CTX, id, sig, graceMs],
            [t.result(null, wit.error)],
        );
        unwrapResult<void>(res);
    }

    async kick(id: SessionId, client?: string): Promise<void> {
        const [res] = await this.#unary(
            wit.SESSIONS_INSTANCE,
            'kick',
            [t.option(wit.callContext), t.string, t.option(t.string)],
            [NO_CTX, id, client],
            [t.result(null, wit.error)],
        );
        unwrapResult<void>(res);
    }

    /**
     * Wait for a session to exit. Resolves with its {@link ExitStatus} (the
     * daemon delivers it as a `future<exit-status>`).
     */
    async wait(id: SessionId): Promise<ExitStatus> {
        const transport = await this.#dial();
        try {
            const { results, done } = await invoke(
                transport,
                wit.SESSIONS_INSTANCE,
                'wait',
                [t.option(wit.callContext), t.string],
                asArgs([NO_CTX, id]),
                [t.future(wit.exitStatus)],
            );
            const status = (await (results[0] as Promise<unknown>)) as ExitStatus;
            await done;
            return status;
        } finally {
            closeTransport(transport);
        }
    }

    /**
     * Stream a session's output log. Each yielded item is one wire chunk — a
     * batch of `list<u8>` records. `window` selects a tail or the whole buffer;
     * `follow` keeps the stream open for new output.
     */
    async *logs(id: SessionId, window: LogWindow, follow: boolean): AsyncIterable<Uint8Array[]> {
        const transport = await this.#dial();
        try {
            const { results, done } = await invoke(
                transport,
                wit.SESSIONS_INSTANCE,
                'logs',
                [t.option(wit.callContext), t.string, wit.logWindow, t.bool],
                asArgs([NO_CTX, id, window, follow]),
                [t.stream(t.list(t.u8))],
            );
            const stream = results[0] as AsyncIterable<Uint8Array[]>;
            for await (const chunk of stream) {
                yield chunk;
            }
            await done;
        } finally {
            closeTransport(transport);
        }
    }

    /**
     * Attach to a session. `clientEvents` is the caller's stream of input,
     * resize, and detach events; ending it (after pushing `detach`) closes the
     * write side and completes the invocation. Each yielded item is a batch of
     * {@link ServerEvent}s (the daemon batches output); the first batch always
     * begins with a `snapshot`.
     */
    async *attach(
        id: SessionId,
        init: AttachInit,
        clientEvents: AsyncIterable<ClientEvent>,
    ): AsyncIterable<ServerEvent[]> {
        const transport = await this.#dial();
        try {
            const { results } = await invoke(
                transport,
                wit.SESSIONS_INSTANCE,
                'attach',
                [t.option(wit.callContext), t.string, wit.attachInit, t.stream(wit.clientEvent)],
                asArgs([NO_CTX, id, init, wrapChunks(clientEvents)]),
                [t.stream(wit.serverEvent)],
            );
            const stream = results[0] as AsyncIterable<RawVariant[]>;
            for await (const batch of stream) {
                yield batch.map(toServerEvent);
            }
            // Intentionally not awaiting `done` here: `done` also tracks the
            // outbound client-event writer, which stays pending until the caller
            // ends `clientEvents`. If the daemon ends the session first (e.g.
            // `exited`), that writer may still be open, so awaiting would hang.
            // The SDK already guards `done` against unhandled rejection.
        } finally {
            closeTransport(transport);
        }
    }

    async send(id: SessionId, chunks: AsyncIterable<Uint8Array>): Promise<void> {
        const transport = await this.#dial();
        try {
            const { results, done } = await invoke(
                transport,
                wit.SESSIONS_INSTANCE,
                'send',
                [t.option(wit.callContext), t.string, t.stream(t.list(t.u8))],
                asArgs([NO_CTX, id, wrapChunks(chunks)]),
                [t.result(null, wit.error)],
            );
            await done;
            unwrapResult<void>(results[0]);
        } finally {
            closeTransport(transport);
        }
    }

    // --- meta ---------------------------------------------------------------

    async authenticate(token: string): Promise<void> {
        const [res] = await this.#unary(
            wit.META_INSTANCE,
            'authenticate',
            [t.option(wit.callContext), t.string],
            [NO_CTX, token],
            [t.result(null, wit.error)],
        );
        unwrapResult<void>(res);
    }

    async whoami(): Promise<string> {
        const [res] = await this.#unary(
            wit.META_INSTANCE,
            'whoami',
            [t.option(wit.callContext)],
            [NO_CTX],
            [t.result(t.string, wit.error)],
        );
        return unwrapResult<string>(res);
    }

    async version(): Promise<VersionInfo> {
        const [info] = await this.#unary(
            wit.META_INSTANCE,
            'version',
            [t.option(wit.callContext)],
            [NO_CTX],
            [wit.versionInfo],
        );
        return info as unknown as VersionInfo;
    }

    // --- internals ----------------------------------------------------------

    /** Dial, run a single non-streaming invocation, and return its raw results. */
    async #unary(
        instance: string,
        func: string,
        paramTypes: Type[],
        args: unknown[],
        resultTypes: Type[],
    ): Promise<Value[]> {
        const transport = await this.#dial();
        try {
            const { results, done } = await invoke(
                transport,
                instance,
                func,
                paramTypes,
                asArgs(args),
                resultTypes,
            );
            await done;
            return results;
        } finally {
            closeTransport(transport);
        }
    }
}

/** `none` for the `ctx: option<call-context>` first parameter of every method. */
const NO_CTX = undefined;

/** A decoded variant value: `{ tag, val? }`. */
type RawVariant = { tag: string; val?: Value };

/** The SDK types invocation args as `Value[]`; our typed values fit structurally. */
function asArgs(args: unknown[]): Value[] {
    return args as Value[];
}

/** Unwrap a `result<T, error>`, throwing a {@link CairnError} on `err`. */
function unwrapResult<T>(value: Value): T {
    const res = value as { tag: 'ok' | 'err'; val?: Value };
    if (res.tag === 'err') {
        const err = res.val as { code: string; message: string };
        throw new CairnError(err.code, err.message);
    }
    return res.val as T;
}

/** Map a decoded `server-event` variant, wrapping `error` payloads as errors. */
function toServerEvent(raw: RawVariant): ServerEvent {
    if (raw.tag === 'error') {
        const err = raw.val as { code: string; message: string };
        return { tag: 'error', val: new CairnError(err.code, err.message) };
    }
    return raw as ServerEvent;
}

/**
 * Adapt a caller's stream of individual elements to the SDK's stream-source
 * shape: for a non-`u8` element type the SDK expects each yielded chunk to be an
 * array of elements, so wrap each element in a single-element chunk.
 */
async function* wrapChunks<T>(src: AsyncIterable<T>): AsyncIterable<T[]> {
    for await (const item of src) {
        yield [item];
    }
}

/** Close the transport if it exposes an explicit close (WebSocket/WebTransport). */
function closeTransport(transport: Transport): void {
    (transport as { close?: () => void }).close?.();
}
