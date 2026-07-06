<!--
	InspectorTrigger — schedule trigger editor. No trigger: an "Add schedule
	trigger" CTA. Present: cron (with a display-only human preview — the
	server's humanizer in src/api/flows.rs is authoritative on the Schedules
	screen), IANA timezone select, catch-up policy chip with semantics help,
	enabled toggle, and a remove button. v1 edits the first trigger.
-->
<script lang="ts">
	import type { CatchupPolicy } from '../api';
	import type { BuilderStore } from './state.svelte';
	import { humanizeCron, issuesUnder, timezoneList } from './defs';
	import SelectChip from '../components/fields/SelectChip.svelte';
	import ToggleField from '../components/fields/ToggleField.svelte';
	import IssueList from './IssueList.svelte';

	let { store }: { store: BuilderStore } = $props();

	const trigger = $derived(store.def.triggers[0]);
	const timezones = timezoneList();

	const CATCHUP_HELP: Record<CatchupPolicy, string> = {
		none: 'Fires missed while the server was down are skipped — the schedule resumes at the next future occurrence. Live fires always queue as they occur.',
		latest:
			'One make-up run for the most recent missed fire, then the schedule resumes. Live fires always queue as they occur.',
		all: 'One make-up run per missed fire (capped at 100), then the schedule resumes. Live fires always queue as they occur.'
	};

	const inline = /^triggers\[0\]\.(cron|timezone|id)$/;
	const unmatched = $derived(
		issuesUnder(store.issues, 'triggers').filter((i) => !inline.test(i.path))
	);

	const cronPreview = $derived(trigger ? humanizeCron(trigger.cron) : '');
</script>

<div class="panel">
	{#if !trigger}
		<h3>Trigger</h3>
		<p class="hint">Queue runs automatically on a cron cadence.</p>
		<button class="cta" type="button" onclick={() => store.addTrigger()}>
			<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
				<circle cx="12" cy="12" r="8"></circle>
				<path d="M12 8v4l2.6 2"></path>
			</svg>
			Add schedule trigger
		</button>
	{:else}
		<div class="head">
			<h3>Schedule trigger</h3>
			<button class="remove" type="button" onclick={() => store.removeTrigger()}>
				<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<polyline points="3 6 5 6 21 6"></polyline>
					<path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6m5 0V4a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1v2"></path>
				</svg>
				Remove trigger
			</button>
		</div>
		<p class="hint">Queues a run automatically on a cron cadence.</p>

		<IssueList issues={unmatched} />

		<div class="grid">
			<div class="cell">
				<div class="label">Id</div>
				<input
					class="mono-input"
					value={trigger.id}
					spellcheck="false"
					aria-label="Trigger id"
					oninput={(e) => (trigger.id = e.currentTarget.value)}
				/>
				{#if store.errorAt('triggers[0].id')}
					<div class="err">{store.errorAt('triggers[0].id')}</div>
				{/if}
			</div>
			<div class="cell wide">
				<div class="label">Cron</div>
				<input
					class="mono-input cron"
					value={trigger.cron}
					spellcheck="false"
					placeholder="0 6 * * *"
					aria-label="Cron expression"
					oninput={(e) => (trigger.cron = e.currentTarget.value)}
				/>
				<div class="preview" title="Display-only preview — the server's humanizer is authoritative">
					{cronPreview === trigger.cron ? '5-field cron · minute hour day month weekday' : cronPreview}
				</div>
				{#if store.errorAt('triggers[0].cron')}
					<div class="err">{store.errorAt('triggers[0].cron')}</div>
				{/if}
			</div>
			<div class="cell wide">
				<div class="label">Timezone</div>
				<select
					class="tz"
					value={trigger.timezone ?? 'UTC'}
					aria-label="Timezone"
					onchange={(e) => (trigger.timezone = e.currentTarget.value)}
				>
					{#each timezones as tz (tz)}
						<option value={tz}>{tz}</option>
					{/each}
					{#if trigger.timezone && !timezones.includes(trigger.timezone)}
						<option value={trigger.timezone}>{trigger.timezone}</option>
					{/if}
				</select>
				{#if store.errorAt('triggers[0].timezone')}
					<div class="err">{store.errorAt('triggers[0].timezone')}</div>
				{/if}
			</div>
		</div>

		<div class="grid">
			<div class="cell">
				<div class="label">Catch-up</div>
				<SelectChip
					value={trigger.catchup ?? 'latest'}
					options={['none', 'latest', 'all']}
					onChange={(v) => (trigger.catchup = v as CatchupPolicy)}
				/>
				<div class="help">{CATCHUP_HELP[trigger.catchup ?? 'latest']}</div>
			</div>
			<div class="cell">
				<div class="label">Enabled</div>
				<ToggleField
					checked={trigger.enabled ?? true}
					onChange={(v) => (trigger.enabled = v)}
				/>
				<div class="help">
					{(trigger.enabled ?? true)
						? 'this trigger queues runs on schedule'
						: 'paused — no scheduled runs from this trigger'}
				</div>
			</div>
		</div>
	{/if}
</div>

<style>
	.panel {
		padding: 18px 22px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 12px;
	}

	h3 {
		margin: 0 0 5px;
		font: 600 14px 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--text);
	}

	.hint {
		margin: 0 0 16px;
		font-size: 12px;
		color: var(--muted);
		line-height: 1.5;
	}

	.cta {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 8px;
		width: 100%;
		border: 1.5px dashed var(--border2);
		border-radius: 11px;
		padding: 18px;
		color: var(--dim);
		cursor: pointer;
		font: 600 12px 'IBM Plex Mono', monospace;
		background: transparent;
	}

	.cta:hover {
		border-color: var(--accent);
		color: var(--accent);
	}

	.remove {
		margin-left: auto;
		height: 30px;
		padding: 0 11px;
		border-radius: 8px;
		border: 1px solid #5a2b2b;
		background: rgba(248, 81, 73, 0.08);
		color: var(--red);
		font: 600 11.5px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.grid {
		display: flex;
		gap: 12px;
		flex-wrap: wrap;
		margin-bottom: 14px;
	}

	.cell {
		flex: 0 1 200px;
		min-width: 160px;
	}

	.cell.wide {
		flex: 1 1 240px;
	}

	.label {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
		margin-bottom: 6px;
	}

	.mono-input {
		width: 100%;
		height: 36px;
		border: 1px solid var(--border2);
		border-radius: 8px;
		background: var(--bg2);
		padding: 0 12px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	.mono-input:focus {
		border-color: var(--accent);
	}

	.cron {
		font-weight: 600;
		color: var(--accent);
	}

	.preview {
		margin-top: 6px;
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--cyan);
	}

	.tz {
		width: 100%;
		height: 36px;
		border: 1px solid var(--border2);
		border-radius: 8px;
		background: var(--bg2);
		padding: 0 8px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	.tz:focus {
		border-color: var(--accent);
	}

	.help {
		margin-top: 8px;
		font-size: 11px;
		color: var(--dim);
		line-height: 1.5;
		max-width: 320px;
	}

	.err {
		margin-top: 6px;
		font-size: 11px;
		color: var(--red);
	}
</style>
