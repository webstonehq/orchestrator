import { describe, expect, it } from 'vitest';
import {
	allTaskIds,
	emptyDefinition,
	humanizeCron,
	initDefinition,
	inlineParallelPathPattern,
	inlineTaskPathPattern,
	issueMap,
	issuesUnder,
	newPluginTask,
	slugifyFlowId,
	uniqueTaskId
} from './defs';
import type { FlowDetail, PluginManifest, ValidationIssue } from '../api';
import fixtureGet from './fixtures/council_alert_pipeline.get.json';

const detail = fixtureGet as unknown as FlowDetail;

describe('initDefinition', () => {
	it('round-trips the real GET response without loss (deep-equal)', () => {
		// The fixture is a live server response; feeding it through the
		// builder's state-init must preserve every field and not invent keys
		// that serde skips when None (retry / timeout_seconds / default).
		const init = initDefinition(detail.definition);
		expect(init).toEqual(detail.definition);
		// Specifically: the parallel child has no retry/timeout on the wire.
		const parallel = init.tasks[1];
		expect(parallel.type).toBe('parallel');
		const child = (parallel as unknown as { tasks: Record<string, unknown>[] }).tasks[0];
		expect('retry' in child).toBe(false);
		expect('timeout_seconds' in child).toBe(false);
	});

	it('clones deeply (edits do not leak back into the source)', () => {
		const init = initDefinition(detail.definition);
		init.tasks[0].id = 'renamed';
		(init.tasks[0] as { config: Record<string, unknown> }).config.method = 'POST';
		expect(detail.definition.tasks[0].id).toBe('discover');
		expect((detail.definition.tasks[0] as { config: Record<string, unknown> }).config.method).toBe(
			'GET'
		);
	});
});

describe('task id helpers', () => {
	it('collects ids including parallel children', () => {
		expect(allTaskIds(detail.definition)).toEqual(['discover', 'fetch_all', 'fetch_one']);
	});

	it('generates unique task_N ids', () => {
		const def = emptyDefinition();
		expect(uniqueTaskId(def)).toBe('task_1');
		def.tasks.push({ id: 'task_1', type: 'x', config: {}, outputs: [] });
		def.tasks.push({ id: 'task_3', type: 'x', config: {}, outputs: [] });
		expect(uniqueTaskId(def)).toBe('task_2');
	});
});

describe('newPluginTask', () => {
	it('seeds config from manifest defaults, skipping nulls', () => {
		const manifest: PluginManifest = {
			type_id: 'http.request',
			label: 'HTTP request',
			description: '',
			icon: 'globe',
			color: '#58a6ff',
			fields: [
				{ key: 'method', label: 'Method', widget: 'select', required: true, default: 'GET', help: '', template: false },
				{ key: 'url', label: 'URL', widget: 'template', required: true, default: null, help: '', template: true },
				{ key: 'success_codes', label: 'Success codes', widget: 'text', required: false, default: '2xx', help: '', template: false }
			]
		};
		const task = newPluginTask('task_1', manifest);
		expect(task).toEqual({
			id: 'task_1',
			type: 'http.request',
			on_error: 'fail',
			config: { method: 'GET', success_codes: '2xx' },
			outputs: []
		});
	});
});

describe('validation error mapping', () => {
	const issues: ValidationIssue[] = [
		{ path: 'tasks[0].config.url', message: 'url is required' },
		{ path: 'tasks[0].config.url', message: 'second message' },
		{ path: 'tasks[1].tasks[0].config.url', message: 'child url' },
		{ path: 'tasks[1].items', message: 'items must not be empty' },
		{ path: 'triggers[0].cron', message: 'invalid cron expression' },
		{ path: 'inputs[2].default', message: 'invalid template' },
		{ path: 'name', message: 'name must not be empty' }
	];

	it('folds issues into an exact path -> message map', () => {
		const map = issueMap(issues);
		expect(map.get('tasks[0].config.url')).toBe('url is required; second message');
		expect(map.get('triggers[0].cron')).toBe('invalid cron expression');
		expect(map.get('tasks[1].items')).toBe('items must not be empty');
		expect(map.get('nope')).toBeUndefined();
	});

	it('scopes issues to a section prefix', () => {
		expect(issuesUnder(issues, 'tasks[1]').map((i) => i.path)).toEqual([
			'tasks[1].tasks[0].config.url',
			'tasks[1].items'
		]);
		expect(issuesUnder(issues, 'inputs').map((i) => i.path)).toEqual(['inputs[2].default']);
		expect(issuesUnder(issues, 'name').map((i) => i.path)).toEqual(['name']);
		// prefix matching is segment-aware: "tasks[1]" must not match "tasks[10]"
		expect(issuesUnder([{ path: 'tasks[10].id', message: 'x' }], 'tasks[1]')).toEqual([]);
	});

	it('aggregates deep config paths under their field key at a . or [ boundary', () => {
		const deep: ValidationIssue[] = [
			{ path: 'tasks[0].config.headers[0].value', message: 'invalid template' },
			{ path: 'tasks[0].config.headers[1].key', message: 'empty key' },
			{ path: 'tasks[0].config.headers1', message: 'other field' },
			{ path: 'tasks[0].config.url', message: 'url is required' }
		];
		// field key "headers": claims indexed sub-paths, not "headers1"
		expect(issuesUnder(deep, 'tasks[0].config.headers').map((i) => i.path)).toEqual([
			'tasks[0].config.headers[0].value',
			'tasks[0].config.headers[1].key'
		]);
		// field key "headers1" must not claim "headers10..." either
		expect(
			issuesUnder([{ path: 'tasks[0].config.headers10.x', message: 'x' }], 'tasks[0].config.headers1')
		).toEqual([]);
		// exact field path still matches
		expect(issuesUnder(deep, 'tasks[0].config.url').map((i) => i.path)).toEqual([
			'tasks[0].config.url'
		]);
	});
});

describe('inline task path patterns', () => {
	it('claims editor-body paths for a top-level task, including deep config', () => {
		const p = inlineTaskPathPattern('tasks[2]');
		expect(p.test('tasks[2].id')).toBe(true);
		expect(p.test('tasks[2].config')).toBe(true);
		expect(p.test('tasks[2].config.url')).toBe(true);
		expect(p.test('tasks[2].config.headers[0].value')).toBe(true);
		expect(p.test('tasks[2].outputs[1].extract')).toBe(true);
		expect(p.test('tasks[2].retry.max_attempts')).toBe(true);
		expect(p.test('tasks[2].timeout_seconds')).toBe(true);
		// not claimed inline -> falls back to the panel-top issue list
		expect(p.test('tasks[2].type')).toBe(false);
		expect(p.test('tasks[2].outputs[1].type')).toBe(false);
		// other tasks are out of scope ("tasks[2]" must not match "tasks[20]")
		expect(p.test('tasks[20].config.url')).toBe(false);
	});

	it('claims parallel fields and full child editor bodies', () => {
		const p = inlineParallelPathPattern('tasks[1]');
		expect(p.test('tasks[1].items')).toBe(true);
		expect(p.test('tasks[1].concurrency')).toBe(true);
		expect(p.test('tasks[1].tasks')).toBe(true);
		expect(p.test('tasks[1].outputs[0].name')).toBe(true);
		expect(p.test('tasks[1].tasks[0].config.headers[2].value')).toBe(true);
		expect(p.test('tasks[1].tasks[1].retry.base_seconds')).toBe(true);
		expect(p.test('tasks[1].tasks[0].type')).toBe(false);
		expect(p.test('tasks[10].items')).toBe(false);
	});
});

describe('humanizeCron (display-only copy of the Rust humanizer)', () => {
	it('matches the Rust shapes', () => {
		expect(humanizeCron('0 9 * * *')).toBe('daily · 09:00');
		expect(humanizeCron('30 17 * * *')).toBe('daily · 17:30');
		expect(humanizeCron('15 * * * *')).toBe('hourly');
		expect(humanizeCron('0 8 * * 1')).toBe('weekly · Mon');
		expect(humanizeCron('0 8 * * 0')).toBe('weekly · Sun');
		expect(humanizeCron('*/5 * * * *')).toBe('*/5 * * * *'); // fallback
		expect(humanizeCron('bogus')).toBe('bogus');
	});
});

describe('slugifyFlowId (mirror of src/api/flows.rs)', () => {
	it('matches the Rust test vectors', () => {
		expect(slugifyFlowId('  My Fancy Flow!  ')).toBe('my_fancy_flow');
		expect(slugifyFlowId('Council Alert Pipeline')).toBe('council_alert_pipeline');
		expect(slugifyFlowId('123 Go')).toBe('go');
		expect(slugifyFlowId('Flow 42')).toBe('flow_42');
		expect(slugifyFlowId('!!!')).toBeNull();
	});
});
