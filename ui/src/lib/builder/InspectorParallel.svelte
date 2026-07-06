<!--
	InspectorParallel — a parallel fan-out task: items template (fan-out
	source, upstream-scoped picker), concurrency stepper, ordered child
	steps (each a full plugin task editor with ITERATION + PRIOR STEPS
	picker groups), and the parallel task's own outputs (extract usually
	`result.items`).
-->
<script lang="ts">
	import type { ParallelTaskSpec } from '../api';
	import type { BuilderStore } from './state.svelte';
	import { inlineParallelPathPattern, issuesUnder } from './defs';
	import TemplateInput from '../components/fields/TemplateInput.svelte';
	import NumberStepper from '../components/fields/NumberStepper.svelte';
	import PluginTaskEditor from './PluginTaskEditor.svelte';
	import OutputsTable from './OutputsTable.svelte';
	import IssueList from './IssueList.svelte';

	let {
		store,
		task,
		index
	}: {
		store: BuilderStore;
		task: ParallelTaskSpec;
		index: number;
	} = $props();

	const pathBase = $derived(`tasks[${index}]`);
	const itemsGroups = $derived(store.groupsForTask(index));

	// Paths rendered inline here or inside child editors.
	const inlinePattern = $derived(inlineParallelPathPattern(pathBase));
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
		<span class="parallel-chip">parallel</span>
		<button class="delete" type="button" onclick={() => store.deleteTask(index)}>
			<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<polyline points="3 6 5 6 21 6"></polyline>
				<path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6m5 0V4a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v2"></path>
			</svg>
			Delete
		</button>
	</div>
	{#if store.errorAt(`${pathBase}.id`)}
		<div class="err">{store.errorAt(`${pathBase}.id`)}</div>
	{/if}

	<IssueList issues={unmatched} />

	<div class="fanout-head">
		<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="#d2a8ff" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
			<circle cx="6" cy="6" r="2.4"></circle>
			<circle cx="6" cy="18" r="2.4"></circle>
			<circle cx="18" cy="12" r="2.4"></circle>
			<path d="M8.2 6.8 15.6 11M8.2 17.2 15.6 13"></path>
		</svg>
		<div class="section-label">Fan-out source</div>
	</div>
	<p class="hint">
		The collection to iterate over — one child run per element. Pick a list (type
		<span class="arr">ARRAY</span>) from an upstream task output.
	</p>
	<div class="items-row">
		<span class="items-key">items</span>
		<TemplateInput
			value={task.items}
			groups={itemsGroups}
			placeholder="pick a list →"
			onChange={(v) => (task.items = v)}
		/>
	</div>
	{#if store.errorAt(`${pathBase}.items`)}
		<div class="err">{store.errorAt(`${pathBase}.items`)}</div>
	{/if}

	<div class="conc-row">
		<div>
			<div class="section-label">Concurrency</div>
			<div class="sub">Max child runs in flight at once</div>
		</div>
		<div class="conc-stepper">
			<NumberStepper
				value={task.concurrency}
				min={1}
				max={256}
				onChange={(v) => (task.concurrency = v)}
			/>
		</div>
	</div>
	{#if store.errorAt(`${pathBase}.concurrency`)}
		<div class="err">{store.errorAt(`${pathBase}.concurrency`)}</div>
	{/if}

	<div class="steps-head">
		<div class="section-label">Child steps</div>
		<span class="sub">run in order, once per item</span>
		<button class="add-step" type="button" onclick={() => store.addChildStep(index)}>
			<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round">
				<line x1="12" y1="5" x2="12" y2="19"></line>
				<line x1="5" y1="12" x2="19" y2="12"></line>
			</svg>
			Add step
		</button>
	</div>
	{#if store.errorAt(`${pathBase}.tasks`)}
		<div class="err">{store.errorAt(`${pathBase}.tasks`)}</div>
	{/if}

	<div class="steps">
		{#each task.tasks as child, j (j)}
			{#if j > 0}
				<div class="connector">
					<div class="line"></div>
					<span>then</span>
				</div>
			{/if}
			<div class="step-card">
				<div class="step-head">
					<span class="step-no">{j + 1}</span>
					<input
						class="step-id"
						value={child.id}
						spellcheck="false"
						aria-label="Step id"
						oninput={(e) => (child.id = e.currentTarget.value)}
					/>
					<span class="step-type">{child.type}</span>
					{#if task.tasks.length > 1}
						<button
							class="step-del"
							type="button"
							aria-label="Delete step {child.id}"
							onclick={() => store.removeChildStep(index, j)}
						>
							<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
								<polyline points="3 6 5 6 21 6"></polyline>
								<path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"></path>
							</svg>
						</button>
					{/if}
				</div>
				{#if store.errorAt(`${pathBase}.tasks[${j}].id`)}
					<div class="err">{store.errorAt(`${pathBase}.tasks[${j}].id`)}</div>
				{/if}
				<p class="step-hint">
					Reference the current element as <span class="iter">taskrun.value</span>, or a prior
					step's output.
				</p>
				<PluginTaskEditor
					task={child}
					{store}
					pathBase={`${pathBase}.tasks[${j}]`}
					groups={store.groupsForChild(index, j)}
				/>
			</div>
		{/each}
	</div>

	<div class="parallel-outputs">
		<OutputsTable {task} {store} {pathBase} extractPlaceholder="result.items" />
	</div>
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
		background: #d2a8ff;
		box-shadow: 0 0 8px #d2a8ff;
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

	.parallel-chip {
		font: 600 9.5px 'IBM Plex Mono', monospace;
		color: #d2a8ff;
		background: rgba(210, 168, 255, 0.14);
		border: 1px solid rgba(210, 168, 255, 0.4);
		border-radius: 5px;
		padding: 1px 6px;
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

	.fanout-head {
		display: flex;
		align-items: center;
		gap: 9px;
		margin-bottom: 5px;
	}

	.section-label {
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
	}

	.hint {
		margin: 0 0 12px;
		font-size: 11.5px;
		color: var(--muted);
		line-height: 1.5;
	}

	.arr {
		font: 600 10.5px 'IBM Plex Mono', monospace;
		color: var(--accent);
	}

	.items-row {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 8px;
	}

	.items-key {
		flex: 0 0 60px;
		font: 600 12px 'IBM Plex Mono', monospace;
		color: #79c0ff;
	}

	.conc-row {
		display: flex;
		align-items: center;
		gap: 10px;
		margin: 16px 0 6px;
	}

	.sub {
		font-size: 11px;
		color: var(--muted);
		margin-top: 3px;
	}

	.conc-stepper {
		margin-left: auto;
	}

	.steps-head {
		display: flex;
		align-items: center;
		gap: 9px;
		margin: 18px 0 11px;
	}

	.steps-head .sub {
		margin-top: 0;
	}

	.add-step {
		margin-left: auto;
		height: 26px;
		padding: 0 10px;
		border-radius: 7px;
		border: 1px solid rgba(210, 168, 255, 0.4);
		background: rgba(210, 168, 255, 0.12);
		color: #d2a8ff;
		font: 600 11px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 5px;
	}

	.steps {
		display: flex;
		flex-direction: column;
		margin-bottom: 24px;
	}

	.connector {
		height: 18px;
		display: flex;
		align-items: center;
		gap: 7px;
		margin-left: 9px;
	}

	.connector .line {
		width: 2px;
		height: 18px;
		background: #4a3d6b;
	}

	.connector span {
		font: 500 9.5px 'IBM Plex Mono', monospace;
		color: #8a7bb0;
	}

	.step-card {
		border: 1px solid var(--border2);
		border-radius: 12px;
		background: var(--bg2);
		padding: 14px;
		box-shadow:
			4px 5px 0 -1px var(--bg),
			4px 5px 0 0 var(--border2);
	}

	.step-head {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 10px;
	}

	.step-no {
		width: 20px;
		height: 20px;
		border-radius: 6px;
		background: rgba(210, 168, 255, 0.14);
		border: 1px solid rgba(210, 168, 255, 0.4);
		color: #d2a8ff;
		font: 600 11px 'IBM Plex Mono', monospace;
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
	}

	.step-id {
		font: 600 12.5px 'IBM Plex Mono', monospace;
		color: var(--text);
		background: transparent;
		border: none;
		border-bottom: 1px dashed var(--border2);
		outline: none;
		padding: 0 0 2px;
		field-sizing: content;
		min-width: 60px;
		max-width: 200px;
	}

	.step-id:focus {
		border-bottom-color: var(--accent);
	}

	.step-type {
		font: 600 9.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border: 1px solid var(--border2);
		border-radius: 5px;
		padding: 2px 7px;
	}

	.step-del {
		margin-left: auto;
		width: 26px;
		height: 26px;
		border-radius: 7px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--dim);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
	}

	.step-del:hover {
		color: var(--red);
		border-color: var(--red);
	}

	.step-hint {
		margin: 0 0 10px;
		font-size: 11px;
		color: var(--muted);
		line-height: 1.5;
	}

	.iter {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: #79c0ff;
	}

	.parallel-outputs {
		margin-top: 6px;
	}

	.err {
		margin: 4px 0 8px;
		font-size: 11px;
		color: var(--red);
	}
</style>
