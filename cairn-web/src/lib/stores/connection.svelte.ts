// Thin Svelte 5 runes wrapper over endpoint discovery + `DaemonClient` +
// `ReconnectController`. All the actual logic (precedence, backoff, status
// transitions) lives in `endpoint.ts`/`reconnect.ts` and is unit-tested there;
// this module's job is only to own the reactive `$state` the UI reads and to
// supply the real `fetch`/`localStorage` boundaries those pure functions need.

import {
    type CloseableDialer,
    DaemonClient,
    wsDialer,
    wsMuxDialer,
    wtDialer,
} from '$lib/protocol';
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
import { sessionListEngine } from './sessionListEngine';

let client = $state<DaemonClient | undefined>(undefined);
let endpoint = $state<EndpointConfig | undefined>(undefined);
let status = $state<ConnectionStatus>({ state: 'connecting' });
let needsManualEndpoint = $state(false);
let manualError = $state<string | undefined>(undefined);

let controller: ReconnectController | undefined;
/** The live muxed control dialer (WS endpoints only), retired on replacement. */
let controlDialer: CloseableDialer | undefined;
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
    // Stop the controller (aborting its stale watch stream) before resetting
    // the engine — otherwise a straggling event from the old stream could
    // repopulate the list right after reset() clears it. The `signal.aborted`
    // guard in `run` below is the backstop if a stale settle lands anyway.
    controller?.stop();
    sessionListEngine.reset();
    controller = undefined;
    controlDialer?.close();
    controlDialer = undefined;
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

/** Notified on every connection status transition — used by `SessionDetail.svelte` to nudge a reattach on recovery. */
export function onConnectionStatusChange(fn: (status: ConnectionStatus) => void): () => void {
    statusListeners.add(fn);
    return () => statusListeners.delete(fn);
}

function connectWith(ep: EndpointConfig): void {
    endpoint = ep;
    needsManualEndpoint = false;
    // Retire the previous connection (e.g. switching daemons): stop its
    // controller (aborting its stale watch stream) BEFORE resetting the
    // engine — otherwise a straggling event from the old stream could
    // repopulate the list right after reset() clears it (the `signal.aborted`
    // guard in `run` below is the backstop if a stale settle lands anyway) —
    // and close its control socket: without the close, the old mux socket
    // stays open on both ends (the daemon's keepalive pings keep it warm),
    // leaking one socket + daemon serve loop per endpoint switch.
    if (controller) {
        controller.stop();
        sessionListEngine.reset();
    }
    controlDialer?.close();
    controlDialer = undefined;

    const isWs = ep.transport === 'ws';
    let c: DaemonClient;
    if (isWs) {
        // Control traffic (unary, wait, watch-sessions) rides one persistent
        // muxed socket; attach/logs/send keep dedicated one-shot sockets so
        // bulk streams stay off the control connection. No `onDown` hook: the
        // watch stream rides this socket, so socket death fails its channel
        // read in the same event-handler turn — the run settling below IS the
        // down signal.
        const control = wsMuxDialer(ep.url);
        controlDialer = control;
        c = new DaemonClient(control, wsDialer(ep.url));
    } else {
        c = new DaemonClient(wtDialer(ep.url, ep.certHash));
    }
    client = c;

    const next = new ReconnectController({
        // The watch stream is both the data feed and the liveness signal: its
        // death (resolve or reject) is what "disconnected" means now.
        run: async (onUp, signal) => {
            let live = false;
            for await (const ev of c.watchSessions(signal)) {
                if (signal.aborted) return; // stale stream must not touch shared state
                if (!live) {
                    live = true;
                    onUp();
                }
                sessionListEngine.applyEvent(ev);
            }
        },
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
