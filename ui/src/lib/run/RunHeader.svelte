<script lang="ts">
	import type { RunDetail } from '$lib/api';
	import { duration, formatNumber, relativeTime } from '$lib/format';
	import StatusPill from '$lib/components/StatusPill.svelte';
	import MetricCard from '$lib/components/MetricCard.svelte';
	import type { LiveItemAgg } from './sse';

	let {
		detail,
		itemsAgg,
		nowMs,
		onreplay,
		oncancel
	}: {
		detail: RunDetail;
		itemsAgg: Record<string, LiveItemAgg>;
		nowMs: number;
		onreplay: () => void;
		oncancel: () => void;
	} = $props();

	const run = $derived(detail.run);
	const active = $derived(run.status === 'running' || run.status === 'queued');

	const elapsed = $derived.by(() => {
		if (!run.started_at) return '—';
		if (run.finished_at) {
			const sec =
				run.duration_sec ??
				(new Date(run.finished_at).getTime() - new Date(run.started_at).getTime()) / 1000;
			return duration(sec);
		}
		return duration((nowMs - new Date(run.started_at).getTime()) / 1000);
	});

	const startedLine = $derived.by(() => {
		const parts = [`run #${run.id}`, `triggered by ${run.trigger}`];
		if (run.started_at) parts.push(`started ${relativeTime(run.started_at, new Date(nowMs))}`);
		else if (run.scheduled_for) parts.push(`scheduled ${relativeTime(run.scheduled_for)}`);
		else parts.push('not started');
		return parts.join(' · ');
	});

	interface Metric {
		label: string;
		value: string;
		sub?: string;
		color?: string;
	}

	const metrics: Metric[] = $derived.by(() => {
		const out: Metric[] = [
			{
				label: 'Progress',
				value: `${run.tasks_done} / ${run.tasks_total}`,
				sub: 'tasks complete'
			}
		];
		// One card per parallel fan-out (usually a single one).
		for (const [taskId, agg] of Object.entries(itemsAgg).slice(0, 2)) {
			if (agg.total === 0) continue;
			const done = agg.success + agg.failed + agg.dropped;
			out.push({
				label: taskId,
				value: `${formatNumber(done)} / ${formatNumber(agg.total)}`,
				sub:
					agg.throughput_per_sec > 0
						? `items · ~${Math.round(agg.throughput_per_sec)}/s`
						: 'items',
				color: agg.failed > 0 ? 'var(--amber)' : undefined
			});
		}
		const taskRetries = detail.tasks.reduce((n, t) => n + Math.max(0, t.attempt - 1), 0);
		const itemRetries = Object.values(itemsAgg).reduce((n, a) => n + a.retried, 0);
		out.push({
			label: 'Retries',
			value: String(taskRetries),
			sub: itemRetries > 0 ? `${formatNumber(itemRetries)} item retries` : 'task attempts > 1',
			color: taskRetries > 0 || itemRetries > 0 ? 'var(--amber)' : undefined
		});
		if (run.error) {
			out.push({ label: 'Error', value: 'failed', sub: run.error.slice(0, 80), color: 'var(--red)' });
		}
		return out.slice(0, 4);
	});
</script>

<div class="head">
	<div class="row">
		<a class="back" href="/runs" aria-label="Back to runs">
			<svg
				width="16"
				height="16"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<polyline points="15 18 9 12 15 6"></polyline>
			</svg>
		</a>
		<div class="ident">
			<div class="name-row">
				<span class="name">{run.flow_id}</span>
				<StatusPill status={run.status} />
			</div>
			<div class="sub">{startedLine}</div>
		</div>
		<div class="spacer"></div>
		<div class="elapsed">
			<div class="elapsed-val" class:live={run.status === 'running'}>{elapsed}</div>
			<div class="elapsed-label">{run.finished_at ? 'duration' : 'elapsed'}</div>
		</div>
		<button type="button" class="btn" onclick={onreplay}>
			<svg
				width="13"
				height="13"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<path d="M1 4v6h6M23 20v-6h-6"></path>
				<path d="M20.5 9A9 9 0 0 0 5 5.6L1 10m22 4l-4 4.4A9 9 0 0 1 3.5 15"></path>
			</svg>
			Replay
		</button>
		{#if active}
			<button type="button" class="btn cancel" onclick={oncancel}>Cancel run</button>
		{/if}
	</div>
	<div class="metrics">
		{#each metrics as m (m.label)}
			<MetricCard label={m.label} value={m.value} sub={m.sub ?? ''} valueColor={m.color} />
		{/each}
	</div>
</div>

<style>
	.head {
		padding: 15px 24px;
		border-bottom: 1px solid var(--border);
		flex: 0 0 auto;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 13px;
	}

	.back {
		width: 30px;
		height: 30px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--muted);
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
		text-decoration: none;
	}

	.back:hover {
		color: var(--text);
	}

	.ident {
		min-width: 0;
	}

	.name-row {
		display: flex;
		align-items: center;
		gap: 11px;
	}

	.name {
		font: 600 15px 'IBM Plex Mono', monospace;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.sub {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		margin-top: 3px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.spacer {
		flex: 1;
	}

	.elapsed {
		text-align: right;
		margin-right: 6px;
	}

	.elapsed-val {
		font: 600 20px 'IBM Plex Mono', monospace;
		letter-spacing: 0.5px;
		color: var(--text);
	}

	.elapsed-val.live {
		color: var(--cyan);
	}

	.elapsed-label {
		font-size: 10px;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
	}

	.btn {
		height: 32px;
		padding: 0 13px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--muted);
		font: 600 12px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
		flex: 0 0 auto;
	}

	.btn:hover {
		color: var(--text);
	}

	.btn.cancel {
		border-color: #5a2b2b;
		background: rgba(248, 81, 73, 0.1);
		color: var(--red);
	}

	.btn.cancel:hover {
		color: var(--red);
		border-color: var(--red);
	}

	.metrics {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 10px;
		margin-top: 14px;
	}
</style>
