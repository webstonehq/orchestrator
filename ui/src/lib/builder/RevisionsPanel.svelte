<!--
	RevisionsPanel — dropdown listing a flow's revisions (rev, message,
	age). Clicking a revision loads its definition into the editor (after a
	confirm when dirty); the caller marks the state dirty and shows a
	"viewing rev N" banner. The current revision is marked.

	Props:
	- revisions: RevisionInfo[]
	- currentRev: number
	- onPick: (rev: number) => void
	- onClose: () => void
-->
<script lang="ts">
	import type { RevisionInfo } from '../api';
	import { relativeTime } from '../format';

	let {
		revisions,
		currentRev,
		onPick,
		onClose
	}: {
		revisions: RevisionInfo[];
		currentRev: number;
		onPick: (rev: number) => void;
		onClose: () => void;
	} = $props();

	let panel: HTMLDivElement | undefined = $state();

	// Move focus into the panel when it opens (the caller restores focus to
	// the toggle chip in its onClose).
	$effect(() => {
		panel?.focus();
	});

	function onWindowPointerDown(e: PointerEvent) {
		if (panel && !panel.contains(e.target as Node)) onClose();
	}
</script>

<svelte:window
	onpointerdown={onWindowPointerDown}
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
	}}
/>

<div
	class="panel"
	role="dialog"
	aria-label="Revision history"
	tabindex="-1"
	bind:this={panel}
>
	<div class="title">Revision history</div>
	{#if revisions.length === 0}
		<div class="empty">No revisions yet</div>
	{/if}
	{#each revisions as r (r.rev)}
		<button class="row" type="button" onclick={() => onPick(r.rev)}>
			<span class="dot" class:current={r.rev === currentRev}></span>
			<span class="body">
				<span class="rev">
					rev {r.rev}
					{#if r.rev === currentRev}<span class="cur">current</span>{/if}
				</span>
				<span class="msg">{r.message || '—'}</span>
			</span>
			<span class="ago">{relativeTime(r.created_at)}</span>
		</button>
	{/each}
</div>

<style>
	.panel {
		position: absolute;
		top: calc(100% + 8px);
		right: 0;
		z-index: 40;
		width: 300px;
		max-height: 340px;
		overflow: auto;
		border: 1px solid var(--border2);
		border-radius: 11px;
		background: var(--panel2);
		box-shadow: 0 18px 40px -12px rgba(0, 0, 0, 0.7);
		outline: none;
	}

	.title {
		padding: 10px 13px;
		border-bottom: 1px solid var(--border);
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.empty {
		padding: 16px 13px;
		font-size: 12px;
		color: var(--dim);
	}

	.row {
		display: flex;
		gap: 10px;
		align-items: center;
		width: 100%;
		padding: 10px 13px;
		border: none;
		border-bottom: 1px solid var(--border);
		background: transparent;
		cursor: pointer;
		text-align: left;
	}

	.row:hover {
		background: var(--panel3);
	}

	.row:last-child {
		border-bottom: none;
	}

	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--panel3);
		border: 1px solid var(--border2);
		flex: 0 0 auto;
	}

	.dot.current {
		background: var(--accent);
		border-color: var(--accent);
		box-shadow: 0 0 7px var(--accent);
	}

	.body {
		min-width: 0;
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.rev {
		font: 600 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.cur {
		font: 600 9px 'IBM Plex Mono', monospace;
		color: var(--accent);
		background: rgba(126, 231, 135, 0.1);
		border: 1px solid rgba(126, 231, 135, 0.35);
		border-radius: 5px;
		padding: 1px 5px;
	}

	.msg {
		font-size: 11px;
		color: var(--dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.ago {
		font-size: 10px;
		color: var(--dim);
		white-space: nowrap;
	}
</style>
