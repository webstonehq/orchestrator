<!--
	CodeField — multi-line mono textarea that auto-grows with its content up
	to 240px, then scrolls.

	When `groups` is provided (template-capable code fields, e.g. a raw JSON
	body with widget=code + template=true), an "{ } insert" button row is
	rendered above the textarea; picking a ref inserts "{{ path }}" at the
	textarea caret (selectionStart/selectionEnd persist through the button
	click's blur) into the raw string. Without `groups` it is a plain code
	editor.

	Props:
	- value: string
	- placeholder?: string
	- groups?: PickerGroup[]  — enables the ref picker when provided
	- onChange: (value: string) => void
-->
<script lang="ts">
	import type { PickerGroup } from '../../picker';
	import ExprPicker from '../ExprPicker.svelte';

	let {
		value,
		placeholder = '',
		groups = undefined,
		onChange
	}: {
		value: string;
		placeholder?: string;
		groups?: PickerGroup[];
		onChange: (value: string) => void;
	} = $props();

	let el: HTMLTextAreaElement | undefined = $state();
	let pickerOpen = $state(false);

	const MAX_HEIGHT = 240;

	function autogrow() {
		if (!el) return;
		el.style.height = 'auto';
		el.style.height = Math.min(el.scrollHeight + 2, MAX_HEIGHT) + 'px';
	}

	$effect(() => {
		// Re-measure whenever the value changes (including external updates).
		void value;
		autogrow();
	});

	function onInput(e: Event) {
		onChange((e.currentTarget as HTMLTextAreaElement).value);
	}

	function insertRef(path: string) {
		const refText = `{{ ${path} }}`;
		const start = el?.selectionStart ?? value.length;
		const end = el?.selectionEnd ?? start;
		onChange(value.slice(0, start) + refText + value.slice(end));
		pickerOpen = false;
		const caret = start + refText.length;
		requestAnimationFrame(() => {
			el?.focus();
			el?.setSelectionRange(caret, caret);
		});
	}
</script>

<div class="code-field">
	{#if groups}
		<div class="insert-row">
			<!-- stopPropagation keeps the picker's outside-pointerdown handler from
			     closing it before this click toggles, which would re-open it. -->
			<button
				class="insert"
				type="button"
				onpointerdown={(e) => e.stopPropagation()}
				onclick={() => (pickerOpen = !pickerOpen)}
			>
				{'{ }'} insert
			</button>
			{#if pickerOpen}
				<ExprPicker {groups} onPick={insertRef} onClose={() => (pickerOpen = false)} />
			{/if}
		</div>
	{/if}
	<textarea bind:this={el} {value} {placeholder} spellcheck="false" oninput={onInput}></textarea>
</div>

<style>
	.code-field {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.insert-row {
		position: relative;
		display: flex;
		justify-content: flex-end;
	}

	.insert {
		height: 26px;
		padding: 0 9px;
		border-radius: 6px;
		border: 1px solid rgba(88, 166, 255, 0.35);
		background: rgba(88, 166, 255, 0.1);
		color: #79c0ff;
		font: 600 11px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 5px;
	}

	.insert:hover {
		background: rgba(88, 166, 255, 0.18);
	}

	textarea {
		width: 100%;
		box-sizing: border-box;
		min-height: 68px;
		max-height: 240px;
		resize: none;
		overflow-y: auto;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--bg2);
		padding: 9px 12px;
		font: 500 12px / 1.55 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	textarea:focus {
		border-color: var(--accent);
	}

	textarea::placeholder {
		color: var(--dim);
	}
</style>
