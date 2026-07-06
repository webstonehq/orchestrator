/**
 * Fetches the flow JSON Schema the backend assembles from its live plugin
 * registry (`GET /api/flow.schema.json`). The YAML editor feeds it to
 * `codemirror-json-schema` for autocomplete, hover docs, and advisory
 * validation.
 *
 * Autocomplete is a progressive enhancement: any failure (offline, non-2xx,
 * malformed body) resolves to `null` so the editor falls back to plain YAML
 * editing rather than throwing.
 */
export async function loadFlowSchema(
	fetchFn: typeof fetch = fetch
): Promise<Record<string, unknown> | null> {
	try {
		const response = await fetchFn('/api/flow.schema.json');
		if (!response.ok) return null;
		return (await response.json()) as Record<string, unknown>;
	} catch {
		return null;
	}
}
