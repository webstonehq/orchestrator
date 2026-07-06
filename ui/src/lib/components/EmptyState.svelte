<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		title,
		hint = '',
		cta
	}: {
		title: string;
		hint?: string;
		cta?: Snippet;
	} = $props();
</script>

<div class="empty">
	<div class="title">{title}</div>
	{#if hint}<div class="hint">{hint}</div>{/if}
	{#if cta}
		<div class="cta">{@render cta()}</div>
	{/if}
</div>

<style>
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 7px;
		padding: 54px 24px;
		border: 1px dashed var(--border2);
		border-radius: 12px;
		background: var(--panel);
		text-align: center;
	}

	.title {
		font: 600 14px 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--muted);
	}

	.hint {
		font: 400 12px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.cta {
		margin-top: 10px;
	}
</style>
