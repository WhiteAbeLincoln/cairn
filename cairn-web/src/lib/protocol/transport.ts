import type { Transport } from '@bytecodealliance/wrpc';

// Re-export the SDK's Transport so downstream code has a single import surface.
export type { Transport };

/**
 * Opens one byte-duplex {@link Transport} per wRPC invocation.
 *
 * The wRPC JS SDK models every invocation as a single bidirectional byte
 * stream, so `DaemonClient` dials a fresh transport for each call and closes it
 * when the call (or its stream) completes. Concrete dialers: {@link wsDialer}
 * (WebSocket) and {@link wtDialer} (WebTransport).
 */
export type Dialer = () => Promise<Transport>;
