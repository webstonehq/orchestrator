<!--
	EnvelopePanel — the core task envelope shared by every plugin task:
	retry policy (none / exponential + knobs), per-attempt timeout, and
	on_error behavior. Mutates the task in place (deep $state).

	Props:
	- task: RegularTaskSpec
	- store: BuilderStore   — inline validation errors
	- pathBase: string      — e.g. "tasks[2]" / "tasks[1].tasks[0]"
-->
<script lang="ts">
	import type { RegularTaskSpec } from '../api';
	import type { BuilderStore } from './state.svelte';
	import SelectChip from '../components/fields/SelectChip.svelte';
	import NumberStepper from '../components/fields/NumberStepper.svelte';
	import DurationInput from '../components/fields/DurationInput.svelte';

	let {
		task,
		store,
		pathBase
	}: {
		task: RegularTaskSpec;
		store: BuilderStore;
		pathBase: string;
	} = $props();

	const retryMode = $derived(task.retry ? 'exponential' : 'none');

	function setRetryMode(mode: string) {
		if (mode === 'exponential') {
			task.retry = { type: 'exponential', max_attempts: 3, base_seconds: 5 };
		} else {
			delete task.retry;
		}
	}

	const retryError = $derived(
		store.errorAt(`${pathBase}.retry.type`) ??
			store.errorAt(`${pathBase}.retry.max_attempts`) ??
			store.errorAt(`${pathBase}.retry.base_seconds`)
	);
	const timeoutError = $derived(store.errorAt(`${pathBase}.timeout_seconds`));
</script>

<div class="section-label">Envelope</div>
<div class="grid">
	<div class="cell">
		<div class="cell-label">Retry</div>
		<div class="cell-row">
			<SelectChip value={retryMode} options={['none', 'exponential']} onChange={setRetryMode} />
			{#if task.retry}
				<div class="knob">
					<span class="knob-label">attempts</span>
					<NumberStepper
						value={task.retry.max_attempts}
						min={1}
						max={20}
						onChange={(v) => {
							if (task.retry) task.retry.max_attempts = v;
						}}
					/>
				</div>
				<div class="knob">
					<span class="knob-label">base</span>
					<DurationInput
						value={task.retry.base_seconds}
						min={1}
						max={3600}
						onChange={(v) => {
							if (task.retry) task.retry.base_seconds = v;
						}}
					/>
				</div>
			{/if}
		</div>
		{#if task.retry}
			<div class="cell-help">
				attempt n sleeps base × 2ⁿ⁻¹ before retrying (attempts include the first)
			</div>
		{/if}
		{#if retryError}<div class="err">{retryError}</div>{/if}
	</div>

	<div class="cell">
		<div class="cell-label">Timeout</div>
		<div class="cell-row">
			<DurationInput
				value={task.timeout_seconds ?? 60}
				min={1}
				max={3600}
				onChange={(v) => (task.timeout_seconds = v)}
			/>
		</div>
		<div class="cell-help">per attempt; engine default 60s when unset</div>
		{#if timeoutError}<div class="err">{timeoutError}</div>{/if}
	</div>

	<div class="cell">
		<div class="cell-label">On error</div>
		<div class="cell-row">
			<SelectChip
				value={task.on_error ?? 'fail'}
				options={['fail', 'continue']}
				colorMap={{ fail: '#f85149', continue: '#e3b341' }}
				onChange={(v) => (task.on_error = v as 'fail' | 'continue')}
			/>
		</div>
		<div class="cell-help">
			{task.on_error === 'continue'
				? 'record the failure and keep going (in a fan-out: the item is dropped)'
				: 'fail the whole run after retries are exhausted'}
		</div>
	</div>
</div>

<style>
	.section-label {
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
		margin-bottom: 9px;
	}

	.grid {
		display: flex;
		flex-wrap: wrap;
		gap: 12px;
		margin-bottom: 22px;
	}

	.cell {
		flex: 1 1 180px;
		min-width: 180px;
		border: 1px solid var(--border);
		border-radius: 10px;
		background: var(--panel);
		padding: 11px 13px;
	}

	.cell-label {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
		margin-bottom: 8px;
	}

	.cell-row {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
	}

	.knob {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.knob-label {
		font: 500 10.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.cell-help {
		margin-top: 8px;
		font-size: 11px;
		color: var(--dim);
		line-height: 1.5;
	}

	.err {
		margin-top: 7px;
		font-size: 11px;
		color: var(--red);
	}
</style>
