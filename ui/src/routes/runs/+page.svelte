<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { api, ApiError, type RunCounts, type RunListResponse } from '$lib/api';
	import { breadcrumb } from '$lib/breadcrumb';
	import { duration, formatNumber, relativeTime } from '$lib/format';
	import { toast } from '$lib/toast';
	import StatusPill from '$lib/components/StatusPill.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';

	breadcrumb.set(['runs']);

	const PER = 25;
	const ACTIVE_POLL_MS = 3000;
	const IDLE_POLL_MS = 10000;

	const CHIPS: ReadonlyArray<readonly [keyof RunCounts, string]> = [
		['all', 'All'],
		['running', 'Running'],
		['success', 'Success'],
		['failed', 'Failed'],
		['queued', 'Queued'],
		['canceled', 'Canceled']
	];

	let statusFilter: keyof RunCounts = $state('all');
	let pageNum = $state(1);
	let data: RunListResponse | null = $state(null);
	let loadError: string | null = $state(null);
	let retryNonce = $state(0);

	// ?flow=<id> arrives as a normal search param (e.g. /runs?flow=x) and is
	// reactive via page.url.
	const flowFilter = $derived(page.url.searchParams.get('flow'));

	// Reset to page 1 whenever the flow filter changes via navigation.
	$effect(() => {
		void flowFilter;
		pageNum = 1;
	});

	// Fetch + poll. Poll fast (3s) while any run on the current page is
	// running/queued, otherwise slow (10s). Restarts whenever a filter,
	// the page number, or the retry nonce changes.
	$effect(() => {
		const params = {
			flow: flowFilter ?? undefined,
			status: statusFilter === 'all' ? undefined : statusFilter,
			page: pageNum,
			per: PER
		};
		void retryNonce;

		let disposed = false;
		let timer: ReturnType<typeof setTimeout> | undefined;

		const tick = async () => {
			try {
				const res = await api.runs.list(params);
				if (disposed) return;
				data = res;
				loadError = null;
				const active = res.runs.some((r) => r.status === 'running' || r.status === 'queued');
				timer = setTimeout(tick, active ? ACTIVE_POLL_MS : IDLE_POLL_MS);
			} catch (err) {
				if (disposed) return;
				const message = err instanceof ApiError ? err.message : 'Failed to load runs';
				if (loadError === null) toast.error(`Runs: ${message}`);
				loadError = message;
				timer = setTimeout(tick, IDLE_POLL_MS);
			}
		};

		void tick();
		return () => {
			disposed = true;
			clearTimeout(timer);
		};
	});

	const maxPage = $derived.by(() => (data ? Math.max(1, Math.ceil(data.total / PER)) : 1));

	function selectChip(key: keyof RunCounts) {
		statusFilter = key;
		pageNum = 1;
	}

	function clearFlowFilter() {
		void goto('/runs');
	}

	function triggerColor(trigger: string): string {
		if (trigger.startsWith('schedule')) return 'var(--accent)';
		if (trigger.startsWith('api')) return 'var(--cyan)';
		return 'var(--muted)';
	}

	function absoluteTime(iso: string): string {
		const d = new Date(iso);
		return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
	}
</script>

<svelte:head>
	<title>Runs · Orchestrator</title>
</svelte:head>

<div class="page">
	<h1 class="page-title">Runs</h1>
	<p class="page-desc">Execution history across all flows.</p>

	<div class="chips">
		{#each CHIPS as [key, label] (key)}
			<button class="chip" class:on={statusFilter === key} onclick={() => selectChip(key)}>
				{label}
				<span class="chip-count" class:on={statusFilter === key}>
					{data ? formatNumber(data.counts[key]) : '–'}
				</span>
			</button>
		{/each}
		{#if flowFilter}
			<button
				class="chip flow-chip"
				title="Clear flow filter"
				aria-label="Clear flow filter {flowFilter}"
				onclick={clearFlowFilter}
			>
				flow: {flowFilter}
				<span class="flow-chip-x" aria-hidden="true">×</span>
			</button>
		{/if}
	</div>

	{#if data === null && loadError !== null}
		<div class="error-box" role="alert">
			<div class="error-msg">{loadError}</div>
			<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
		</div>
	{:else}
		{#if loadError !== null && data !== null}
			<div class="error-strip" role="alert">
				<span>{loadError}</span>
				<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
			</div>
		{/if}

		<div
			class="table-grid"
			role={data !== null && data.runs.length > 0 ? 'table' : undefined}
			aria-label="Runs"
		>
			<div class="thead" role="row">
				<div role="columnheader">Status</div>
				<div role="columnheader">Flow · Run</div>
				<div role="columnheader">Trigger</div>
				<div role="columnheader">Started</div>
				<div role="columnheader">Duration</div>
				<div class="right" role="columnheader">Tasks</div>
			</div>
			{#if data === null}
				{#each Array(6), i (i)}
					<div class="row skeleton-row" aria-hidden="true">
						<div><span class="sk" style="width:70%"></span></div>
						<div><span class="sk" style="width:65%"></span></div>
						<div><span class="sk" style="width:55%"></span></div>
						<div><span class="sk" style="width:50%"></span></div>
						<div><span class="sk" style="width:40%"></span></div>
						<div class="right"><span class="sk" style="width:35%"></span></div>
					</div>
				{/each}
			{:else if data.runs.length === 0}
				<div class="table-empty">
					<EmptyState
						title="No runs found"
						hint={statusFilter !== 'all' || flowFilter
							? 'No runs match the current filter.'
							: 'Trigger a flow to see its runs here.'}
					/>
				</div>
			{:else}
				{#each data.runs as r (r.id)}
					<a class="row" href="/runs/{r.id}" role="row">
						<div role="cell"><StatusPill status={r.status} /></div>
						<div class="cell-flow" role="cell">
							<div class="flow-id">{r.flow_id}</div>
							<div class="run-id">run #{r.id}</div>
						</div>
						<div class="cell-mono trigger" role="cell">
							<span class="trig-dot" style="background:{triggerColor(r.trigger)}"></span>
							{r.trigger}
						</div>
						<div
							class="cell-mono"
							role="cell"
							title={r.started_at ? absoluteTime(r.started_at) : undefined}
						>
							{r.started_at ? relativeTime(r.started_at) : '—'}
						</div>
						<div class="cell-mono" role="cell">
							{r.duration_sec != null ? duration(r.duration_sec) : '—'}
						</div>
						<div class="cell-mono dim right" role="cell">{r.tasks_done} / {r.tasks_total}</div>
					</a>
				{/each}
			{/if}
		</div>

		{#if data !== null && data.total > 0}
			<div class="pager">
				<button class="btn-secondary" disabled={pageNum <= 1} onclick={() => pageNum--}>
					← Prev
				</button>
				<span class="pager-info">
					page {pageNum} / {maxPage} · {formatNumber(data.total)} runs
				</span>
				<button class="btn-secondary" disabled={pageNum >= maxPage} onclick={() => pageNum++}>
					Next →
				</button>
			</div>
		{/if}
	{/if}
</div>

<style>
	.chips {
		display: flex;
		gap: 8px;
		margin-bottom: 16px;
		flex-wrap: wrap;
	}

	.chip {
		display: flex;
		align-items: center;
		gap: 7px;
		height: 32px;
		padding: 0 13px;
		border-radius: 8px;
		cursor: pointer;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--muted);
		background: transparent;
		border: 1px solid var(--border);
	}

	.chip:hover {
		color: var(--text);
	}

	.chip.on {
		color: var(--text);
		background: var(--panel2);
		border-color: var(--border2);
	}

	.chip-count {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border-radius: 5px;
		padding: 1px 6px;
	}

	.chip-count.on {
		color: var(--accent);
	}

	.flow-chip {
		color: var(--cyan);
		border-color: rgba(88, 166, 255, 0.32);
		background: rgba(88, 166, 255, 0.08);
	}

	.flow-chip:hover {
		color: var(--cyan);
		background: rgba(88, 166, 255, 0.14);
	}

	.flow-chip-x {
		font-size: 14px;
		line-height: 1;
		color: var(--muted);
	}

	/* Columns + row height for the shared .table-grid scaffold (theme.css). */
	.table-grid .thead,
	.table-grid .row {
		grid-template-columns: 0.7fr 2fr 1fr 1.1fr 0.9fr 0.9fr;
	}

	.table-grid .row {
		padding: 13px 18px;
	}

	.cell-flow {
		min-width: 0;
	}

	.flow-id {
		font: 600 12.5px 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.run-id {
		font-size: 11px;
		color: var(--dim);
		margin-top: 2px;
	}

	.cell-mono.dim {
		color: var(--dim);
	}

	.trigger {
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.trig-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		flex: 0 0 auto;
	}

	.table-empty {
		padding: 14px;
	}

	.pager {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 14px;
		margin-top: 14px;
	}

	.pager-info {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	/* .btn-secondary, .sk, .error-box, .error-strip come from theme.css. */
	.table-grid .skeleton-row {
		padding: 17px 18px;
	}
</style>
