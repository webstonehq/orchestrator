<!--
	PluginTaskEditor — the body of a plugin task inspector: config fields
	rendered from the plugin manifest (template-aware, scoped picker groups),
	the core envelope (retry/timeout/on_error), and the outputs table.
	Shared by the top-level task inspector and parallel child steps.

	Props:
	- task: RegularTaskSpec
	- store: BuilderStore
	- pathBase: string          — error-path base, e.g. "tasks[2]"
	- groups: PickerGroup[]     — scoped by the caller (upstream / iteration)
	- showEnvelope?: boolean    — default true
-->
<script lang="ts">
	import type { RegularTaskSpec } from '../api';
	import type { PickerGroup } from '../picker';
	import type { BuilderStore } from './state.svelte';
	import { issuesUnder } from './defs';
	import FieldRenderer from '../components/fields/FieldRenderer.svelte';
	import EnvelopePanel from './EnvelopePanel.svelte';
	import OutputsTable from './OutputsTable.svelte';

	let {
		task,
		store,
		pathBase,
		groups,
		showEnvelope = true
	}: {
		task: RegularTaskSpec;
		store: BuilderStore;
		pathBase: string;
		groups: PickerGroup[];
		showEnvelope?: boolean;
	} = $props();

	const manifest = $derived(store.manifestFor(task.type));

	// Adapt the API manifest FieldSpec (nullable options/min/max) to the
	// renderer's local FieldSpec shape (undefined instead of null).
	const rendererFields = $derived(
		(manifest?.fields ?? []).map((f) => ({
			key: f.key,
			label: f.label,
			widget: f.widget,
			required: f.required,
			default: f.default,
			help: f.help,
			options: f.options ?? undefined,
			min: f.min ?? undefined,
			max: f.max ?? undefined,
			template: f.template
		}))
	);

	// Per-field prefix aggregation: everything under `config.<key>` —
	// including deep paths like `config.headers[0].value` — renders under
	// that field, with the relative sub-path as context. `issuesUnder` is
	// boundary-aware (`.` / `[`), so field `h1` never claims `h10.x`.
	function fieldError(key: string): string | undefined {
		const prefix = `${pathBase}.config.${key}`;
		const list = issuesUnder(store.issues, prefix);
		if (list.length === 0) return undefined;
		return list
			.map((i) => {
				const rel = i.path.slice(prefix.length).replace(/^\./, '');
				return rel === '' ? i.message : `${rel}: ${i.message}`;
			})
			.join('; ');
	}

	// Config issues no field claims (the exact `.config` path, or — with no
	// manifest — any config sub-path) render as a block below the fields.
	const configError = $derived.by(() => {
		const prefix = `${pathBase}.config`;
		const keys = new Set(rendererFields.map((f) => f.key));
		const list = issuesUnder(store.issues, prefix).filter((i) => {
			const rel = i.path.slice(prefix.length).replace(/^\./, '');
			const key = /^[^.[]*/.exec(rel)?.[0] ?? '';
			return key === '' || !keys.has(key);
		});
		if (list.length === 0) return undefined;
		return list
			.map((i) => {
				const rel = i.path.slice(prefix.length).replace(/^\./, '');
				return rel === '' ? i.message : `${rel}: ${i.message}`;
			})
			.join('; ');
	});
</script>

{#if manifest}
	<div class="fields">
		{#each rendererFields as field (field.key)}
			<div class="field-slot">
				<FieldRenderer
					{field}
					value={task.config[field.key]}
					{groups}
					onChange={(v) => (task.config[field.key] = v)}
				/>
				{#if fieldError(field.key)}
					<div class="err">{fieldError(field.key)}</div>
				{/if}
			</div>
		{/each}
	</div>
{:else}
	<div class="unknown">
		Unknown task type <span class="mono">{task.type}</span> — no plugin manifest; config is
		preserved as-is.
	</div>
{/if}
{#if configError}
	<div class="err block">{configError}</div>
{/if}

{#if showEnvelope}
	<EnvelopePanel {task} {store} {pathBase} />
{/if}

<OutputsTable {task} {store} {pathBase} />

<style>
	.fields {
		display: flex;
		flex-direction: column;
		gap: 16px;
		margin-bottom: 22px;
	}

	.field-slot {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.err {
		font-size: 11px;
		color: var(--red);
	}

	.err.block {
		margin-bottom: 14px;
	}

	.unknown {
		border: 1px dashed var(--border2);
		border-radius: 10px;
		padding: 13px 15px;
		font-size: 12px;
		color: var(--muted);
		margin-bottom: 22px;
		line-height: 1.5;
	}

	.mono {
		font: 600 11.5px 'IBM Plex Mono', monospace;
		color: var(--amber);
	}
</style>
