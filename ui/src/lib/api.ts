// Typed fetch client for the Orchestrator API.
// Server-sent events (/api/runs/:id/events) are intentionally NOT wrapped
// here — live-run streaming owns its EventSource wiring.

// ---------------------------------------------------------------------------
// Wire types (mirror the Rust API contract)
// ---------------------------------------------------------------------------

export type RunStatus = 'queued' | 'running' | 'success' | 'degraded' | 'failed' | 'canceled';
export type TaskStatus = 'pending' | 'running' | 'success' | 'failed' | 'canceled' | 'skipped';
export type ItemStatus = 'queued' | 'running' | 'success' | 'failed' | 'canceled' | 'dropped';
export type LogLevel = 'INFO' | 'OK' | 'WARN' | 'ERR' | 'DBG';
export type InputType = 'STRING' | 'ARRAY' | 'DATE' | 'INT' | 'BOOLEAN' | 'JSON';
export type CatchupPolicy = 'none' | 'latest' | 'all';

export type FieldWidget =
	| 'select'
	| 'template'
	| 'keyvalue'
	| 'number'
	| 'duration'
	| 'toggle'
	| 'text'
	| 'code';

export interface FieldSpec {
	key: string;
	label: string;
	widget: FieldWidget;
	required: boolean;
	default?: unknown;
	help: string;
	options?: string[] | null;
	min?: number | null;
	max?: number | null;
	template: boolean;
}

export interface PluginManifest {
	type_id: string;
	label: string;
	description: string;
	icon: string;
	color: string;
	fields: FieldSpec[];
}

export interface InputSpec {
	id: string;
	type: InputType;
	required?: boolean;
	default?: string | null;
}

export interface VariableSpec {
	id: string;
	value: string;
}

export interface TriggerSpec {
	id: string;
	type: 'schedule';
	cron: string;
	timezone?: string;
	catchup?: CatchupPolicy;
	enabled?: boolean;
}

export interface RetrySpec {
	type: 'exponential';
	max_attempts: number;
	base_seconds: number;
}

export interface OutputSpec {
	name: string;
	type: InputType;
	extract: string;
}

/**
 * A plugin-backed task. Per the Rust wire shape (src/model/flow.rs),
 * `config` and `outputs` are always serialized; `retry` / `timeout_seconds`
 * are skip-if-None. NOTE: `type` is `string` (any plugin type_id), so
 * `task.type === 'parallel'` alone does not narrow — use `isParallel()`.
 */
export interface RegularTaskSpec {
	id: string;
	type: string;
	retry?: RetrySpec;
	timeout_seconds?: number;
	on_error?: 'fail' | 'continue';
	config: Record<string, unknown>;
	outputs: OutputSpec[];
}

/** A parallel fan-out task; all fields are always present on the wire. */
export interface ParallelTaskSpec {
	id: string;
	type: 'parallel';
	items: string;
	concurrency: number;
	tasks: RegularTaskSpec[];
	outputs: OutputSpec[];
}

export type TaskSpec = ParallelTaskSpec | RegularTaskSpec;

/** Type guard: narrows a TaskSpec to the parallel variant. */
export function isParallel(task: TaskSpec): task is ParallelTaskSpec {
	return task.type === 'parallel';
}

export interface FlowDefinition {
	name: string;
	namespace: string;
	description: string;
	inputs: InputSpec[];
	variables: VariableSpec[];
	/**
	 * Env var names the flow declares, referenced as `{{ env.<NAME> }}`.
	 * Optional/absent when empty — the server skips it on serialize, so it is
	 * omitted (never invented as `[]`) to keep wire round-trips lossless.
	 */
	env?: string[];
	triggers: TriggerSpec[];
	tasks: TaskSpec[];
}

export interface FlowSummary {
	id: string;
	name: string;
	namespace: string;
	paused: boolean;
	schedule_human: string;
	last_run: { status: RunStatus; finished_at: string | null } | null;
	success_rate_30d: number | null;
	avg_duration_sec: number | null;
	current_rev: number;
}

export interface FlowDetail {
	id: string;
	definition: FlowDefinition;
	current_rev: number;
	paused: boolean;
	updated_at: string;
}

export interface RevisionInfo {
	rev: number;
	message: string;
	created_at: string;
}

export interface ValidationIssue {
	path: string;
	message: string;
}

export interface Dashboard {
	active_flows: number;
	runs_24h: { total: number; ok: number; degraded: number; failed: number; running: number };
	success_rate_30d: number | null;
	avg_duration_sec: number | null;
	next_scheduled: { flow_id: string; at: string } | null;
}

export interface RunSummary {
	id: number;
	flow_id: string;
	flow_rev: number;
	status: RunStatus;
	trigger: string;
	inputs: Record<string, unknown>;
	scheduled_for: string | null;
	started_at: string | null;
	finished_at: string | null;
	error: string | null;
	/** Wall-clock seconds; null until both timestamps exist. */
	duration_sec: number | null;
	/** Task runs in a terminal state (success/failed/canceled/skipped). */
	tasks_done: number;
	/** Top-level task count in the run's definition revision. */
	tasks_total: number;
}

export interface RunCounts {
	all: number;
	running: number;
	success: number;
	degraded: number;
	failed: number;
	queued: number;
	canceled: number;
}

export interface RunListResponse {
	runs: RunSummary[];
	total: number;
	counts: RunCounts;
}

export interface TaskRunView {
	id: number;
	run_id: number;
	task_id: string;
	status: TaskStatus;
	attempt: number;
	/** Only present when the detail was fetched with `include_result=true`. */
	result?: unknown;
	outputs: Record<string, unknown> | null;
	error: string | null;
	started_at: string | null;
	finished_at: string | null;
}

export interface ItemAgg {
	total: number;
	queued: number;
	running: number;
	success: number;
	failed: number;
	dropped: number;
	retried: number;
}

export interface RunDetail {
	run: RunSummary;
	tasks: TaskRunView[];
	fanout: Record<string, ItemAgg>;
}

export interface LogLine {
	id: number;
	ts: string;
	level: LogLevel;
	task: string;
	message: string;
}

export interface ItemView {
	id: number;
	idx: number;
	item: unknown;
	status: ItemStatus;
	attempt: number;
	result: unknown;
	error: string | null;
	started_at: string | null;
	finished_at: string | null;
}

export interface ItemListResponse {
	items: ItemView[];
	total: number;
}

export interface ScheduleView {
	flow_id: string;
	flow_name: string;
	trigger_id: string;
	cron: string;
	timezone: string;
	human: string;
	catchup: CatchupPolicy;
	enabled: boolean;
	next_fire_at: string | null;
	last_fired_at: string | null;
	last_run_status: RunStatus | null;
}

export interface SecretInfo {
	name: string;
	created_at: string;
	updated_at: string;
}

export interface WorkerView {
	worker_id: string;
	queues: string[];
	capacity: number;
	in_flight: number;
	last_seen: string;
	online: boolean;
}

export interface WorkersResponse {
	/** Whether worker tokens are configured (BYOW enabled at all). */
	enabled: boolean;
	workers: WorkerView[];
}

export interface AuthUser {
	username: string;
}

// ---------------------------------------------------------------------------
// Client core
// ---------------------------------------------------------------------------

/**
 * Fired whenever any API call returns 401 (session missing or expired), so the
 * app can drop back to the login view from anywhere. Registered by the root
 * layout; `null` disables it.
 */
let onUnauthorized: (() => void) | null = null;
export function setUnauthorizedHandler(fn: (() => void) | null) {
	onUnauthorized = fn;
}

export class ApiError extends Error {
	status: number;
	/** Structured validation errors from 422 responses (`{errors: [...]}`). */
	errors?: ValidationIssue[];

	constructor(status: number, message: string, errors?: ValidationIssue[]) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.errors = errors;
	}
}

interface ExtractedError {
	message: string;
	errors?: ValidationIssue[];
}

async function extractError(res: Response): Promise<ExtractedError> {
	const fallback = `HTTP ${res.status}`;
	let text: string;
	try {
		text = await res.text();
	} catch {
		return { message: fallback };
	}
	text = text.trim();
	if (!text) return { message: fallback };
	try {
		const parsed: unknown = JSON.parse(text);
		if (parsed !== null && typeof parsed === 'object') {
			const obj = parsed as { error?: unknown; errors?: unknown };
			if (typeof obj.error === 'string') {
				return { message: obj.error };
			}
			// Validation shape: {errors: [{path, message}]} (422s).
			if (Array.isArray(obj.errors)) {
				const errors = obj.errors.filter(
					(e): e is ValidationIssue =>
						e !== null &&
						typeof e === 'object' &&
						typeof (e as ValidationIssue).path === 'string' &&
						typeof (e as ValidationIssue).message === 'string'
				);
				if (errors.length > 0) {
					const message = errors.map((e) => `${e.path}: ${e.message}`).join('; ');
					return { message, errors };
				}
			}
		}
	} catch {
		// not JSON — fall through to raw text
	}
	return { message: text.length > 300 ? fallback : text };
}

interface RequestOptions {
	json?: unknown;
	text?: string;
	contentType?: string;
}

async function request<T>(method: string, path: string, opts: RequestOptions = {}): Promise<T> {
	const headers: Record<string, string> = {};
	let body: string | undefined;
	if (opts.json !== undefined) {
		headers['content-type'] = 'application/json';
		body = JSON.stringify(opts.json);
	} else if (opts.text !== undefined) {
		headers['content-type'] = opts.contentType ?? 'text/yaml';
		body = opts.text;
	}

	const res = await fetch(path, { method, headers, body });
	if (!res.ok) {
		if (res.status === 401) onUnauthorized?.();
		const { message, errors } = await extractError(res);
		throw new ApiError(res.status, message, errors);
	}

	const raw = await res.text();
	if (!raw) return undefined as T;
	const contentType = res.headers.get('content-type') ?? '';
	if (contentType.includes('json')) return JSON.parse(raw) as T;
	return raw as T;
}

const get = <T>(path: string) => request<T>('GET', path);
const post = <T>(path: string, json?: unknown) => request<T>('POST', path, { json });
const put = <T>(path: string, json?: unknown) => request<T>('PUT', path, { json });
const del = <T>(path: string) => request<T>('DELETE', path);
const e = encodeURIComponent;

type QueryValue = string | number | boolean | undefined;

function qs(params: Record<string, QueryValue>): string {
	const search = new URLSearchParams();
	for (const [key, value] of Object.entries(params)) {
		if (value !== undefined && value !== '') search.set(key, String(value));
	}
	const s = search.toString();
	return s ? `?${s}` : '';
}

// ---------------------------------------------------------------------------
// API surface
// ---------------------------------------------------------------------------

export const api = {
	health: () => get<{ ok: boolean }>('/api/health'),
	plugins: () => get<PluginManifest[]>('/api/plugins'),
	dashboard: () => get<Dashboard>('/api/dashboard'),

	auth: {
		me: () => get<AuthUser>('/api/auth/me'),
		login: (username: string, password: string) =>
			post<AuthUser>('/api/auth/login', { username, password }),
		logout: () => post<{ ok: boolean }>('/api/auth/logout'),
		setupNeeded: () => get<{ needed: boolean }>('/api/auth/setup'),
		setup: (username: string, password: string) =>
			post<AuthUser>('/api/auth/setup', { username, password })
	},

	flows: {
		list: () => get<FlowSummary[]>('/api/flows'),
		create: (body: { id?: string; definition: FlowDefinition }) =>
			post<FlowDetail>('/api/flows', body),
		get: (id: string) => get<FlowDetail>(`/api/flows/${e(id)}`),
		update: (id: string, body: { definition: FlowDefinition; message?: string }) =>
			put<{ current_rev: number }>(`/api/flows/${e(id)}`, body),
		delete: (id: string) => del<void>(`/api/flows/${e(id)}`),
		pause: (id: string, paused: boolean) => post<void>(`/api/flows/${e(id)}/pause`, { paused }),
		revisions: (id: string) => get<RevisionInfo[]>(`/api/flows/${e(id)}/revisions`),
		revision: (id: string, rev: number) =>
			get<{ definition: FlowDefinition }>(`/api/flows/${e(id)}/revisions/${rev}`),
		validate: (definition: FlowDefinition) =>
			post<{ errors: ValidationIssue[] }>('/api/flows/validate', { definition }),
		exportYaml: (id: string) => get<string>(`/api/flows/${e(id)}/export`),
		importYaml: (yaml: string) =>
			request<FlowDetail>('POST', '/api/flows/import', { text: yaml, contentType: 'text/yaml' }),
		run: (id: string, body: { inputs: Record<string, unknown>; trigger?: string }) =>
			post<{ run_id: number }>(`/api/flows/${e(id)}/run`, body)
	},

	runs: {
		list: (params: { flow?: string; status?: string; page?: number; per?: number } = {}) =>
			get<RunListResponse>(`/api/runs${qs(params)}`),
		get: (id: number, opts: { includeResult?: boolean } = {}) =>
			get<RunDetail>(`/api/runs/${id}${qs({ include_result: opts.includeResult })}`),
		cancel: (id: number) => post<void>(`/api/runs/${id}/cancel`),
		replay: (id: number) => post<{ run_id: number }>(`/api/runs/${id}/replay`),
		logs: (id: number, afterId?: number) =>
			get<{ logs: LogLine[] }>(`/api/runs/${id}/logs${qs({ after_id: afterId })}`),
		items: (
			id: number,
			task: string,
			params: { status?: string; page?: number; per?: number } = {}
		) => get<ItemListResponse>(`/api/runs/${id}/tasks/${e(task)}/items${qs(params)}`),
		/** Compact per-item status string (one char per item, idx order). */
		itemsHeatmap: (id: number, task: string) =>
			get<{ statuses: string; total: number }>(
				`/api/runs/${id}/tasks/${e(task)}/items?format=heatmap`
			),
		retryFailed: (id: number, task: string) =>
			post<{ run_id: number }>(`/api/runs/${id}/tasks/${e(task)}/retry-failed`)
	},

	schedules: {
		// Read-only summary: enabled state is owned by each flow's definition
		// (edit it in the flow builder), so there is no toggle here.
		list: () => get<ScheduleView[]>('/api/schedules')
	},

	secrets: {
		list: () => get<SecretInfo[]>('/api/secrets'),
		put: (name: string, value: string) => put<void>(`/api/secrets/${e(name)}`, { value }),
		delete: (name: string) => del<void>(`/api/secrets/${e(name)}`)
	},

	workers: {
		list: () => get<WorkersResponse>('/api/workers')
	}
};
