import { describe, expect, it } from 'vitest';
import { pickerGroups } from './picker';

const base = {
	inputs: [
		{ id: 'since', type: 'DATE' },
		{ id: 'count', type: 'INT' }
	],
	variables: [{ id: 'server' }],
	envNames: [] as string[],
	secretNames: ['API_TOKEN'],
	upstreamTasks: [
		{ id: 'discover', outputs: [{ name: 'ids', type: 'ARRAY' }] },
		{
			id: 'fetch',
			outputs: [
				{ name: 'body', type: 'JSON' },
				{ name: 'status', type: 'INT' }
			]
		}
	]
};

describe('pickerGroups', () => {
	it('produces groups in canonical order', () => {
		const groups = pickerGroups(base);
		expect(groups.map((g) => g.title)).toEqual([
			'INPUTS',
			'OUTPUTS · discover',
			'OUTPUTS · fetch',
			'VARIABLES',
			'SECRETS',
			'FUNCTIONS'
		]);
	});

	it('formats input items as inputs.<id> with declared type', () => {
		const groups = pickerGroups(base);
		expect(groups[0].items).toEqual([
			{ label: 'inputs.since', type: 'DATE', value: 'inputs.since' },
			{ label: 'inputs.count', type: 'INT', value: 'inputs.count' }
		]);
	});

	it('formats output items with short label and full outputs.<task>.<name> value', () => {
		const groups = pickerGroups(base);
		expect(groups[1].items).toEqual([
			{ label: 'ids', type: 'ARRAY', value: 'outputs.discover.ids' }
		]);
		expect(groups[2].items).toEqual([
			{ label: 'body', type: 'JSON', value: 'outputs.fetch.body' },
			{ label: 'status', type: 'INT', value: 'outputs.fetch.status' }
		]);
	});

	it('formats variables as vars.<id> STRING and secrets as secrets.<NAME>', () => {
		const groups = pickerGroups(base);
		const vars = groups.find((g) => g.title === 'VARIABLES');
		expect(vars?.items).toEqual([
			{ label: 'vars.server', type: 'STRING', value: 'vars.server' }
		]);
		const secrets = groups.find((g) => g.title === 'SECRETS');
		expect(secrets?.items).toEqual([
			{ label: 'secrets.API_TOKEN', type: 'STRING', value: 'secrets.API_TOKEN' }
		]);
	});

	it('formats env names as env.<NAME> STRING, placed between VARIABLES and SECRETS', () => {
		const groups = pickerGroups({ ...base, envNames: ['GITHUB_TOKEN', 'REGION'] });
		const envGroup = groups.find((g) => g.title === 'ENV');
		expect(envGroup?.items).toEqual([
			{ label: 'env.GITHUB_TOKEN', type: 'STRING', value: 'env.GITHUB_TOKEN' },
			{ label: 'env.REGION', type: 'STRING', value: 'env.REGION' }
		]);
		const titles = groups.map((g) => g.title);
		expect(titles.indexOf('ENV')).toBeGreaterThan(titles.indexOf('VARIABLES'));
		expect(titles.indexOf('ENV')).toBeLessThan(titles.indexOf('SECRETS'));
	});

	it('skips the ENV group when no env names are declared', () => {
		const groups = pickerGroups({ ...base, envNames: [] });
		expect(groups.some((g) => g.title === 'ENV')).toBe(false);
	});

	it('always includes FUNCTIONS with now() DATE', () => {
		const groups = pickerGroups({
			inputs: [],
			variables: [],
			envNames: [],
			secretNames: [],
			upstreamTasks: []
		});
		expect(groups).toEqual([
			{ title: 'FUNCTIONS', items: [{ label: 'now()', type: 'DATE', value: 'now()' }] }
		]);
	});

	it('skips empty groups', () => {
		const groups = pickerGroups({
			inputs: [],
			variables: [{ id: 'server' }],
			envNames: [],
			secretNames: [],
			upstreamTasks: [{ id: 'noout', outputs: [] }]
		});
		expect(groups.map((g) => g.title)).toEqual(['VARIABLES', 'FUNCTIONS']);
	});

	it('adds ITERATION group only when iteration is set', () => {
		expect(
			pickerGroups({ ...base, iteration: false }).some((g) => g.title === 'ITERATION')
		).toBe(false);

		const groups = pickerGroups({ ...base, iteration: true });
		const iter = groups.find((g) => g.title === 'ITERATION');
		expect(iter?.items).toEqual([
			{ label: 'taskrun.value', type: 'JSON', value: 'taskrun.value' },
			{ label: 'taskrun.value.id', type: 'STRING', value: 'taskrun.value.id' }
		]);
		// ITERATION comes after FUNCTIONS.
		expect(groups.map((g) => g.title).indexOf('ITERATION')).toBeGreaterThan(
			groups.map((g) => g.title).indexOf('FUNCTIONS')
		);
	});

	it('adds PRIOR STEPS groups after ITERATION, skipping outputless steps', () => {
		const groups = pickerGroups({
			...base,
			iteration: true,
			priorSteps: [
				{ id: 'download', outputs: [{ name: 'body', type: 'JSON' }] },
				{ id: 'silent', outputs: [] }
			]
		});
		const titles = groups.map((g) => g.title);
		expect(titles).toEqual([
			'INPUTS',
			'OUTPUTS · discover',
			'OUTPUTS · fetch',
			'VARIABLES',
			'SECRETS',
			'FUNCTIONS',
			'ITERATION',
			'PRIOR STEPS · download'
		]);
		expect(groups[titles.indexOf('PRIOR STEPS · download')].items).toEqual([
			{ label: 'body', type: 'JSON', value: 'outputs.download.body' }
		]);
	});

	it('passes upstream tasks through in order without filtering (scoping is the caller responsibility)', () => {
		// The caller passes only genuinely upstream tasks; pickerGroups must not
		// reorder, dedupe, or drop non-empty ones.
		const groups = pickerGroups({
			inputs: [],
			variables: [],
			envNames: [],
			secretNames: [],
			upstreamTasks: [
				{ id: 'b', outputs: [{ name: 'x', type: 'STRING' }] },
				{ id: 'a', outputs: [{ name: 'y', type: 'STRING' }] }
			]
		});
		expect(groups.map((g) => g.title)).toEqual(['OUTPUTS · b', 'OUTPUTS · a', 'FUNCTIONS']);
	});
});
