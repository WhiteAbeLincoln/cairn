import type { Type } from '@bytecodealliance/wrpc';
import { t } from '@bytecodealliance/wrpc';

// Runtime wRPC/WIT type descriptors mirroring crates/cairn-protocol/wit/cairn.wit
// (the authority). These are pure data the SDK's codec walks — no encoding
// logic lives here.
//
// Record field names and variant/enum case names below are JS-side labels only.
// The component-model wire format encodes records positionally and
// variants/enums by discriminant index, so what matters for interop is the
// ORDER of fields/cases (which matches the WIT exactly); the camelCased labels
// are chosen for idiomatic JS and never appear on the wire.

/** wRPC instance identifier for the `sessions` interface. Travels on the wire. */
export const SESSIONS_INSTANCE = 'cairn:daemon/sessions@0.1.0';
/** wRPC instance identifier for the `meta` interface. Travels on the wire. */
export const META_INSTANCE = 'cairn:daemon/meta@0.1.0';

/** `types.call-context`. */
export const callContext: Type = t.record({
    traceContext: t.option(t.string),
});

/** `types.error`. */
export const error: Type = t.record({
    code: t.string,
    message: t.string,
});

/** `types.exit-status`. */
export const exitStatus: Type = t.record({
    code: t.option(t.s32),
    signal: t.option(t.u8),
    unixMs: t.u64,
    reason: t.option(t.string),
});

/** `types.session-spec`. */
export const sessionSpec: Type = t.record({
    name: t.option(t.string),
    command: t.list(t.string),
    env: t.list(t.tuple(t.string, t.string)),
    envInherit: t.bool,
    workdir: t.option(t.string),
    tty: t.bool,
    stdin: t.bool,
    idleTimeoutSecs: t.option(t.u64),
    scrollbackLines: t.u32,
});

/** `types.session-info`. */
export const sessionInfo: Type = t.record({
    id: t.string,
    name: t.option(t.string),
    pid: t.option(t.u32),
    cols: t.u16,
    rows: t.u16,
    attachedClients: t.list(t.string),
    createdAtUnixMs: t.u64,
    exit: t.option(exitStatus),
    spec: sessionSpec,
});

/** `types.signal-name` — order matches the WIT enum (discriminant index). */
export const signalName: Type = t.enum([
    'hup',
    'int',
    'quit',
    'ill',
    'trap',
    'abrt',
    'bus',
    'fpe',
    'kill',
    'usr1',
    'segv',
    'usr2',
    'pipe',
    'alrm',
    'term',
    'chld',
    'cont',
    'stop',
    'tstp',
    'ttin',
    'ttou',
    'urg',
    'xcpu',
    'xfsz',
    'vtalrm',
    'prof',
    'winch',
    'io',
    'sys',
]);

/** `types.signal`. */
export const signal: Type = t.variant({
    named: signalName,
    numbered: t.u8,
});

/** `types.log-window`. */
export const logWindow: Type = t.variant({
    tail: t.u32,
    all: null,
});

/** `types.attach-init`. */
export const attachInit: Type = t.record({
    cols: t.u16,
    rows: t.u16,
    noStdin: t.bool,
});

/** `types.client-event`. */
export const clientEvent: Type = t.variant({
    input: t.list(t.u8),
    resize: t.tuple(t.u16, t.u16),
    detach: null,
});

/** `types.server-event`. */
export const serverEvent: Type = t.variant({
    snapshot: t.list(t.u8),
    output: t.list(t.u8),
    exited: exitStatus,
    error: error,
});

/** `meta.version-info`. */
export const versionInfo: Type = t.record({
    daemon: t.string,
    protocol: t.string,
});
