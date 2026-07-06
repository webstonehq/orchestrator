<!--
	OutputsTable — declares what a task exposes downstream. Each row: name
	(mono), type chip, extract path (mono, validated server-side), the ref
	token downstream tasks use, and delete. Mutates `task.outputs` in place
	(the array lives inside the store's deep $state definition).

	Props:
	- task: TaskSpec (regular or parallel — both carry outputs)
	- store: BuilderStore            — for inline validation errors
	- pathBase: string               — e.g. "tasks[2]" for error paths
	- extractPlaceholder?: string
-->
<script lang="ts">
	import type { TaskSpec } from '../api';
	import type { BuilderStore } from './state.svelte';
	import { INPUT_TYPES } from './defs';
	import SelectChip from '../components/fields/SelectChip.svelte';

	let {
		task,
		store,
		pathBase,
		extractPlaceholder = 'result.body.…'
	}: {
		task: TaskSpec;
		store: BuilderStore;
		pathBase: string;
		extractPlaceholder?: string;
	} = $props();

	function addOutput() {
		const taken = new Set(task.outputs.map((o) => o.name));
		let n = 1;
		while (taken.has(`output_${n}`)) n += 1;
		task.outputs.push({ name: `output_${n}`, type: 'ARRAY', extract: '' });
	}

	function rowError(k: number): string | undefined {
		return (
			store.errorAt(`${pathBase}.outputs[${k}].name`) ??
			store.errorAt(`${pathBase}.outputs[${k}].extract`)
		);
	}
</script>

<div class="head">
	<div class="section-label">Outputs</div>
	<button class="add" type="button" onclick={addOutput}>+ output</button>
</div>
<p class="hint">
	Declare what this task exposes. Downstream tasks reference it as
	<span class="ref">outputs.{task.id}.&lt;name&gt;</span>.
</p>

{#if task.outputs.length === 0}
	<button class="none" type="button" onclick={addOutput}>
		This task exposes no outputs yet — add one to make it available downstream
	</button>
{:else}
	<div class="table">
		{#each task.outputs as output, k (k)}
			<div class="row" class:bad={rowError(k) !== undefined}>
				<input
					class="name"
					value={output.name}
					spellcheck="false"
					aria-label="Output name"
					oninput={(e) => (output.name = e.currentTarget.value)}
				/>
				<SelectChip
					value={output.type}
					options={INPUT_TYPES}
					onChange={(v) => (output.type = v as typeof output.type)}
				/>
				<input
					class="extract"
					value={output.extract}
					placeholder={extractPlaceholder}
					spellcheck="false"
					aria-label="Extract path"
					oninput={(e) => (output.extract = e.currentTarget.value)}
				/>
				<span class="token" title="Reference token for downstream tasks">
					outputs.{task.id}.{output.name}
				</span>
				<button
					class="del"
					type="button"
					aria-label="Delete output {output.name}"
					onclick={() => task.outputs.splice(k, 1)}
				>
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
						<line x1="6" y1="6" x2="18" y2="18"></line>
						<line x1="18" y1="6" x2="6" y2="18"></line>
					</svg>
				</button>
				{#if rowError(k)}
					<div class="err">{rowError(k)}</div>
				{/if}
			</div>
		{/each}
	</div>
{/if}

<style>
	.head {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 5px;
	}

	.section-label {
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
	}

	.add {
		margin-left: auto;
		height: 26px;
		padding: 0 10px;
		border-radius: 7px;
		border: 1px solid var(--accent);
		background: rgba(126, 231, 135, 0.1);
		color: var(--accent);
		font: 600 11px 'IBM Plex Mono', monospace;
		cursor: pointer;
	}

	.hint {
		margin: 0 0 12px;
		font-size: 11.5px;
		color: var(--muted);
		line-height: 1.5;
	}

	.ref {
		font: 500 11.5px 'IBM Plex Mono', monospace;
		color: var(--accent);
	}

	.none {
		display: block;
		width: 100%;
		border: 1.5px dashed var(--border2);
		border-radius: 11px;
		padding: 16px;
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
		overflow: hidden;
		background: var(--panel);
	}

	.row {
		display: grid;
		grid-template-columns: minmax(80px, 0.8fr) auto minmax(120px, 1.2fr) minmax(0, 1fr) 26px;
		gap: 12px;
		align-items: center;
		padding: 10px 15px;
		border-bottom: 1px solid var(--border);
	}

	.row:last-child {
		border-bottom: none;
	}

	.row.bad {
		background: rgba(248, 81, 73, 0.04);
	}

	.name {
		font: 600 12.5px 'IBM Plex Mono', monospace;
		color: var(--accent);
		background: transparent;
		border: none;
		border-bottom: 1px dashed var(--border2);
		outline: none;
		padding: 0 0 2px;
		min-width: 0;
	}

	.name:focus {
		border-bottom-color: var(--accent);
	}

	.extract {
		height: 30px;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--bg2);
		padding: 0 10px;
		font: 500 11.5px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
		min-width: 0;
	}

	.extract:focus {
		border-color: var(--accent);
	}

	.extract::placeholder {
		color: var(--dim);
	}

	.token {
		font: 400 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
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
