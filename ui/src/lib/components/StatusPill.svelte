<script lang="ts">
	import { STATUS, type Status } from '$lib/status';

	let { status }: { status: Status } = $props();

	const s = $derived(STATUS[status]);
</script>

<span
	class="pill"
	style="color:{s.color};background:{s.bg};border-color:{s.color}40"
>
	{s.label}
</span>

<style>
	.pill {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 2px 9px;
		border-radius: 999px;
		border: 1px solid transparent;
		font: 600 11px 'IBM Plex Mono', monospace;
		white-space: nowrap;
	}
</style>
