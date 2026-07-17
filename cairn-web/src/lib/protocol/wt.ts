import { fromWebStreams } from '@bytecodealliance/wrpc';
import type { Dialer } from './transport';

/**
 * A WebTransport-backed {@link Dialer}. One WebTransport session is opened and
 * cached; each dial opens a fresh bidirectional stream on it (one stream per
 * wRPC invocation, mirroring the one-socket-per-invocation WebSocket model).
 *
 * `certHash` is the hex SHA-256 of the daemon's certificate SPKI; when present
 * it is passed as `serverCertificateHashes`, letting the browser trust a
 * daemon's self-signed certificate (as published in `/cairn.json`).
 *
 * Note: WebTransport is a browser-only API with no Node implementation, so this
 * dialer is exercised in-browser only. It is intentionally thin: it just yields
 * a `Transport` via `fromWebStreams`, after which the shared invocation logic in
 * `DaemonClient` — covered by the in-process tests — takes over.
 */
export function wtDialer(url: string, certHash?: string): Dialer {
    let session: Promise<WebTransport> | undefined;

    const openSession = (): Promise<WebTransport> => {
        if (session) return session;
        const options: WebTransportOptions = {};
        if (certHash) {
            options.serverCertificateHashes = [
                { algorithm: 'sha-256', value: hexToBytes(certHash) },
            ];
        }
        const wt = new WebTransport(url, options);
        const ready = wt.ready.then(() => wt);
        session = ready;
        // Drop the cached session when it fails to open or later closes, so the
        // next dial reconnects instead of reusing a dead session.
        const forget = () => {
            if (session === ready) session = undefined;
        };
        ready.catch(forget);
        wt.closed.then(forget, forget);
        return ready;
    };

    return async () => {
        const wt = await openSession();
        const stream = await wt.createBidirectionalStream();
        return fromWebStreams(stream);
    };
}

/** Decode a hex string (tolerating `0x`, whitespace, and `:` separators). */
function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
    const clean = hex.trim().replace(/^0x/i, '').replace(/[\s:]/g, '');
    if (clean.length % 2 !== 0) {
        throw new Error('certHash must have an even number of hex digits');
    }
    const bytes = new Uint8Array(clean.length / 2);
    for (let i = 0; i < bytes.length; i++) {
        const byte = Number.parseInt(clean.slice(i * 2, i * 2 + 2), 16);
        if (Number.isNaN(byte)) {
            throw new Error(`invalid hex digit in certHash: ${hex}`);
        }
        bytes[i] = byte;
    }
    return bytes;
}
