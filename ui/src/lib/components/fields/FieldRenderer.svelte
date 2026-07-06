<!--
	FieldRenderer — renders one plugin-manifest field from its FieldSpec:
	label row (label + required asterisk), the widget, and help text.

	Widget map:
	- "text"     -> TextField, or TemplateInput when spec.template is true
	- "template" -> TemplateInput
	- "select"   -> SelectChip (spec.options)
	- "number"   -> NumberStepper (spec.min / spec.max)
	- "duration" -> DurationInput (seconds; spec.min / spec.max)
	- "toggle"   -> ToggleField
	- "code"     -> CodeField; when spec.template is true it gets the picker
	               groups and shows an "{ } insert" affordance that inserts a
	               ref at the caret in the raw string
	- "keyvalue" -> KeyValueRows (value: { key, value }[]; values are templates)
	- anything else falls back to TextField

	template=true is honored for the text, keyvalue, and code widgets
	(keyvalue values are always template-capable). Other widgets ignore it —
	their values are structured (number/boolean/option), not template strings.

	Props:
	- field: FieldSpec
	- value: unknown            — current config value ("default" used when undefined)
	- groups: PickerGroup[]     — passed through to template-capable widgets
	- onChange: (value: unknown) => void
-->
<script lang="ts" module>
	export interface FieldSpec {
		key: string;
		label: string;
		widget: string;
		required?: boolean;
		default?: unknown;
		help?: string;
		options?: string[];
		min?: number;
		max?: number;
		/** For "text": treat the value as a template string (chip editor). */
		template?: boolean;
		/** Optional per-option accent colors for "select" (e.g. GET/POST). */
		colorMap?: Record<string, string>;
	}
</script>

<script lang="ts">
	import type { PickerGroup } from '../../picker';
	import TemplateInput from './TemplateInput.svelte';
	import KeyValueRows from './KeyValueRows.svelte';
	import SelectChip from './SelectChip.svelte';
	import NumberStepper from './NumberStepper.svelte';
	import DurationInput from './DurationInput.svelte';
	import ToggleField from './ToggleField.svelte';
	import TextField from './TextField.svelte';
	import CodeField from './CodeField.svelte';

	let {
		field,
		value,
		groups,
		onChange
	}: {
		field: FieldSpec;
		value: unknown;
		groups: PickerGroup[];
		onChange: (value: unknown) => void;
	} = $props();

	const current = $derived(value === undefined ? field.default : value);

	const asString = $derived(typeof current === 'string' ? current : (current ?? '').toString());
	const asNumber = $derived(typeof current === 'number' ? current : Number(current ?? 0) || 0);
	const asBool = $derived(current === true);
	const asRows = $derived(
		Array.isArray(current) ? (current as { key: string; value: string }[]) : []
	);
	const isTemplate = $derived(field.widget === 'template' || field.template === true);
</script>

<div class="field">
	<div class="label-row">
		<span class="label">{field.label}</span>
		{#if field.required}
			<span class="required" title="Required">*</span>
		{/if}
	</div>

	{#if field.widget === 'keyvalue'}
		<KeyValueRows rows={asRows} {groups} onChange={(rows) => onChange(rows)} />
	{:else if field.widget === 'select'}
		<SelectChip
			value={asString}
			options={field.options ?? []}
			colorMap={field.colorMap}
			onChange={(v) => onChange(v)}
		/>
	{:else if field.widget === 'number'}
		<NumberStepper value={asNumber} min={field.min} max={field.max} onChange={(v) => onChange(v)} />
	{:else if field.widget === 'duration'}
		<DurationInput value={asNumber} min={field.min} max={field.max} onChange={(v) => onChange(v)} />
	{:else if field.widget === 'toggle'}
		<ToggleField checked={asBool} onChange={(v) => onChange(v)} />
	{:else if field.widget === 'code'}
		<CodeField
			value={asString}
			groups={field.template === true ? groups : undefined}
			onChange={(v) => onChange(v)}
		/>
	{:else if isTemplate}
		<TemplateInput value={asString} {groups} onChange={(v) => onChange(v)} />
	{:else}
		<TextField value={asString} onChange={(v) => onChange(v)} />
	{/if}

	{#if field.help}
		<div class="help">{field.help}</div>
	{/if}
</div>

<style>
	.field {
		display: flex;
		flex-direction: column;
		gap: 7px;
	}

	.label-row {
		display: flex;
		align-items: baseline;
		gap: 4px;
	}

	.label {
		font-weight: 600;
		font-size: 12px;
		color: var(--text);
	}

	.required {
		color: var(--amber);
		font: 600 12px 'IBM Plex Mono', monospace;
	}

	.help {
		font-size: 11px;
		color: var(--dim);
		line-height: 1.5;
	}
</style>
