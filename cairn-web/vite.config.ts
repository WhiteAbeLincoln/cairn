import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
    plugins: [sveltekit()],
    optimizeDeps: {
        // `@wterm/ghostty` locates its WASM via `new URL('../wasm/…', import.meta.url)`.
        // esbuild dep pre-bundling rewrites that reference and breaks the lookup, so
        // exclude the package: Vite then serves it as source and its own asset
        // pipeline emits (dev) / bundles (build) the `.wasm` with the right URL.
        exclude: ['@wterm/ghostty'],
    },
});
