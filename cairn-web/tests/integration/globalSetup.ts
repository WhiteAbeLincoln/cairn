// Fail the whole integration run fast, with an actionable message, if the
// daemon binary the suite needs hasn't been built. We do NOT build it here:
// the Rust build requires the nix devshell (zig + GHOSTTY_SOURCE_DIR) and can
// take minutes, so building is left to the developer / CI step. Point
// `CAIRN_DAEMON_BIN` at a prebuilt binary to override the default location.

import { existsSync } from 'node:fs';
import { DAEMON_BIN } from './harness';

export default function setup(): void {
    if (!existsSync(DAEMON_BIN)) {
        throw new Error(
            `cairn-daemon binary not found at ${DAEMON_BIN}.\n\n` +
                `The wire-interop suite drives the real daemon over ws://, so the\n` +
                `binary must exist first. Build it:\n\n` +
                `    cargo build -p cairn-daemon\n\n` +
                `Or set CAIRN_DAEMON_BIN to an existing binary.`,
        );
    }
}
