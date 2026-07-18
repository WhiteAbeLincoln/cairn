import type { Value } from '@bytecodealliance/wrpc';
import { accept, Chan, t } from '@bytecodealliance/wrpc';
import { describe, expect, it } from 'vitest';
import { DaemonClient } from './client';
import type { Dialer, Transport } from './transport';
import { CairnError, type ClientEvent, type SessionInfo, type SessionSpec } from './types';
import * as wit from './wit';

const enc = (s: string): Uint8Array => new TextEncoder().encode(s);
const dec = (b: Uint8Array): string => new TextDecoder().decode(b);

// `interface` types have no implicit index signature, so they aren't directly
// assignable to the SDK's structural `Value`; the codec accepts them by shape
// at runtime, so coerce at the server boundary.
const asValue = (v: unknown): Value => v as Value;

const sampleSpec: SessionSpec = {
    name: 'demo',
    command: ['bash', '-l'],
    env: [
        ['TERM', 'xterm-256color'],
        ['LANG', 'en_US.UTF-8'],
    ],
    envInherit: true,
    workdir: '/home/abe',
    tty: true,
    stdin: true,
    idleTimeoutSecs: 300n,
    scrollbackLines: 1000,
};

const sampleInfo: SessionInfo = {
    id: '018f9a2b-0000-7000-8000-000000000001',
    name: 'demo',
    pid: 4242,
    cols: 80,
    rows: 24,
    attachedClients: ['client-a', 'client-b'],
    createdAtUnixMs: 1_700_000_000_000n,
    exit: undefined,
    spec: sampleSpec,
};

/** A decoded variant `{ tag, val? }`. */
type RawVariant = { tag: string; val?: Value };

/**
 * A minimal in-process daemon built on the SDK's `accept()`. Each invocation
 * gets a fresh in-memory transport pair; the server side is handled here.
 */
class FakeDaemon {
    readonly received: Record<string, unknown> = {};
    readonly errors: unknown[] = [];
    readonly #pending: Promise<void>[] = [];

    dialer(): Dialer {
        return async () => {
            const [clientSide, serverSide] = transportPair();
            const job = this.#handle(serverSide).catch((err) => {
                this.errors.push(err);
            });
            this.#pending.push(job);
            return clientSide;
        };
    }

    /** Resolve once every accepted invocation has finished serving. */
    async settle(): Promise<void> {
        await Promise.all(this.#pending);
    }

    async #handle(transport: Transport): Promise<void> {
        const inv = await accept(transport);
        switch (inv.func) {
            case 'list-all': {
                const { done } = await inv.receiveParams([t.option(wit.callContext)]);
                await done;
                await inv.sendResults([t.list(wit.sessionInfo)], [[asValue(sampleInfo)]]);
                return;
            }
            case 'inspect': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                ]);
                await done;
                const id = params[1] as string;
                this.received.inspect = id;
                if (id === 'missing') {
                    await inv.sendResults(
                        [t.result(wit.sessionInfo, wit.error)],
                        [
                            {
                                tag: 'err',
                                val: { code: 'not-found', message: `no such session: ${id}` },
                            },
                        ],
                    );
                } else {
                    await inv.sendResults(
                        [t.result(wit.sessionInfo, wit.error)],
                        [{ tag: 'ok', val: asValue(sampleInfo) }],
                    );
                }
                return;
            }
            case 'create': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    wit.sessionSpec,
                ]);
                await done;
                this.received.create = params[1];
                await inv.sendResults(
                    [t.result(wit.sessionInfo, wit.error)],
                    [{ tag: 'ok', val: asValue(sampleInfo) }],
                );
                return;
            }
            case 'rename': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                    t.string,
                ]);
                await done;
                this.received.rename = { id: params[1], newName: params[2] };
                await inv.sendResults([t.result(null, wit.error)], [{ tag: 'ok' }]);
                return;
            }
            case 'restart': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                    t.bool,
                ]);
                await done;
                this.received.restart = { id: params[1], force: params[2] };
                await inv.sendResults([t.result(null, wit.error)], [{ tag: 'ok' }]);
                return;
            }
            case 'kill': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                    wit.signal,
                    t.option(t.u32),
                ]);
                await done;
                this.received.kill = { id: params[1], sig: params[2], graceMs: params[3] };
                await inv.sendResults([t.result(null, wit.error)], [{ tag: 'ok' }]);
                return;
            }
            case 'kick': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                    t.option(t.string),
                ]);
                await done;
                this.received.kick = { id: params[1], client: params[2] };
                await inv.sendResults([t.result(null, wit.error)], [{ tag: 'ok' }]);
                return;
            }
            case 'wait': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                ]);
                await done;
                this.received.wait = params[1];
                await inv.sendResults(
                    [t.future(wit.exitStatus)],
                    [{ code: 0, signal: undefined, unixMs: 42n, reason: 'bye' }],
                );
                return;
            }
            case 'logs': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                    wit.logWindow,
                    t.bool,
                ]);
                await done;
                this.received.logs = { id: params[1], window: params[2], follow: params[3] };
                const source: AsyncIterable<Value[]> = (async function* () {
                    yield [enc('line1\n'), enc('line2\n')];
                    yield [enc('line3\n')];
                })();
                await inv.sendResults([t.stream(t.list(t.u8))], [source]);
                return;
            }
            case 'attach': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                    wit.attachInit,
                    t.stream(wit.clientEvent),
                ]);
                this.received.attach = { id: params[1], init: params[2] };
                const events = params[3] as AsyncIterable<RawVariant[]>;
                const source: AsyncIterable<Value[]> = (async function* () {
                    yield [{ tag: 'snapshot', val: enc('screen') }];
                    let stop = false;
                    for await (const batch of events) {
                        for (const ev of batch) {
                            if (ev.tag === 'input') {
                                yield [{ tag: 'output', val: ev.val }];
                            } else if (ev.tag === 'resize') {
                                const [cols, rows] = ev.val as [number, number];
                                yield [{ tag: 'output', val: enc(`resize:${cols}x${rows}`) }];
                            } else if (ev.tag === 'detach') {
                                stop = true;
                            }
                        }
                        if (stop) break;
                    }
                    yield [{ tag: 'error', val: { code: 'session.ending', message: 'closing' } }];
                    yield [
                        {
                            tag: 'exited',
                            val: { code: 0, signal: undefined, unixMs: 7n, reason: undefined },
                        },
                    ];
                })();
                await Promise.all([done, inv.sendResults([t.stream(wit.serverEvent)], [source])]);
                return;
            }
            case 'send': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                    t.stream(t.list(t.u8)),
                ]);
                const chunks = params[2] as AsyncIterable<Uint8Array[]>;
                const collected: Uint8Array[] = [];
                const consume = (async () => {
                    for await (const batch of chunks) {
                        for (const el of batch) collected.push(el);
                    }
                })();
                await Promise.all([done, consume]);
                this.received.send = {
                    id: params[1],
                    data: collected.map(dec).join(''),
                };
                await inv.sendResults([t.result(null, wit.error)], [{ tag: 'ok' }]);
                return;
            }
            case 'authenticate': {
                const { params, done } = await inv.receiveParams([
                    t.option(wit.callContext),
                    t.string,
                ]);
                await done;
                const token = params[1] as string;
                this.received.authenticate = token;
                if (token === 'bad') {
                    await inv.sendResults(
                        [t.result(null, wit.error)],
                        [{ tag: 'err', val: { code: 'auth.denied', message: 'invalid token' } }],
                    );
                } else {
                    await inv.sendResults([t.result(null, wit.error)], [{ tag: 'ok' }]);
                }
                return;
            }
            case 'whoami': {
                const { done } = await inv.receiveParams([t.option(wit.callContext)]);
                await done;
                await inv.sendResults(
                    [t.result(t.string, wit.error)],
                    [{ tag: 'ok', val: 'anonymous' }],
                );
                return;
            }
            case 'version': {
                const { done } = await inv.receiveParams([t.option(wit.callContext)]);
                await done;
                await inv.sendResults(
                    [wit.versionInfo],
                    [{ daemon: 'cairn 0.1.0', protocol: 'cairn:daemon@0.1.0' }],
                );
                return;
            }
            default:
                throw new Error(`unexpected func ${inv.instance}#${inv.func}`);
        }
    }
}

/** An in-memory bidirectional byte-duplex pair (client side, server side). */
function transportPair(): [Transport, Transport] {
    const toServer = new Chan<Uint8Array>();
    const toClient = new Chan<Uint8Array>();
    const recv = (ch: Chan<Uint8Array>) => async (): Promise<Uint8Array | undefined> => {
        const { value, done } = await ch.next();
        return done ? undefined : value;
    };
    const clientSide: Transport = {
        read: recv(toClient),
        write: (bytes: Uint8Array) => {
            toServer.push(bytes);
        },
        closeWrite: () => {
            toServer.close();
        },
    };
    const serverSide: Transport = {
        read: recv(toServer),
        write: (bytes: Uint8Array) => {
            toClient.push(bytes);
        },
        closeWrite: () => {
            toClient.close();
        },
    };
    return [clientSide, serverSide];
}

function fixture(): { fake: FakeDaemon; client: DaemonClient } {
    const fake = new FakeDaemon();
    return { fake, client: new DaemonClient(fake.dialer()) };
}

describe('DaemonClient round-trips', () => {
    it('list-all decodes nested records, options, tuples, and bigints', async () => {
        const { fake, client } = fixture();
        const list = await client.listAll();
        expect(list).toEqual([sampleInfo]);
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('inspect returns ok and forwards the id', async () => {
        const { fake, client } = fixture();
        const info = await client.inspect(sampleInfo.id);
        expect(info).toEqual(sampleInfo);
        expect(fake.received.inspect).toBe(sampleInfo.id);
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('inspect rejects with CairnError on result::err', async () => {
        const { fake, client } = fixture();
        const rejection = client.inspect('missing');
        await expect(rejection).rejects.toBeInstanceOf(CairnError);
        await expect(rejection).rejects.toMatchObject({
            code: 'not-found',
            message: 'no such session: missing',
        });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('create round-trips the full session spec', async () => {
        const { fake, client } = fixture();
        const info = await client.create(sampleSpec);
        expect(info).toEqual(sampleInfo);
        expect(fake.received.create).toEqual(sampleSpec);
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('rename and restart forward their arguments', async () => {
        const { fake, client } = fixture();
        await client.rename(sampleInfo.id, 'renamed');
        expect(fake.received.rename).toEqual({ id: sampleInfo.id, newName: 'renamed' });
        await client.restart(sampleInfo.id, true);
        expect(fake.received.restart).toEqual({ id: sampleInfo.id, force: true });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('kill encodes both named and numbered signals and the optional grace', async () => {
        const { fake, client } = fixture();
        await client.kill(sampleInfo.id, { tag: 'named', val: 'term' }, 5000);
        expect(fake.received.kill).toEqual({
            id: sampleInfo.id,
            sig: { tag: 'named', val: 'term' },
            graceMs: 5000,
        });
        await client.kill(sampleInfo.id, { tag: 'numbered', val: 9 });
        expect(fake.received.kill).toEqual({
            id: sampleInfo.id,
            sig: { tag: 'numbered', val: 9 },
            graceMs: undefined,
        });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('kick sends the optional client both present and absent', async () => {
        const { fake, client } = fixture();
        await client.kick(sampleInfo.id, 'client-a');
        expect(fake.received.kick).toEqual({ id: sampleInfo.id, client: 'client-a' });
        await client.kick(sampleInfo.id);
        expect(fake.received.kick).toEqual({ id: sampleInfo.id, client: undefined });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('wait resolves the future exit-status', async () => {
        const { fake, client } = fixture();
        const status = await client.wait(sampleInfo.id);
        expect(status).toEqual({ code: 0, signal: undefined, unixMs: 42n, reason: 'bye' });
        expect(fake.received.wait).toBe(sampleInfo.id);
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('logs yields every batched chunk then terminates', async () => {
        const { fake, client } = fixture();
        const collected: string[] = [];
        for await (const batch of client.logs(sampleInfo.id, { tag: 'tail', val: 100 }, false)) {
            for (const rec of batch) collected.push(dec(rec));
        }
        expect(collected).toEqual(['line1\n', 'line2\n', 'line3\n']);
        expect(fake.received.logs).toEqual({
            id: sampleInfo.id,
            window: { tag: 'tail', val: 100 },
            follow: false,
        });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('send streams every chunk to the daemon', async () => {
        const { fake, client } = fixture();
        async function* input(): AsyncIterable<Uint8Array> {
            yield enc('hello ');
            yield enc('world');
        }
        await client.send(sampleInfo.id, input());
        expect(fake.received.send).toEqual({ id: sampleInfo.id, data: 'hello world' });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('attach carries events both directions, maps errors, and ends on detach', async () => {
        const { fake, client } = fixture();
        const clientEvents = new Chan<ClientEvent>();
        const stream = client.attach(
            sampleInfo.id,
            { cols: 80, rows: 24, noStdin: false },
            clientEvents,
        );
        const it = stream[Symbol.asyncIterator]();

        const snapshot = await it.next();
        expect(snapshot.value).toEqual([{ tag: 'snapshot', val: enc('screen') }]);

        clientEvents.push({ tag: 'input', val: enc('ls\n') });
        const echoed = await it.next();
        expect(echoed.value).toEqual([{ tag: 'output', val: enc('ls\n') }]);

        clientEvents.push({ tag: 'resize', val: [120, 40] });
        const resized = await it.next();
        expect(resized.value).toEqual([{ tag: 'output', val: enc('resize:120x40') }]);

        clientEvents.push({ tag: 'detach' });
        clientEvents.close();

        const errored = await it.next();
        const errBatch = errored.value as { tag: string; val: unknown }[];
        expect(errBatch[0].tag).toBe('error');
        expect(errBatch[0].val).toBeInstanceOf(CairnError);
        expect(errBatch[0].val).toMatchObject({ code: 'session.ending', message: 'closing' });

        const exited = await it.next();
        expect(exited.value).toEqual([
            { tag: 'exited', val: { code: 0, signal: undefined, unixMs: 7n, reason: undefined } },
        ]);

        const end = await it.next();
        expect(end.done).toBe(true);

        expect(fake.received.attach).toEqual({
            id: sampleInfo.id,
            init: { cols: 80, rows: 24, noStdin: false },
        });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('authenticate rejects with CairnError on a bad token', async () => {
        const { fake, client } = fixture();
        await expect(client.authenticate('bad')).rejects.toBeInstanceOf(CairnError);
        await client.authenticate('good');
        expect(fake.received.authenticate).toBe('good');
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('whoami and version return their values', async () => {
        const { fake, client } = fixture();
        expect(await client.whoami()).toBe('anonymous');
        expect(await client.version()).toEqual({
            daemon: 'cairn 0.1.0',
            protocol: 'cairn:daemon@0.1.0',
        });
        await fake.settle();
        expect(fake.errors).toEqual([]);
    });
});

describe('DaemonClient dialer routing', () => {
    /** Wrap a dialer so each dial is counted; the calls still reach `inner`. */
    function counted(inner: Dialer): { dialer: Dialer; count: () => number } {
        let dials = 0;
        return {
            dialer: () => {
                dials += 1;
                return inner();
            },
            count: () => dials,
        };
    }

    /** Run a minimal attach (immediate detach) to completion. */
    async function drainAttach(client: DaemonClient): Promise<void> {
        const clientEvents = new Chan<ClientEvent>();
        clientEvents.push({ tag: 'detach' });
        clientEvents.close();
        const stream = client.attach(
            sampleInfo.id,
            { cols: 80, rows: 24, noStdin: false },
            clientEvents,
        );
        for await (const _batch of stream) {
            // drain to completion
        }
    }

    it('routes unary methods and wait via control, streaming methods via streams', async () => {
        const fake = new FakeDaemon();
        const control = counted(fake.dialer());
        const streams = counted(fake.dialer());
        const client = new DaemonClient(control.dialer, streams.dialer);

        await client.listAll();
        await client.inspect(sampleInfo.id);
        await client.create(sampleSpec);
        await client.rename(sampleInfo.id, 'renamed');
        await client.restart(sampleInfo.id, false);
        await client.kill(sampleInfo.id, { tag: 'named', val: 'term' });
        await client.kick(sampleInfo.id);
        await client.version();
        await client.whoami();
        await client.authenticate('good');
        await client.wait(sampleInfo.id);
        expect(control.count()).toBe(11);
        expect(streams.count()).toBe(0);

        for await (const _batch of client.logs(sampleInfo.id, { tag: 'tail', val: 1 }, false)) {
            // drain to completion
        }
        await drainAttach(client);
        async function* input(): AsyncIterable<Uint8Array> {
            yield enc('x');
        }
        await client.send(sampleInfo.id, input());
        expect(streams.count()).toBe(3);
        expect(control.count()).toBe(11);

        await fake.settle();
        expect(fake.errors).toEqual([]);
    });

    it('one-arg construction routes every method through the single dialer', async () => {
        const fake = new FakeDaemon();
        const only = counted(fake.dialer());
        const client = new DaemonClient(only.dialer);

        await client.version();
        await client.wait(sampleInfo.id);
        for await (const _batch of client.logs(sampleInfo.id, { tag: 'tail', val: 1 }, false)) {
            // drain to completion
        }
        await drainAttach(client);
        expect(only.count()).toBe(4);

        await fake.settle();
        expect(fake.errors).toEqual([]);
    });
});
