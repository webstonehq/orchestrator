<script lang="ts">
	import { api, ApiError, type FlowSummary } from '$lib/api';
	import { breadcrumb } from '$lib/breadcrumb';
	import { dashboardStore } from '$lib/dashboard';
	import { duration, formatNumber, formatPercent, relativeTime } from '$lib/format';
	import { toast } from '$lib/toast';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import MetricCard from '$lib/components/MetricCard.svelte';
	import StatusDot from '$lib/components/StatusDot.svelte';
	import StatusPill from '$lib/components/StatusPill.svelte';

	breadcrumb.set(['flows']);

	const POLL_MS = 5000;

	let flows: FlowSummary[] | null = $state(null);
	let loadError: string | null = $state(null);
	let retryNonce = $state(0);

	// Poll the flows list every 5s while mounted (dashboard metrics come from
	// dashboardStore, which polls on its own 5s cadence while subscribed).
	$effect(() => {
		void retryNonce;
		let disposed = false;

		const load = async () => {
			try {
				const data = await api.flows.list();
				if (disposed) return;
				flows = data;
				loadError = null;
			} catch (err) {
				if (disposed) return;
				const message = err instanceof ApiError ? err.message : 'Failed to load flows';
				if (loadError === null) toast.error(`Flows: ${message}`);
				loadError = message;
			}
		};

		void load();
		const timer = setInterval(() => {
			if (!document.hidden) void load();
		}, POLL_MS);

		return () => {
			disposed = true;
			clearInterval(timer);
		};
	});

	const dash = $derived($dashboardStore.data);
	const dashError = $derived($dashboardStore.error);
	const runsSub = $derived.by(() => {
		if (!dash) return '';
		const r = dash.runs_24h;
		// Surface degraded only when present, to keep the common case terse.
		const parts = [`${r.ok} ok`];
		if (r.degraded > 0) parts.push(`${r.degraded} degraded`);
		parts.push(`${r.failed} failed`, `${r.running} running`);
		return parts.join(' · ');
	});

	function rateColor(rate: number): string {
		if (rate >= 0.9) return 'var(--green)';
		if (rate >= 0.8) return 'var(--amber)';
		return 'var(--red)';
	}

	function lastRunAgo(f: FlowSummary): string {
		if (!f.last_run) return '';
		if (f.last_run.status === 'running') return 'running now';
		return f.last_run.finished_at ? relativeTime(f.last_run.finished_at) : '—';
	}
</script>

<svelte:head>
	<title>Flows · Orchestrator</title>
</svelte:head>

<div class="page">
	<div class="head">
		<div>
			<h1 class="page-title">Flows</h1>
			<p class="page-desc">Orchestrated pipelines and their recent activity.</p>
		</div>
		<div class="head-actions">
			<a class="btn-secondary" href="/flows/new?import=1">
				<svg
					width="14"
					height="14"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
					<polyline points="17 8 12 3 7 8"></polyline>
					<line x1="12" y1="3" x2="12" y2="15"></line>
				</svg>
				Import YAML
			</a>
			<a class="btn-accent" href="/flows/new">
				<svg
					width="15"
					height="15"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2.4"
					stroke-linecap="round"
				>
					<line x1="12" y1="5" x2="12" y2="19"></line>
					<line x1="5" y1="12" x2="19" y2="12"></line>
				</svg>
				New flow
			</a>
		</div>
	</div>

	<div class="metrics">
		<MetricCard label="Active flows" value={dash ? formatNumber(dash.active_flows) : '—'} />
		<MetricCard
			label="Runs · 24h"
			value={dash ? formatNumber(dash.runs_24h.total) : '—'}
			sub={runsSub}
		/>
		<MetricCard
			label="Success rate · 30d"
			value={dash?.success_rate_30d != null ? formatPercent(dash.success_rate_30d) : '—'}
			valueColor={dash?.success_rate_30d != null && dash.success_rate_30d >= 0.9
				? 'var(--green)'
				: undefined}
		/>
		<MetricCard
			label="Avg duration"
			value={dash?.avg_duration_sec != null ? duration(dash.avg_duration_sec) : '—'}
		/>
		<MetricCard
			label="Next scheduled"
			value={dash?.next_scheduled ? relativeTime(dash.next_scheduled.at) : '—'}
			valueColor={dash?.next_scheduled ? 'var(--accent)' : undefined}
			sub={dash?.next_scheduled ? dash.next_scheduled.flow_id : ''}
		/>
	</div>

	{#if dashError}
		<div class="metrics-note" role="status">metrics unavailable — retrying</div>
	{/if}

	{#if loadError !== null && flows !== null}
		<div class="error-strip" role="alert">
			<span>{loadError}</span>
			<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
		</div>
	{/if}

	{#if flows === null && loadError !== null}
		<div class="error-box" role="alert">
			<div class="error-msg">{loadError}</div>
			<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
		</div>
	{:else if flows === null}
		<div class="table-grid" aria-hidden="true">
			<div class="thead">
				<div>Flow</div>
				<div>Schedule</div>
				<div>Last run</div>
				<div>Success (30d)</div>
				<div class="right">Avg duration</div>
			</div>
			{#each Array(4), i (i)}
				<div class="row skeleton-row">
					<div><span class="sk" style="width:62%"></span></div>
					<div><span class="sk" style="width:70%"></span></div>
					<div><span class="sk" style="width:55%"></span></div>
					<div><span class="sk" style="width:80%"></span></div>
					<div class="right"><span class="sk" style="width:40%"></span></div>
				</div>
			{/each}
		</div>
	{:else if flows.length === 0}
		<EmptyState title="No flows yet" hint="Create your first flow to start orchestrating.">
			{#snippet cta()}
				<a class="btn-accent" href="/flows/new">New flow</a>
			{/snippet}
		</EmptyState>
	{:else}
		<div class="table-grid" role="table" aria-label="Flows">
			<div class="thead" role="row">
				<div role="columnheader">Flow</div>
				<div role="columnheader">Schedule</div>
				<div role="columnheader">Last run</div>
				<div role="columnheader">Success (30d)</div>
				<div class="right" role="columnheader">Avg duration</div>
			</div>
			{#each flows as f (f.id)}
				<a class="row" href="/flows/{f.id}" role="row">
					<div class="cell-flow" role="cell">
						<div class="flow-name">
							<StatusDot status={f.last_run?.status ?? 'pending'} />
							<span class="flow-name-text">{f.name}</span>
						</div>
						<div class="flow-ns">{f.namespace}</div>
					</div>
					<div class="cell-mono" role="cell">{f.schedule_human ?? 'manual'}</div>
					<div role="cell">
						{#if f.last_run}
							<StatusPill status={f.last_run.status} />
							<div class="cell-sub">{lastRunAgo(f)}</div>
						{:else}
							<span class="dash">—</span>
						{/if}
					</div>
					<div class="cell-rate" role="cell">
						{#if f.success_rate_30d != null}
							<div class="bar-track">
								<div
									class="bar-fill"
									style="width:{Math.max(0, Math.min(1, f.success_rate_30d)) *
										100}%;background:{rateColor(f.success_rate_30d)}"
								></div>
							</div>
							<span class="bar-pct">{formatPercent(f.success_rate_30d)}</span>
						{:else}
							<span class="dash">—</span>
						{/if}
					</div>
					<div class="cell-mono right" role="cell">
						{f.avg_duration_sec != null ? duration(f.avg_duration_sec) : '—'}
					</div>
				</a>
			{/each}
		</div>
	{/if}
</div>

<style>
	.head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 20px;
		margin-bottom: 22px;
	}

	.head .page-desc {
		margin-bottom: 0;
	}

	.head-actions {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.btn-accent {
		height: 36px;
		padding: 0 16px;
		border-radius: 9px;
		border: 1px solid var(--accent);
		background: var(--accent);
		color: #08110a;
		font: 600 13px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 7px;
		box-shadow: 0 0 20px -6px var(--accent);
		text-decoration: none;
		white-space: nowrap;
	}

	/* Base .btn-secondary comes from theme.css; this screen uses the larger
	   sans variant. */
	.btn-secondary {
		height: 36px;
		padding: 0 13px;
		border-radius: 9px;
		font: 500 12.5px 'IBM Plex Sans', system-ui, sans-serif;
	}

	.metrics {
		display: grid;
		grid-template-columns: repeat(5, 1fr);
		gap: 12px;
		margin-bottom: 26px;
	}

	.metrics-note {
		margin: -16px 0 18px;
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	/* Columns + row height for the shared .table-grid scaffold (theme.css). */
	.table-grid .thead,
	.table-grid .row {
		grid-template-columns: 2.4fr 1.2fr 1.1fr 1fr 0.9fr;
	}

	.table-grid .row {
		padding: 14px 18px;
	}

	.cell-flow {
		min-width: 0;
	}

	.flow-name {
		font: 600 13px 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.flow-name-text {
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.flow-ns {
		font-size: 11px;
		color: var(--dim);
		margin-top: 3px;
		padding-left: 16px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.cell-sub {
		font-size: 11px;
		color: var(--dim);
		margin-top: 4px;
	}

	.cell-rate {
		display: flex;
		align-items: center;
		gap: 9px;
	}

	.bar-track {
		flex: 1;
		height: 5px;
		border-radius: 3px;
		background: var(--panel3);
		overflow: hidden;
	}

	.bar-fill {
		height: 100%;
	}

	.bar-pct {
		font: 600 12px 'IBM Plex Mono', monospace;
		color: var(--muted);
		min-width: 34px;
	}

	.table-grid .skeleton-row {
		padding: 18px;
	}

	.error-strip {
		padding: 8px 8px 8px 14px;
	}

	.error-strip .btn-secondary {
		height: 28px;
	}
</style>
