import { describe, expect, it } from 'vitest';
import { commandBasename, relativeTime } from './format';

describe('commandBasename', () => {
    it('takes the last path segment', () => {
        expect(commandBasename(['/usr/bin/bash', '-l'])).toBe('bash');
    });

    it('passes through a bare command name unchanged', () => {
        expect(commandBasename(['bash'])).toBe('bash');
    });

    it('returns empty string for an empty argv', () => {
        expect(commandBasename([])).toBe('');
    });
});

describe('relativeTime', () => {
    const now = Date.parse('2026-01-01T00:00:00Z');

    it('formats seconds', () => {
        expect(relativeTime(now - 5_000, now)).toBe('5s ago');
    });

    it('formats minutes', () => {
        expect(relativeTime(now - 5 * 60_000, now)).toBe('5m ago');
    });

    it('formats hours', () => {
        expect(relativeTime(now - 3 * 3_600_000, now)).toBe('3h ago');
    });

    it('formats days', () => {
        expect(relativeTime(now - 2 * 86_400_000, now)).toBe('2d ago');
    });

    it('accepts a bigint timestamp (as SessionInfo.createdAtUnixMs carries)', () => {
        expect(relativeTime(BigInt(now - 90_000), now)).toBe('1m ago');
    });

    it('clamps a future timestamp to 0s rather than going negative', () => {
        expect(relativeTime(now + 5_000, now)).toBe('0s ago');
    });
});
