import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		// SPA mode with pathname routing: SSR/prerendering are disabled in
		// src/routes/+layout.ts and adapter-static emits a single fallback page
		// served for every route by the Rust binary (src/ui.rs). With
		// bundleStrategy 'inline' every JS/CSS chunk is inlined into it -> one
		// self-contained ui/build/index.html.
		adapter: adapter({ fallback: 'index.html' }),
		output: { bundleStrategy: 'inline' }
	}
};

export default config;
