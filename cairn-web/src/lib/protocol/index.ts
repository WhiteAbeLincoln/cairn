// The framework-free protocol layer: plain TypeScript over the official wRPC JS
// SDK. This is the surface later tasks (and future plugins) import from.

// The SDK's `Chan` is a proper awaitable async queue (push / close / async
// iterator, no polling) — it backs the attach client-event stream. Re-exported
// so callers get one from the protocol boundary rather than reaching into the
// vendored SDK directly.
export { Chan } from '@bytecodealliance/wrpc';
export { DaemonClient } from './client';
export type { Dialer, Transport } from './transport';
export * from './types';
export * as wit from './wit';
export { wsDialer } from './ws';
export { MUX_SUBPROTOCOL, type WsMuxOptions, wsMuxDialer } from './wsmux';
export { wtDialer } from './wt';
