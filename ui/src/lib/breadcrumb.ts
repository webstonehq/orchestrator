import { writable } from 'svelte/store';

/**
 * Breadcrumb segments shown in the header after the fixed "orchestrator"
 * root. Each route sets this on mount, e.g. `breadcrumb.set(['flows', 'new'])`.
 * The last segment renders in --text, the rest in --dim.
 */
export const breadcrumb = writable<string[]>([]);
