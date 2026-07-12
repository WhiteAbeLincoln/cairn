// Endpoint selection: fetch `/cairn.json` -> prefer same-origin WS -> WT
// fallback -> manual entry (persisted, `?endpoint=`-overridable). Pure,
// framework-free logic (no direct `fetch`/`localStorage` reads) so it's
// testable with fakes at the boundary — see the design spec's "Web client"
// section and Task 7's brief.
//
// This is deliberately the *minimal* fallback: `?endpoint=`/manual entry only
// accept a direct `ws://`/`wss://` URL. The full standalone-hosting screen
// (daemon base-URL bootstrap, cert-hash field) is Task 9.

/** A resolved endpoint a `Dialer` can be built from. */
export interface EndpointConfig {
    transport: 'ws' | 'wt';
    url: string;
    certHash?: string;
}

/** The shape of `/cairn.json` relevant to endpoint selection. */
export interface CairnJsonDoc {
    endpoints?: {
        websocket?: string;
        webtransport?: { url: string; certHash?: string };
    };
}

export interface DiscoverDeps {
    /** The current page's full URL (read for `?endpoint=` and to resolve relative endpoints). */
    locationHref: string;
    /** Fetches same-origin `/cairn.json`. Resolves `undefined` on any failure — discovery falls through rather than throwing. */
    fetchCairnJson: () => Promise<CairnJsonDoc | undefined>;
    /** Reads a previously-persisted manual endpoint, if any. */
    readStored: () => EndpointConfig | undefined;
}

export type DiscoveryResult =
    | { status: 'resolved'; endpoint: EndpointConfig; source: 'query' | 'discovery' | 'stored' }
    | { status: 'manual-required' };

/**
 * Resolve the daemon endpoint in precedence order:
 *
 * 1. `?endpoint=<ws-url>` — explicit override, skips discovery entirely.
 * 2. `/cairn.json` — same-origin WebSocket preferred, WebTransport fallback.
 * 3. A previously-persisted manual endpoint (localStorage).
 * 4. Otherwise the caller must show a manual-entry prompt.
 */
export async function discoverEndpoint(deps: DiscoverDeps): Promise<DiscoveryResult> {
    const location = new URL(deps.locationHref);
    const queryEndpoint = location.searchParams.get('endpoint');
    if (queryEndpoint) {
        return {
            status: 'resolved',
            endpoint: { transport: 'ws', url: queryEndpoint },
            source: 'query',
        };
    }

    const doc = await deps.fetchCairnJson();
    const endpoints = doc?.endpoints;
    if (endpoints?.websocket) {
        return {
            status: 'resolved',
            endpoint: { transport: 'ws', url: toWsUrl(endpoints.websocket, location.origin) },
            source: 'discovery',
        };
    }
    if (endpoints?.webtransport) {
        return {
            status: 'resolved',
            endpoint: {
                transport: 'wt',
                url: endpoints.webtransport.url,
                certHash: endpoints.webtransport.certHash,
            },
            source: 'discovery',
        };
    }

    const stored = deps.readStored();
    if (stored) {
        return { status: 'resolved', endpoint: stored, source: 'stored' };
    }

    return { status: 'manual-required' };
}

/** Resolve `/cairn.json`'s `websocket` value (relative or absolute) to a `ws(s)://` URL. */
function toWsUrl(value: string, origin: string): string {
    const resolved = new URL(value, origin);
    if (resolved.protocol === 'http:') resolved.protocol = 'ws:';
    else if (resolved.protocol === 'https:') resolved.protocol = 'wss:';
    return resolved.toString();
}

// --- localStorage persistence ------------------------------------------

const STORAGE_KEY = 'cairn:endpoint';

/** The subset of the `Storage` interface the persistence helpers need. */
export interface StorageLike {
    getItem(key: string): string | null;
    setItem(key: string, value: string): void;
}

/** Read and validate a persisted endpoint; `undefined` for absent or corrupt data. */
export function loadStoredEndpoint(storage: StorageLike): EndpointConfig | undefined {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return undefined;
    try {
        const parsed = JSON.parse(raw);
        if (
            parsed &&
            typeof parsed.url === 'string' &&
            (parsed.transport === 'ws' || parsed.transport === 'wt')
        ) {
            return parsed as EndpointConfig;
        }
    } catch {
        // Corrupt localStorage value; treat as absent rather than throwing.
    }
    return undefined;
}

export function saveEndpoint(storage: StorageLike, endpoint: EndpointConfig): void {
    storage.setItem(STORAGE_KEY, JSON.stringify(endpoint));
}
