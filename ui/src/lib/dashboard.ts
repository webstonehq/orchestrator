import { readable } from 'svelte/store';
import { api, type Dashboard } from '$lib/api';

const POLL_MS = 5000;

export interface DashboardState {
	/** Last successfully fetched metrics (null until the first success). */
	data: Dashboard | null;
	/** True when the most recent poll failed; cleared on the next success. */
	error: boolean;
}

/**
 * Polls GET /api/dashboard every 5s while subscribed. `data` keeps its last
 * known value across failures (including 404 while the backend endpoint
 * doesn't exist yet); `error` reflects only the most recent poll.
 */
export const dashboardStore = readable<DashboardState>({ data: null, error: false }, (set) => {
	if (typeof window === 'undefined') return;

	let disposed = false;
	let current: DashboardState = { data: null, error: false };

	const poll = async () => {
		try {
			const data = await api.dashboard();
			if (disposed) return;
			current = { data, error: false };
			set(current);
		} catch {
			if (disposed || current.error) return;
			current = { data: current.data, error: true };
			set(current);
		}
	};

	void poll();
	const timer = setInterval(poll, POLL_MS);

	return () => {
		disposed = true;
		clearInterval(timer);
	};
});
