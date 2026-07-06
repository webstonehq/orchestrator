<!--
	NumberStepper — "− value +" stepper (mock's concurrency control).

	Props:
	- value: number
	- min?: number (default 0)
	- max?: number (default Infinity)
	- step?: number (default 1)
	- onChange: (value: number) => void
-->
<script lang="ts">
	let {
		value,
		min = 0,
		max = Infinity,
		step = 1,
		onChange
	}: {
		value: number;
		min?: number;
		max?: number;
		step?: number;
		onChange: (value: number) => void;
	} = $props();

	function clamp(n: number): number {
		return Math.max(min, Math.min(max, n));
	}

	function bump(delta: number) {
		const next = clamp(value + delta);
		if (next !== value) onChange(next);
	}
</script>

<div class="stepper">
	<button type="button" aria-label="Decrease" disabled={value <= min} onclick={() => bump(-step)}>
		−
	</button>
	<div class="value">{value}</div>
	<button type="button" aria-label="Increase" disabled={value >= max} onclick={() => bump(step)}>
		+
	</button>
</div>

<style>
	.stepper {
		display: inline-flex;
		align-items: center;
		border: 1px solid var(--border2);
		border-radius: 8px;
		overflow: hidden;
	}

	button {
		width: 32px;
		height: 34px;
		border: none;
		background: var(--panel);
		color: var(--muted);
		cursor: pointer;
		font: 600 16px 'IBM Plex Mono', monospace;
	}

	button:hover:not(:disabled) {
		color: var(--text);
	}

	button:disabled {
		color: var(--dim);
		cursor: default;
		opacity: 0.5;
	}

	.value {
		width: 52px;
		text-align: center;
		font: 600 14px 'IBM Plex Mono', monospace;
		color: var(--accent);
		border-left: 1px solid var(--border2);
		border-right: 1px solid var(--border2);
		height: 34px;
		line-height: 34px;
	}
</style>
