<!--
	TextField — single-line mono text input.

	Props:
	- value: string
	- placeholder?: string
	- onChange: (value: string) => void
-->
<script lang="ts">
	let {
		value,
		placeholder = '',
		onChange
	}: {
		value: string;
		placeholder?: string;
		onChange: (value: string) => void;
	} = $props();
</script>

<input
	{value}
	{placeholder}
	spellcheck="false"
	oninput={(e) => onChange(e.currentTarget.value)}
/>

<style>
	input {
		width: 100%;
		box-sizing: border-box;
		height: 34px;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--bg2);
		padding: 0 12px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	input:focus {
		border-color: var(--accent);
	}

	input::placeholder {
		color: var(--dim);
	}
</style>
