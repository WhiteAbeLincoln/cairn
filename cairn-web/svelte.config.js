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
    // A plain `customElement: true` boolean is normalized by Svelte to apply
    // to *every* compiled component, not just the one file that opts in via
    // its own `<svelte:options customElement={...}>` tag (see
    // `validate-options.js`'s `parametric()` — a boolean is wrapped into a
    // function returning that value for any filename). That forces every
    // component through the custom-element code path: `inject_styles` gets
    // set project-wide, so every component's `<style>` block is JS-injected
    // at runtime instead of being extracted into a cacheable `.css` asset,
    // and a dead, discarded `create_custom_element(...)` call is appended to
    // every component's compiled output. The function form scopes this to
    // the single file that actually needs it —
    // `src/lib/webcomponent/CairnTerminalElement.svelte` (the
    // `<cairn-terminal>` web component, built separately by
    // `vite.element.config.ts`) — so every other component compiles
    // normally; it's needed here (in the shared config `svelte-check` also
    // loads) so `svelte-check` (`npm run check`) recognizes that file's use
    // of the `$host()` rune as valid rather than an error.
    compilerOptions: {
        customElement: ({ filename }) =>
            filename?.endsWith('/webcomponent/CairnTerminalElement.svelte') ?? false,
    },
};

export default config;
