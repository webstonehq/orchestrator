<!--
	InspectorTask — a top-level plugin task: header (id, type chip, delete)
	plus the shared PluginTaskEditor body (config fields from the manifest,
	envelope, outputs). Picker groups are scoped to UPSTREAM tasks only.
-->
<script lang="ts">
	import type { RegularTaskSpec } from '../api';
	import type { BuilderStore } from './state.svelte';
	import { inlineTaskPathPattern, issuesUnder } from './defs';
	import PluginTaskEditor from './PluginTaskEditor.svelte';
	import IssueList from './IssueList.svelte';

	let {
		store,
		task,
		index
	}: {
		store: BuilderStore;
		task: RegularTaskSpec;
		index: number;
	} = $props();

	const pathBase = $derived(`tasks[${index}]`);
	const manifest = $derived(store.manifestFor(task.type));
	const groups = $derived(store.groupsForTask(index));

	// Paths rendered inline by the editor body.
	const inlinePattern = $derived(inlineTaskPathPattern(pathBase));
	const unmatched = $derived(
		issuesUnder(store.issues, pathBase).filter((i) => !inlinePattern.test(i.path))
	);
</script>

<div class="panel">
	<div class="head">
		<span class="dot"></span>
		<input
			class="id"
			value={task.id}
			spellcheck="false"
			aria-label="Task id"
			oninput={(e) => (task.id = e.currentTarget.value)}
		/>
		<span class="type-chip" title={manifest?.description ?? task.type}>
			{manifest ? manifest.label : task.type}
			<span class="type-id">{task.type}</span>
		</span>
		<button class="delete" type="button" onclick={() => store.deleteTask(index)}>
			<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<polyline points="3 6 5 6 21 6"></polyline>
				<path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6m5 0V4a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v2"></path>
			</svg>
			Delete
		</button>
	</div>
	{#if store.errorAt(`${pathBase}.id`)}
		<div class="id-err">{store.errorAt(`${pathBase}.id`)}</div>
	{/if}

	<IssueList issues={unmatched} />

	<PluginTaskEditor {task} {store} {pathBase} {groups} />
</div>

<style>
	.panel {
		padding: 18px 22px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 14px;
	}

	.dot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: var(--accent);
		box-shadow: 0 0 8px var(--accent);
		flex: 0 0 auto;
	}

	.id {
		font: 600 15px 'IBM Plex Mono', monospace;
		color: var(--text);
		background: transparent;
		border: none;
		border-bottom: 1px dashed var(--border2);
		outline: none;
		padding: 0 0 2px;
		field-sizing: content;
		min-width: 80px;
		max-width: 280px;
	}

	.id:focus {
		border-bottom-color: var(--accent);
	}

	.type-chip {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--muted);
		background: var(--panel3);
		border: 1px solid var(--border2);
		border-radius: 6px;
		padding: 3px 8px;
		display: inline-flex;
		align-items: center;
		gap: 6px;
		white-space: nowrap;
	}

	.type-id {
		color: var(--dim);
		font-weight: 500;
	}

	.delete {
		margin-left: auto;
		height: 30px;
		padding: 0 11px;
		border-radius: 8px;
		border: 1px solid #5a2b2b;
		background: rgba(248, 81, 73, 0.08);
		color: var(--red);
		font: 600 11.5px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.id-err {
		margin: -8px 0 12px;
		font-size: 11px;
		color: var(--red);
	}
</style>
