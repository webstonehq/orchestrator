<script lang="ts">
	import { api, ApiError, type WorkerView } from '$lib/api';
	import { breadcrumb } from '$lib/breadcrumb';
	import { relativeTime } from '$lib/format';
	import { toast } from '$lib/toast';
	import EmptyState from '$lib/components/EmptyState.svelte';

	breadcrumb.set(['workers']);

	// Workers are live data (they poll the server every ~3s); refresh briskly.
	const POLL_MS = 3000;

	let workers: WorkerView[] | null = $state(null);
	let enabled = $state(true);
	let loadError: string | null = $state(null);
	let retryNonce = $state(0);

	$effect(() => {
		void retryNonce;
		let disposed = false;

		const load = async () => {
			try {
				const data = await api.workers.list();
				if (disposed) return;
				workers = data.workers;
				enabled = data.enabled;
				loadError = null;
			} catch (err) {
				if (disposed) return;
				const message = err instanceof ApiError ? err.message : 'Failed to load workers';
				if (loadError === null) toast.error(`Workers: ${message}`);
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

	function loadPct(w: WorkerView): number {
		if (w.capacity <= 0) return 0;
		return Math.min(100, Math.round((w.in_flight / w.capacity) * 100));
	}
</script>

<svelte:head>
	<title>Workers · Orchestrator</title>
</svelte:head>

<div class="page">
	<h1 class="page-title">Workers</h1>
	<p class="page-desc">
		Remote workers connected to this server, the queues they serve, and their current load.
	</p>

	{#if workers === null && loadError !== null}
		<div class="error-box" role="alert">
			<div class="error-msg">{loadError}</div>
			<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
		</div>
	{:else if workers === null}
		<div class="list" aria-hidden="true">
			{#each Array(2), i (i)}
				<div class="card skeleton-card">
					<div class="dot-box"></div>
					<div class="sk-col">
						<span class="sk" style="width:160px"></span>
						<span class="sk" style="width:110px"></span>
					</div>
				</div>
			{/each}
		</div>
	{:else if !enabled}
		<EmptyState
			title="Workers not enabled"
			hint="Start the server with a worker token to accept remote workers, e.g. `orchestrator serve --worker-token s3cret` (or set ORCH_WORKER_TOKENS)."
		/>
	{:else if workers.length === 0}
		<EmptyState
			title="No workers connected"
			hint="Run a worker against this server: `orchestrator worker --server <url> --token <token> --queues <queue>`."
		/>
	{:else}
		{#if loadError !== null}
			<div class="error-strip" role="alert">
				<span>{loadError}</span>
				<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
			</div>
		{/if}
		<div class="list">
			{#each workers as w (w.worker_id)}
				<div class="card" class:off={!w.online}>
					<div class="dot-box" title={w.online ? 'Online' : 'Not seen recently'}>
						<span class="dot" class:online={w.online}></span>
					</div>
					<div class="id-cell">
						<div class="worker-id">{w.worker_id}</div>
						<div class="worker-sub">
							{w.online ? 'online' : 'stale'} · seen {relativeTime(w.last_seen)}
						</div>
					</div>
					<div class="queues-cell">
						{#if w.queues.length > 0}
							{#each w.queues as q (q)}
								<span class="queue-chip">{q}</span>
							{/each}
						{:else}
							<span class="dash">—</span>
						{/if}
					</div>
					<div class="spacer"></div>
					<div class="load-cell">
						<div class="load-head">
							<span class="load-label">Load</span>
							<span class="load-count">{w.in_flight}<span class="load-cap">/{w.capacity}</span></span>
						</div>
						<div class="load-track">
							<div class="load-fill" style="width:{loadPct(w)}%"></div>
						</div>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.list {
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.card {
		border: 1px solid var(--border);
		background: var(--panel);
		border-radius: 12px;
		padding: 16px 18px;
		display: flex;
		align-items: center;
		gap: 20px;
	}

	.card.off .id-cell,
	.card.off .queues-cell {
		opacity: 0.55;
	}

	.dot-box {
		width: 40px;
		height: 40px;
		border-radius: 10px;
		background: var(--panel3);
		border: 1px solid var(--border2);
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
	}

	.dot {
		width: 10px;
		height: 10px;
		border-radius: 50%;
		background: var(--dim);
		box-shadow: 0 0 0 3px var(--panel);
	}

	.dot.online {
		background: var(--accent);
		box-shadow: 0 0 0 3px rgba(126, 231, 135, 0.18);
	}

	.id-cell {
		flex: 0 0 260px;
		min-width: 0;
	}

	.worker-id {
		font: 600 13.5px 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.worker-sub {
		font-size: 11px;
		color: var(--dim);
		margin-top: 3px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.queues-cell {
		flex: 1 1 auto;
		min-width: 0;
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.queue-chip {
		display: inline-block;
		font: 600 12px 'IBM Plex Mono', monospace;
		color: var(--accent);
		background: rgba(126, 231, 135, 0.08);
		border: 1px solid rgba(126, 231, 135, 0.2);
		padding: 4px 10px;
		border-radius: 7px;
		white-space: nowrap;
	}

	.spacer {
		flex: 0 0 8px;
	}

	.load-cell {
		flex: 0 0 160px;
		min-width: 0;
	}

	.load-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 8px;
	}

	.load-label {
		font-size: 10px;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.load-count {
		font: 600 13px 'IBM Plex Mono', monospace;
		color: var(--text);
	}

	.load-cap {
		color: var(--dim);
		font-weight: 500;
	}

	.load-track {
		margin-top: 6px;
		height: 6px;
		border-radius: 3px;
		background: var(--panel3);
		border: 1px solid var(--border2);
		overflow: hidden;
	}

	.load-fill {
		height: 100%;
		background: var(--accent);
		border-radius: 3px;
		transition: width 0.3s ease;
	}

	.skeleton-card {
		height: 74px;
	}

	.sk-col {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	/* .sk, .dash, .btn-secondary, .error-box, .error-strip come from theme.css. */
</style>
