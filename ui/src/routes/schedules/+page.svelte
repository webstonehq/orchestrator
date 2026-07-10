<script lang="ts">
	import { api, ApiError, type ScheduleView } from '$lib/api';
	import { breadcrumb } from '$lib/breadcrumb';
	import { relativeTime } from '$lib/format';
	import { toast } from '$lib/toast';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import StatusPill from '$lib/components/StatusPill.svelte';

	breadcrumb.set(['schedules']);

	const POLL_MS = 10000;

	let schedules: ScheduleView[] | null = $state(null);
	let loadError: string | null = $state(null);
	let retryNonce = $state(0);

	// A schedule only has a meaningful "next run" when it will actually fire.
	// A disabled schedule keeps a frozen next_fire_at (the scheduler never
	// advances disabled rows), which drifts into the past — so there is no
	// upcoming run to show.
	const nextRun = (s: ScheduleView) => (s.enabled ? s.next_fire_at : null);

	// Read-only summary sorted by "up next": enabled schedules with a next
	// fire time first (soonest at the top), then disabled ones (no effective
	// next run) at the bottom, alphabetically by flow.
	const sorted = $derived.by(() => {
		if (schedules === null) return null;
		return [...schedules].sort((a, b) => {
			const na = nextRun(a);
			const nb = nextRun(b);
			if (na && nb) return na < nb ? -1 : na > nb ? 1 : 0;
			if (na) return -1;
			if (nb) return 1;
			return a.flow_name.localeCompare(b.flow_name);
		});
	});

	$effect(() => {
		void retryNonce;
		let disposed = false;

		const load = async () => {
			try {
				const data = await api.schedules.list();
				if (disposed) return;
				schedules = data;
				loadError = null;
			} catch (err) {
				if (disposed) return;
				const message = err instanceof ApiError ? err.message : 'Failed to load schedules';
				if (loadError === null) toast.error(`Schedules: ${message}`);
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
</script>

<svelte:head>
	<title>Schedules · Orchestrator</title>
</svelte:head>

<div class="page">
	<h1 class="page-title">Schedules</h1>
	<p class="page-desc">
		Cron triggers across all flows, soonest first. Enable or disable a schedule
		from its flow.
	</p>

	{#if schedules === null && loadError !== null}
		<div class="error-box" role="alert">
			<div class="error-msg">{loadError}</div>
			<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
		</div>
	{:else if schedules === null}
		<div class="list" aria-hidden="true">
			{#each Array(3), i (i)}
				<div class="card skeleton-card">
					<div class="icon-box"></div>
					<div class="sk-col">
						<span class="sk" style="width:180px"></span>
						<span class="sk" style="width:120px"></span>
					</div>
				</div>
			{/each}
		</div>
	{:else if schedules.length === 0}
		<EmptyState
			title="No schedules"
			hint="Add a schedule trigger to a flow to run it on a cron."
		>
			{#snippet cta()}
				<a class="btn-secondary" href="/">Browse flows</a>
			{/snippet}
		</EmptyState>
	{:else}
		{#if loadError !== null}
			<div class="error-strip" role="alert">
				<span>{loadError}</span>
				<button class="btn-secondary" onclick={() => retryNonce++}>Retry</button>
			</div>
		{/if}
		<div class="list">
			{#each sorted ?? [] as s (s.flow_id + '/' + s.trigger_id)}
				{@const next = nextRun(s)}
				<div class="card" class:off={!s.enabled}>
					<div class="icon-box">
						<svg
							width="19"
							height="19"
							viewBox="0 0 24 24"
							fill="none"
							stroke="var(--accent)"
							stroke-width="1.8"
							stroke-linecap="round"
							stroke-linejoin="round"
						>
							<circle cx="12" cy="12" r="8"></circle>
							<path d="M12 8v4l2.6 2"></path>
						</svg>
					</div>
					<div class="flow-cell">
						<a class="flow-name" href="/flows/{s.flow_id}">{s.flow_name}</a>
						<div class="flow-human">{s.human} · {s.timezone}</div>
					</div>
					<div class="cron-cell">
						<span class="cron">{s.cron}</span>
					</div>
					<div class="kv">
						<div class="kv-label">Next run</div>
						<div class="kv-value">
							{next ? relativeTime(next) : '—'}
						</div>
					</div>
					<div class="kv">
						<div class="kv-label">Last</div>
						<div class="kv-pill">
							{#if s.last_run_status}
								<StatusPill status={s.last_run_status} />
							{:else}
								<span class="dash">—</span>
							{/if}
						</div>
					</div>
					<div class="catchup" title="Catch-up policy for missed fires">
						catchup · {s.catchup}
					</div>
					<div class="spacer"></div>
					<span class="state" class:on={s.enabled}>
						{s.enabled ? 'enabled' : 'disabled'}
					</span>
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

	.card.off .flow-cell,
	.card.off .cron-cell,
	.card.off .kv,
	.card.off .catchup,
	.card.off .icon-box {
		opacity: 0.55;
	}

	.icon-box {
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

	.flow-cell {
		flex: 0 0 260px;
		min-width: 0;
	}

	.flow-name {
		display: block;
		font: 600 13.5px 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		text-decoration: none;
	}

	.flow-name:hover {
		color: var(--accent);
	}

	.flow-human {
		font-size: 11px;
		color: var(--dim);
		margin-top: 3px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.cron-cell {
		flex: 0 0 140px;
		min-width: 0;
	}

	.cron {
		display: inline-block;
		font: 600 12px 'IBM Plex Mono', monospace;
		color: var(--accent);
		background: rgba(126, 231, 135, 0.08);
		border: 1px solid rgba(126, 231, 135, 0.2);
		padding: 4px 10px;
		border-radius: 7px;
		white-space: nowrap;
	}

	.kv {
		flex: 0 0 130px;
		min-width: 0;
	}

	.kv-label {
		font-size: 10px;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.kv-value {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		margin-top: 2px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.kv-pill {
		margin-top: 3px;
	}

	.catchup {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border: 1px solid var(--border2);
		border-radius: 6px;
		padding: 3px 8px;
		white-space: nowrap;
	}

	.spacer {
		flex: 1;
	}

	.state {
		flex: 0 0 auto;
		font: 600 10px 'IBM Plex Mono', monospace;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--dim);
		background: var(--panel3);
		border: 1px solid var(--border2);
		border-radius: 6px;
		padding: 4px 9px;
	}

	.state.on {
		color: var(--accent);
		background: rgba(126, 231, 135, 0.08);
		border-color: rgba(126, 231, 135, 0.2);
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
