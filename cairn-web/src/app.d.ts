// See https://svelte.dev/docs/kit/types#app.d.ts
// for information about these interfaces.
declare global {
    namespace App {
        // interface Error {}
        // interface Locals {}
        // interface PageData {}
        // interface PageState {}
        // interface Platform {}
    }
}

// wterm ships its stylesheet under a bare subpath export (`@wterm/dom/css`)
// that doesn't end in `.css`, so vite/client's `*.css` ambient doesn't cover
// it. Declare it as a side-effect-only module so the import type-checks.
declare module '@wterm/dom/css';

export {};
