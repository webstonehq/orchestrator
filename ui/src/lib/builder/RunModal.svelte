<!--
	RunModal — "Run flow" dialog generated from definition.inputs.

	Per type: STRING/DATE -> text input · INT -> number input · BOOLEAN ->
	toggle · ARRAY/JSON -> code area validated as JSON on blur/submit.
	Defaults prefill raw (template defaults show a mono "template — resolved
	at run time" hint). Values left equal to their default are omitted from
	the request so the server renders the default itself.

	Props:
	- open: bindable boolean
	- flowId: string
	- inputs: InputSpec[]
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { api, ApiError, type InputSpec } from '../api';
	import { toast } from '../toast';
	import Modal from '../components/Modal.svelte';
	import ToggleField from '../components/fields/ToggleField.svelte';
	import CodeField from '../components/fields/CodeField.svelte';

	let {
		open = $bindable(false),
		flowId,
		inputs
	}: {
		open?: boolean;
		flowId: string;
		inputs: InputSpec[];
	} = $props();

	interface FieldState {
		raw: string;
		bool: boolean;
		/** BOOLEAN only: whether the user flipped the toggle. Untouched
		 *  toggles with a default are omitted so the server renders the
		 *  default itself (it may be a template). */
		touched: boolean;
		error: string | null;
	}

	let fields = $state<Record<string, FieldState>>({});
	let topError = $state<string | null>(null);
	let running = $state(false);

	// Re-seed field state each time the modal opens.
	$effect(() => {
		if (!open) return;
		const next: Record<string, FieldState> = {};
		for (const input of inputs) {
			next[input.id] = {
				raw: input.default ?? '',
				// Visual seed only (a template default cannot be evaluated
				// client-side and shows as off until touched).
				bool: typeof input.default === 'string' && input.default.toLowerCase() === 'true',
				touched: false,
				error: null
			};
		}
		fields = next;
		topError = null;
	});

	function isTemplateDefault(input: InputSpec): boolean {
		return typeof input.default === 'string' && input.default.includes('{{');
	}

	function validateJson(input: InputSpec) {
		const f = fields[input.id];
		if (!f || f.raw === '' || f.raw === input.default) {
			if (f) f.error = null;
			return;
		}
		try {
			const parsed: unknown = JSON.parse(f.raw);
			if (input.type === 'ARRAY' && !Array.isArray(parsed)) {
				f.error = 'must be a JSON array';
			} else {
				f.error = null;
			}
		} catch (e) {
			f.error = e instanceof Error ? `invalid JSON: ${e.message}` : 'invalid JSON';
		}
	}

	function collect(): Record<string, unknown> | null {
		const out: Record<string, unknown> = {};
		let ok = true;
		for (const input of inputs) {
			const f = fields[input.id];
			if (!f) continue;
			f.error = null;

			if (input.type === 'BOOLEAN') {
				// Untouched toggle with a default: omit — the server renders
				// the default (which may be a template, not a literal bool).
				if (input.default !== undefined && input.default !== null && !f.touched) continue;
				out[input.id] = f.bool;
				continue;
			}

			// Untouched default -> let the server render it (templates too).
			if (input.default !== undefined && input.default !== null && f.raw === input.default)
				continue;

			if (f.raw === '') {
				if (input.required && (input.default === undefined || input.default === null)) {
					f.error = 'required';
					ok = false;
				}
				continue;
			}

			switch (input.type) {
				case 'INT': {
					const n = Number(f.raw);
					if (!Number.isInteger(n)) {
						f.error = 'must be an integer';
						ok = false;
					} else {
						out[input.id] = n;
					}
					break;
				}
				case 'ARRAY':
				case 'JSON': {
					try {
						const parsed: unknown = JSON.parse(f.raw);
						if (input.type === 'ARRAY' && !Array.isArray(parsed)) {
							f.error = 'must be a JSON array';
							ok = false;
						} else {
							out[input.id] = parsed;
						}
					} catch {
						f.error = 'invalid JSON';
						ok = false;
					}
					break;
				}
				default:
					out[input.id] = f.raw;
			}
		}
		return ok ? out : null;
	}

	async function execute() {
		topError = null;
		const values = collect();
		if (values === null) return;
		running = true;
		try {
			const { run_id } = await api.flows.run(flowId, { inputs: values });
			open = false;
			toast.info(`Run #${run_id} queued`);
			void goto(`/runs/${run_id}`);
		} catch (e) {
			if (e instanceof ApiError) {
				if (e.status === 409) {
					toast.error('Flow is paused — resume it before running');
				} else if (e.status === 422 && e.errors) {
					for (const issue of e.errors) {
						const id = issue.path.replace(/^inputs\./, '').replace(/^inputs\[\d+\]\.?/, '');
						const f = fields[id];
						if (f) f.error = issue.message;
						else topError = topError ? `${topError}; ${issue.message}` : issue.message;
					}
				} else {
					topError = e.message;
				}
			} else {
				topError = e instanceof Error ? e.message : String(e);
			}
		} finally {
			running = false;
		}
	}
</script>

<Modal bind:open title="Run {flowId}" width={520}>
	{#if topError}
		<div class="top-error" role="alert">{topError}</div>
	{/if}

	{#if inputs.length === 0}
		<p class="none">This flow takes no inputs.</p>
	{:else}
		<div class="fields">
			{#each inputs as input (input.id)}
				{@const f = fields[input.id]}
				{#if f}
					<div class="field">
						<div class="label-row">
							<span class="label">{input.id}</span>
							<span class="type">{input.type}</span>
							{#if input.required}<span class="req" title="Required">*</span>{/if}
						</div>
						{#if input.type === 'BOOLEAN'}
							<ToggleField
								checked={f.bool}
								onChange={(v) => {
									f.bool = v;
									f.touched = true;
								}}
							/>
						{:else if input.type === 'INT'}
							<input
								class="text"
								type="number"
								value={f.raw}
								oninput={(e) => (f.raw = e.currentTarget.value)}
							/>
						{:else if input.type === 'ARRAY' || input.type === 'JSON'}
							<!-- focusout wrapper: CodeField has no blur callback -->
							<div onfocusout={() => validateJson(input)}>
								<CodeField
									value={f.raw}
									placeholder={input.type === 'ARRAY' ? '["…"]' : '{ … }'}
									onChange={(v) => (f.raw = v)}
								/>
							</div>
						{:else}
							<input
								class="text"
								value={f.raw}
								spellcheck="false"
								oninput={(e) => (f.raw = e.currentTarget.value)}
							/>
						{/if}
						{#if isTemplateDefault(input) && f.raw === input.default}
							<div class="tpl-hint">template — resolved at run time</div>
						{/if}
						{#if f.error}
							<div class="err">{f.error}</div>
						{/if}
					</div>
				{/if}
			{/each}
		</div>
	{/if}

	{#snippet footer()}
		<button class="btn" type="button" onclick={() => (open = false)}>Cancel</button>
		<button class="btn primary" type="button" disabled={running} onclick={execute}>
			<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" stroke="none">
				<polygon points="6,4 20,12 6,20"></polygon>
			</svg>
			{running ? 'Starting…' : 'Execute'}
		</button>
	{/snippet}
</Modal>

<style>
	.top-error {
		border: 1px solid rgba(248, 81, 73, 0.35);
		border-radius: 9px;
		background: rgba(248, 81, 73, 0.06);
		padding: 9px 12px;
		font-size: 12px;
		color: var(--red);
		margin-bottom: 14px;
	}

	.none {
		margin: 0;
		font-size: 12.5px;
		color: var(--muted);
	}

	.fields {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 7px;
	}

	.label-row {
		display: flex;
		align-items: baseline;
		gap: 8px;
	}

	.label {
		font: 600 12.5px 'IBM Plex Mono', monospace;
		color: var(--text);
	}

	.type {
		font: 600 9.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border: 1px solid var(--border2);
		border-radius: 5px;
		padding: 1px 6px;
	}

	.req {
		color: var(--amber);
		font: 600 12px 'IBM Plex Mono', monospace;
	}

	.text {
		width: 100%;
		box-sizing: border-box;
		height: 34px;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--bg2);
		padding: 0 12px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	.text:focus {
		border-color: var(--accent);
	}

	.tpl-hint {
		font: 500 10.5px 'IBM Plex Mono', monospace;
		color: var(--cyan);
	}

	.err {
		font-size: 11px;
		color: var(--red);
	}

	.btn {
		height: 32px;
		padding: 0 14px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--muted);
		font: 600 12px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.btn:hover {
		color: var(--text);
	}

	.btn.primary {
		border-color: var(--accent);
		background: var(--accent);
		color: #08110a;
	}

	.btn.primary:disabled {
		opacity: 0.6;
		cursor: default;
	}
</style>
