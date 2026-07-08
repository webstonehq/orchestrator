// Pure helpers for the flow builder: definition scaffolding, wire-shape
// normalization, validation-error path mapping, id generation, and a tiny
// display-only cron humanizer.

import {
	isParallel,
	type FlowDefinition,
	type InputType,
	type ParallelTaskSpec,
	type PluginManifest,
	type RegularTaskSpec,
	type TaskSpec,
	type TriggerSpec,
	type ValidationIssue
} from '../api';

export const INPUT_TYPES: InputType[] = ['STRING', 'ARRAY', 'DATE', 'INT', 'BOOLEAN', 'JSON'];

/** Fresh definition for the "new flow" screen. */
export function emptyDefinition(): FlowDefinition {
	return {
		name: 'new-flow',
		namespace: 'default',
		description: '',
		inputs: [],
		variables: [],
		triggers: [],
		tasks: []
	};
}

/**
 * Initialize builder state from a wire definition. The wire shape from
 * GET /api/flows/:id is already complete (serde serializes every non-skip
 * field), so this is a deep clone that only fills fields the serde layer
 * marks `#[serde(default)]` — it must NOT invent keys that serde skips when
 * None (`inputs[].default`, task `retry` / `timeout_seconds`), so a
 * round-trip back to the wire is lossless (deep-equal).
 */
export function initDefinition(def: FlowDefinition): FlowDefinition {
	return {
		name: def.name,
		namespace: def.namespace ?? 'default',
		description: def.description ?? '',
		inputs: (def.inputs ?? []).map((inp) => ({
			id: inp.id,
			type: inp.type,
			required: inp.required ?? false,
			...(inp.default !== undefined && inp.default !== null ? { default: inp.default } : {})
		})),
		variables: (def.variables ?? []).map((v) => ({ id: v.id, value: v.value })),
		// `env` is skip-when-empty on the wire: include it only when non-empty so
		// a round-trip stays deep-equal (never invent `env: []`).
		...(def.env && def.env.length > 0 ? { env: [...def.env] } : {}),
		triggers: (def.triggers ?? []).map((t) => ({
			id: t.id,
			type: t.type,
			cron: t.cron,
			timezone: t.timezone ?? 'UTC',
			catchup: t.catchup ?? 'latest',
			enabled: t.enabled ?? true
		})),
		tasks: (def.tasks ?? []).map(initTask)
	};
}

function initTask(task: TaskSpec): TaskSpec {
	if (isParallel(task)) {
		return {
			id: task.id,
			type: 'parallel',
			items: task.items,
			concurrency: task.concurrency,
			tasks: (task.tasks ?? []).map((t) => initTask(t) as RegularTaskSpec),
			outputs: (task.outputs ?? []).map((o) => ({ ...o }))
		};
	}
	const t: RegularTaskSpec = {
		id: task.id,
		type: task.type,
		...(task.retry !== undefined ? { retry: { ...task.retry } } : {}),
		...(task.timeout_seconds !== undefined ? { timeout_seconds: task.timeout_seconds } : {}),
		on_error: task.on_error ?? 'fail',
		config: structuredClone(task.config ?? {}),
		outputs: (task.outputs ?? []).map((o) => ({ ...o }))
	};
	return t;
}

/** All task ids in a definition, including parallel children. */
export function allTaskIds(def: FlowDefinition): string[] {
	const ids: string[] = [];
	for (const task of def.tasks) {
		ids.push(task.id);
		if (isParallel(task)) for (const child of task.tasks) ids.push(child.id);
	}
	return ids;
}

/** Next unused `<base>_N` id (N >= 1), unique across the whole flow. */
export function uniqueTaskId(def: FlowDefinition, base = 'task'): string {
	const taken = new Set(allTaskIds(def));
	let n = 1;
	while (taken.has(`${base}_${n}`)) n += 1;
	return `${base}_${n}`;
}

/**
 * Deep-sort object keys to mirror the canonical YAML emitter (sortDeep in
 * yaml.ts): serde_json (no preserve_order) sorts all config object keys
 * alphabetically. Scaffolding config in the same order means a freshly added
 * task already matches what a YAML round-trip produces, so an editor blur does
 * NOT reassign store.def just to reorder keys.
 */
function sortConfigDeep(value: unknown): unknown {
	if (Array.isArray(value)) return value.map(sortConfigDeep);
	if (value !== null && typeof value === 'object') {
		const out: Record<string, unknown> = {};
		for (const key of Object.keys(value as Record<string, unknown>).sort()) {
			out[key] = sortConfigDeep((value as Record<string, unknown>)[key]);
		}
		return out;
	}
	return value;
}

/** New plugin task scaffold: manifest defaults seeded into config. */
export function newPluginTask(id: string, manifest: PluginManifest): RegularTaskSpec {
	const config: Record<string, unknown> = {};
	for (const field of manifest.fields) {
		if (field.default !== null && field.default !== undefined) config[field.key] = field.default;
	}
	return {
		id,
		type: manifest.type_id,
		on_error: 'fail',
		config: sortConfigDeep(config) as Record<string, unknown>,
		outputs: []
	};
}

/** New parallel task scaffold with one child from `childManifest`. */
export function newParallelTask(
	id: string,
	childId: string,
	childManifest: PluginManifest | undefined
): ParallelTaskSpec {
	const child: RegularTaskSpec = childManifest
		? newPluginTask(childId, childManifest)
		: { id: childId, type: 'http.request', on_error: 'fail', config: {}, outputs: [] };
	return { id, type: 'parallel', items: '', concurrency: 8, tasks: [child], outputs: [] };
}

/** Default schedule trigger scaffold. */
export function newTrigger(): TriggerSpec {
	return {
		id: 'schedule',
		type: 'schedule',
		cron: '0 6 * * *',
		timezone: 'UTC',
		catchup: 'latest',
		enabled: true
	};
}

// ---------------------------------------------------------------------------
// Validation-error path mapping
// ---------------------------------------------------------------------------

/**
 * Fold validation issues into a path -> message map (first message wins per
 * path; extras are appended with "; "). Server paths look like
 * `tasks[2].config.url`, `triggers[0].cron`, `inputs[1].default`,
 * `tasks[1].tasks[0].config.url`, `tasks[0].outputs[1].extract`.
 */
export function issueMap(issues: ValidationIssue[]): Map<string, string> {
	const map = new Map<string, string>();
	for (const issue of issues) {
		const prev = map.get(issue.path);
		map.set(issue.path, prev ? `${prev}; ${issue.message}` : issue.message);
	}
	return map;
}

/**
 * Issues whose path is NOT in `handled` (exact match) and does not extend
 * any handled prefix rendered inline elsewhere — shown in a panel-top list.
 */
export function unmatchedIssues(
	issues: ValidationIssue[],
	matched: (path: string) => boolean
): ValidationIssue[] {
	return issues.filter((i) => !matched(i.path));
}

/** Issues scoped to one top-level definition section prefix. */
export function issuesUnder(issues: ValidationIssue[], prefix: string): ValidationIssue[] {
	return issues.filter((i) => i.path === prefix || i.path.startsWith(prefix + '.') || i.path.startsWith(prefix + '['));
}

function escapeRegExp(s: string): string {
	return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Paths a plugin task editor body renders inline: the id, everything under
 * `config` (per-field prefix aggregation in PluginTaskEditor covers deep
 * paths like `config.headers[0].value`), output name/extract cells, the
 * retry knobs, and the timeout.
 */
const TASK_BODY_PATTERN =
	'id$|config($|\\.)|outputs\\[\\d+\\]\\.(name|extract)$|retry\\.|timeout_seconds$';

/** Inline-path matcher for a top-level plugin task editor at `pathBase`. */
export function inlineTaskPathPattern(pathBase: string): RegExp {
	return new RegExp(`^${escapeRegExp(pathBase)}\\.(${TASK_BODY_PATTERN})`);
}

/**
 * Inline-path matcher for a parallel task editor at `pathBase`: its own
 * fields (id/items/concurrency/tasks/outputs) plus each child step's full
 * plugin-task-editor body.
 */
export function inlineParallelPathPattern(pathBase: string): RegExp {
	return new RegExp(
		`^${escapeRegExp(pathBase)}\\.(id$|items$|concurrency$|tasks$` +
			`|outputs\\[\\d+\\]\\.(name|extract)$` +
			`|tasks\\[\\d+\\]\\.(${TASK_BODY_PATTERN}))`
	);
}

// ---------------------------------------------------------------------------
// Cron humanizer (display-only)
// ---------------------------------------------------------------------------

const DAY_NAMES = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

/**
 * Best-effort humanizer for 5-field cron expressions, a display-only copy
 * of the Rust `humanize_cron` in src/api/flows.rs (which is authoritative
 * for the Schedules screen). Recognizes the same common numeric shapes and
 * falls back to the raw cron string.
 */
export function humanizeCron(cron: string): string {
	const fields = cron.trim().split(/\s+/);
	if (fields.length !== 5) return cron;
	const [minute, hour, dom, month, dow] = fields;
	const num = (s: string): number | null => (/^\d+$/.test(s) ? Number(s) : null);
	const m = num(minute);
	if (m !== null && m <= 59) {
		if (hour === '*' && dom === '*' && month === '*' && dow === '*') return 'hourly';
		const h = num(hour);
		if (h !== null && h <= 23 && dom === '*' && month === '*') {
			const hh = String(h).padStart(2, '0');
			const mm = String(m).padStart(2, '0');
			if (dow === '*') return `daily · ${hh}:${mm}`;
			const d = num(dow);
			if (d !== null && d <= 7) return `weekly · ${DAY_NAMES[d]}`;
		}
	}
	return cron;
}

// ---------------------------------------------------------------------------
// Timezones
// ---------------------------------------------------------------------------

const FALLBACK_TIMEZONES = [
	'UTC',
	'America/New_York',
	'America/Chicago',
	'America/Denver',
	'America/Los_Angeles',
	'America/Toronto',
	'America/Vancouver',
	'Europe/London',
	'Europe/Paris',
	'Europe/Berlin',
	'Asia/Tokyo',
	'Asia/Shanghai',
	'Asia/Kolkata',
	'Australia/Sydney'
];

/** IANA timezone names for the trigger timezone select. */
export function timezoneList(): string[] {
	try {
		const zones = Intl.supportedValuesOf('timeZone');
		return zones.includes('UTC') ? zones : ['UTC', ...zones];
	} catch {
		return FALLBACK_TIMEZONES;
	}
}

/**
 * Mirror of the Rust `slugify_flow_id` (src/api/flows.rs) used only to
 * preview the `id:` line in the YAML pane before a flow is created — the
 * server remains authoritative when the flow is saved.
 */
export function slugifyFlowId(name: string): string | null {
	const lower = name.toLowerCase();
	let out = '';
	for (const c of lower) {
		if (/[a-z]/.test(c) || (out !== '' && /[0-9]/.test(c))) out += c;
		else if (out !== '' && !out.endsWith('_')) out += '_';
	}
	while (out.endsWith('_')) out = out.slice(0, -1);
	out = out.slice(0, 64);
	return /^[a-z][a-z0-9_]*$/.test(out) ? out : null;
}
