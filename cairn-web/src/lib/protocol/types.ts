// TypeScript interfaces mirroring the `cairn:daemon@0.1.0` WIT types
// (crates/cairn-protocol/wit/cairn.wit is the authority).
//
// Field names are camelCased for idiomatic JS. They match the JS object keys
// the wRPC value codec produces and consumes — see wit.ts. The wire format
// itself is positional (records) or discriminant-indexed (variants/enums), so
// the JS key/case names never travel on the wire; only field/case ORDER does.

/** `types.session-id` — a UUIDv7 string. */
export type SessionId = string;
/** `types.client-id`. */
export type ClientId = string;

export interface HttpRoute {
    methods: string[];
    host?: string;
    pathPrefix?: string;
}

export interface HttpProxySpec {
    routes: HttpRoute[];
}

/** `types.session-spec`. */
export interface SessionSpec {
    name?: string;
    command: string[];
    env: [string, string][];
    envInherit: boolean;
    workdir?: string;
    tty: boolean;
    stdin: boolean;
    idleTimeoutSecs?: bigint;
    scrollbackLines: number;
    httpProxy?: HttpProxySpec;
}

/** `types.exit-status`. */
export interface ExitStatus {
    code?: number;
    signal?: number;
    unixMs: bigint;
    reason?: string;
}

/** `types.session-info`. */
export interface SessionInfo {
    id: SessionId;
    name?: string;
    pid?: number;
    cols: number;
    rows: number;
    attachedClients: ClientId[];
    createdAtUnixMs: bigint;
    exit?: ExitStatus;
    spec: SessionSpec;
}

/** `types.attach-init`. */
export interface AttachInit {
    cols: number;
    rows: number;
    noStdin: boolean;
}

/** `types.signal-name` — POSIX signal names carried symbolically. */
export type SignalName =
    | 'hup'
    | 'int'
    | 'quit'
    | 'ill'
    | 'trap'
    | 'abrt'
    | 'bus'
    | 'fpe'
    | 'kill'
    | 'usr1'
    | 'segv'
    | 'usr2'
    | 'pipe'
    | 'alrm'
    | 'term'
    | 'chld'
    | 'cont'
    | 'stop'
    | 'tstp'
    | 'ttin'
    | 'ttou'
    | 'urg'
    | 'xcpu'
    | 'xfsz'
    | 'vtalrm'
    | 'prof'
    | 'winch'
    | 'io'
    | 'sys';

/** `types.signal`. */
export type Signal = { tag: 'named'; val: SignalName } | { tag: 'numbered'; val: number };

/** `types.log-window`. */
export type LogWindow = { tag: 'tail'; val: number } | { tag: 'all' };

/** `types.client-event` — events a client pushes into an attach stream. */
export type ClientEvent =
    | { tag: 'input'; val: Uint8Array }
    | { tag: 'resize'; val: [number, number] }
    | { tag: 'detach' };

/** `types.server-event` — events the daemon emits on an attach stream. */
export type ServerEvent =
    | { tag: 'snapshot'; val: Uint8Array }
    | { tag: 'output'; val: Uint8Array }
    | { tag: 'exited'; val: ExitStatus }
    | { tag: 'error'; val: CairnError };

/** `types.session-event` — events on a watch-sessions stream. */
export type SessionEvent =
    | { tag: 'snapshot'; val: SessionInfo[] }
    | { tag: 'upsert'; val: SessionInfo }
    | { tag: 'removed'; val: SessionId };

/** `meta.version-info`. */
export interface VersionInfo {
    daemon: string;
    protocol: string;
}

/** `types.call-context` — optional trace propagation on every call. */
export interface CallContext {
    traceContext?: string;
}

/**
 * The `types.error` record, surfaced as a throwable `Error`.
 *
 * Unary `DaemonClient` methods reject with a `CairnError` when the daemon
 * returns `result::err`; the in-band `server-event::error` also carries one.
 */
export class CairnError extends Error {
    /** The daemon's stable error code, e.g. `client.kicked`, `not-found`. */
    readonly code: string;

    constructor(code: string, message: string) {
        super(message);
        this.name = 'CairnError';
        this.code = code;
    }
}
