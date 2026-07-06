import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, ApiError, isParallel } from './api';
import type { ItemStatus, ParallelTaskSpec, RegularTaskSpec, TaskSpec, TaskStatus } from './api';
import type { Status } from './status';

function stubFetch(body: string, init: { status?: number; contentType?: string } = {}) {
	const { status = 200, contentType = 'application/json' } = init;
	const mock = vi.fn(async (_input: string, _init?: RequestInit) => {
		return new Response(body, {
			status,
			headers: contentType ? { 'content-type': contentType } : {}
		});
	});
	vi.stubGlobal('fetch', mock);
	return mock;
}

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('api success paths', () => {
	it('parses JSON responses', async () => {
		const mock = stubFetch(JSON.stringify({ active_flows: 3 }));
		const data = await api.dashboard();
		expect(data.active_flows).toBe(3);
		expect(mock).toHaveBeenCalledWith('/api/dashboard', {
			method: 'GET',
			headers: {},
			body: undefined
		});
	});

	it('builds query strings for runs.list, omitting empty params', async () => {
		const mock = stubFetch(JSON.stringify({ runs: [], total: 0, counts: {} }));
		await api.runs.list({ flow: 'my-flow', status: 'failed', page: 2, per: 25 });
		expect(mock.mock.calls[0][0]).toBe('/api/runs?flow=my-flow&status=failed&page=2&per=25');

		await api.runs.list();
		expect(mock.mock.calls[1][0]).toBe('/api/runs');
	});

	it('sends JSON bodies with content-type', async () => {
		const mock = stubFetch('');
		await api.secrets.put('api token', 's3cr3t');
		const [url, init] = mock.mock.calls[0] as unknown as [string, RequestInit];
		expect(url).toBe('/api/secrets/api%20token');
		expect(init.method).toBe('PUT');
		expect(init.headers).toEqual({ 'content-type': 'application/json' });
		expect(init.body).toBe(JSON.stringify({ value: 's3cr3t' }));
	});

	it('returns raw text for yaml export and sends yaml on import', async () => {
		const mock = stubFetch('id: my-flow\n', { contentType: 'text/yaml' });
		const yaml = await api.flows.exportYaml('my-flow');
		expect(yaml).toBe('id: my-flow\n');

		stubFetch(JSON.stringify({ id: 'my-flow' }));
		await api.flows.importYaml('id: my-flow\n');
		const importMock = globalThis.fetch as unknown as ReturnType<typeof vi.fn>;
		const [, init] = importMock.mock.calls[0] as unknown as [string, RequestInit];
		expect(init.headers).toEqual({ 'content-type': 'text/yaml' });
		expect(init.body).toBe('id: my-flow\n');
		expect(mock).toHaveBeenCalledOnce();
	});

	it('returns undefined for empty response bodies', async () => {
		stubFetch('', { status: 200, contentType: '' });
		await expect(api.runs.cancel(7)).resolves.toBeUndefined();
	});
});

describe('type contracts', () => {
	it('TaskStatus and ItemStatus are assignable to Status', () => {
		// Compile-time assertions: every TaskStatus/ItemStatus is a valid Status.
		const taskStatuses: Status[] = [
			'pending',
			'running',
			'success',
			'failed',
			'canceled',
			'skipped'
		] satisfies TaskStatus[];
		const itemStatuses: Status[] = [
			'queued',
			'running',
			'success',
			'failed',
			'canceled',
			'dropped'
		] satisfies ItemStatus[];
		expect(taskStatuses).toHaveLength(6);
		expect(itemStatuses).toHaveLength(6);
	});

	it('isParallel narrows the TaskSpec union', () => {
		const regular: RegularTaskSpec = {
			id: 'fetch',
			type: 'http.request',
			config: { method: 'GET', url: 'https://example.com' },
			outputs: []
		};
		const parallel: ParallelTaskSpec = {
			id: 'fan_out',
			type: 'parallel',
			items: '{{ outputs.fetch.ids }}',
			concurrency: 8,
			tasks: [regular],
			outputs: []
		};
		const tasks: TaskSpec[] = [regular, parallel];

		expect(tasks.filter(isParallel)).toEqual([parallel]);
		const first = tasks[1];
		if (isParallel(first)) {
			// Narrowed: parallel-only fields are accessible without casts.
			expect(first.concurrency).toBe(8);
			expect(first.tasks[0].id).toBe('fetch');
		} else {
			expect.unreachable('expected the parallel variant');
		}
	});
});

describe('api error handling', () => {
	it('extracts the message from {"error": ...} bodies', async () => {
		stubFetch(JSON.stringify({ error: 'flow is paused' }), { status: 409 });
		const err = await api.flows
			.run('my-flow', { inputs: {} })
			.then(() => null)
			.catch((e: unknown) => e);
		expect(err).toBeInstanceOf(ApiError);
		expect((err as ApiError).status).toBe(409);
		expect((err as ApiError).message).toBe('flow is paused');
	});

	it('tolerates non-JSON error bodies', async () => {
		stubFetch('gateway exploded', { status: 502, contentType: 'text/plain' });
		await expect(api.plugins()).rejects.toMatchObject({
			status: 502,
			message: 'gateway exploded'
		});
	});

	it('tolerates JSON error bodies without an "error" key', async () => {
		stubFetch(JSON.stringify({ detail: 'nope' }), { status: 400 });
		await expect(api.plugins()).rejects.toMatchObject({
			status: 400,
			message: JSON.stringify({ detail: 'nope' })
		});
	});

	it('falls back to HTTP status for empty error bodies', async () => {
		stubFetch('', { status: 500, contentType: '' });
		await expect(api.plugins()).rejects.toMatchObject({
			status: 500,
			message: 'HTTP 500'
		});
	});
});
