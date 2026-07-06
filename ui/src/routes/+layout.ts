// Fully client-rendered SPA with normal pathname routing: SSR and
// prerendering are disabled here, and adapter-static's `fallback` option
// (see svelte.config.js) emits a single index.html that the Rust server
// returns for every non-API route, so deep links work.
export const ssr = false;
export const prerender = false;
