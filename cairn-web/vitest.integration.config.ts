import { defineConfig } from 'vitest/config';

// Integration ("wire interop gate") config: the browser protocol stack driven
// against a REAL cairn-daemon over ws://. Kept separate from vitest.config.ts
// (which runs the fast in-process protocol tests) because these spawn a daemon
// process and need long timeouts.
//
// Prerequisite: the daemon binary must be built first —
//     cargo build -p cairn-daemon
// globalSetup fails fast with instructions if it is missing.
//
// Run with:  npm run test:integration
export default defineConfig({
    test: {
        include: ['tests/integration/**/*.test.ts'],
        environment: 'node',
        globalSetup: ['tests/integration/globalSetup.ts'],
        // A daemon spawn + PTY round-trips + a multi-second CPU window all fit
        // inside one generous per-test budget.
        testTimeout: 30_000,
        hookTimeout: 30_000,
        // One daemon is shared across the file; never run test files in
        // parallel against it.
        fileParallelism: false,
    },
});
