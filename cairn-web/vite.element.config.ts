import { fileURLToPath } from 'node:url';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

// Separate build target for the `<cairn-terminal>` web component
// (`npm run build:element`), producing a standalone bundle that does not
// depend on SvelteKit or the rest of the app — plain HTML pages embed it with
// a <script type="module"> + <link rel="stylesheet"> pair (see the README).
//
// Kept out of `vite.config.ts` because the two builds compile Svelte
// differently: the SvelteKit app never sets `compilerOptions.customElement`
// (so `<Terminal>` behaves as a normal component there), while this target
// does (so `CairnTerminalElement.svelte`'s `<svelte:options customElement>`
// takes effect and it self-registers `customElements.define('cairn-terminal',
// ...)` on load).
export default defineConfig({
    plugins: [
        svelte({
            compilerOptions: { customElement: true },
            // Component <style> blocks get injected via JS at runtime (into
            // `document.head`, since the element uses `shadow: 'none'')
            // instead of Vite extracting them into a separate CSS module —
            // needed so *this* CSS (Terminal.svelte's + the wrapper's own
            // scoped styles) ships inside the single JS bundle. The
            // `@wterm/dom/css` side-effect import is a plain (non-Svelte)
            // stylesheet, so it isn't covered by this and still lands in the
            // separate `cairn-terminal.css` this build emits (see
            // `cssCodeSplit` below) — the README documents including both.
            emitCss: false,
        }),
    ],
    resolve: {
        alias: {
            $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
        },
    },
    optimizeDeps: {
        // Same reasoning as vite.config.ts: `@wterm/ghostty` locates its .wasm
        // via `new URL('../wasm/…', import.meta.url)`, which esbuild
        // pre-bundling would rewrite and break.
        exclude: ['@wterm/ghostty'],
    },
    build: {
        outDir: 'dist-element',
        emptyOutDir: true,
        // One JS + one CSS file for the whole component, not a chunk per
        // dependency — this is meant to be dropped into a plain HTML page.
        cssCodeSplit: false,
        lib: {
            entry: fileURLToPath(
                new URL('./src/lib/webcomponent/CairnTerminalElement.svelte', import.meta.url),
            ),
            formats: ['es'],
            fileName: () => 'cairn-terminal.js',
        },
        rollupOptions: {
            output: {
                // Vite defaults the CSS asset's basename to the package name
                // (`cairn-web.css`) rather than the lib entry's `fileName`;
                // override so the pair reads as `cairn-terminal.js` +
                // `cairn-terminal.css`.
                assetFileNames: 'cairn-terminal[extname]',
            },
        },
    },
});
