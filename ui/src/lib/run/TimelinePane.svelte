<script lang="ts">
	import type { RunSummary, TaskRunView } from '$lib/api';
	import { STATUS, type Status } from '$lib/status';
	import { duration, formatNumber } from '$lib/format';
	import type { LiveItemAgg } from './sse';
	import { barGeometry, timelineTicks } from './timeline';

	let {
		run,
		tasks,
		itemsAgg,
		nowMs
	}: {
		run: RunSummary;
		tasks: TaskRunView[];
		itemsAgg: Record<string, LiveItemAgg>;
		nowMs: number;
	} = $props();

	const runStartMs = $derived(run.started_at ? new Date(run.started_at).getTime() : NaN);
	const runEndMs = $derived(run.finished_at ? new Date(run.finished_at).getTime() : nowMs);
	const spanSec = $derived(
		Number.isNaN(runStartMs) ? 0 : Math.max((runEndMs - runStartMs) / 1000, 1)
	);
	const axis = $derived(timelineTicks(spanSec));

	interface Row {
		label: string;
		status: Status;
		par: boolean;
		leftPct: number;
		widthPct: number;
		caption: string;
		running: boolean;
	}

	const rows: Row[] = $derived.by(() => {
		if (Number.isNaN(runStartMs)) return [];
		return tasks.map((t) => {
			const par = t.task_id in itemsAgg;
			const geo = barGeometry(t.started_at, t.finished_at, runStartMs, axis.axisSec, nowMs);
			const running = t.status === 'running';
			let caption = '';
			if (geo) {
				const dur = t.started_at
					? duration(
							((t.finished_at ? new Date(t.finished_at).getTime() : nowMs) -
								new Date(t.started_at).getTime()) /
								1000
						)
					: '';
				const agg = itemsAgg[t.task_id];
				caption =
					par && agg && agg.total > 0
						? `${formatNumber(agg.total)} items · ${dur}${running ? ' …' : ''}`
						: `${dur}${running ? ' …' : ''}`;
			}
			return {
				label: t.task_id,
				status: t.status as Status,
				par,
				leftPct: geo?.leftPct ?? 0,
				widthPct: geo?.widthPct ?? 0,
				caption,
				running
			};
		});
	});

	function barColor(row: Row): string {
		if (row.par && (row.running || row.status === 'running')) return 'var(--cyan)';
		if (row.par && row.status === 'success') return 'var(--cyan)';
		return STATUS[row.status]?.color ?? 'var(--dim)';
	}
</script>

<div class="pane">
	{#if Number.isNaN(runStartMs)}
		<div class="idle">run has not started yet</div>
	{:else}
		<div class="axis">
			{#each axis.ticks as tick (tick.sec)}
				<span>{tick.label}</span>
			{/each}
		</div>
		{#each rows as row (row.label)}
			<div class="trow">
				<div class="label">{row.label}{row.par ? ' · fan-out' : ''}</div>
				<div class="track">
					{#if row.widthPct > 0}
						<div
							class="bar"
							class:running={row.running}
							class:cyan={barColor(row) === 'var(--cyan)'}
							style:left="{row.leftPct}%"
							style:width="{row.widthPct}%"
							style:background={barColor(row)}
						>
							{#if row.caption}<span class="cap">{row.caption}</span>{/if}
						</div>
					{:else}
						<div class="pending-dash">{row.status}</div>
					{/if}
				</div>
			</div>
		{/each}
	{/if}
</div>

<style>
	@keyframes barGrow {
		from {
			width: 0;
		}
	}

	@keyframes barPulse {
		0%,
		100% {
			filter: brightness(1);
		}
		50% {
			filter: brightness(1.35);
		}
	}

	.pane {
		flex: 1;
		overflow: auto;
		padding: 16px 20px;
	}

	.axis {
		display: flex;
		justify-content: space-between;
		font: 500 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		margin-bottom: 10px;
		padding-left: 150px;
	}

	.trow {
		display: flex;
		align-items: center;
		height: 30px;
		gap: 10px;
	}

	.label {
		flex: 0 0 140px;
		font: 500 11.5px 'IBM Plex Mono', monospace;
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.track {
		flex: 1;
		position: relative;
		height: 18px;
	}

	.bar {
		position: absolute;
		top: 0;
		height: 100%;
		border-radius: 5px;
		display: flex;
		align-items: center;
		overflow: hidden;
		animation: barGrow 0.5s ease-out;
	}

	.bar.cyan {
		box-shadow: 0 0 12px -2px var(--cyan);
	}

	.bar.running {
		animation:
			barGrow 0.5s ease-out,
			barPulse 1.6s ease-in-out infinite;
	}

	.cap {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: #08110a;
		padding: 0 6px;
		white-space: nowrap;
	}

	.pending-dash {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		font: 500 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		border-top: 1px dashed var(--border2);
		top: 50%;
		padding-left: 4px;
	}

	.idle {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--dim);
		padding: 20px 4px;
	}
</style>
