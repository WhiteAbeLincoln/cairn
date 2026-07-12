import { defineConfig } from 'vitest/config';

// Kept separate from vite.config.ts so protocol tests run without the SvelteKit
// plugin: the protocol layer is plain TypeScript with zero framework deps.
export default defineConfig({
    test: {
        include: ['src/**/*.test.ts'],
        environment: 'node',
    },
});
