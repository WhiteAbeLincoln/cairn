import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

// Kept separate from vite.config.ts so protocol tests run without the SvelteKit
// plugin: the protocol layer is plain TypeScript with zero framework deps.
export default defineConfig({
    // SvelteKit's `$lib` alias is provided by its Vite plugin, which we omit
    // here; wire it up so the framework-free modules under test can use the same
    // import specifier as the app code (and its runtime imports resolve).
    resolve: {
        alias: {
            $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
        },
    },
    test: {
        include: ['src/**/*.test.ts'],
        environment: 'node',
    },
});
