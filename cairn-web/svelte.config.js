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
};

export default config;
