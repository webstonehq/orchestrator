<!--
	ExprPicker — dropdown panel listing picker groups (from pickerGroups()).
	Absolutely positioned below its anchor; the parent provides a
	position:relative container. Closes on outside click or Escape.

	KNOWN LIMITATION (F2 risk): the panel always opens downward (top: 100%)
	with no viewport-overflow flip/reposition handling. Anchors near the
	bottom of a scroll container or the viewport will clip the panel; F2
	should place template fields accordingly or add flip logic here.

	Props:
	- groups: PickerGroup[]
	- onPick: (value: string) => void   — called with the ref path to insert
	- onClose: () => void               — outside click / Escape
-->
<script lang="ts">
	import type { PickerGroup } from '../picker';

	let {
		groups,
		onPick,
		onClose
	}: {
		groups: PickerGroup[];
		onPick: (value: string) => void;
		onClose: () => void;
	} = $props();

	let panel: HTMLDivElement | undefined = $state();

	const TYPE_COLORS: Record<string, string> = {
		STRING: '#a5d6ff',
		ARRAY: '#7ee787',
		DATE: '#f69d50',
		INT: '#f69d50',
		BOOLEAN: '#e3b341',
		JSON: '#d2a8ff'
	};

	function typeColor(type: string): string {
		return TYPE_COLORS[type] ?? '#adbac7';
	}

	function onWindowPointerDown(e: PointerEvent) {
		if (panel && !panel.contains(e.target as Node)) {
			onClose();
		}
	}

	// Escape must suppress other window-level Escape handlers (e.g. Modal's)
	// while the picker is open. stopPropagation cannot do that from a
	// bubble-phase listener on the same target: listeners registered on the
	// SAME EventTarget for the same phase all run regardless of
	// stopPropagation (only stopImmediatePropagation skips the rest, and only
	// those registered after this one). A CAPTURE-phase window listener runs
	// before any bubble/target-phase window listener, so registering with
	// capture=true and calling both stopPropagation (halts the capture ->
	// target -> bubble traversal, so Modal's bubble-phase window listener
	// never fires) and stopImmediatePropagation (also skips any later
	// capture-phase window listeners) guarantees this handler wins.
	$effect(() => {
		const onKeydownCapture = (e: KeyboardEvent) => {
			if (e.key === 'Escape') {
				e.stopPropagation();
				e.stopImmediatePropagation();
				onClose();
			}
		};
		window.addEventListener('keydown', onKeydownCapture, true);
		return () => window.removeEventListener('keydown', onKeydownCapture, true);
	});
</script>

<svelte:window onpointerdown={onWindowPointerDown} />

<div class="picker" bind:this={panel}>
	<div class="header">
		<svg
			aria-hidden="true"
			width="12"
			height="12"
			viewBox="0 0 24 24"
			fill="none"
			stroke="var(--cyan)"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
		>
			<path
				d="M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5a2 2 0 0 0 2 2h1M16 3h1a2 2 0 0 1 2 2v5a2 2 0 0 0 2 2 2 2 0 0 0-2 2v5a2 2 0 0 1-2 2h-1"
			></path>
		</svg>
		Insert a value
	</div>
	{#each groups as group (group.title)}
		<div class="group-title">{group.title}</div>
		{#each group.items as item (item.value)}
			<button class="item" type="button" onclick={() => onPick(item.value)}>
				<span class="label">{item.label}</span>
				<span
					class="type"
					style:color={typeColor(item.type)}
					style:background="{typeColor(item.type)}1a"
					style:border-color="{typeColor(item.type)}55"
				>
					{item.type}
				</span>
			</button>
		{/each}
	{/each}
</div>

<style>
	.picker {
		position: absolute;
		top: calc(100% + 6px);
		left: 0;
		z-index: 30;
		width: 340px;
		max-height: 280px;
		overflow: auto;
		border: 1px solid var(--border2);
		border-radius: 10px;
		background: var(--panel2);
		box-shadow: 0 20px 44px -14px rgba(0, 0, 0, 0.75);
	}

	.header {
		padding: 9px 13px;
		border-bottom: 1px solid var(--border);
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--text);
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.group-title {
		padding: 8px 13px 3px;
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
	}

	.item {
		display: flex;
		align-items: center;
		gap: 9px;
		width: 100%;
		padding: 7px 13px;
		border: none;
		background: transparent;
		cursor: pointer;
		text-align: left;
	}

	.item:hover,
	.item:focus-visible {
		background: var(--panel3);
		outline: none;
	}

	.label {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: #79c0ff;
		flex: 1;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.type {
		font: 600 10px 'IBM Plex Mono', monospace;
		border: 1px solid;
		border-radius: 5px;
		padding: 2px 7px;
		white-space: nowrap;
	}
</style>
