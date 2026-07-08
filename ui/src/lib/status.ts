// Status color system, mirroring the design mock's `S` map exactly.

export type Status =
	| 'success'
	| 'degraded'
	| 'running'
	| 'failed'
	| 'queued'
	| 'pending'
	| 'canceled'
	| 'skipped'
	| 'dropped';

export interface StatusStyle {
	color: string;
	bg: string;
	label: string;
}

export const STATUS: Record<Status, StatusStyle> = {
	success: { color: '#3fb950', bg: 'rgba(63,185,80,.12)', label: 'Success' },
	// run finished but a task failed under on_error: continue — amber, like the
	// item-level `dropped` it mirrors
	degraded: { color: '#e3b341', bg: 'rgba(227,179,65,.12)', label: 'Degraded' },
	running: { color: '#58a6ff', bg: 'rgba(88,166,255,.12)', label: 'Running' },
	failed: { color: '#f85149', bg: 'rgba(248,81,73,.12)', label: 'Failed' },
	queued: { color: '#e3b341', bg: 'rgba(227,179,65,.12)', label: 'Queued' },
	pending: { color: '#5a6675', bg: 'rgba(90,102,117,.10)', label: 'Pending' },
	canceled: { color: '#8a95a6', bg: 'rgba(138,149,166,.12)', label: 'Canceled' },
	// task_runs only: dimmer gray than canceled (--dim palette, like pending)
	skipped: { color: '#5a6675', bg: 'rgba(90,102,117,.10)', label: 'Skipped' },
	// fan-out items only: "dropped from batch" (on_error: continue) — amber
	dropped: { color: '#e3b341', bg: 'rgba(227,179,65,.12)', label: 'Dropped' }
};
