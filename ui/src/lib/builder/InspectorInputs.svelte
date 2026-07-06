<!--
	InspectorInputs — flow inputs table: id (mono), type chip (cycles), a
	required toggle chip, default value (template editor scoped to
	vars/secrets/functions — no outputs exist at trigger time), delete.
-->
<script lang="ts">
	import type { BuilderStore } from './state.svelte';
	import { INPUT_TYPES, issuesUnder } from './defs';
	import SelectChip from '../components/fields/SelectChip.svelte';
	import TemplateInput from '../components/fields/TemplateInput.svelte';
	import IssueList from './IssueList.svelte';

	let { store }: { store: BuilderStore } = $props();

	const groups = $derived(store.inputDefaultGroups());

	// Paths rendered inline on rows; everything else under inputs[...] goes
	// to the top list.
	const unmatched = $derived(
		issuesUnder(store.issues, 'inputs').filter(
			(i) => !/^inputs\[\d+\]\.(id|default)$/.test(i.path)
		)
	);

	function setDefault(i: number, value: string) {
		if (value === '') delete store.def.inputs[i].default;
		else store.def.inputs[i].default = value;
	}
</script>

<div class="panel">
	<div class="head">
		<h3>Flow inputs</h3>
		<button class="add" type="button" onclick={() => store.addInput()}>
			<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round">
				<line x1="12" y1="5" x2="12" y2="19"></line>
				<line x1="5" y1="12" x2="19" y2="12"></line>
			</svg>
			Add input
		</button>
	</div>
	<p class="hint">
		Values supplied when the flow is triggered. Reference them in any task with the value picker
		as <span class="ref">inputs.&lt;id&gt;</span>. Defaults are template strings resolved at
		trigger time.
	</p>

	<IssueList issues={unmatched} />

	{#if store.def.inputs.length === 0}
		<button class="none" type="button" onclick={() => store.addInput()}>
			No inputs yet — add one to parameterize runs
		</button>
	{:else}
		<div class="table">
			<div class="thead">
				<div>Name</div>
				<div>Type</div>
				<div>Required</div>
				<div>Default</div>
				<div></div>
			</div>
			{#each store.def.inputs as input, i (i)}
				<div class="row">
					<input
						class="id"
						value={input.id}
						spellcheck="false"
						aria-label="Input id"
						oninput={(e) => (input.id = e.currentTarget.value)}
					/>
					<div>
						<SelectChip
							value={input.type}
							options={INPUT_TYPES}
							onChange={(v) => (input.type = v as typeof input.type)}
						/>
					</div>
					<div>
						<button
							class="req"
							class:on={input.required === true}
							type="button"
							onclick={() => (input.required = !input.required)}
						>
							{input.required ? 'required' : 'optional'}
						</button>
					</div>
					<TemplateInput
						value={input.default ?? ''}
						{groups}
						placeholder="—"
						onChange={(v) => setDefault(i, v)}
					/>
					<button
						class="del"
						type="button"
						aria-label="Delete input {input.id}"
						onclick={() => store.def.inputs.splice(i, 1)}
					>
						<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
							<line x1="6" y1="6" x2="18" y2="18"></line>
							<line x1="18" y1="6" x2="6" y2="18"></line>
						</svg>
					</button>
					{#if store.errorAt(`inputs[${i}].id`)}
						<div class="err">{store.errorAt(`inputs[${i}].id`)}</div>
					{/if}
					{#if store.errorAt(`inputs[${i}].default`)}
						<div class="err">{store.errorAt(`inputs[${i}].default`)}</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.panel {
		padding: 18px 22px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 12px;
		margin-bottom: 5px;
	}

	h3 {
		margin: 0;
		font: 600 14px 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--text);
	}

	.add {
		margin-left: auto;
		height: 30px;
		padding: 0 12px;
		border-radius: 8px;
		border: 1px solid var(--accent);
		background: rgba(126, 231, 135, 0.1);
		color: var(--accent);
		font: 600 12px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.hint {
		margin: 0 0 16px;
		font-size: 12px;
		color: var(--muted);
		line-height: 1.5;
	}

	.ref {
		font: 500 11.5px 'IBM Plex Mono', monospace;
		color: #79c0ff;
	}

	.none {
		display: block;
		width: 100%;
		border: 1.5px dashed var(--border2);
		border-radius: 11px;
		padding: 20px;
		text-align: center;
		color: var(--dim);
		cursor: pointer;
		font: 500 12px 'IBM Plex Mono', monospace;
		background: transparent;
	}

	.none:hover {
		border-color: var(--accent);
		color: var(--accent);
	}

	.table {
		border: 1px solid var(--border);
		border-radius: 11px;
		overflow: visible;
		background: var(--panel);
	}

	.thead,
	.row {
		display: grid;
		grid-template-columns: 1.1fr auto auto 1.9fr 34px;
		gap: 12px;
		padding: 11px 15px;
		border-bottom: 1px solid var(--border);
		align-items: center;
	}

	.thead {
		padding: 9px 15px;
		background: var(--bg2);
		border-radius: 11px 11px 0 0;
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.row:last-child {
		border-bottom: none;
	}

	.id {
		width: 100%;
		font: 600 12.5px 'IBM Plex Mono', monospace;
		color: var(--text);
		background: transparent;
		border: none;
		border-bottom: 1px dashed var(--border2);
		outline: none;
		padding: 0 0 2px;
		min-width: 0;
	}

	.id:focus {
		border-bottom-color: var(--accent);
	}

	.req {
		font: 600 10.5px 'IBM Plex Mono', monospace;
		border-radius: 6px;
		padding: 4px 9px;
		cursor: pointer;
		border: 1px solid var(--border2);
		background: var(--panel3);
		color: var(--dim);
		white-space: nowrap;
	}

	.req.on {
		border-color: rgba(227, 179, 65, 0.55);
		background: rgba(227, 179, 65, 0.12);
		color: var(--amber);
	}

	.del {
		width: 26px;
		height: 26px;
		border-radius: 7px;
		border: 1px solid var(--border2);
		background: var(--panel2);
		color: var(--dim);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.del:hover {
		color: var(--red);
		border-color: var(--red);
	}

	.err {
		grid-column: 1 / -1;
		font-size: 11px;
		color: var(--red);
	}
</style>
