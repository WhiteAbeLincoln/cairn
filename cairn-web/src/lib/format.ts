// Small formatting helpers shared by the session list and detail views. Kept
// framework-free and pure so they're trivially unit-testable.

import type { ExitStatus } from './protocol';

/** Human summary of an exit (e.g. `code 3`, `signal 15`, or a reason string). */
export function describeExit(exit: ExitStatus): string {
    const parts: string[] = [];
    if (exit.code !== undefined) parts.push(`code ${exit.code}`);
    if (exit.signal !== undefined) parts.push(`signal ${exit.signal}`);
    if (exit.reason) parts.push(exit.reason);
    return parts.join(', ') || 'exited';
}

/** Last path segment of a command's argv[0] (e.g. `/usr/bin/bash` -> `bash`). */
export function commandBasename(command: readonly string[]): string {
    const argv0 = command[0];
    if (!argv0) return '';
    const parts = argv0.split('/');
    return parts[parts.length - 1] ?? argv0;
}

/** Coarse "N unit ago" relative time, matching the granularity a session list needs. */
export function relativeTime(unixMs: bigint | number, now: number = Date.now()): string {
    const then = typeof unixMs === 'bigint' ? Number(unixMs) : unixMs;
    const seconds = Math.max(0, Math.floor((now - then) / 1000));
    if (seconds < 60) return `${seconds}s ago`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
}
