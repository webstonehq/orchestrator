<script lang="ts">
	let {
		label,
		value,
		sub = '',
		valueColor
	}: {
		label: string;
		value: string | number;
		sub?: string;
		valueColor?: string;
	} = $props();
</script>

<div class="card">
	<div class="label">{label}</div>
	<div class="value" style:color={valueColor}>{value}</div>
	{#if sub}<div class="sub">{sub}</div>{/if}
</div>

<style>
	.card {
		border: 1px solid var(--border);
		background: var(--panel);
		border-radius: 12px;
		padding: 14px 15px;
	}

	.label {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
		margin-bottom: 9px;
	}

	.value {
		font: 600 25px/1 'IBM Plex Sans', system-ui, sans-serif;
		letter-spacing: -0.5px;
	}

	.sub {
		font-size: 11px;
		color: var(--muted);
		margin-top: 7px;
	}
</style>
