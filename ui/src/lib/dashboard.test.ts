import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { dashboardStore } from './dashboard';

function stubFetchOk(body: unknown) {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => {
			return new Response(JSON.stringify(body), {
				status: 200,
				headers: { 'content-type': 'application/json' }
			});
		})
	);
}

function stubFetchFail() {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => {
			throw new TypeError('network down');
		})
	);
}

/** Let pending fetch/json promise chains settle (microtasks only). */
async function flush() {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}

describe('dashboardStore', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		// The store bails out during SSR; pretend we are in a browser.
		vi.stubGlobal('window', {});
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	it('flags failures, keeps last data, and recovers on success', async () => {
		stubFetchFail();
		const unsub = dashboardStore.subscribe(() => {});

		// initial value before the first poll settles
		expect(get(dashboardStore)).toEqual({ data: null, error: false });

		// first poll fails -> error flag set, still no data
		await flush();
		expect(get(dashboardStore)).toEqual({ data: null, error: true });

		// next poll succeeds -> data lands, error clears
		stubFetchOk({ active_flows: 2 });
		await vi.advanceTimersByTimeAsync(5000);
		await flush();
		let state = get(dashboardStore);
		expect(state.error).toBe(false);
		expect(state.data).toMatchObject({ active_flows: 2 });

		// a later failure keeps the last data but raises the error flag
		stubFetchFail();
		await vi.advanceTimersByTimeAsync(5000);
		await flush();
		state = get(dashboardStore);
		expect(state.error).toBe(true);
		expect(state.data).toMatchObject({ active_flows: 2 });

		unsub();
	});
});
