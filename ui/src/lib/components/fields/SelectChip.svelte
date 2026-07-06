<!--
	SelectChip — compact chip that cycles through its options on click
	(mock's HTTP-method chip). Keyboard accessible: it is a <button>, so
	Enter/Space cycle natively.

	Props:
	- value: string
	- options: string[]
	- onChange: (value: string) => void
	- colorMap?: Record<string, string>   — per-value accent color
	                                        (e.g. { GET: '#7ee787', POST: '#f0a878' });
	                                        values without an entry render neutral.
-->
<script lang="ts">
	let {
		value,
		options,
		onChange,
		colorMap = {}
	}: {
		value: string;
		options: string[];
		onChange: (value: string) => void;
		colorMap?: Record<string, string>;
	} = $props();

	const color = $derived(colorMap[value] ?? '#adbac7');

	function cycle() {
		if (options.length === 0) return;
		const i = options.indexOf(value);
		onChange(options[(i + 1) % options.length]);
	}
</script>

<button
	class="chip"
	type="button"
	title="Click to cycle: {options.join(' / ')}"
	style:color
	style:background="{color}1a"
	style:border-color="{color}4d"
	onclick={cycle}
>
	{value}
</button>

<style>
	.chip {
		font: 600 11px 'IBM Plex Mono', monospace;
		border: 1px solid;
		border-radius: 7px;
		padding: 0 11px;
		height: 34px;
		display: flex;
		align-items: center;
		cursor: pointer;
		white-space: nowrap;
		user-select: none;
		background: transparent;
	}

	.chip:focus-visible {
		outline: 1px solid var(--accent);
		outline-offset: 1px;
	}
</style>
