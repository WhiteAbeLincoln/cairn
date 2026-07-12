// Thin Svelte 5 runes wrapper over endpoint discovery + `DaemonClient` +
// `ReconnectController`. All the actual logic (precedence, backoff, status
// transitions) lives in `endpoint.ts`/`reconnect.ts` and is unit-tested there;
// this module's job is only to own the reactive `$state` the UI reads and to
// supply the real `fetch`/`localStorage` boundaries those pure functions need.

import { DaemonClient, wsDialer, wtDialer } from '$lib/protocol';
import {
    type CairnJsonDoc,
    clearStoredEndpoint,
    discoverEndpoint,
    type EndpointConfig,
    loadStoredEndpoint,
    parseDirectWsUrl,
    parseDirectWtEndpoint,
    resolveFromBaseUrl,
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

/**
 * Bootstrap from a daemon base URL (the primary standalone-hosting path):
 * fetch that host's CORS-open `/cairn.json` and apply the normal
 * WS-preferred/WT-fallback selection.
 */
export async function submitBaseUrl(url: string): Promise<void> {
    const result = await resolveFromBaseUrl(url, fetchCairnJsonAt);
    if (result.status === 'error') {
        manualError = result.message;
        return;
    }
    manualError = undefined;
    saveEndpoint(window.localStorage, result.endpoint);
    connectWith(result.endpoint);
}

/** Submit a direct `ws://`/`wss://` URL, bypassing `/cairn.json` discovery entirely. */
export function submitDirectWs(url: string): void {
    const ep = parseDirectWsUrl(url);
    if (!ep) {
        manualError = 'Enter a ws:// or wss:// URL';
        return;
    }
    manualError = undefined;
    saveEndpoint(window.localStorage, ep);
    connectWith(ep);
}

/** Submit a direct WebTransport endpoint (`https://` URL + optional self-signed cert hash). */
export function submitDirectWt(url: string, certHash: string): void {
    const result = parseDirectWtEndpoint(url, certHash);
    if ('error' in result) {
        manualError = result.error;
        return;
    }
    manualError = undefined;
    saveEndpoint(window.localStorage, result.endpoint);
    connectWith(result.endpoint);
}

/** Forget the persisted endpoint and return to the manual-entry screen, e.g. to switch daemons. */
export function forgetEndpoint(): void {
    clearStoredEndpoint(window.localStorage);
    controller?.stop();
    controller = undefined;
    client = undefined;
    endpoint = undefined;
    status = { state: 'connecting' };
    manualError = undefined;
    needsManualEndpoint = true;
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

function fetchCairnJson(): Promise<CairnJsonDoc | undefined> {
    return fetchCairnJsonAt('/cairn.json');
}

/** Fetch and parse a `/cairn.json` document from an arbitrary (possibly cross-origin) URL. */
async function fetchCairnJsonAt(url: string): Promise<CairnJsonDoc | undefined> {
    try {
        const res = await fetch(url);
        if (!res.ok) return undefined;
        return (await res.json()) as CairnJsonDoc;
    } catch {
        return undefined;
    }
}
