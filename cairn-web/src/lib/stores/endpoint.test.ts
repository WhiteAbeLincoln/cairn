import { describe, expect, it, vi } from 'vitest';
import {
    type CairnJsonDoc,
    discoverEndpoint,
    type EndpointConfig,
    loadStoredEndpoint,
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
});
