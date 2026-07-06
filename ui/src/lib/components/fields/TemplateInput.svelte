<!--
	TemplateInput — chip editor for template strings ("{{ inputs.x }}" refs
	interleaved with literal text).

	Value model: the template STRING is the single source of truth. It is
	parsed into tokens for display; edits serialize back through onChange.

	v1 simplification (matches the mock's cSetLiteral/_insertRef behavior):
	instead of one editable text gap per interleaved position, the editor
	renders all ref tokens as chips followed by ONE literal text input that
	represents the concatenation of all text segments. This is lossy for
	DISPLAY from the very first render, not just after an edit: a multi-gap
	template like "https://{{ vars.host }}:{{ vars.port }}/x" shows as
	[vars.host][vars.port] chips + the single literal "https://:/x", which no
	longer conveys where the refs sit. (The underlying value string is only
	rewritten once an edit is made: editing the literal replaces ALL text
	tokens with a single trailing text token; inserting a ref places it
	before the first text token, or appends when there is none.) F2 should
	prefer single-ref-per-field authoring or accept this limitation; faithful
	per-gap editing is deferred to a later revision.

	Hand-typed refs: if the literal contains "{{", the resulting template is
	re-parsed; when valid the ref is absorbed into chips, when invalid the
	whole value is kept as literal text and the editor shows an amber border
	with the TemplateError message as a title tooltip.

	Props:
	- value: string                       — template string
	- groups: PickerGroup[]               — picker groups for "{ } insert"
	- placeholder?: string
	- onChange: (newTemplate: string) => void
-->
<script lang="ts">
	import { parse, serialize, refDisplay, TemplateError, type Token } from '../../template';
	import type { PickerGroup } from '../../picker';
	import ExprPicker from '../ExprPicker.svelte';

	let {
		value,
		groups,
		placeholder = 'value…',
		onChange
	}: {
		value: string;
		groups: PickerGroup[];
		placeholder?: string;
		onChange: (newTemplate: string) => void;
	} = $props();

	let pickerOpen = $state(false);

	const parsed = $derived.by((): { tokens: Token[]; error: TemplateError | null } => {
		try {
			return { tokens: parse(value), error: null };
		} catch (e) {
			if (e instanceof TemplateError) {
				return { tokens: value ? [{ kind: 'text', value }] : [], error: e };
			}
			throw e;
		}
	});

	const refs = $derived(parsed.tokens.filter((t) => t.kind === 'ref'));

	// Stable-ish synthetic keys for the chip list: canonical display text
	// plus an occurrence counter for duplicates, so chips keep identity as
	// long as the ref list is unchanged (an index-only key would remount
	// every chip after a deletion shifts positions).
	const refKeys = $derived.by(() => {
		const counts = new Map<string, number>();
		return refs.map((r) => {
			const d = refDisplay(r);
			const n = counts.get(d) ?? 0;
			counts.set(d, n + 1);
			return `${d}#${n}`;
		});
	});
	const literal = $derived(
		parsed.tokens
			.filter((t) => t.kind === 'text')
			.map((t) => t.value)
			.join('')
	);

	function emit(tokens: Token[]) {
		onChange(serialize(tokens));
	}

	function setLiteral(newLiteral: string) {
		// Replace all text tokens with one trailing text token. The raw string
		// (refs serialized canonically + literal) is emitted; if the literal
		// contains a valid "{{ ... }}" it is absorbed into chips on re-parse,
		// if invalid the error state renders instead.
		const next: Token[] = [...refs];
		if (newLiteral !== '') next.push({ kind: 'text', value: newLiteral });
		emit(next);
	}

	function deleteRef(refIndex: number) {
		let seen = -1;
		const next = parsed.tokens.filter((t) => {
			if (t.kind !== 'ref') return true;
			seen += 1;
			return seen !== refIndex;
		});
		emit(next);
	}

	function insertRef(path: string) {
		// Insert before the first text token; append when there is none.
		const next = [...parsed.tokens];
		const token: Token = { kind: 'ref', path, filters: [] };
		const textIndex = next.findIndex((t) => t.kind === 'text');
		if (textIndex === -1) next.push(token);
		else next.splice(textIndex, 0, token);
		emit(next);
		pickerOpen = false;
	}

	function onLiteralInput(e: Event) {
		setLiteral((e.currentTarget as HTMLInputElement).value);
	}
</script>

<div class="wrap">
	<div
		class="box"
		class:invalid={parsed.error !== null}
		title={parsed.error ? parsed.error.message : undefined}
	>
		{#each refs as ref, i (refKeys[i])}
			<span class="chip">
				{refDisplay(ref)}
				<button
					class="chip-del"
					type="button"
					aria-label="Remove reference {ref.path}"
					onclick={() => deleteRef(i)}>×</button
				>
			</span>
		{/each}
		<input
			class="literal"
			value={literal}
			{placeholder}
			spellcheck="false"
			oninput={onLiteralInput}
		/>
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
	</div>
	{#if pickerOpen}
		<ExprPicker {groups} onPick={insertRef} onClose={() => (pickerOpen = false)} />
	{/if}
</div>

<style>
	.wrap {
		position: relative;
		flex: 1;
		min-width: 0;
	}

	.box {
		min-height: 38px;
		border: 1px solid var(--border2);
		border-radius: 8px;
		background: var(--bg2);
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 6px 8px;
		flex-wrap: wrap;
	}

	.box:focus-within {
		border-color: var(--accent);
	}

	.box.invalid,
	.box.invalid:focus-within {
		border-color: var(--amber);
	}

	.chip {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font: 500 11px 'IBM Plex Mono', monospace;
		color: #79c0ff;
		background: rgba(88, 166, 255, 0.12);
		border: 1px solid rgba(88, 166, 255, 0.32);
		border-radius: 5px;
		padding: 2px 4px 2px 7px;
		white-space: nowrap;
	}

	.chip-del {
		cursor: pointer;
		opacity: 0.6;
		font-size: 13px;
		line-height: 1;
		padding: 0 1px;
		border: none;
		background: transparent;
		color: inherit;
	}

	.chip-del:hover {
		opacity: 1;
	}

	.literal {
		flex: 1;
		min-width: 70px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: #f0a878;
		background: transparent;
		border: none;
		outline: none;
		padding: 2px 0;
	}

	.literal::placeholder {
		color: var(--dim);
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
		flex: 0 0 auto;
	}

	.insert:hover {
		background: rgba(88, 166, 255, 0.18);
	}
</style>
