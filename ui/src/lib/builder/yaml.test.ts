import { describe, expect, it } from 'vitest';
import { flowToYaml } from './yaml';
import type { FlowDefinition, FlowDetail } from '../api';
import fixtureGet from './fixtures/council_alert_pipeline.get.json';
import fixtureExport from './fixtures/council_alert_pipeline.export.yaml?raw';
import edgeGet from './fixtures/edge_case.get.json';
import edgeExport from './fixtures/edge_case.export.yaml?raw';

// The GET fixture is a real server response for the design-doc example flow
// (docs/plans/2026-07-05-orchestrator-implementation.md), captured live; the
// export fixture is the server's GET /api/flows/:id/export for the same flow.
const detail = fixtureGet as unknown as FlowDetail;

describe('flowToYaml', () => {
	it('matches the server export byte-for-byte for the design-doc example', () => {
		expect(flowToYaml(detail.id, detail.definition)).toBe(fixtureExport);
	});

	it('matches the server export byte-for-byte for scalar edge cases', () => {
		// Colon-space, YAML-1.1 bool words (yes/no stay plain in 1.2),
		// leading '- ', '%', ' #', trailing space, hex/float lookalikes,
		// long unwrapped strings, and multi-line literal blocks (| and |-).
		const edge = edgeGet as unknown as FlowDetail;
		expect(flowToYaml(edge.id, edge.definition)).toBe(edgeExport);
	});

	it('emits id first and sections in wire order', () => {
		const lines = flowToYaml(detail.id, detail.definition).split('\n');
		expect(lines[0]).toBe('id: council_alert_pipeline');
		const order = ['name:', 'namespace:', 'description:', 'inputs:', 'variables:', 'triggers:', 'tasks:'];
		const indices = order.map((k) => lines.findIndex((l) => l.startsWith(k)));
		expect(indices.every((i) => i >= 0)).toBe(true);
		expect([...indices].sort((a, b) => a - b)).toEqual(indices);
	});

	it('renders expected key lines for the example definition', () => {
		const yaml = flowToYaml(detail.id, detail.definition);
		expect(yaml).toContain("default: '[\"ON\",\"QC\"]'");
		expect(yaml).toContain('cron: 0 3 * * *'); // plain scalar, like libyaml
		expect(yaml).toContain("  url: '{{ vars.server }}/api/municipalities'");
		expect(yaml).toContain('  raw_body: null');
		expect(yaml).toContain('- id: fetch_all\n  type: parallel\n');
		expect(yaml).toContain('  outputs: []');
		expect(yaml).toContain('    extract: result.items');
	});

	it('quotes strings that would parse as other scalar types', () => {
		const def: FlowDefinition = {
			name: 'x',
			namespace: 'default',
			description: '',
			inputs: [
				{ id: 'a', type: 'BOOLEAN', required: false, default: 'false' },
				{ id: 'b', type: 'INT', required: false, default: '42' },
				{ id: 'c', type: 'DATE', required: false, default: "now() | dateAdd(-7,'DAYS')" }
			],
			variables: [],
			triggers: [],
			tasks: []
		};
		const yaml = flowToYaml('x', def);
		expect(yaml).toContain("default: 'false'");
		expect(yaml).toContain("default: '42'");
		// contains quotes/pipes mid-string but is still a legal plain scalar
		expect(yaml).toContain("default: now() | dateAdd(-7,'DAYS')");
	});

	it('renders empty flow sections as empty flow-style collections', () => {
		const def: FlowDefinition = {
			name: 'new-flow',
			namespace: 'default',
			description: '',
			inputs: [],
			variables: [],
			triggers: [],
			tasks: []
		};
		expect(flowToYaml('new_flow', def)).toBe(
			[
				'id: new_flow',
				'name: new-flow',
				'namespace: default',
				"description: ''",
				'inputs: []',
				'variables: []',
				'triggers: []',
				'tasks: []',
				''
			].join('\n')
		);
	});

	it('emits declared env names as a block sequence between variables and triggers, omitted when empty', () => {
		const withEnv: FlowDefinition = {
			name: 'x',
			namespace: 'default',
			description: '',
			inputs: [],
			variables: [],
			env: ['GITHUB_TOKEN', 'REGION'],
			triggers: [],
			tasks: []
		};
		// Dashes sit at the key's own indent (serde_yaml block-sequence style),
		// positioned right after `variables:` and before `triggers:`.
		expect(flowToYaml('x', withEnv)).toContain(
			'variables: []\nenv:\n- GITHUB_TOKEN\n- REGION\ntriggers: []'
		);
		// Empty/absent env produces no `env:` line at all.
		expect(flowToYaml('x', { ...withEnv, env: [] })).not.toContain('env:');
	});

	it('emits multi-line strings as literal blocks', () => {
		const def: FlowDefinition = {
			name: 'x',
			namespace: 'default',
			description: 'line one\nline two',
			inputs: [],
			variables: [],
			triggers: [],
			tasks: []
		};
		expect(flowToYaml('x', def)).toContain('description: |-\n  line one\n  line two');
	});
});
