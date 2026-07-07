import { describe, expect, it } from 'vitest';
import { CompletionContext } from '@codemirror/autocomplete';
import { EditorState } from '@codemirror/state';
import { expressionGroups, makeExpressionCompletion, refInsertion } from './expr-complete';
import type { FlowDefinition } from '../api';

const def: FlowDefinition = {
	name: 'demo',
	namespace: 'default',
	description: '',
	inputs: [{ id: 'since', type: 'DATE' }],
	variables: [{ id: 'server', value: 'https://x' }],
	triggers: [],
	tasks: [
		{
			id: 'discover',
			type: 'http',
			config: {},
			outputs: [{ name: 'ids', type: 'ARRAY', extract: '$.ids' }]
		},
		{
			id: 'fan',
			type: 'parallel',
			items: '{{ outputs.discover.ids }}',
			concurrency: 4,
			tasks: [
				{
					id: 'child',
					type: 'http',
					config: {},
					outputs: [{ name: 'body', type: 'JSON', extract: '$' }]
				}
			],
			outputs: [{ name: 'results', type: 'ARRAY', extract: '$' }]
		}
	]
} as unknown as FlowDefinition;

const secretNames = ['API_TOKEN'];
const groups = () => expressionGroups(def, secretNames);
const source = makeExpressionCompletion(groups);

/** Build a CompletionContext with the cursor at the `|` marker. */
function contextFor(withCursor: string, explicit = false): CompletionContext {
	const pos = withCursor.indexOf('|');
	const doc = withCursor.slice(0, pos) + withCursor.slice(pos + 1);
	return new CompletionContext(EditorState.create({ doc }), pos, explicit);
}

describe('expressionGroups (global scope)', () => {
	it('includes inputs, all task outputs (incl. parallel children), vars, secrets, functions', () => {
		const values = groups().flatMap((g) => g.items.map((i) => i.value));
		expect(values).toContain('inputs.since');
		expect(values).toContain('outputs.discover.ids');
		expect(values).toContain('outputs.fan.results');
		expect(values).toContain('outputs.child.body'); // parallel child, not just upstream
		expect(values).toContain('vars.server');
		expect(values).toContain('secrets.API_TOKEN');
		expect(values).toContain('now()');
	});
});

describe('makeExpressionCompletion', () => {
	it('does not fire outside of {{ }}', () => {
		expect(source(contextFor('config:|'))).toBeNull();
		expect(source(contextFor('config: plain value|'))).toBeNull();
	});

	it('fires right after {{ and offers every ref', () => {
		const result = source(contextFor('url: {{ |'));
		expect(result).not.toBeNull();
		const labels = result!.options.map((o) => o.label);
		expect(labels).toContain('vars.server');
		expect(labels).toContain('outputs.child.body');
	});

	it('anchors `from` at the start of the partial path so filtering replaces it', () => {
		const withCursor = 'url: {{ vars.se|';
		const result = source(contextFor(withCursor));
		expect(result).not.toBeNull();
		const pos = withCursor.indexOf('|');
		expect(result!.from).toBe(pos - 'vars.se'.length);
	});

	it('does not fire once the expression is closed', () => {
		expect(source(contextFor('url: {{ vars.server }}|'))).toBeNull();
	});

	it('targets the open expression when an earlier one is already closed on the line', () => {
		const withCursor = 'x: {{ vars.server }} y: {{ inp|';
		const result = source(contextFor(withCursor));
		expect(result).not.toBeNull();
		const pos = withCursor.indexOf('|');
		expect(result!.from).toBe(pos - 'inp'.length);
	});

	it('tags functions and refs with distinct completion types', () => {
		const result = source(contextFor('url: {{ |'))!;
		const now = result.options.find((o) => o.label === 'now()');
		const ref = result.options.find((o) => o.label === 'vars.server');
		expect(now!.type).toBe('function');
		expect(ref!.type).toBe('variable');
	});
});

describe('refInsertion', () => {
	it('adds an opening space and closes when typed tight against {{', () => {
		// user typed "{{" then accepted; char before is the second "{"
		expect(refInsertion('{', '', 'vars.server')).toEqual({
			insert: ' vars.server }}',
			cursorOffset: ' vars.server'.length
		});
	});

	it('keeps the existing space and closes when a space was already typed', () => {
		expect(refInsertion(' ', '', 'vars.server')).toEqual({
			insert: 'vars.server }}',
			cursorOffset: 'vars.server'.length
		});
	});

	it('does not double-close when the expression is already closed ahead', () => {
		expect(refInsertion(' ', ' }}', 'vars.server')).toEqual({
			insert: 'vars.server',
			cursorOffset: 'vars.server'.length
		});
	});
});
