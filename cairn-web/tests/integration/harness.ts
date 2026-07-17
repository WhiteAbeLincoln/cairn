// Integration harness: spawns a real `cairn-daemon` on a free TCP port with a
// throwaway runtime dir, waits for `/healthz`, and hands back a `DaemonClient`
// wired to its `ws://…/ws` endpoint. This is the fixture the wire-interop gate
// runs against — the browser protocol stack talking to the actual daemon.
//
// Teardown is defensive: `stop()` does SIGTERM → wait → SIGKILL, and a
// process-exit handler SIGKILLs any child still tracked, so a crashed or
// short-circuited run never leaks a daemon.

import { type ChildProcess, execFile, spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { DaemonClient, wsDialer } from '../../src/lib/protocol';

const execFileAsync = promisify(execFile);

/**
 * Path to the built daemon. Override with `CAIRN_DAEMON_BIN`; otherwise the
 * workspace debug build (`cargo build -p cairn-daemon`) two levels up.
 */
export const DAEMON_BIN =
    process.env.CAIRN_DAEMON_BIN ??
    join(import.meta.dirname, '../../../target/debug/cairn-daemon');

/** How long to wait for the daemon to answer `/healthz` before giving up. */
const READY_TIMEOUT_MS = 15_000;
const READY_POLL_MS = 50;
/** How long to wait for a graceful SIGTERM exit before escalating to SIGKILL. */
const STOP_TIMEOUT_MS = 5_000;

/** Every daemon child we spawn, so an exit handler can reap survivors. */
const liveChildren = new Set<ChildProcess>();
let cleanupInstalled = false;

/** Install a best-effort reaper so an abnormal exit can't orphan a daemon. */
function installCleanup(): void {
    if (cleanupInstalled) return;
    cleanupInstalled = true;
    process.on('exit', () => {
        for (const child of liveChildren) {
            try {
                child.kill('SIGKILL');
            } catch {
                // Nothing actionable while the process is already tearing down.
            }
        }
    });
}

export class DaemonHarness {
    private constructor(
        readonly wsUrl: string,
        readonly port: number,
        readonly client: DaemonClient,
        private readonly child: ChildProcess,
        private readonly runtimeDir: string,
        private readonly stderr: () => string,
    ) {}

    /** Spawn a daemon with a `ws://` listener and block until it is serving. */
    static async start(): Promise<DaemonHarness> {
        installCleanup();
        const port = await freePort();
        const runtimeDir = mkdtempSync(join(tmpdir(), 'cairn-it-'));

        const child = spawn(
            DAEMON_BIN,
            [
                '--listen',
                `ws://127.0.0.1:${port}`,
                '--log-format',
                'off',
                // Keep teardown snappy: sessions get a short drain window.
                '--shutdown-grace',
                '1s',
            ],
            {
                stdio: ['ignore', 'pipe', 'pipe'],
                env: {
                    ...process.env,
                    // Point the runtime dir at the tempdir so nothing leaks into
                    // the developer's real `$XDG_RUNTIME_DIR/cairn`.
                    XDG_RUNTIME_DIR: runtimeDir,
                    TMPDIR: runtimeDir,
                    // No OTLP collector in tests; silence exporter retries.
                    OTEL_SDK_DISABLED: 'true',
                },
            },
        );
        liveChildren.add(child);

        let stderrBuf = '';
        child.stderr?.on('data', (chunk: Buffer) => {
            stderrBuf += chunk.toString();
        });
        // Drain stdout so a full pipe can never block the daemon.
        child.stdout?.resume();

        child.once('exit', () => {
            liveChildren.delete(child);
        });
        let spawnError: Error | undefined;
        child.once('error', (err) => {
            spawnError = err;
        });

        const base = `http://127.0.0.1:${port}`;
        const deadline = Date.now() + READY_TIMEOUT_MS;
        let ready = false;
        while (Date.now() < deadline) {
            if (spawnError) {
                throw new Error(
                    `failed to spawn cairn-daemon at ${DAEMON_BIN}: ${spawnError.message}`,
                );
            }
            // exitCode/signalCode become non-null the moment the child exits.
            if (child.exitCode !== null || child.signalCode !== null) {
                throw new Error(
                    `cairn-daemon exited (code=${child.exitCode}, signal=${child.signalCode}) ` +
                        `before answering ${base}/healthz.\nstderr:\n${stderrBuf}`,
                );
            }
            try {
                const res = await fetch(`${base}/healthz`, {
                    signal: AbortSignal.timeout(1_000),
                });
                if (res.ok) {
                    ready = true;
                    break;
                }
            } catch {
                // Listener not up yet — keep polling.
            }
            await delay(READY_POLL_MS);
        }

        if (!ready) {
            child.kill('SIGKILL');
            liveChildren.delete(child);
            rmSync(runtimeDir, { recursive: true, force: true });
            throw new Error(
                `cairn-daemon did not answer ${base}/healthz within ${READY_TIMEOUT_MS}ms.\n` +
                    `Binary: ${DAEMON_BIN}\n` +
                    `Build it with: cargo build -p cairn-daemon\nstderr:\n${stderrBuf}`,
            );
        }

        const wsUrl = `ws://127.0.0.1:${port}/ws`;
        const client = new DaemonClient(wsDialer(wsUrl));
        return new DaemonHarness(wsUrl, port, client, child, runtimeDir, () => stderrBuf);
    }

    /** The daemon process id (for CPU sampling). */
    get pid(): number {
        const pid = this.child.pid;
        if (pid === undefined) throw new Error('daemon has no pid');
        return pid;
    }

    /** Captured daemon stderr so far (empty unless it logged before failing). */
    stderrDump(): string {
        return this.stderr();
    }

    /** SIGTERM the daemon, escalate to SIGKILL on timeout, remove the tempdir. */
    async stop(): Promise<void> {
        const child = this.child;
        if (child.exitCode === null && child.signalCode === null) {
            const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()));
            child.kill('SIGTERM');
            const graceful = await Promise.race([
                exited.then(() => true),
                delay(STOP_TIMEOUT_MS).then(() => false),
            ]);
            if (!graceful) {
                child.kill('SIGKILL');
                await exited;
            }
        }
        liveChildren.delete(child);
        rmSync(this.runtimeDir, { recursive: true, force: true });
    }
}

/** Result of a CPU sampling window over a single process. */
export interface CpuSample {
    /** CPU-seconds consumed per wall-second — 1.0 == one core fully busy. */
    fraction: number;
    /** CPU-seconds the process accrued during the window. */
    cpuDelta: number;
    /** Wall-clock seconds the window actually spanned. */
    wallSeconds: number;
}

/**
 * Measure a process's CPU usage as a fraction of one core over `windowMs`, by
 * differencing cumulative CPU time (`ps -o time`, all threads) across a wall
 * window. Robust for the regression class this suite guards: a busy-spinning
 * daemon (the v1 ~600% incident) accrues CPU-seconds far in excess of the wall
 * window; a quiet one accrues ~none.
 */
export async function sampleCpuFraction(pid: number, windowMs: number): Promise<CpuSample> {
    const start = await cpuSeconds(pid);
    const startWall = Date.now();
    await delay(windowMs);
    const end = await cpuSeconds(pid);
    const wallSeconds = (Date.now() - startWall) / 1000;
    const cpuDelta = end - start;
    return { fraction: cpuDelta / wallSeconds, cpuDelta, wallSeconds };
}

/** Read cumulative CPU time (seconds) for a pid via `ps -o time=`. */
async function cpuSeconds(pid: number): Promise<number> {
    const { stdout } = await execFileAsync('ps', ['-o', 'time=', '-p', String(pid)]);
    return parseCpuTime(stdout.trim());
}

/**
 * Parse a `ps` TIME field (`[[DD-]HH:]MM:SS.cc`) into seconds. Each `:`-part is
 * one more sexagesimal place; a leading `DD-` day count is split off first.
 */
export function parseCpuTime(field: string): number {
    let text = field;
    let days = 0;
    const dash = text.indexOf('-');
    if (dash !== -1) {
        days = Number(text.slice(0, dash));
        text = text.slice(dash + 1);
    }
    const parts = text.split(':').map(Number);
    const seconds = parts.reduce((acc, part) => acc * 60 + part, 0);
    return days * 86_400 + seconds;
}

/** Ask the OS for an unused loopback TCP port by binding port 0 and releasing it. */
function freePort(): Promise<number> {
    return new Promise((resolve, reject) => {
        const srv = createServer();
        srv.once('error', reject);
        srv.listen(0, '127.0.0.1', () => {
            const addr = srv.address();
            if (addr === null || typeof addr === 'string') {
                srv.close();
                reject(new Error('could not determine a free port'));
                return;
            }
            const { port } = addr;
            srv.close(() => resolve(port));
        });
    });
}

export function delay(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
}
