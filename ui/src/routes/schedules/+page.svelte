<script lang="ts">
	import { api, ApiError, type ScheduleView } from '$lib/api';
	import { breadcrumb } from '$lib/breadcrumb';
	import { relativeTime } from '$lib/format';
	import { toast } from '$lib/toast';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import StatusPill from '$lib/components/StatusPill.svelte';
	import Toggle from '$lib/components/Toggle.svelte';

	breadcrumb.set(['schedules']);

	const POLL_MS = 10000;

	let schedules: ScheduleView[] | null = $state(null);
	let loadError: string | null = $state(null);
	let retryNonce = $state(0);

	// Toggle generation counter. Bumped when a toggle starts *and* when it
	// settles; a poll response is only applied if the generation it started
	// under is still current. This drops both polls in flight during a toggle
	// and polls that started before the toggle (whose server snapshot may
	// predate the PUT) but land after it settles.
	let toggleGen = 0;

	$effect(() => {
		void retryNonce;
		let disposed = false;

		const load = async () => {
			const gen = toggleGen;
			try {
				const data = await api.schedules.list();
				if (disposed || gen !== toggleGen) return;
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

	async function toggleSchedule(s: ScheduleView, enabled: boolean) {
		const prev = s.enabled;
		s.enabled = enabled; // optimistic
		toggleGen++;
		try {
			await api.schedules.toggle(s.flow_id, s.trigger_id, enabled);
		} catch (err) {
			s.enabled = prev; // revert
			const message = err instanceof ApiError ? err.message : 'Failed to update schedule';
			toast.error(`Schedule ${s.flow_name}: ${message}`);
		} finally {
			toggleGen++;
		}
	}
</script>

<svelte:head>
	<title>Schedules · Orchestrator</title>
</svelte:head>

<div class="page">
	<h1 class="page-title">Schedules</h1>
	<p class="page-desc">Cron triggers that queue flow executions automatically.</p>

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
			{#each schedules as s (s.flow_id + '/' + s.trigger_id)}
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
							{s.next_fire_at ? relativeTime(s.next_fire_at) : '—'}
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
					<!-- `checked` is deliberately passed unbound: Toggle's $bindable flips
					     its own local copy on click, and this prop only overwrites that
					     copy when s.enabled *changes*. The optimistic write in
					     toggleSchedule keeps s.enabled in sync with the local flip, so a
					     failed API call's revert (s.enabled = prev) is a real change and
					     snaps the knob back. Without the optimistic write, the revert
					     would be a no-op prop value and the knob would stay flipped. -->
					<Toggle
						checked={s.enabled}
						label="Toggle schedule {s.trigger_id} for {s.flow_name}"
						onchange={(v) => void toggleSchedule(s, v)}
					/>
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
