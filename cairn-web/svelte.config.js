import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
    preprocess: vitePreprocess(),
    kit: {
        // Static SPA: a single fallback page handles all client-side routes
        // (the daemon serves the same file for unknown paths).
        adapter: adapter({ fallback: 'index.html' }),
    },
    // Gates whether a component's own `<svelte:options customElement={...}>`
    // tag takes effect (per-file, via Svelte's own analysis) — without this,
    // it's silently ignored with a warning. Only
    // `src/lib/webcomponent/CairnTerminalElement.svelte` declares that tag
    // (the `<cairn-terminal>` web component, built separately by
    // `vite.element.config.ts`), so enabling this here has no effect on any
    // other component's compiled output; it's needed so `svelte-check`
    // (`npm run check`, which loads this shared config) recognizes that file's
    // use of the `$host()` rune as valid rather than an error.
    compilerOptions: { customElement: true },
};

export default config;
