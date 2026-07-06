<!--
	DurationInput — number input with a "sec" suffix label.

	Props:
	- value: number          — duration in seconds
	- min?: number (default 0)
	- max?: number
	- onChange: (value: number) => void
-->
<script lang="ts">
	let {
		value,
		min = 0,
		max,
		onChange
	}: {
		value: number;
		min?: number;
		max?: number;
		onChange: (value: number) => void;
	} = $props();

	function onInput(e: Event) {
		const raw = (e.currentTarget as HTMLInputElement).value;
		const n = Number(raw);
		if (raw === '' || !Number.isFinite(n)) return;
		let next = n;
		if (min !== undefined) next = Math.max(min, next);
		if (max !== undefined) next = Math.min(max, next);
		onChange(next);
	}
</script>

<div class="duration">
	<input type="number" {value} {min} {max} oninput={onInput} />
	<span class="suffix">sec</span>
</div>

<style>
	.duration {
		display: inline-flex;
		align-items: center;
		height: 34px;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--bg2);
		overflow: hidden;
	}

	.duration:focus-within {
		border-color: var(--accent);
	}

	input {
		width: 72px;
		height: 100%;
		border: none;
		background: transparent;
		padding: 0 10px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
		appearance: textfield;
		-moz-appearance: textfield;
	}

	input::-webkit-outer-spin-button,
	input::-webkit-inner-spin-button {
		-webkit-appearance: none;
		margin: 0;
	}

	.suffix {
		padding: 0 10px;
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		border-left: 1px solid var(--border2);
		height: 100%;
		display: flex;
		align-items: center;
		background: var(--panel);
	}
</style>
