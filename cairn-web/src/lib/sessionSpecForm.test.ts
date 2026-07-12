import { describe, expect, it } from 'vitest';
import {
    buildSessionSpec,
    DEFAULT_SCROLLBACK_LINES,
    type SessionFormValues,
} from './sessionSpecForm';

/** Baseline form values matching the create form's initial state. */
function form(overrides: Partial<SessionFormValues> = {}): SessionFormValues {
    return {
        name: '',
        command: 'bash',
        workdir: '',
        envText: '',
        envInherit: true,
        scrollbackLines: DEFAULT_SCROLLBACK_LINES,
        idleTimeoutSecs: null,
        tty: true,
        stdin: true,
        ...overrides,
    };
}

describe('buildSessionSpec', () => {
    it('maps a minimal form to a spec with optional fields absent', () => {
        expect(buildSessionSpec(form())).toEqual({
            name: undefined,
            command: ['bash'],
            env: [],
            envInherit: true,
            workdir: undefined,
            tty: true,
            stdin: true,
            idleTimeoutSecs: undefined,
            scrollbackLines: DEFAULT_SCROLLBACK_LINES,
        });
    });

    it('splits the command on whitespace and trims name/workdir', () => {
        const spec = buildSessionSpec(
            form({ name: '  my-session  ', command: '  bash  -l ', workdir: ' /tmp ' }),
        );
        expect(spec.command).toEqual(['bash', '-l']);
        expect(spec.name).toBe('my-session');
        expect(spec.workdir).toBe('/tmp');
    });

    it('throws when the command is empty or whitespace-only', () => {
        expect(() => buildSessionSpec(form({ command: '   ' }))).toThrow('command is required');
    });

    // The idle-timeout regression class: Svelte's number-input binding yields
    // `number` for a filled field and `null` for a cleared one — never a string.
    it('converts an integer idle timeout to a bigint', () => {
        expect(buildSessionSpec(form({ idleTimeoutSecs: 300 })).idleTimeoutSecs).toBe(300n);
    });

    it('treats a cleared (null) idle timeout as no timeout', () => {
        expect(buildSessionSpec(form({ idleTimeoutSecs: null })).idleTimeoutSecs).toBeUndefined();
    });

    it('truncates a fractional idle timeout instead of throwing', () => {
        expect(buildSessionSpec(form({ idleTimeoutSecs: 2.5 })).idleTimeoutSecs).toBe(2n);
    });

    it('treats zero, negative, and NaN idle timeouts as no timeout', () => {
        expect(buildSessionSpec(form({ idleTimeoutSecs: 0 })).idleTimeoutSecs).toBeUndefined();
        expect(buildSessionSpec(form({ idleTimeoutSecs: -5 })).idleTimeoutSecs).toBeUndefined();
        expect(
            buildSessionSpec(form({ idleTimeoutSecs: Number.NaN })).idleTimeoutSecs,
        ).toBeUndefined();
    });

    it('falls back to the default when the scrollback input is cleared (null)', () => {
        expect(buildSessionSpec(form({ scrollbackLines: null })).scrollbackLines).toBe(
            DEFAULT_SCROLLBACK_LINES,
        );
    });

    it('parses env text into pairs, splitting on the first = and skipping malformed lines', () => {
        const spec = buildSessionSpec(
            form({ envText: 'FOO=bar\n  BAZ=a=b \nnot-a-pair\n\nEMPTY=' }),
        );
        expect(spec.env).toEqual([
            ['FOO', 'bar'],
            ['BAZ', 'a=b'],
            ['EMPTY', ''],
        ]);
    });
});
