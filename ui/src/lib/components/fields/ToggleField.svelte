<!--
	ToggleField — small on/off switch.

	NOTE: intentionally independent of lib/components/Toggle.svelte (owned by
	another workstream) to avoid cross-agent file dependencies during parallel
	development. Consolidate the two after both land.

	Props:
	- checked: boolean
	- onChange: (checked: boolean) => void
	- disabled?: boolean
-->
<script lang="ts">
	let {
		checked,
		onChange,
		disabled = false
	}: {
		checked: boolean;
		onChange: (checked: boolean) => void;
		disabled?: boolean;
	} = $props();
</script>

<button
	class="toggle"
	class:on={checked}
	type="button"
	role="switch"
	aria-checked={checked}
	aria-label={checked ? 'On' : 'Off'}
	{disabled}
	onclick={() => onChange(!checked)}
>
	<span class="knob"></span>
</button>

<style>
	.toggle {
		width: 40px;
		height: 24px;
		border-radius: 12px;
		border: 1px solid var(--border2);
		background: var(--panel3);
		cursor: pointer;
		padding: 0;
		position: relative;
		transition:
			background 0.15s ease,
			border-color 0.15s ease;
		flex: 0 0 auto;
	}

	.toggle.on {
		background: rgba(126, 231, 135, 0.22);
		border-color: var(--accent);
	}

	.toggle:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.toggle:focus-visible {
		outline: 1px solid var(--accent);
		outline-offset: 1px;
	}

	.knob {
		position: absolute;
		top: 3px;
		left: 3px;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--muted);
		transition:
			left 0.15s ease,
			background 0.15s ease;
	}

	.toggle.on .knob {
		left: 19px;
		background: var(--accent);
	}
</style>
