// Pure form-values -> `SessionSpec` conversion for the create-session form.
// Framework-free so the numeric edge cases (cleared number inputs bind as
// `null` in Svelte, fractional values, zero) are unit-testable without
// mounting the component.

import type { SessionSpec } from '$lib/protocol';

/** Used when the scrollback number input is cleared (Svelte binds it as `null`). */
export const DEFAULT_SCROLLBACK_LINES = 10_000;

/**
 * The create form's raw field values. Numeric fields are `number | null`
 * because Svelte's `<input type="number">` binding yields `null` (not `''`)
 * for an empty or invalid field.
 */
export interface SessionFormValues {
    name: string;
    command: string;
    workdir: string;
    /** Newline-separated `KEY=value` pairs; lines without `=` are ignored. */
    envText: string;
    envInherit: boolean;
    scrollbackLines: number | null;
    idleTimeoutSecs: number | null;
    tty: boolean;
    stdin: boolean;
}

/**
 * Build a wire-ready {@link SessionSpec}. Throws if the command is empty
 * (the form disables submit in that case; this is a defensive backstop).
 */
export function buildSessionSpec(form: SessionFormValues): SessionSpec {
    const command = form.command.trim().split(/\s+/).filter(Boolean);
    if (command.length === 0) {
        throw new Error('command is required');
    }
    return {
        name: form.name.trim() || undefined,
        command,
        env: parseEnvText(form.envText),
        envInherit: form.envInherit,
        workdir: form.workdir.trim() || undefined,
        tty: form.tty,
        stdin: form.stdin,
        idleTimeoutSecs: toIdleTimeout(form.idleTimeoutSecs),
        scrollbackLines: toScrollbackLines(form.scrollbackLines),
        httpProxy: undefined,
    };
}

/** Empty/cleared (`null`), invalid, and non-positive values all mean "no idle timeout". */
function toIdleTimeout(value: number | null): bigint | undefined {
    if (value === null || Number.isNaN(value) || value <= 0) return undefined;
    return BigInt(Math.trunc(value));
}

/**
 * Coerce the scrollback input to a wire-safe `u32`. Empty/cleared (`null`),
 * `NaN`, and negative values fall back to the default; fractional values are
 * truncated (the same treatment as {@link toIdleTimeout}). Without this, a
 * fractional or negative value would throw at the `u32` wire encoder.
 */
function toScrollbackLines(value: number | null): number {
    if (value === null || Number.isNaN(value) || value < 0) return DEFAULT_SCROLLBACK_LINES;
    return Math.trunc(value);
}

function parseEnvText(text: string): [string, string][] {
    return text
        .split('\n')
        .map((line) => line.trim())
        .filter((line) => line.includes('='))
        .map((line) => {
            const idx = line.indexOf('=');
            return [line.slice(0, idx), line.slice(idx + 1)] as [string, string];
        });
}
