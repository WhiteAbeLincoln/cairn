import { describe, expect, it, vi } from 'vitest';
import {
    type CairnJsonDoc,
    clearStoredEndpoint,
    discoverEndpoint,
    type EndpointConfig,
    loadStoredEndpoint,
    parseDirectWsUrl,
    parseDirectWtEndpoint,
    resolveFromBaseUrl,
    type StorageLike,
    saveEndpoint,
} from './endpoint';

/** An in-memory `StorageLike` fake, standing in for `window.localStorage`. */
function fakeStorage(initial: Record<string, string> = {}): StorageLike {
    const map = new Map(Object.entries(initial));
    return {
        getItem: (key) => map.get(key) ?? null,
        setItem: (key, value) => {
            map.set(key, value);
        },
        removeItem: (key) => {
            map.delete(key);
        },
    };
}

const noStored = () => undefined;
const wtEndpoint: EndpointConfig = { transport: 'wt', url: 'https://example.com:4433' };

describe('discoverEndpoint precedence', () => {
    it('prefers the `?endpoint=` query override and never calls fetchCairnJson', async () => {
        const fetchCairnJson = vi.fn(async () => ({ endpoints: { websocket: '/ws' } }));
        const result = await discoverEndpoint({
            locationHref: 'https://app.example/sessions?endpoint=ws://localhost:9000/ws',
            fetchCairnJson,
            readStored: noStored,
        });
        expect(result).toEqual({
            status: 'resolved',
            endpoint: { transport: 'ws', url: 'ws://localhost:9000/ws' },
            source: 'query',
        });
        expect(fetchCairnJson).not.toHaveBeenCalled();
    });

    it('prefers a same-origin websocket endpoint over webtransport when both are present', async () => {
        const doc: CairnJsonDoc = {
            endpoints: { websocket: '/ws', webtransport: { url: 'https://host:4433' } },
        };
        const result = await discoverEndpoint({
            locationHref: 'http://127.0.0.1:8080/sessions',
            fetchCairnJson: async () => doc,
            readStored: noStored,
        });
        expect(result).toEqual({
            status: 'resolved',
            endpoint: { transport: 'ws', url: 'ws://127.0.0.1:8080/ws' },
            source: 'discovery',
        });
    });

    it('resolves an absolute websocket URL from /cairn.json unchanged (http -> ws, https -> wss)', async () => {
        const result = await discoverEndpoint({
            locationHref: 'https://ui.example/',
            fetchCairnJson: async () => ({ endpoints: { websocket: 'https://daemon:8080/ws' } }),
            readStored: noStored,
        });
        expect(result).toEqual({
            status: 'resolved',
            endpoint: { transport: 'ws', url: 'wss://daemon:8080/ws' },
            source: 'discovery',
        });
    });

    it('falls back to webtransport when discovery has no websocket entry', async () => {
        const result = await discoverEndpoint({
            locationHref: 'http://127.0.0.1:8080/',
            fetchCairnJson: async () => ({
                endpoints: { webtransport: { url: 'https://127.0.0.1:4433', certHash: 'abcd' } },
            }),
            readStored: noStored,
        });
        expect(result).toEqual({
            status: 'resolved',
            endpoint: { transport: 'wt', url: 'https://127.0.0.1:4433', certHash: 'abcd' },
            source: 'discovery',
        });
    });

    it('falls back to the stored endpoint when the fetch fails', async () => {
        const result = await discoverEndpoint({
            locationHref: 'http://127.0.0.1:8080/',
            fetchCairnJson: async () => undefined,
            readStored: () => wtEndpoint,
        });
        expect(result).toEqual({ status: 'resolved', endpoint: wtEndpoint, source: 'stored' });
    });

    it('falls back to the stored endpoint when /cairn.json has no usable endpoints', async () => {
        const result = await discoverEndpoint({
            locationHref: 'http://127.0.0.1:8080/',
            fetchCairnJson: async () => ({ endpoints: {} }),
            readStored: () => wtEndpoint,
        });
        expect(result).toEqual({ status: 'resolved', endpoint: wtEndpoint, source: 'stored' });
    });

    it('requires manual entry when nothing else resolves', async () => {
        const result = await discoverEndpoint({
            locationHref: 'http://127.0.0.1:8080/',
            fetchCairnJson: async () => undefined,
            readStored: noStored,
        });
        expect(result).toEqual({ status: 'manual-required' });
    });
});

describe('endpoint persistence', () => {
    it('round-trips a saved endpoint through storage', () => {
        const storage = fakeStorage();
        saveEndpoint(storage, wtEndpoint);
        expect(loadStoredEndpoint(storage)).toEqual(wtEndpoint);
    });

    it('treats missing storage as absent', () => {
        expect(loadStoredEndpoint(fakeStorage())).toBeUndefined();
    });

    it('treats corrupt JSON as absent rather than throwing', () => {
        expect(loadStoredEndpoint(fakeStorage({ 'cairn:endpoint': '{not json' }))).toBeUndefined();
    });

    it('treats a malformed stored value (wrong shape) as absent', () => {
        const storage = fakeStorage({
            'cairn:endpoint': JSON.stringify({ transport: 'carrier-pigeon' }),
        });
        expect(loadStoredEndpoint(storage)).toBeUndefined();
    });

    it('clearStoredEndpoint removes a previously saved endpoint', () => {
        const storage = fakeStorage();
        saveEndpoint(storage, wtEndpoint);
        clearStoredEndpoint(storage);
        expect(loadStoredEndpoint(storage)).toBeUndefined();
    });
});

describe('resolveFromBaseUrl (standalone manual screen: daemon base URL)', () => {
    it('fetches <origin>/cairn.json and prefers websocket, resolved against the base origin', async () => {
        const fetchJson = vi.fn(async (url: string) => {
            expect(url).toBe('http://daemon.example:8080/cairn.json');
            return { endpoints: { websocket: '/ws' } } satisfies CairnJsonDoc;
        });
        const result = await resolveFromBaseUrl('http://daemon.example:8080', fetchJson);
        expect(result).toEqual({
            status: 'resolved',
            endpoint: { transport: 'ws', url: 'ws://daemon.example:8080/ws' },
        });
    });

    it('treats a bare host:port (no scheme) as http://', async () => {
        const fetchJson = vi.fn(async (url: string) => {
            expect(url).toBe('http://localhost:8080/cairn.json');
            return { endpoints: { websocket: '/ws' } } satisfies CairnJsonDoc;
        });
        const result = await resolveFromBaseUrl('localhost:8080', fetchJson);
        expect(result.status).toBe('resolved');
    });

    it('falls back to webtransport when no websocket entry is present', async () => {
        const result = await resolveFromBaseUrl('https://daemon.example:4433', async () => ({
            endpoints: { webtransport: { url: 'https://daemon.example:4433', certHash: 'ab12' } },
        }));
        expect(result).toEqual({
            status: 'resolved',
            endpoint: {
                transport: 'wt',
                url: 'https://daemon.example:4433',
                certHash: 'ab12',
            },
        });
    });

    it('reports an error when the fetch fails', async () => {
        const result = await resolveFromBaseUrl('http://unreachable:9', async () => undefined);
        expect(result).toEqual({
            status: 'error',
            message: 'Could not reach http://unreachable:9/cairn.json',
        });
    });

    it('reports an error when cairn.json has no usable endpoints', async () => {
        const result = await resolveFromBaseUrl('http://daemon.example:8080', async () => ({
            endpoints: {},
        }));
        expect(result.status).toBe('error');
    });

    it('reports an error for an unparseable URL', async () => {
        const result = await resolveFromBaseUrl('http://[::not-an-ip', async () => undefined);
        expect(result.status).toBe('error');
    });

    it('rejects an empty URL without calling fetch', async () => {
        const fetchJson = vi.fn();
        const result = await resolveFromBaseUrl('   ', fetchJson);
        expect(result).toEqual({ status: 'error', message: 'Enter a daemon URL' });
        expect(fetchJson).not.toHaveBeenCalled();
    });
});

describe('parseDirectWsUrl', () => {
    it('accepts a ws:// URL', () => {
        expect(parseDirectWsUrl('ws://localhost:8080/ws')).toEqual({
            transport: 'ws',
            url: 'ws://localhost:8080/ws',
        });
    });

    it('accepts a wss:// URL', () => {
        expect(parseDirectWsUrl(' wss://daemon.example/ws ')).toEqual({
            transport: 'ws',
            url: 'wss://daemon.example/ws',
        });
    });

    it('rejects a non-ws URL', () => {
        expect(parseDirectWsUrl('https://daemon.example')).toBeUndefined();
        expect(parseDirectWsUrl('not a url')).toBeUndefined();
    });
});

describe('parseDirectWtEndpoint', () => {
    it('accepts an https:// URL with no cert hash', () => {
        expect(parseDirectWtEndpoint('https://daemon.example:4433')).toEqual({
            endpoint: { transport: 'wt', url: 'https://daemon.example:4433', certHash: undefined },
        });
    });

    it('accepts an https:// URL with a hex cert hash, tolerating separators', () => {
        expect(parseDirectWtEndpoint('https://127.0.0.1:4433', 'ab:12:ef:00')).toEqual({
            endpoint: {
                transport: 'wt',
                url: 'https://127.0.0.1:4433',
                certHash: 'ab:12:ef:00',
            },
        });
    });

    it('rejects a non-https URL', () => {
        expect(parseDirectWtEndpoint('http://daemon.example:4433')).toEqual({
            error: 'WebTransport endpoint must be an https:// URL',
        });
        expect(parseDirectWtEndpoint('ws://daemon.example:4433')).toEqual({
            error: 'WebTransport endpoint must be an https:// URL',
        });
    });

    it('rejects a malformed cert hash', () => {
        expect(parseDirectWtEndpoint('https://daemon.example:4433', 'not-hex!!')).toEqual({
            error: 'Cert hash must be a hex string (as printed by the daemon)',
        });
        expect(parseDirectWtEndpoint('https://daemon.example:4433', 'abc')).toEqual({
            error: 'Cert hash must be a hex string (as printed by the daemon)',
        });
    });
});
