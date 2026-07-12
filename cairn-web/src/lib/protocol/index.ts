// The framework-free protocol layer: plain TypeScript over the official wRPC JS
// SDK. This is the surface later tasks (and future plugins) import from.

export { DaemonClient } from './client';
export type { Dialer, Transport } from './transport';
export * from './types';
export * as wit from './wit';
export { wsDialer } from './ws';
export { wtDialer } from './wt';
