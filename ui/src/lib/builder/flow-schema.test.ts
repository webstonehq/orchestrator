import { describe, expect, it } from 'vitest';
import { loadFlowSchema } from './flow-schema';

/** Minimal Response-like stub for the injected fetch. */
function res(body: unknown, ok = true): Response {
	return {
		ok,
		json: async () => {
			if (body instanceof Error) throw body;
			return body;
		}
	} as unknown as Response;
}

describe('loadFlowSchema', () => {
	it('returns the parsed schema on success', async () => {
		const schema = { type: 'object', $defs: { Task: { oneOf: [] } } };
		const got = await loadFlowSchema(async () => res(schema));
		expect(got).toEqual(schema);
	});

	it('degrades to null on a non-ok response', async () => {
		const got = await loadFlowSchema(async () => res({ error: 'nope' }, false));
		expect(got).toBeNull();
	});

	it('degrades to null when fetch throws (offline)', async () => {
		const got = await loadFlowSchema(async () => {
			throw new Error('network down');
		});
		expect(got).toBeNull();
	});

	it('degrades to null on a non-JSON body', async () => {
		const got = await loadFlowSchema(async () => res(new Error('bad json')));
		expect(got).toBeNull();
	});

	it('requests the schema endpoint', async () => {
		let requested: string | undefined;
		await loadFlowSchema(async (url) => {
			requested = String(url);
			return res({});
		});
		expect(requested).toBe('/api/flow.schema.json');
	});
});
