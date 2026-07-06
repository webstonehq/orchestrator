<script lang="ts">
	let {
		checked = $bindable(false),
		onchange,
		label,
		disabled = false
	}: {
		checked?: boolean;
		onchange?: (checked: boolean) => void;
		label?: string;
		disabled?: boolean;
	} = $props();

	function toggle() {
		if (disabled) return;
		checked = !checked;
		onchange?.(checked);
	}
</script>

<button
	type="button"
	class="track"
	class:on={checked}
	role="switch"
	aria-checked={checked}
	aria-label={label}
	{disabled}
	onclick={toggle}
>
	<span class="knob"></span>
</button>

<style>
	.track {
		width: 38px;
		height: 22px;
		padding: 0;
		border-radius: 999px;
		cursor: pointer;
		flex: 0 0 auto;
		position: relative;
		background: var(--panel3);
		border: 1px solid var(--border2);
		transition: all 0.15s;
	}

	.track.on {
		background: var(--accent);
		border-color: var(--accent);
	}

	.track:disabled {
		cursor: default;
		opacity: 0.5;
	}

	.knob {
		position: absolute;
		top: 2px;
		left: 2px;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--dim);
		transition: all 0.15s;
	}

	.track.on .knob {
		left: 18px;
		background: #08110a;
	}
</style>
