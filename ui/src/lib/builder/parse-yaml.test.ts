import { describe, expect, it } from 'vitest';
import { yamlToDefinition } from './parse-yaml';
import { flowToYaml } from './yaml';
import { initDefinition, newPluginTask } from './defs';
import type { FieldSpec, FlowDefinition, FlowDetail, PluginManifest } from '../api';
import fixtureGet from './fixtures/council_alert_pipeline.get.json';
import edgeGet from './fixtures/edge_case.get.json';

const detail = fixtureGet as unknown as FlowDetail;
const edge = edgeGet as unknown as FlowDetail;

// A hand-built definition exercising shapes the fixtures don't cover together:
// a parallel task with a child, header key/value config, a retry block, and an
// ARRAY input whose default is a JSON-encoded array literal.
const richDef: FlowDefinition = {
	name: 'rich flow',
	namespace: 'ops',
	description: 'covers parallel + retry + headers + array default',
	inputs: [
		{ id: 'regions', type: 'ARRAY', required: true, default: '["ON","QC"]' },
		{ id: 'note', type: 'STRING', required: false }
	],
	variables: [{ id: 'server', value: 'https://api.example.com' }],
	triggers: [
		{ id: 'nightly', type: 'schedule', cron: '0 3 * * *', timezone: 'UTC', catchup: 'latest', enabled: true }
	],
	tasks: [
		{
			id: 'fetch',
			type: 'http.request',
			retry: { type: 'exponential', max_attempts: 3, base_seconds: 2 },
			timeout_seconds: 30,
			on_error: 'continue',
			config: {
				url: '{{ vars.server }}/items',
				method: 'GET',
				headers: [
					{ key: 'Accept', value: 'application/json' },
					{ key: 'X-Token', value: '{{ secrets.token }}' }
				],
				raw_body: null
			},
			outputs: [{ name: 'items', type: 'JSON', extract: 'result.body.items' }]
		},
		{
			id: 'process',
			type: 'parallel',
			items: '{{ tasks.fetch.items }}',
			concurrency: 4,
			tasks: [
				{
					id: 'process_step',
					type: 'http.request',
					on_error: 'fail',
					config: { url: '{{ item }}', method: 'POST' },
					outputs: []
				}
			],
			outputs: [{ name: 'results', type: 'JSON', extract: 'result.items' }]
		}
	]
};

describe('yamlToDefinition — round-trip contract', () => {
	// For any def the builder can produce:
	//   yamlToDefinition(flowToYaml(id, def)).def === initDefinition(def-as-wire)
	function roundTrip(id: string, def: FlowDefinition) {
		const yaml = flowToYaml(id, def);
		const { def: parsed, errors } = yamlToDefinition(yaml);
		expect(errors).toEqual([]);
		expect(parsed).toEqual(initDefinition(def));
	}

	it('round-trips the design-doc example flow', () => {
		roundTrip(detail.id, detail.definition);
	});

	it('round-trips the scalar edge-case flow (quoting, literal blocks, lookalikes)', () => {
		roundTrip(edge.id, edge.definition);
	});

	it('round-trips a rich flow (parallel child, retry, headers, ARRAY default)', () => {
		roundTrip('rich_flow', richDef);
	});

	it('round-trips declared env names', () => {
		roundTrip('env_flow', {
			name: 'env-flow',
			namespace: 'default',
			description: '',
			inputs: [],
			variables: [],
			env: ['GITHUB_TOKEN', 'REGION'],
			triggers: [],
			tasks: []
		});
	});
});

describe('yamlToDefinition — scaffolded config matches canonical order (no blur churn)', () => {
	// newPluginTask must emit config keys in the same (alphabetical, deep) order
	// the canonical YAML round-trip yields, so adding a task then blurring the editor
	// does not reassign store.def purely to reorder keys. JSON.stringify is
	// order-sensitive, which is exactly what store.json / dirty tracking compares.
	function field(key: string, def: unknown): FieldSpec {
		return { key, label: key, widget: 'text', required: false, help: '', template: false, default: def };
	}
	const manifest: PluginManifest = {
		type_id: 'http.request',
		label: 'HTTP',
		description: '',
		icon: '',
		color: '',
		// Intentionally NOT alphabetical, with a nested object default too.
		fields: [
			field('url', 'https://example.com'),
			field('method', 'GET'),
			field('accept', 'json'),
			field('meta', { z: 1, a: 2 })
		]
	};

	it('emits scaffolded config already in canonical key order', () => {
		const task = newPluginTask('t1', manifest);
		const def: FlowDefinition = {
			name: 'x',
			namespace: 'default',
			description: '',
			inputs: [],
			variables: [],
			triggers: [],
			tasks: [task]
		};
		const parsed = yamlToDefinition(flowToYaml('x', def)).def;
		expect(parsed).not.toBeNull();
		const scaffoldedConfig = (task as { config: unknown }).config;
		const parsedConfig = (parsed!.tasks[0] as { config: unknown }).config;
		// Order-sensitive equality: no reassignment would occur on blur.
		expect(JSON.stringify(scaffoldedConfig)).toBe(JSON.stringify(parsedConfig));
	});
});

describe('yamlToDefinition — the id field is immutable and ignored', () => {
	it('ignores the top-level id and never maps it into the definition', () => {
		const yaml = flowToYaml('the_real_id', richDef);
		const changed = yaml.replace('id: the_real_id', 'id: a_totally_different_id');
		const { def, errors } = yamlToDefinition(changed);
		expect(errors).toEqual([]);
		// Same definition regardless of the id line, and no `id` key leaks in.
		expect(def).toEqual(initDefinition(richDef));
		expect(def).not.toHaveProperty('id');
	});
});

describe('yamlToDefinition — syntax errors', () => {
	it('reports a YAML syntax error with a 1-based line', () => {
		const text = 'name: ok\ntasks: [1, 2\nnamespace: default';
		const { def, errors } = yamlToDefinition(text);
		expect(def).toBeNull();
		expect(errors.length).toBeGreaterThan(0);
		const p = errors[0];
		expect(p.startLine).toBeGreaterThanOrEqual(1);
		expect(typeof p.message).toBe('string');
		expect(p.message.length).toBeGreaterThan(0);
	});

	it('points at the offending line (bad indentation)', () => {
		// Line 3 dedents illegally under a mapping value.
		const text = 'name: ok\ninputs:\n  - id: a\n   type: STRING';
		const { def, errors } = yamlToDefinition(text);
		expect(def).toBeNull();
		expect(errors.length).toBeGreaterThan(0);
		expect(errors[0].startLine).toBeGreaterThanOrEqual(1);
	});
});

describe('yamlToDefinition — structurally broken top level', () => {
	it('rejects a collection field that is a bare primitive (tasks: 5)', () => {
		const { def, errors } = yamlToDefinition('name: x\ntasks: 5');
		expect(def).toBeNull();
		expect(errors).toHaveLength(1);
		expect(errors[0].message).toContain('tasks');
		expect(errors[0].startLine).toBe(1);
	});

	it('rejects a scalar document (not a mapping)', () => {
		const { def, errors } = yamlToDefinition('just a string');
		expect(def).toBeNull();
		expect(errors).toHaveLength(1);
		expect(errors[0].message).toContain('mapping');
	});

	it('rejects an empty document', () => {
		const { def, errors } = yamlToDefinition('   \n# only a comment\n');
		expect(def).toBeNull();
		expect(errors).toHaveLength(1);
	});
});

describe('yamlToDefinition — tolerant loading', () => {
	it('defaults missing optional fields sensibly', () => {
		const { def, errors } = yamlToDefinition('name: minimal');
		expect(errors).toEqual([]);
		expect(def).not.toBeNull();
		expect(def).toEqual(
			initDefinition({
				name: 'minimal',
				namespace: 'default',
				description: '',
				inputs: [],
				variables: [],
				triggers: [],
				tasks: []
			})
		);
	});

	it('loads a structurally valid but semantically unknown plugin type (server validates)', () => {
		const text =
			'name: x\nnamespace: default\ntasks:\n  - id: t1\n    type: not.a.real.plugin\n    on_error: fail\n    config: {}\n    outputs: []';
		const { def, errors } = yamlToDefinition(text);
		expect(errors).toEqual([]);
		expect(def?.tasks[0].type).toBe('not.a.real.plugin');
	});
});
