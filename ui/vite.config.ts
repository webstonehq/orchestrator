import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	build: {
		// Inline all imported assets (fonts) as data: URIs so the build is a
		// single self-contained index.html with no external file references.
		assetsInlineLimit: Infinity
	},
	server: {
		proxy: {
			'/api': 'http://127.0.0.1:4400'
		}
	}
});
