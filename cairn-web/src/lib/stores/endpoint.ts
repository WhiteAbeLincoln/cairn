// Endpoint selection: fetch `/cairn.json` -> prefer same-origin WS -> WT
// fallback -> manual entry (persisted, `?endpoint=`-overridable). Pure,
// framework-free logic (no direct `fetch`/`localStorage` reads) so it's
// testable with fakes at the boundary — see the design spec's "Web client"
// section and Task 7's brief.
//
// Task 9 adds the full standalone-hosting manual screen on top of this: a
// daemon base-URL bootstrap (fetch that host's `/cairn.json`, same
// WS-preferred/WT-fallback selection as automatic discovery, just resolved
// against the given base's origin instead of `location.origin`), plus direct
// `ws(s)://` and WebTransport (`https://` + optional cert-hash) entry.

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
    const endpoint = pickEndpointFromDoc(doc, location.origin);
    if (endpoint) {
        return { status: 'resolved', endpoint, source: 'discovery' };
    }

    const stored = deps.readStored();
    if (stored) {
        return { status: 'resolved', endpoint: stored, source: 'stored' };
    }

    return { status: 'manual-required' };
}

/**
 * Pick an endpoint from a `/cairn.json` document: same-origin WebSocket
 * preferred, WebTransport fallback. `origin` is the origin relative-URL
 * fields are resolved against — the page's own origin for automatic
 * same-origin discovery, or a manually-entered daemon base URL's origin for
 * {@link resolveFromBaseUrl}.
 */
function pickEndpointFromDoc(
    doc: CairnJsonDoc | undefined,
    origin: string,
): EndpointConfig | undefined {
    const endpoints = doc?.endpoints;
    if (endpoints?.websocket) {
        return { transport: 'ws', url: toWsUrl(endpoints.websocket, origin) };
    }
    if (endpoints?.webtransport) {
        return {
            transport: 'wt',
            url: endpoints.webtransport.url,
            certHash: endpoints.webtransport.certHash,
        };
    }
    return undefined;
}

/** Resolve `/cairn.json`'s `websocket` value (relative or absolute) to a `ws(s)://` URL. */
function toWsUrl(value: string, origin: string): string {
    const resolved = new URL(value, origin);
    if (resolved.protocol === 'http:') resolved.protocol = 'ws:';
    else if (resolved.protocol === 'https:') resolved.protocol = 'wss:';
    return resolved.toString();
}

// --- manual endpoint screen (standalone hosting) ------------------------

export type BaseUrlResolution =
    | { status: 'resolved'; endpoint: EndpointConfig }
    | { status: 'error'; message: string };

/**
 * Bootstrap an endpoint from a daemon base URL entered by hand (the
 * standalone-hosting manual screen): fetch `<base>/cairn.json` (served
 * CORS-open specifically so this works from any origin) and apply the same
 * WS-preferred/WT-fallback selection as automatic discovery, resolved
 * against the *given* base URL's origin rather than `location.origin`. A
 * bare `host:port` (no scheme) is treated as `http://host:port`, matching the
 * daemon's plain-HTTP `ws://` listener posture.
 */
export async function resolveFromBaseUrl(
    baseUrl: string,
    fetchJson: (url: string) => Promise<CairnJsonDoc | undefined>,
): Promise<BaseUrlResolution> {
    const trimmed = baseUrl.trim();
    if (!trimmed) return { status: 'error', message: 'Enter a daemon URL' };
    const withScheme = /^https?:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`;

    let origin: string;
    try {
        origin = new URL(withScheme).origin;
    } catch {
        return { status: 'error', message: `Invalid URL: ${baseUrl}` };
    }

    const doc = await fetchJson(`${origin}/cairn.json`);
    if (!doc) {
        return { status: 'error', message: `Could not reach ${origin}/cairn.json` };
    }
    const endpoint = pickEndpointFromDoc(doc, origin);
    if (!endpoint) {
        return { status: 'error', message: `${origin}/cairn.json has no usable endpoints` };
    }
    return { status: 'resolved', endpoint };
}

/** Parse a direct `ws://`/`wss://` URL entry. `undefined` when it doesn't match. */
export function parseDirectWsUrl(value: string): EndpointConfig | undefined {
    const trimmed = value.trim();
    if (!/^wss?:\/\//i.test(trimmed)) return undefined;
    return { transport: 'ws', url: trimmed };
}

export type WtEndpointResult = { endpoint: EndpointConfig } | { error: string };

/**
 * Parse a direct WebTransport endpoint entry: an `https://` URL plus an
 * optional hex cert-hash (needed only when the daemon presents a self-signed
 * certificate — see `wtDialer`'s `serverCertificateHashes` pinning).
 */
export function parseDirectWtEndpoint(url: string, certHash?: string): WtEndpointResult {
    const trimmed = url.trim();
    if (!/^https:\/\//i.test(trimmed)) {
        return { error: 'WebTransport endpoint must be an https:// URL' };
    }
    const hash = certHash?.trim();
    if (hash && !isHexHash(hash)) {
        return { error: 'Cert hash must be a hex string (as printed by the daemon)' };
    }
    return { endpoint: { transport: 'wt', url: trimmed, certHash: hash || undefined } };
}

/** Loosely validate a hex cert-hash string (tolerating `0x`/whitespace/`:` separators, matching `wtDialer`'s own parser). */
function isHexHash(value: string): boolean {
    const clean = value.replace(/^0x/i, '').replace(/[\s:]/g, '');
    return clean.length > 0 && clean.length % 2 === 0 && /^[0-9a-fA-F]+$/.test(clean);
}

// --- localStorage persistence ------------------------------------------

const STORAGE_KEY = 'cairn:endpoint';

/** The subset of the `Storage` interface the persistence helpers need. */
export interface StorageLike {
    getItem(key: string): string | null;
    setItem(key: string, value: string): void;
    removeItem(key: string): void;
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

/** Forget a persisted endpoint, so the manual-entry screen resurfaces on next load. */
export function clearStoredEndpoint(storage: StorageLike): void {
    storage.removeItem(STORAGE_KEY);
}
