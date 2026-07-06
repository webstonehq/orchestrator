// Live run streaming: EventSource wiring + pure state reducers.
//
// Protocol (see src/api/runs.rs module doc):
//   GET /api/runs/:id/events
//     event `snapshot`  → RunDetail JSON + `last_log_id`
//     event `run`       → { status, finished_at?, error? }        (full-state)
//     event `task`      → { task_id, status, attempt }            (full-state)
//     event `items`     → { task_id, ...ItemAgg, throughput_per_sec }
//     event `log`       → { id, ts, level, task, message }        (append-only)
//     event `end`       → stream closes (run finished / not active)
//   A lagged receiver gets a fresh `snapshot` and continues.
//
// CRITICAL dedup rule: the snapshot and the live bridge overlap, so a `log`
// event whose id is <= the snapshot's `last_log_id` — or <= the max log id
// already seen — MUST be dropped. run/task/items events are idempotent
// full-state replacements, so replays are harmless.

import { writable, type Readable, type Subscriber, type Unsubscriber } from 'svelte/store';
import {
	api,
	ApiError,
	type FlowDefinition,
	type ItemAgg,
	type LogLine,
	type RunDetail,
	type RunStatus,
	type TaskRunView,
	type TaskStatus
} from '$lib/api';

// ---------------------------------------------------------------------------
// State + wire payload types
// ---------------------------------------------------------------------------

export interface LiveItemAgg extends ItemAgg {
	throughput_per_sec: number;
}

export interface RunStreamState {
	detail: RunDetail | null;
	/** Log lines sorted by id, deduplicated. */
	logs: LogLine[];
	/** Live fan-out aggregates keyed by parallel task id. */
	itemsAgg: Record<string, LiveItemAgg>;
	/** EventSource connected and streaming. */
	live: boolean;
	/** Server sent `end` (or polling saw a terminal run) — stream is over. */
	ended: boolean;
	/** SSE failed repeatedly; 3s polling fallback is active. */
	polling: boolean;
	/** The run does not exist (404). */
	notFound: boolean;
	/** Highest log id seen (snapshot last_log_id or appended lines). */
	maxLogId: number;
	/** True once older lines have been evicted by the MAX_LOG_LINES cap. */
	logsTruncated: boolean;
}

/**
 * Retention cap for the in-memory log buffer: only the newest lines are kept
 * (both live appends and history merges evict from the front). Dedup is
 * unaffected — `maxLogId` keeps advancing, so replays of evicted lines are
 * still dropped.
 */
export const MAX_LOG_LINES = 5000;

export interface SnapshotPayload extends RunDetail {
	last_log_id: number;
}

export interface RunEventPayload {
	status: RunStatus;
	finished_at?: string;
	error?: string;
}

export interface TaskEventPayload {
	task_id: string;
	status: TaskStatus;
	attempt: number;
}

export interface ItemsEventPayload extends LiveItemAgg {
	task_id: string;
}

export function initialState(): RunStreamState {
	return {
		detail: null,
		logs: [],
		itemsAgg: {},
		live: false,
		ended: false,
		polling: false,
		notFound: false,
		maxLogId: 0,
		logsTruncated: false
	};
}

// ---------------------------------------------------------------------------
// Deterministic reducers (mutate their argument; exported for tests)
// ---------------------------------------------------------------------------

const TERMINAL_TASK: ReadonlySet<TaskStatus> = new Set(['success', 'failed', 'canceled', 'skipped']);

/**
 * Replace the whole detail from a snapshot (initial, lagged-resync or poll).
 * Seeds itemsAgg from `fanout` (keeping any previously known throughput) and
 * advances maxLogId to `last_log_id`.
 *
 * Returns the log id history should be fetched after (the previous maxLogId)
 * or null when no history fetch is needed.
 */
export function applySnapshot(state: RunStreamState, snap: SnapshotPayload): number | null {
	const { last_log_id, ...detail } = snap;
	state.detail = detail;
	for (const [taskId, agg] of Object.entries(detail.fanout)) {
		const prev = state.itemsAgg[taskId];
		state.itemsAgg[taskId] = { ...agg, throughput_per_sec: prev?.throughput_per_sec ?? 0 };
	}
	const fetchAfter = last_log_id > state.maxLogId ? state.maxLogId : null;
	state.maxLogId = Math.max(state.maxLogId, last_log_id);
	return fetchAfter;
}

/**
 * Append one live `log` event. Returns false (dropped) when the id is <= the
 * max log id already seen — the snapshot/bridge overlap duplicate rule.
 */
export function applyLog(state: RunStreamState, line: LogLine): boolean {
	if (line.id <= state.maxLogId) return false;
	state.logs.push(line);
	state.maxLogId = line.id;
	if (state.logs.length > MAX_LOG_LINES) {
		state.logs.splice(0, state.logs.length - MAX_LOG_LINES);
		state.logsTruncated = true;
	}
	return true;
}

/**
 * Merge a page of history fetched from `/logs?after_id=`. Lines already
 * present (by id) are skipped; the result stays sorted by id. Unlike
 * `applyLog` this may insert *below* maxLogId — that is the point: history
 * fills the gap [after_id, last_log_id] announced by a snapshot.
 */
export function mergeLogs(state: RunStreamState, lines: LogLine[]): void {
	if (lines.length === 0) return;
	const seen = new Set(state.logs.map((l) => l.id));
	const fresh = lines.filter((l) => !seen.has(l.id));
	if (fresh.length === 0) return;
	const merged = [...state.logs, ...fresh].sort((a, b) => a.id - b.id);
	state.maxLogId = Math.max(state.maxLogId, merged[merged.length - 1].id);
	if (merged.length > MAX_LOG_LINES) {
		state.logs = merged.slice(merged.length - MAX_LOG_LINES);
		state.logsTruncated = true;
	} else {
		state.logs = merged;
	}
}

/**
 * Apply a `task` event: full-state replacement of status/attempt for one
 * task. The event carries no timestamps, so started_at/finished_at are
 * approximated with `nowIso` on the running → terminal transitions; the next
 * snapshot or the final refresh corrects them to server values.
 *
 * The snapshot only lists tasks that already have task_run rows, so an event
 * can name a task we have not seen yet — a synthetic row is appended for it
 * (definition ordering is restored by `orderTasks`).
 */
export function applyTask(state: RunStreamState, ev: TaskEventPayload, nowIso: string): void {
	if (!state.detail) return;
	let task = state.detail.tasks.find((t) => t.task_id === ev.task_id);
	if (!task) {
		task = {
			id: 0,
			run_id: state.detail.run.id,
			task_id: ev.task_id,
			status: 'pending',
			attempt: 0,
			result: null,
			outputs: null,
			error: null,
			started_at: null,
			finished_at: null
		};
		state.detail.tasks.push(task);
	}
	const wasTerminal = TERMINAL_TASK.has(task.status);
	task.status = ev.status;
	if (ev.attempt > 0) task.attempt = ev.attempt;
	if (ev.status === 'running' && !task.started_at) task.started_at = nowIso;
	if (TERMINAL_TASK.has(ev.status) && !wasTerminal && task.started_at && !task.finished_at) {
		task.finished_at = nowIso;
	}
	state.detail.run.tasks_done = state.detail.tasks.filter((t) =>
		TERMINAL_TASK.has(t.status)
	).length;
}

/** Apply a `run` event: full-state replacement of the run status fields. */
export function applyRun(state: RunStreamState, ev: RunEventPayload): void {
	if (!state.detail) return;
	state.detail.run.status = ev.status;
	if (ev.finished_at) state.detail.run.finished_at = ev.finished_at;
	if (ev.error !== undefined) state.detail.run.error = ev.error;
	const { started_at, finished_at } = state.detail.run;
	if (started_at && finished_at) {
		state.detail.run.duration_sec =
			(new Date(finished_at).getTime() - new Date(started_at).getTime()) / 1000;
	}
}

/** Apply an `items` event: full-state replacement of one fan-out aggregate. */
export function applyItems(state: RunStreamState, ev: ItemsEventPayload): void {
	const { task_id, ...agg } = ev;
	state.itemsAgg[task_id] = agg;
	if (state.detail && task_id in state.detail.fanout) {
		const { throughput_per_sec: _tps, ...counts } = agg;
		state.detail.fanout[task_id] = counts;
	}
}

/**
 * Order the run's task rows by the flow definition and add synthetic pending
 * rows for definition tasks that have no task_run row yet (the snapshot only
 * carries started tasks). Rows not present in the definition (e.g. renamed
 * tasks) keep their original order at the end. Pure; used by the run screen.
 */
export function orderTasks(tasks: TaskRunView[], def: FlowDefinition | null): TaskRunView[] {
	if (!def) return tasks;
	const byId = new Map(tasks.map((t) => [t.task_id, t]));
	const out: TaskRunView[] = def.tasks.map(
		(spec) =>
			byId.get(spec.id) ?? {
				id: 0,
				run_id: tasks[0]?.run_id ?? 0,
				task_id: spec.id,
				status: 'pending',
				attempt: 0,
				result: null,
				outputs: null,
				error: null,
				started_at: null,
				finished_at: null
			}
	);
	const defIds = new Set(def.tasks.map((t) => t.id));
	for (const t of tasks) if (!defIds.has(t.task_id)) out.push(t);
	return out;
}

// ---------------------------------------------------------------------------
// RunStream: EventSource + reconnect/backoff + polling fallback
// ---------------------------------------------------------------------------

const BACKOFF_MS = [1000, 2000, 4000, 8000, 15000];
const BACKOFF_CAP_MS = 15000;
const POLL_MS = 3000;
/** Server-side page size for GET /runs/:id/logs (default `limit`). */
const LOG_PAGE = 500;

/**
 * Rune-friendly store for one run's live state. Subscribe like any Svelte
 * store; call `start()` after construction and `destroy()` on teardown.
 *
 * Lifecycle: EventSource → snapshot → (one-time paged history fetch) → live
 * events. On error: reconnect with 1s,2s,4s,… (cap 15s) backoff; after 2
 * consecutive failures a 3s polling loop (runs.get + logs?after_id) runs
 * alongside the reconnect attempts until either SSE re-attaches or the run
 * reaches a terminal state. `end` → close, final runs.get refresh.
 */
export class RunStream implements Readable<RunStreamState> {
	private state = initialState();
	private store = writable<RunStreamState>(this.state);

	private es: EventSource | null = null;
	private failures = 0;
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	private pollTimer: ReturnType<typeof setInterval> | null = null;
	private destroyed = false;
	private everSnapshotted = false;

	constructor(private readonly runId: number) {}

	subscribe(run: Subscriber<RunStreamState>): Unsubscriber {
		return this.store.subscribe(run);
	}

	start(): void {
		this.connect();
	}

	destroy(): void {
		this.destroyed = true;
		this.closeEs();
		if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
		this.stopPolling();
	}

	/** Mutate state and notify subscribers. */
	private mutate(fn: (s: RunStreamState) => void): void {
		fn(this.state);
		this.store.set(this.state);
	}

	private closeEs(): void {
		this.es?.close();
		this.es = null;
	}

	// -- SSE ------------------------------------------------------------------

	private connect(): void {
		if (this.destroyed || this.state.ended || this.state.notFound) return;
		this.closeEs();
		const es = new EventSource(`/api/runs/${this.runId}/events`);
		this.es = es;

		es.addEventListener('snapshot', (e) => {
			this.failures = 0;
			this.everSnapshotted = true;
			this.stopPolling();
			let fetchAfter: number | null = null;
			this.mutate((s) => {
				s.live = true;
				s.polling = false;
				fetchAfter = applySnapshot(s, JSON.parse((e as MessageEvent).data) as SnapshotPayload);
			});
			if (fetchAfter !== null) void this.fetchLogs(fetchAfter);
		});

		es.addEventListener('log', (e) => {
			const line = JSON.parse((e as MessageEvent).data) as LogLine;
			this.mutate((s) => applyLog(s, line));
		});

		es.addEventListener('task', (e) => {
			const ev = JSON.parse((e as MessageEvent).data) as TaskEventPayload;
			this.mutate((s) => applyTask(s, ev, new Date().toISOString()));
		});

		es.addEventListener('items', (e) => {
			const ev = JSON.parse((e as MessageEvent).data) as ItemsEventPayload;
			this.mutate((s) => applyItems(s, ev));
		});

		es.addEventListener('run', (e) => {
			const ev = JSON.parse((e as MessageEvent).data) as RunEventPayload;
			this.mutate((s) => applyRun(s, ev));
		});

		es.addEventListener('end', () => {
			this.closeEs();
			this.stopPolling();
			this.mutate((s) => {
				s.live = false;
				s.ended = true;
			});
			void this.finalRefresh();
		});

		es.onerror = () => {
			if (this.destroyed || this.state.ended) return;
			this.closeEs();
			this.failures += 1;
			this.mutate((s) => {
				s.live = false;
			});
			// A run that never produced a snapshot may simply not exist.
			if (!this.everSnapshotted && this.failures === 1) void this.probeNotFound();
			if (this.failures >= 2) this.startPolling();
			const backoff = Math.min(
				BACKOFF_MS[Math.min(this.failures - 1, BACKOFF_MS.length - 1)],
				BACKOFF_CAP_MS
			);
			this.reconnectTimer = setTimeout(() => this.connect(), backoff);
		};
	}

	// -- HTTP helpers -----------------------------------------------------------

	/** Page /logs?after_id= forward until the backlog is drained. */
	private async fetchLogs(afterId: number): Promise<void> {
		let after = afterId;
		try {
			for (;;) {
				const { logs } = await api.runs.logs(this.runId, after);
				if (this.destroyed || logs.length === 0) return;
				this.mutate((s) => mergeLogs(s, logs));
				after = logs[logs.length - 1].id;
				if (logs.length < LOG_PAGE) return;
			}
		} catch {
			// history stays partial; the next snapshot/poll retries
		}
	}

	private async probeNotFound(): Promise<void> {
		try {
			await api.runs.get(this.runId);
		} catch (err) {
			if (!this.destroyed && err instanceof ApiError && err.status === 404) {
				this.mutate((s) => {
					s.notFound = true;
				});
				this.destroy();
			}
		}
	}

	private async finalRefresh(): Promise<void> {
		try {
			const detail = await api.runs.get(this.runId);
			if (this.destroyed) return;
			const after = this.state.maxLogId;
			this.mutate((s) => applySnapshot(s, { ...detail, last_log_id: after }));
			await this.fetchLogsTail();
		} catch {
			// keep the last streamed state
		}
	}

	/** Catch up any log lines written after the last one we have. */
	private async fetchLogsTail(): Promise<void> {
		await this.fetchLogs(this.state.maxLogId);
	}

	// -- Polling fallback ---------------------------------------------------------

	private startPolling(): void {
		if (this.pollTimer || this.destroyed) return;
		this.mutate((s) => {
			s.polling = true;
		});
		void this.pollOnce();
		this.pollTimer = setInterval(() => void this.pollOnce(), POLL_MS);
	}

	private stopPolling(): void {
		if (this.pollTimer) clearInterval(this.pollTimer);
		this.pollTimer = null;
		if (this.state.polling) {
			this.mutate((s) => {
				s.polling = false;
			});
		}
	}

	private async pollOnce(): Promise<void> {
		try {
			const detail = await api.runs.get(this.runId);
			if (this.destroyed) return;
			const after = this.state.maxLogId;
			this.mutate((s) => applySnapshot(s, { ...detail, last_log_id: after }));
			await this.fetchLogsTail();
			const status = detail.run.status;
			if (status !== 'running' && status !== 'queued') {
				this.mutate((s) => {
					s.ended = true;
				});
				this.destroy();
			}
		} catch (err) {
			if (err instanceof ApiError && err.status === 404) {
				this.mutate((s) => {
					s.notFound = true;
				});
				this.destroy();
			}
		}
	}
}
