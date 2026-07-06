<!--
	InspectorVars — flow variables: id = value rows. Values are LITERAL
	strings (no template editor): variables are constants that templates
	reference, they do not themselves contain expressions.
-->
<script lang="ts">
	import type { BuilderStore } from './state.svelte';
	import { issuesUnder } from './defs';
	import IssueList from './IssueList.svelte';

	let { store }: { store: BuilderStore } = $props();

	const unmatched = $derived(
		issuesUnder(store.issues, 'variables').filter((i) => !/^variables\[\d+\]\.id$/.test(i.path))
	);
</script>

<div class="panel">
	<div class="head">
		<h3>Variables</h3>
		<button class="add" type="button" onclick={() => store.addVariable()}>
			<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round">
				<line x1="12" y1="5" x2="12" y2="19"></line>
				<line x1="5" y1="12" x2="19" y2="12"></line>
			</svg>
			Add variable
		</button>
	</div>
	<p class="hint">
		Static values reused across tasks; reference as
		<span class="ref">vars.&lt;id&gt;</span>. Values are literals — they are not templates and
		resolve exactly as written.
	</p>

	<IssueList issues={unmatched} />

	{#if store.def.variables.length === 0}
		<button class="none" type="button" onclick={() => store.addVariable()}>
			No variables yet — add one for values you reuse across tasks
		</button>
	{:else}
		<div class="table">
			{#each store.def.variables as variable, i (i)}
				<div class="row">
					<input
						class="id"
						value={variable.id}
						spellcheck="false"
						aria-label="Variable id"
						oninput={(e) => (variable.id = e.currentTarget.value)}
					/>
					<span class="eq">=</span>
					<input
						class="val"
						value={variable.value}
						spellcheck="false"
						placeholder="literal value"
						aria-label="Variable value"
						oninput={(e) => (variable.value = e.currentTarget.value)}
					/>
					<button
						class="del"
						type="button"
						aria-label="Delete variable {variable.id}"
						onclick={() => store.def.variables.splice(i, 1)}
					>
						<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
							<line x1="6" y1="6" x2="18" y2="18"></line>
							<line x1="18" y1="6" x2="6" y2="18"></line>
						</svg>
					</button>
					{#if store.errorAt(`variables[${i}].id`)}
						<div class="err">{store.errorAt(`variables[${i}].id`)}</div>
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
		color: #d2a8ff;
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
		overflow: hidden;
		background: var(--panel);
	}

	.row {
		display: grid;
		grid-template-columns: minmax(90px, 0.7fr) auto 2fr 34px;
		align-items: center;
		gap: 10px;
		padding: 11px 15px;
		border-bottom: 1px solid var(--border);
	}

	.row:last-child {
		border-bottom: none;
	}

	.id {
		font: 600 12.5px 'IBM Plex Mono', monospace;
		color: #d2a8ff;
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

	.eq {
		color: var(--dim);
	}

	.val {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: #a5d6ff;
		background: transparent;
		border: none;
		border-bottom: 1px dashed var(--border2);
		outline: none;
		padding: 0 0 2px;
		min-width: 0;
	}

	.val:focus {
		border-bottom-color: var(--accent);
	}

	.val::placeholder {
		color: var(--dim);
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
