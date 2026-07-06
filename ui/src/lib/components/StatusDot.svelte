<script lang="ts">
	import { STATUS, type Status } from '$lib/status';

	let { status }: { status: Status } = $props();

	const s = $derived(STATUS[status]);
</script>

<span
	class="dot"
	class:pulse={status === 'running'}
	style="background:{s.color};box-shadow:0 0 7px {s.color}"
	role="img"
	aria-label={s.label}
></span>

<style>
	.dot {
		display: inline-block;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		flex: 0 0 auto;
	}

	.pulse {
		animation: dotPulse 1s ease-in-out infinite;
	}
</style>
