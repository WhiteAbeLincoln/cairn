// Thin Svelte 5 runes wrapper over endpoint discovery + `DaemonClient` +
// `ReconnectController`. All the actual logic (precedence, backoff, status
// transitions) lives in `endpoint.ts`/`reconnect.ts` and is unit-tested there;
// this module's job is only to own the reactive `$state` the UI reads and to
// supply the real `fetch`/`localStorage` boundaries those pure functions need.

import { DaemonClient, wsDialer, wtDialer } from '$lib/protocol';
import {
    type CairnJsonDoc,
    discoverEndpoint,
    type EndpointConfig,
    loadStoredEndpoint,
    saveEndpoint,
} from './endpoint';
import { type ConnectionStatus, ReconnectController } from './reconnect';

let client = $state<DaemonClient | undefined>(undefined);
let endpoint = $state<EndpointConfig | undefined>(undefined);
let status = $state<ConnectionStatus>({ state: 'connecting' });
let needsManualEndpoint = $state(false);
let manualError = $state<string | undefined>(undefined);

let controller: ReconnectController | undefined;
const statusListeners = new Set<(status: ConnectionStatus) => void>();

/**
 * Resolve the daemon endpoint and start the connectivity loop. Call once,
 * from the root layout. Never rejects: any unexpected discovery failure
 * falls through to the manual-endpoint screen with a visible error, so the
 * caller doesn't need a `.catch()`.
 */
export async function initConnection(locationHref: string): Promise<void> {
    try {
        const result = await discoverEndpoint({
            locationHref,
            fetchCairnJson,
            readStored: () => loadStoredEndpoint(window.localStorage),
        });
        if (result.status === 'manual-required') {
            needsManualEndpoint = true;
            return;
        }
        connectWith(result.endpoint);
    } catch (err) {
        manualError = err instanceof Error ? err.message : String(err);
        needsManualEndpoint = true;
    }
}

/** Submit a manually-entered endpoint (the Task 7 minimal fallback: a direct `ws://`/`wss://` URL). */
export function submitManualEndpoint(url: string): void {
    const trimmed = url.trim();
    if (!/^wss?:\/\//i.test(trimmed)) {
        manualError = 'Enter a ws:// or wss:// URL';
        return;
    }
    manualError = undefined;
    const ep: EndpointConfig = { transport: 'ws', url: trimmed };
    saveEndpoint(window.localStorage, ep);
    connectWith(ep);
}

export function getClient(): DaemonClient | undefined {
    return client;
}

export function getConnectionStatus(): ConnectionStatus {
    return status;
}

export function getConnectionEndpoint(): EndpointConfig | undefined {
    return endpoint;
}

export function getNeedsManualEndpoint(): boolean {
    return needsManualEndpoint;
}

export function getManualError(): string | undefined {
    return manualError;
}

/** Notified on every connection status transition — used by `sessions.svelte.ts` to refresh on (re)connect. */
export function onConnectionStatusChange(fn: (status: ConnectionStatus) => void): () => void {
    statusListeners.add(fn);
    return () => statusListeners.delete(fn);
}

function connectWith(ep: EndpointConfig): void {
    endpoint = ep;
    needsManualEndpoint = false;
    const c = new DaemonClient(
        ep.transport === 'ws' ? wsDialer(ep.url) : wtDialer(ep.url, ep.certHash),
    );
    client = c;

    controller?.stop();
    const next = new ReconnectController({
        // A cheap, data-free connectivity check — keeps the connection status
        // independent of any particular RPC (sessions, attach, ...).
        probe: () => c.version().then(() => undefined),
    });
    next.onStatusChange((s) => {
        status = s;
        for (const listener of statusListeners) listener(s);
    });
    controller = next;
    next.start();
}

async function fetchCairnJson(): Promise<CairnJsonDoc | undefined> {
    try {
        const res = await fetch('/cairn.json');
        if (!res.ok) return undefined;
        return (await res.json()) as CairnJsonDoc;
    } catch {
        return undefined;
    }
}
