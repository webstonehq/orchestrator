<!--
	KeyValueRows — editable key/value parameter rows. Keys are plain mono
	text inputs; values are TemplateInput chip editors.

	Props:
	- rows: { key: string; value: string }[]
	- groups: PickerGroup[]                — passed through to TemplateInput
	- onChange: (rows: { key: string; value: string }[]) => void
	- emptyText?: string                   — line shown when rows is empty
	- addLabel?: string
-->
<script lang="ts">
	import type { PickerGroup } from '../../picker';
	import TemplateInput from './TemplateInput.svelte';

	let {
		rows,
		groups,
		onChange,
		emptyText = 'No parameters — sends an empty request.',
		addLabel = '+ parameter'
	}: {
		rows: { key: string; value: string }[];
		groups: PickerGroup[];
		onChange: (rows: { key: string; value: string }[]) => void;
		emptyText?: string;
		addLabel?: string;
	} = $props();

	// Stable synthetic ids for #each keys: an index key would remount every
	// row below a deletion, and rows carry no natural unique key (keys can be
	// empty or duplicated while typing). Ids live in a WeakMap keyed by row
	// object identity; edits transfer the old row's id to its replacement so
	// the row's inputs keep focus across keystrokes.
	let nextId = 0;
	const rowIds = new WeakMap<object, number>();

	function idOf(row: object): number {
		let id = rowIds.get(row);
		if (id === undefined) {
			id = nextId++;
			rowIds.set(row, id);
		}
		return id;
	}

	function replaceRow(index: number, patch: Partial<{ key: string; value: string }>) {
		onChange(
			rows.map((r, i) => {
				if (i !== index) return r;
				const next = { ...r, ...patch };
				rowIds.set(next, idOf(r));
				return next;
			})
		);
	}

	function setKey(index: number, key: string) {
		replaceRow(index, { key });
	}

	function setValue(index: number, value: string) {
		replaceRow(index, { value });
	}

	function removeRow(index: number) {
		onChange(rows.filter((_, i) => i !== index));
	}

	function addRow() {
		onChange([...rows, { key: '', value: '' }]);
	}
</script>

<div class="kv">
	<div class="toolbar">
		<button class="add" type="button" onclick={addRow}>{addLabel}</button>
	</div>
	{#if rows.length === 0}
		<div class="empty">{emptyText}</div>
	{:else}
		{#each rows as row, i (idOf(row))}
			<div class="row">
				<input
					class="key"
					value={row.key}
					placeholder="key"
					spellcheck="false"
					oninput={(e) => setKey(i, e.currentTarget.value)}
				/>
				<TemplateInput value={row.value} {groups} onChange={(v) => setValue(i, v)} />
				<button
					class="del"
					type="button"
					aria-label={row.key ? `Remove row ${row.key}` : 'Remove row'}
					onclick={() => removeRow(i)}
				>
					<svg
						width="12"
						height="12"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
					>
						<line x1="6" y1="6" x2="18" y2="18"></line>
						<line x1="18" y1="6" x2="6" y2="18"></line>
					</svg>
				</button>
			</div>
		{/each}
	{/if}
</div>

<style>
	.kv {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.toolbar {
		display: flex;
		justify-content: flex-end;
	}

	.add {
		height: 26px;
		padding: 0 10px;
		border-radius: 7px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--muted);
		font: 600 11px 'IBM Plex Mono', monospace;
		cursor: pointer;
	}

	.add:hover {
		color: var(--text);
		border-color: var(--dim);
	}

	.empty {
		font: 400 11.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		padding: 4px 2px;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.key {
		flex: 0 0 130px;
		font: 600 12px 'IBM Plex Mono', monospace;
		color: #79c0ff;
		background: transparent;
		border: none;
		border-bottom: 1px dashed var(--border2);
		outline: none;
		padding: 0 0 2px;
	}

	.key:focus {
		border-bottom-color: var(--accent);
	}

	.key::placeholder {
		color: var(--dim);
	}

	.del {
		width: 26px;
		height: 26px;
		border-radius: 7px;
		border: 1px solid var(--border2);
		background: var(--panel2);
		color: var(--dim);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
	}

	.del:hover {
		color: var(--red);
		border-color: var(--red);
	}
</style>
