<script lang="ts">
	import { goto } from '$app/navigation';
	import { api, ApiError, type ItemView } from '$lib/api';
	import { duration, formatNumber, formatPercent } from '$lib/format';
	import { toast } from '$lib/toast';
	import Modal from '$lib/components/Modal.svelte';
	import type { LiveItemAgg } from './sse';

	let {
		runId,
		taskId = null,
		agg,
		live,
		concurrency = null,
		nowMs,
		onclose
	}: {
		runId: number;
		/** Parallel task to inspect; null = closed. */
		taskId?: string | null;
		agg?: LiveItemAgg;
		live: boolean;
		concurrency?: number | null;
		nowMs: number;
		onclose: () => void;
	} = $props();

	const open = $derived(taskId !== null);

	type Tab = 'failed' | 'dropped' | 'slow';
	let tab: Tab = $state('failed');

	let heat: { statuses: string; total: number } | null = $state(null);
	let runningItems: ItemView[] = $state([]);
	let tabRows: ItemView[] = $state([]);
	let retryBusy = $state(false);

	const POLL_MS = 2000;

	function itemDurationSec(item: ItemView): number {
		if (!item.started_at) return 0;
		const end = item.finished_at ? new Date(item.finished_at).getTime() : nowMs;
		return (end - new Date(item.started_at).getTime()) / 1000;
	}

	async function refresh(task: string, activeTab: Tab): Promise<void> {
		try {
			const [heatRes, runningRes, rows] = await Promise.all([
				api.runs.itemsHeatmap(runId, task),
				api.runs.items(runId, task, { status: 'running', per: 24 }),
				fetchTabRows(task, activeTab)
			]);
			if (taskId !== task) return; // closed / switched task while in flight
			heat = heatRes;
			runningItems = runningRes.items;
			// The tab may have changed mid-fetch; stale rows must not land under
			// the new label (there is no self-correcting poll when not live).
			if (tab === activeTab) tabRows = rows;
		} catch {
			// transient — next poll retries
		}
	}

	/**
	 * Failed/Dropped come straight from `items?status=`. "Slow" is an
	 * approximation: the first page (per=100) of successful items sorted by
	 * duration desc, top 20 — good enough without a server-side sort.
	 */
	async function fetchTabRows(task: string, activeTab: Tab): Promise<ItemView[]> {
		if (activeTab === 'slow') {
			const res = await api.runs.items(runId, task, { status: 'success', per: 100 });
			return res.items
				.slice()
				.sort((a, b) => itemDurationSec(b) - itemDurationSec(a))
				.slice(0, 20);
		}
		const res = await api.runs.items(runId, task, { status: activeTab, per: 50 });
		return res.items;
	}

	// Fetch on open/tab change; poll every 2s while the run is live.
	$effect(() => {
		const task = taskId;
		const activeTab = tab;
		if (!task) return;
		void refresh(task, activeTab);
		if (!live) return;
		const timer = setInterval(() => void refresh(task, activeTab), POLL_MS);
		return () => clearInterval(timer);
	});

	// -- heatmap canvas ---------------------------------------------------------

	const HEAT_COLOR: Record<string, string> = {
		s: '#2ea043',
		r: '#58a6ff',
		f: '#f85149',
		d: '#e3b341',
		q: '#222c38',
		c: '#8a95a6'
	};

	let canvasEl: HTMLCanvasElement | undefined = $state();
	let heatWrap: HTMLElement | undefined = $state();

	$effect(() => {
		const statuses = heat?.statuses ?? '';
		const canvas = canvasEl;
		const wrap = heatWrap;
		if (!canvas || !wrap || statuses.length === 0) return;
		const cell = 8;
		const gap = 2;
		const step = cell + gap;
		const cols = Math.max(10, Math.floor((wrap.clientWidth - 28) / step));
		const total = statuses.length;
		const rows = Math.ceil(total / cols);
		const w = cols * step - gap;
		const h = rows * step - gap;
		const dpr = window.devicePixelRatio || 1;
		canvas.width = w * dpr;
		canvas.height = h * dpr;
		canvas.style.width = `${w}px`;
		canvas.style.height = `${h}px`;
		const ctx = canvas.getContext('2d');
		if (!ctx) return;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.clearRect(0, 0, w, h);
		for (let i = 0; i < total; i++) {
			ctx.fillStyle = HEAT_COLOR[statuses[i]] ?? '#39424e';
			ctx.fillRect((i % cols) * step, Math.floor(i / cols) * step, cell, cell);
		}
	});

	// -- derived display bits ---------------------------------------------------

	const done = $derived(agg ? agg.success + agg.failed + agg.dropped : 0);
	const eta = $derived.by(() => {
		if (!agg || agg.queued === 0) return '—';
		if (!agg.throughput_per_sec || agg.throughput_per_sec <= 0) return '—';
		return duration(agg.queued / agg.throughput_per_sec);
	});

	interface StatTile {
		label: string;
		value: string;
		color: string;
	}

	const stats: StatTile[] = $derived.by(() => {
		if (!agg) return [];
		return [
			{ label: 'Total items', value: formatNumber(agg.total), color: 'var(--text)' },
			{ label: 'Completed', value: formatNumber(agg.success), color: 'var(--green)' },
			{ label: 'In flight', value: formatNumber(agg.running), color: 'var(--cyan)' },
			{ label: 'Failed', value: formatNumber(agg.failed), color: 'var(--red)' },
			{ label: 'Dropped', value: formatNumber(agg.dropped), color: 'var(--amber)' },
			{
				label: 'Throughput',
				value: agg.throughput_per_sec > 0 ? `${agg.throughput_per_sec.toFixed(1)}/s` : '—',
				color: 'var(--text)'
			}
		];
	});

	interface Seg {
		color: string;
		pct: number;
		glow?: boolean;
	}

	const segs: Seg[] = $derived.by(() => {
		if (!agg || agg.total === 0) return [];
		const pct = (n: number) => (n / agg.total) * 100;
		return [
			{ color: 'var(--green)', pct: pct(agg.success) },
			{ color: '#58a6ff', pct: pct(agg.running), glow: true },
			{ color: 'var(--red)', pct: pct(agg.failed) },
			{ color: 'var(--amber)', pct: pct(agg.dropped) },
			{ color: '#222c38', pct: pct(agg.queued) }
		].filter((s) => s.pct > 0);
	});

	const legend = $derived.by(() => {
		if (!agg) return [];
		return [
			{ label: 'Done', value: formatNumber(agg.success), color: '#2ea043' },
			{ label: 'Running', value: formatNumber(agg.running), color: '#58a6ff' },
			{ label: 'Failed', value: formatNumber(agg.failed), color: '#f85149' },
			{ label: 'Dropped', value: formatNumber(agg.dropped), color: '#e3b341' },
			{ label: 'Queued', value: formatNumber(agg.queued), color: '#222c38' }
		];
	});

	const tabs: { key: Tab; label: string; count: number }[] = $derived([
		{ key: 'failed', label: 'Failed', count: agg?.failed ?? 0 },
		{ key: 'dropped', label: 'Dropped', count: agg?.dropped ?? 0 },
		{ key: 'slow', label: 'Slow', count: Math.min(tabRows.length, 20) }
	]);

	// Left/Right arrow navigation between tabs (roving tabindex).
	function onTablistKeydown(e: KeyboardEvent) {
		const keys = tabs.map((t) => t.key);
		let next: number;
		const cur = keys.indexOf(tab);
		if (e.key === 'ArrowRight') next = (cur + 1) % keys.length;
		else if (e.key === 'ArrowLeft') next = (cur - 1 + keys.length) % keys.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = keys.length - 1;
		else return;
		e.preventDefault();
		tab = keys[next];
		const buttons = (e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('[role="tab"]');
		buttons[next]?.focus();
	}

	function itemSummary(item: ItemView): string {
		let text: string;
		try {
			text = JSON.stringify(item.item) ?? 'null';
		} catch {
			text = String(item.item);
		}
		return text.length > 60 ? `${text.slice(0, 60)}…` : text;
	}

	function rowBadge(item: ItemView): { text: string; color: string } {
		if (tab === 'slow') return { text: duration(itemDurationSec(item)), color: 'var(--cyan)' };
		const err = item.error?.trim();
		const color = tab === 'failed' ? 'var(--red)' : 'var(--amber)';
		return { text: err ? (err.length > 44 ? `${err.slice(0, 44)}…` : err) : tab, color };
	}

	async function retryFailed(): Promise<void> {
		if (!taskId || retryBusy) return;
		retryBusy = true;
		try {
			const res = await api.runs.retryFailed(runId, taskId);
			toast.info(`Retry started · run #${res.run_id}`);
			close();
			await goto(`/runs/${res.run_id}`);
		} catch (err) {
			toast.error(err instanceof ApiError ? err.message : 'Retry failed');
		} finally {
			retryBusy = false;
		}
	}

	function close(): void {
		heat = null;
		runningItems = [];
		tabRows = [];
		tab = 'failed';
		onclose();
	}
</script>

<Modal open={open} title={taskId ?? ''} width={880} onclose={close}>
	{#if taskId}
		<div class="subhead">
			parallel fan-out{concurrency !== null ? ` · concurrency ${concurrency}` : ''}
			{#if live}<span class="live-chip"><span class="live-dot"></span>live</span>{/if}
		</div>

		{#if agg}
			<div class="stats">
				{#each stats as st (st.label)}
					<div class="tile">
						<div class="tile-label">{st.label}</div>
						<div class="tile-value" style:color={st.color}>{st.value}</div>
					</div>
				{/each}
			</div>

			<div class="prog-head">
				<span class="prog-title">
					Overall progress · {formatNumber(done)} / {formatNumber(agg.total)}
					{#if agg.total > 0}({formatPercent(done / agg.total, 0)}){/if}
				</span>
				<span class="prog-eta">eta {eta}</span>
			</div>
			<div class="prog-bar">
				{#each segs as s, i (i)}
					<div class="seg" class:glow={s.glow} style:width="{s.pct}%" style:background={s.color}></div>
				{/each}
			</div>
			<div class="legend">
				{#each legend as lg (lg.label)}
					<div class="legend-item">
						<span
							class="legend-dot"
							style:background={lg.color}
							style:border={lg.color === '#222c38' ? '1px solid var(--border2)' : 'none'}
						></span>
						<span class="legend-label">{lg.label}</span>
						<span class="legend-value">{lg.value}</span>
					</div>
				{/each}
			</div>
		{/if}

		{#if runningItems.length > 0}
			<div class="section-title">
				Live items
				<span class="busy-badge">{runningItems.length} in flight</span>
			</div>
			<div class="slots">
				{#each runningItems as item (item.id)}
					<div class="slot">
						<div class="slot-head">
							<span class="slot-dot"></span>
							<span class="slot-idx">#{item.idx}</span>
							<span class="slot-elapsed">{duration(itemDurationSec(item))}</span>
						</div>
						<div class="slot-item">{itemSummary(item)}</div>
					</div>
				{/each}
			</div>
		{/if}

		{#if heat && heat.total > 0}
			<div class="section-title">
				All {formatNumber(heat.total)} items
				<span class="section-note">· each cell = one item</span>
			</div>
			<div class="heat-wrap" bind:this={heatWrap}>
				<canvas bind:this={canvasEl}></canvas>
			</div>
		{/if}

		<div class="tabs-row">
			<div
				class="tablist"
				role="tablist"
				aria-label="Item lists"
				tabindex="-1"
				onkeydown={onTablistKeydown}
			>
				{#each tabs as t (t.key)}
					<button
						type="button"
						class="tab"
						class:on={tab === t.key}
						role="tab"
						aria-selected={tab === t.key}
						tabindex={tab === t.key ? 0 : -1}
						onclick={() => (tab = t.key)}
					>
						{t.label}<span class="tab-count">{t.count}</span>
					</button>
				{/each}
			</div>
			{#if agg && agg.failed > 0}
				<button type="button" class="retry-btn" disabled={retryBusy} onclick={retryFailed}>
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
					Retry failed ({formatNumber(agg.failed)})
				</button>
			{/if}
		</div>
		<div class="rows">
			{#if tabRows.length === 0}
				<div class="rows-empty">no {tab} items</div>
			{:else}
				{#each tabRows as item (item.id)}
					{@const badge = rowBadge(item)}
					<div class="item-row">
						<span class="row-dot" style:background={badge.color} style:box-shadow="0 0 6px {badge.color}"
						></span>
						<span class="row-idx">#{item.idx}</span>
						<span class="row-body">{itemSummary(item)}</span>
						<span
							class="row-badge"
							style:color={badge.color}
							style:border-color={badge.color}
						>{badge.text}</span>
					</div>
				{/each}
			{/if}
		</div>
	{/if}
</Modal>

<style>
	.subhead {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		margin: -6px 0 16px;
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.live-chip {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		color: var(--cyan);
		font: 600 10px 'IBM Plex Mono', monospace;
	}

	.live-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--cyan);
		animation: liveDot 1.4s ease-in-out infinite;
	}

	.stats {
		display: grid;
		grid-template-columns: repeat(6, 1fr);
		gap: 9px;
		margin-bottom: 20px;
	}

	.tile {
		border: 1px solid var(--border);
		background: var(--panel);
		border-radius: 10px;
		padding: 11px 12px;
		min-width: 0;
	}

	.tile-label {
		font: 500 9.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.4px;
		white-space: nowrap;
	}

	.tile-value {
		font: 600 18px 'IBM Plex Mono', monospace;
		margin-top: 6px;
	}

	.prog-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		margin-bottom: 9px;
	}

	.prog-title {
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
	}

	.prog-eta {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--muted);
	}

	.prog-bar {
		display: flex;
		height: 14px;
		border-radius: 7px;
		overflow: hidden;
		background: #222c38;
		border: 1px solid var(--border);
		margin-bottom: 11px;
	}

	.seg {
		height: 100%;
	}

	.seg.glow {
		box-shadow: 0 0 12px #58a6ff;
	}

	.legend {
		display: flex;
		gap: 18px;
		flex-wrap: wrap;
		margin-bottom: 24px;
	}

	.legend-item {
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.legend-dot {
		width: 9px;
		height: 9px;
		border-radius: 2px;
		flex: 0 0 auto;
	}

	.legend-label {
		font: 500 11.5px 'IBM Plex Mono', monospace;
		color: var(--muted);
	}

	.legend-value {
		font: 600 11.5px 'IBM Plex Mono', monospace;
		color: var(--text);
	}

	.section-title {
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.6px;
		margin-bottom: 11px;
		display: flex;
		align-items: center;
		gap: 9px;
	}

	.section-note {
		color: var(--dim);
		text-transform: none;
		letter-spacing: 0;
		font-weight: 400;
	}

	.busy-badge {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--cyan);
		background: rgba(88, 166, 255, 0.1);
		border: 1px solid rgba(88, 166, 255, 0.28);
		border-radius: 5px;
		padding: 1px 7px;
	}

	.slots {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 8px;
		margin-bottom: 26px;
	}

	.slot {
		border: 1px solid var(--border2);
		background: var(--panel);
		border-radius: 9px;
		padding: 9px 11px;
		min-width: 0;
	}

	.slot-head {
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.slot-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		flex: 0 0 auto;
		background: #58a6ff;
		box-shadow: 0 0 6px #58a6ff;
		animation: dotPulse 1s ease-in-out infinite;
	}

	.slot-idx {
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--text);
	}

	.slot-elapsed {
		margin-left: auto;
		font: 500 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.slot-item {
		font: 400 10.5px 'IBM Plex Mono', monospace;
		color: var(--muted);
		margin-top: 5px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.heat-wrap {
		border: 1px solid var(--border);
		background: #0b0e13;
		border-radius: 11px;
		padding: 14px;
		margin-bottom: 26px;
		overflow: auto;
	}

	.heat-wrap canvas {
		display: block;
	}

	.tabs-row {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-bottom: 12px;
	}

	/* Semantic wrapper only — the buttons stay direct flex items. */
	.tablist {
		display: contents;
	}

	.tab {
		display: flex;
		align-items: center;
		gap: 7px;
		height: 30px;
		padding: 0 12px;
		border-radius: 8px;
		cursor: pointer;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--muted);
		background: transparent;
		border: 1px solid var(--border);
	}

	.tab.on {
		color: var(--text);
		background: var(--panel3);
		border-color: var(--border2);
	}

	.tab-count {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		background: var(--panel3);
		border-radius: 5px;
		padding: 1px 6px;
	}

	.tab.on .tab-count {
		color: var(--accent);
	}

	.retry-btn {
		margin-left: auto;
		height: 30px;
		padding: 0 13px;
		border-radius: 8px;
		border: 1px solid var(--accent);
		background: rgba(126, 231, 135, 0.1);
		color: var(--accent);
		font: 600 11.5px 'IBM Plex Mono', monospace;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.retry-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.rows {
		border: 1px solid var(--border);
		border-radius: 11px;
		overflow: hidden;
		background: var(--panel);
	}

	.rows-empty {
		padding: 18px 14px;
		font: 500 11.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-align: center;
	}

	.item-row {
		display: flex;
		align-items: center;
		gap: 11px;
		padding: 11px 14px;
		border-bottom: 1px solid var(--border);
	}

	.item-row:last-child {
		border-bottom: none;
	}

	.item-row:hover {
		background: var(--panel2);
	}

	.row-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		flex: 0 0 auto;
	}

	.row-idx {
		font: 600 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		flex: 0 0 52px;
	}

	.row-body {
		font: 400 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		min-width: 0;
		flex: 1;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.row-badge {
		font: 600 10px 'IBM Plex Mono', monospace;
		border: 1px solid;
		background: rgba(255, 255, 255, 0.04);
		padding: 2px 8px;
		border-radius: 6px;
		white-space: nowrap;
		max-width: 260px;
		overflow: hidden;
		text-overflow: ellipsis;
	}
</style>
