<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { breadcrumb } from '$lib/breadcrumb';
	import { api, ApiError, isParallel, type FlowDefinition } from '$lib/api';
	import { toast } from '$lib/toast';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import RunHeader from '$lib/run/RunHeader.svelte';
	import ExecGraph from '$lib/run/ExecGraph.svelte';
	import LogsPane from '$lib/run/LogsPane.svelte';
	import TimelinePane from '$lib/run/TimelinePane.svelte';
	import FanoutModal from '$lib/run/FanoutModal.svelte';
	import { initialState, orderTasks, RunStream, type RunStreamState } from '$lib/run/sse';

	const runId = $derived(Number(page.params.id));

	let rs: RunStreamState = $state(initialState());

	// One RunStream per run id; recreated when the route param changes.
	$effect(() => {
		const id = runId;
		rs = initialState();
		if (!Number.isFinite(id)) return;
		const stream = new RunStream(id);
		const unsub = stream.subscribe((v) => {
			rs = { ...v };
		});
		stream.start();
		return () => {
			unsub();
			stream.destroy();
		};
	});

	$effect(() => {
		breadcrumb.set(['runs', `#${runId}`]);
	});

	// 1s ticker for elapsed / live bars while anything is in flight.
	let nowMs = $state(Date.now());
	$effect(() => {
		if (rs.ended && rs.detail?.run.finished_at) return;
		const t = setInterval(() => (nowMs = Date.now()), 1000);
		return () => clearInterval(t);
	});

	// Flow definition (for concurrency / endpoint labels on the graph);
	// fetched once per flow revision, best-effort.
	let def: FlowDefinition | null = $state(null);
	let defKey = '';
	$effect(() => {
		const run = rs.detail?.run;
		if (!run) return;
		const key = `${run.flow_id}@${run.flow_rev}`;
		if (key === defKey) return;
		defKey = key;
		api.flows
			.revision(run.flow_id, run.flow_rev)
			.then((res) => (def = res.definition))
			.catch(() => (def = null));
	});

	// -- header actions --------------------------------------------------------

	async function replay(): Promise<void> {
		if (!confirm(`Replay run #${runId} with the same inputs?`)) return;
		try {
			const res = await api.runs.replay(runId);
			toast.info(`Replay started · run #${res.run_id}`);
			await goto(`/runs/${res.run_id}`);
		} catch (err) {
			toast.error(err instanceof ApiError ? err.message : 'Replay failed');
		}
	}

	async function cancel(): Promise<void> {
		if (!confirm(`Cancel run #${runId}?`)) return;
		try {
			await api.runs.cancel(runId);
			toast.info('Cancel requested');
		} catch (err) {
			toast.error(err instanceof ApiError ? err.message : 'Cancel failed');
		}
	}

	// -- tabs + fan-out inspector ------------------------------------------------

	let tab: 'logs' | 'timeline' = $state('logs');
	let inspectTask: string | null = $state(null);

	const TABS = ['logs', 'timeline'] as const;

	// Left/Right arrow navigation between tabs (roving tabindex).
	function onTablistKeydown(e: KeyboardEvent) {
		let next: number;
		const cur = TABS.indexOf(tab);
		if (e.key === 'ArrowRight') next = (cur + 1) % TABS.length;
		else if (e.key === 'ArrowLeft') next = (cur - 1 + TABS.length) % TABS.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = TABS.length - 1;
		else return;
		e.preventDefault();
		tab = TABS[next];
		const buttons = (e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('[role="tab"]');
		buttons[next]?.focus();
	}

	const inspectConcurrency = $derived.by(() => {
		if (!inspectTask || !def) return null;
		const spec = def.tasks.find((t) => t.id === inspectTask);
		return spec && isParallel(spec) ? spec.concurrency : null;
	});

	const streamingLive = $derived(
		!rs.ended && rs.detail?.run.status === 'running' && (rs.live || rs.polling)
	);

	// Definition-ordered task rows (synthetic pending rows for tasks that have
	// no task_run yet — the snapshot only carries started tasks).
	const orderedTasks = $derived(rs.detail ? orderTasks(rs.detail.tasks, def) : []);
</script>

<svelte:head>
	<title>Run #{runId} · Orchestrator</title>
</svelte:head>

{#if rs.notFound || !Number.isFinite(runId)}
	<div class="page">
		<EmptyState title="Run not found" hint={`run #${page.params.id} does not exist`}>
			{#snippet cta()}
				<a class="back-link" href="/runs">← Back to runs</a>
			{/snippet}
		</EmptyState>
	</div>
{:else if !rs.detail}
	<div class="page">
		<div class="loading">connecting to run #{runId}…</div>
	</div>
{:else}
	<div class="screen">
		<RunHeader
			detail={rs.detail}
			itemsAgg={rs.itemsAgg}
			{nowMs}
			onreplay={replay}
			oncancel={cancel}
		/>

		<div class="body">
			<section class="graph-col">
				<div class="col-head">Execution graph</div>
				<div class="graph-scroll">
					<ExecGraph
						tasks={orderedTasks}
						itemsAgg={rs.itemsAgg}
						{def}
						{nowMs}
						oninspect={(taskId) => (inspectTask = taskId)}
					/>
				</div>
			</section>

			<section class="right-col">
				<div class="tabs">
					<div
						class="tablist"
						role="tablist"
						aria-label="Run detail panes"
						tabindex="-1"
						onkeydown={onTablistKeydown}
					>
						<button
							type="button"
							class="tab"
							class:on={tab === 'logs'}
							role="tab"
							aria-selected={tab === 'logs'}
							tabindex={tab === 'logs' ? 0 : -1}
							onclick={() => (tab = 'logs')}
						>
							Logs
						</button>
						<button
							type="button"
							class="tab"
							class:on={tab === 'timeline'}
							role="tab"
							aria-selected={tab === 'timeline'}
							tabindex={tab === 'timeline' ? 0 : -1}
							onclick={() => (tab = 'timeline')}
						>
							Timeline
						</button>
					</div>
					<div class="tabs-end">
						{#if streamingLive}
							<span class="live-tail" class:degraded={rs.polling}>
								<span class="live-dot"></span>
								{rs.polling ? 'polling' : 'live tail'}
							</span>
						{/if}
					</div>
				</div>
				{#if tab === 'logs'}
					<LogsPane logs={rs.logs} live={streamingLive} truncated={rs.logsTruncated} />
				{:else}
					<TimelinePane
						run={rs.detail.run}
						tasks={orderedTasks}
						itemsAgg={rs.itemsAgg}
						{nowMs}
					/>
				{/if}
			</section>
		</div>
	</div>

	<FanoutModal
		runId={runId}
		taskId={inspectTask}
		agg={inspectTask ? rs.itemsAgg[inspectTask] : undefined}
		live={streamingLive}
		concurrency={inspectConcurrency}
		{nowMs}
		onclose={() => (inspectTask = null)}
	/>
{/if}

<style>
	.screen {
		height: 100%;
		display: flex;
		flex-direction: column;
	}

	.body {
		flex: 1;
		display: flex;
		min-height: 0;
	}

	.graph-col {
		flex: 0 0 46%;
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.col-head {
		height: 36px;
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		padding: 0 16px;
		border-bottom: 1px solid var(--border);
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
	}

	.graph-scroll {
		flex: 1;
		overflow: auto;
	}

	.right-col {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}

	.tabs {
		height: 36px;
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		padding: 0 8px 0 16px;
		border-bottom: 1px solid var(--border);
		gap: 2px;
	}

	/* Semantic wrapper only — the buttons stay direct flex items. */
	.tablist {
		display: contents;
	}

	.tab {
		height: 36px;
		padding: 0 12px;
		background: transparent;
		border: none;
		border-bottom: 2px solid transparent;
		color: var(--muted);
		font: 600 12px 'IBM Plex Mono', monospace;
		cursor: pointer;
	}

	.tab.on {
		color: var(--text);
		border-bottom-color: var(--accent);
	}

	.tabs-end {
		margin-left: auto;
		padding-right: 10px;
	}

	.live-tail {
		display: flex;
		align-items: center;
		gap: 6px;
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--cyan);
	}

	.live-tail.degraded {
		color: var(--amber);
	}

	.live-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: currentColor;
		animation: liveDot 1.4s ease-in-out infinite;
	}

	.loading {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--dim);
		padding: 30px 4px;
	}

	.back-link {
		font: 600 12px 'IBM Plex Mono', monospace;
		color: var(--accent);
		text-decoration: none;
	}
</style>
