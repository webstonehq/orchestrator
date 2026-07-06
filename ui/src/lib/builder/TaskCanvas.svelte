<!--
	TaskCanvas — the vertical flow chain: definition pills (Inputs /
	Variables / Trigger), trigger node (or "+ Add trigger" when none), task
	nodes (plain plugin nodes and 300px parallel cards), and the dashed
	"Add task" node which opens a plugin-manifest menu (+ a Parallel entry).
-->
<script lang="ts">
	import { isParallel } from '../api';
	import type { PluginManifest, RegularTaskSpec, TaskSpec } from '../api';
	import type { BuilderStore } from './state.svelte';
	import { humanizeCron, issuesUnder } from './defs';

	let { store }: { store: BuilderStore } = $props();

	let menuOpen = $state(false);
	let menuEl: HTMLDivElement | undefined = $state();
	let menuListEl: HTMLDivElement | undefined = $state();
	/** Button that opened the menu — refocused when the menu closes. */
	let menuOpener: HTMLElement | null = null;

	function openMenu(e: MouseEvent, open: boolean) {
		menuOpener = e.currentTarget as HTMLElement;
		menuOpen = open;
	}

	function closeMenu(refocus: boolean) {
		menuOpen = false;
		if (refocus) menuOpener?.focus();
	}

	// Move focus onto the first entry when the menu opens.
	$effect(() => {
		if (menuOpen) menuListEl?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
	});

	// Up/Down/Home/End roving focus inside the menu.
	function onMenuKeydown(e: KeyboardEvent) {
		const items = Array.from(
			menuListEl?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []
		);
		if (items.length === 0) return;
		const cur = items.indexOf(document.activeElement as HTMLElement);
		let next: number;
		if (e.key === 'ArrowDown') next = cur < 0 ? 0 : (cur + 1) % items.length;
		else if (e.key === 'ArrowUp') next = cur < 0 ? items.length - 1 : (cur - 1 + items.length) % items.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = items.length - 1;
		else return;
		e.preventDefault();
		items[next].focus();
	}

	const trigger = $derived(store.def.triggers[0]);

	function nodeSub(task: TaskSpec): string {
		if (isParallel(task)) return '';
		const method = (task as RegularTaskSpec).config?.method;
		return typeof method === 'string' && method !== '' ? `${method} · ${task.type}` : task.type;
	}

	function hasErrors(index: number): boolean {
		return issuesUnder(store.issues, `tasks[${index}]`).length > 0;
	}

	function pickPlugin(manifest: PluginManifest) {
		store.addPluginTask(manifest);
		closeMenu(true);
	}

	function pickParallel() {
		store.addParallelTask();
		closeMenu(true);
	}

	function onWindowPointerDown(e: PointerEvent) {
		if (menuOpen && menuEl && !menuEl.contains(e.target as Node)) closeMenu(false);
	}

	const selectedIndex = $derived(store.selection.kind === 'task' ? store.selection.index : -1);
</script>

<svelte:window
	onpointerdown={onWindowPointerDown}
	onkeydown={(e) => {
		if (e.key === 'Escape' && menuOpen) closeMenu(true);
	}}
/>

<div class="canvas-wrap">
	<div class="pills">
		<button
			class="pill"
			class:active={store.selection.kind === 'inputs'}
			type="button"
			onclick={() => store.select({ kind: 'inputs' })}
		>
			Inputs<span class="count">{store.def.inputs.length}</span>
		</button>
		<button
			class="pill"
			class:active={store.selection.kind === 'vars'}
			type="button"
			onclick={() => store.select({ kind: 'vars' })}
		>
			Variables<span class="count">{store.def.variables.length}</span>
		</button>
		<button
			class="pill"
			class:active={store.selection.kind === 'trigger'}
			type="button"
			onclick={() => store.select({ kind: 'trigger' })}
		>
			Trigger<span class="count">{store.def.triggers.length}</span>
		</button>
		<div class="menu-anchor" bind:this={menuEl}>
			<button
				class="add-task"
				type="button"
				aria-haspopup="menu"
				aria-expanded={menuOpen}
				onpointerdown={(e) => e.stopPropagation()}
				onclick={(e) => openMenu(e, !menuOpen)}
			>
				<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round">
					<line x1="12" y1="5" x2="12" y2="19"></line>
					<line x1="5" y1="12" x2="19" y2="12"></line>
				</svg>
				Add task
			</button>
			{#if menuOpen}
				<div
					class="menu"
					role="menu"
					aria-label="Add a task"
					tabindex="-1"
					bind:this={menuListEl}
					onkeydown={onMenuKeydown}
				>
					<div class="menu-title">Add a task</div>
					{#each store.plugins as plugin (plugin.type_id)}
						<button class="menu-item" type="button" role="menuitem" onclick={() => pickPlugin(plugin)}>
							<span class="menu-dot" style:background={plugin.color}></span>
							<span class="menu-text">
								<span class="menu-label">{plugin.label}</span>
								<span class="menu-desc">{plugin.description}</span>
							</span>
							<span class="menu-type">{plugin.type_id}</span>
						</button>
					{/each}
					<button class="menu-item" type="button" role="menuitem" onclick={pickParallel}>
						<span class="menu-dot" style:background="#d2a8ff"></span>
						<span class="menu-text">
							<span class="menu-label">Parallel</span>
							<span class="menu-desc">Fan out child steps over an items array</span>
						</span>
						<span class="menu-type parallel">parallel</span>
					</button>
				</div>
			{/if}
		</div>
	</div>

	<div class="chain">
		{#if trigger}
			<button
				class="node trigger"
				class:selected={store.selection.kind === 'trigger'}
				type="button"
				onclick={() => store.select({ kind: 'trigger' })}
			>
				<svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" style="flex:0 0 auto">
					<circle cx="12" cy="12" r="8"></circle>
					<path d="M12 8v4l2.6 2"></path>
				</svg>
				<span class="node-body">
					<span class="node-label">{trigger.id}</span>
					<span class="node-sub">{humanizeCron(trigger.cron)} · {trigger.timezone ?? 'UTC'}</span>
				</span>
				<span class="badge" class:off={trigger.enabled === false}>
					{trigger.enabled === false ? 'disabled' : 'trigger'}
				</span>
			</button>
		{:else}
			<button class="node ghost" type="button" onclick={() => store.addTrigger()}>
				<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<circle cx="12" cy="12" r="8"></circle>
					<path d="M12 8v4l2.6 2"></path>
				</svg>
				Add trigger
			</button>
		{/if}

		{#each store.def.tasks as task, i (i)}
			<div class="link"></div>
			{#if isParallel(task)}
				<button
					class="fanout"
					class:selected={selectedIndex === i}
					class:bad={hasErrors(i)}
					type="button"
					onclick={() => store.select({ kind: 'task', index: i })}
				>
					<span class="fanout-head">
						<span class="pdot"></span>
						<span class="node-label">{task.id}</span>
						<span class="pchip">parallel</span>
						<span class="conc">×{task.concurrency}</span>
					</span>
					<span class="items" title={task.items}>{task.items || 'items: —'}</span>
					<span class="bar"></span>
					<span class="fanout-foot">
						<span class="children">{task.tasks.length} child step{task.tasks.length === 1 ? '' : 's'} / item</span>
						<span class="out" class:zero={task.outputs.length === 0}>
							{task.outputs.length} output{task.outputs.length === 1 ? '' : 's'}
						</span>
					</span>
				</button>
			{:else}
				<button
					class="node task"
					class:selected={selectedIndex === i}
					class:bad={hasErrors(i)}
					type="button"
					onclick={() => store.select({ kind: 'task', index: i })}
				>
					<span class="dot" style:background={store.manifestFor(task.type)?.color ?? 'var(--accent)'}></span>
					<span class="node-body">
						<span class="node-label">{task.id}</span>
						<span class="node-sub">{nodeSub(task)}</span>
					</span>
					<span class="out" class:zero={task.outputs.length === 0}>
						{task.outputs.length} output{task.outputs.length === 1 ? '' : 's'}
					</span>
				</button>
			{/if}
		{/each}

		<div class="link"></div>
		<button
			class="node ghost"
			type="button"
			aria-haspopup="menu"
			aria-expanded={menuOpen}
			onpointerdown={(e) => e.stopPropagation()}
			onclick={(e) => openMenu(e, true)}
		>
			<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round">
				<line x1="12" y1="5" x2="12" y2="19"></line>
				<line x1="5" y1="12" x2="19" y2="12"></line>
			</svg>
			Add task
		</button>
	</div>
</div>

<style>
	.canvas-wrap {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		background:
			radial-gradient(circle at 40% 0%, rgba(126, 231, 135, 0.03), transparent 55%),
			var(--bg);
	}

	.pills {
		height: 44px;
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0 14px;
		border-bottom: 1px solid var(--border);
	}

	.pill {
		height: 30px;
		padding: 0 12px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--muted);
		font: 600 12px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.pill:hover {
		color: var(--text);
	}

	.pill.active {
		border-color: var(--accent);
		color: var(--text);
		background: rgba(126, 231, 135, 0.08);
	}

	.count {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border-radius: 5px;
		padding: 1px 6px;
	}

	.pill.active .count {
		color: var(--accent);
		background: rgba(126, 231, 135, 0.12);
	}

	.menu-anchor {
		margin-left: auto;
		position: relative;
	}

	.add-task {
		height: 30px;
		padding: 0 12px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--text);
		font: 600 12px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.add-task:hover {
		border-color: var(--accent);
	}

	.menu {
		position: absolute;
		top: calc(100% + 6px);
		right: 0;
		z-index: 30;
		width: 320px;
		border: 1px solid var(--border2);
		border-radius: 11px;
		background: var(--panel2);
		box-shadow: 0 20px 44px -14px rgba(0, 0, 0, 0.75);
		overflow: hidden;
	}

	.menu-title {
		padding: 9px 13px;
		border-bottom: 1px solid var(--border);
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--text);
	}

	.menu-item {
		display: flex;
		align-items: center;
		gap: 10px;
		width: 100%;
		padding: 9px 13px;
		border: none;
		background: transparent;
		cursor: pointer;
		text-align: left;
	}

	.menu-item:hover {
		background: var(--panel3);
	}

	.menu-dot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		flex: 0 0 auto;
	}

	.menu-text {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
		flex: 1;
	}

	.menu-label {
		font: 600 12.5px 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--text);
	}

	.menu-desc {
		font-size: 11px;
		color: var(--dim);
		line-height: 1.4;
	}

	.menu-type {
		font: 600 9.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border: 1px solid var(--border2);
		border-radius: 5px;
		padding: 2px 7px;
		white-space: nowrap;
	}

	.menu-type.parallel {
		color: #d2a8ff;
		background: rgba(210, 168, 255, 0.14);
		border-color: rgba(210, 168, 255, 0.4);
	}

	.chain {
		flex: 1;
		overflow: auto;
		padding: 20px 16px 30px;
		display: flex;
		flex-direction: column;
		align-items: center;
	}

	.link {
		width: 2px;
		height: 22px;
		background: var(--border2);
		margin: 6px auto;
		flex: 0 0 auto;
	}

	.node {
		width: 300px;
		min-height: 52px;
		border: 1px solid var(--border2);
		border-radius: 11px;
		background: var(--panel);
		display: flex;
		align-items: center;
		padding: 9px 13px;
		cursor: pointer;
		text-align: left;
		flex: 0 0 auto;
	}

	.node:hover {
		border-color: var(--dim);
	}

	.node.selected {
		border-color: var(--accent);
		box-shadow: 0 0 0 1px rgba(126, 231, 135, 0.35);
	}

	.node.bad,
	.fanout.bad {
		border-color: rgba(248, 81, 73, 0.55);
	}

	.node.selected.bad,
	.fanout.selected.bad {
		box-shadow: 0 0 0 1px rgba(248, 81, 73, 0.4);
	}

	.node.ghost {
		border: 1.5px dashed var(--border2);
		background: transparent;
		justify-content: center;
		gap: 8px;
		color: var(--dim);
		font: 600 12px 'IBM Plex Mono', monospace;
		height: 46px;
		min-height: 46px;
	}

	.node.ghost:hover {
		border-color: var(--accent);
		color: var(--accent);
	}

	.dot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		flex: 0 0 auto;
	}

	.node-body {
		min-width: 0;
		flex: 1;
		margin: 0 10px;
		display: flex;
		flex-direction: column;
		gap: 1px;
	}

	.node-label {
		font: 600 12.5px 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.node-sub {
		font: 400 10.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.badge {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border: 1px solid var(--border2);
		border-radius: 6px;
		padding: 2px 7px;
		white-space: nowrap;
	}

	.badge.off {
		color: var(--amber);
		border-color: rgba(227, 179, 65, 0.45);
		background: rgba(227, 179, 65, 0.1);
	}

	.out {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--accent);
		background: rgba(126, 231, 135, 0.1);
		border: 1px solid rgba(126, 231, 135, 0.35);
		border-radius: 6px;
		padding: 2px 7px;
		white-space: nowrap;
	}

	.out.zero {
		color: var(--dim);
		background: var(--panel3);
		border-color: var(--border2);
	}

	.fanout {
		width: 300px;
		border: 1px solid rgba(210, 168, 255, 0.4);
		border-radius: 12px;
		background: var(--panel);
		display: flex;
		flex-direction: column;
		align-items: stretch;
		padding: 11px 13px;
		cursor: pointer;
		text-align: left;
		flex: 0 0 auto;
	}

	.fanout:hover {
		border-color: #d2a8ff;
	}

	.fanout.selected {
		border-color: #d2a8ff;
		box-shadow: 0 0 0 1px rgba(210, 168, 255, 0.35);
	}

	.fanout-head {
		display: flex;
		align-items: center;
		gap: 9px;
		width: 100%;
	}

	.pdot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: #d2a8ff;
		flex: 0 0 auto;
	}

	.pchip {
		font: 600 9.5px 'IBM Plex Mono', monospace;
		color: #d2a8ff;
		background: rgba(210, 168, 255, 0.14);
		border: 1px solid rgba(210, 168, 255, 0.4);
		border-radius: 5px;
		padding: 1px 6px;
	}

	.conc {
		margin-left: auto;
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.items {
		font: 400 10.5px 'IBM Plex Mono', monospace;
		color: #79c0ff;
		width: 100%;
		margin-top: 5px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.bar {
		display: block;
		height: 7px;
		border-radius: 4px;
		background:
			repeating-linear-gradient(
				90deg,
				rgba(210, 168, 255, 0.28) 0 10px,
				transparent 10px 16px
			),
			var(--panel3);
		width: 100%;
		margin-top: 10px;
	}

	.fanout-foot {
		display: flex;
		align-items: center;
		width: 100%;
		margin-top: 9px;
		gap: 8px;
	}

	.children {
		font: 400 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.fanout-foot .out {
		margin-left: auto;
	}
</style>
