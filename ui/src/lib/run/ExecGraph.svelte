<script lang="ts">
	import { isParallel, type FlowDefinition, type TaskRunView, type TaskSpec } from '$lib/api';
	import { STATUS, type Status } from '$lib/status';
	import { duration, formatNumber } from '$lib/format';
	import type { LiveItemAgg } from './sse';

	let {
		tasks,
		itemsAgg,
		def = null,
		nowMs,
		oninspect
	}: {
		tasks: TaskRunView[];
		itemsAgg: Record<string, LiveItemAgg>;
		def?: FlowDefinition | null;
		nowMs: number;
		oninspect: (taskId: string) => void;
	} = $props();

	const W = 360;
	const PLAIN_W = 220;
	const PLAIN_H = 60;
	const PAR_W = 300;
	const PAR_H = 150;
	const GAP = 44;
	const TOP = 16;

	const specById = $derived.by(() => {
		const map = new Map<string, TaskSpec>();
		for (const t of def?.tasks ?? []) map.set(t.id, t);
		return map;
	});

	/** A task is a fan-out when its definition (or an aggregate) says so. */
	function parallel(taskId: string): boolean {
		const spec = specById.get(taskId);
		if (spec) return isParallel(spec);
		return taskId in itemsAgg;
	}

	interface Seg {
		color: string;
		pct: number;
		glow?: boolean;
	}

	interface GNode {
		t: TaskRunView;
		par: boolean;
		x: number;
		y: number;
		w: number;
		h: number;
		subtitle: string;
		chip: string;
		chipColor: string;
		chipBg: string;
		dotColor: string;
		agg: LiveItemAgg | null;
		conc: number | null;
		segs: Seg[];
	}

	function subtitleFor(t: TaskRunView): string {
		const spec = specById.get(t.task_id);
		if (!spec) return parallel(t.task_id) ? 'parallel fan-out' : 'task';
		if (isParallel(spec)) return `fan-out · concurrency ${spec.concurrency}`;
		const cfg = spec.config ?? {};
		const url = [cfg.url, cfg.uri].find((v) => typeof v === 'string') as string | undefined;
		const method = typeof cfg.method === 'string' ? `${cfg.method} ` : '';
		return url ? `${method}${url}` : spec.type;
	}

	function chipFor(t: TaskRunView): string {
		if (t.attempt > 1) return `retry ${t.attempt}`;
		if (t.started_at && t.finished_at) {
			return duration((new Date(t.finished_at).getTime() - new Date(t.started_at).getTime()) / 1000);
		}
		if (t.status === 'running' && t.started_at) {
			return duration((nowMs - new Date(t.started_at).getTime()) / 1000);
		}
		if (t.status === 'skipped') return 'skipped';
		if (t.status === 'canceled') return 'canceled';
		return '—';
	}

	const nodes: GNode[] = $derived.by(() => {
		let y = TOP;
		return tasks.map((t) => {
			const par = parallel(t.task_id);
			const w = par ? PAR_W : PLAIN_W;
			const h = par ? PAR_H : PLAIN_H;
			const node: GNode = {
				t,
				par,
				x: (W - w) / 2,
				y,
				w,
				h,
				subtitle: subtitleFor(t),
				chip: chipFor(t),
				chipColor: STATUS[t.status as Status]?.color ?? 'var(--dim)',
				chipBg: STATUS[t.status as Status]?.bg ?? 'var(--panel3)',
				dotColor: STATUS[t.status as Status]?.color ?? 'var(--dim)',
				agg: itemsAgg[t.task_id] ?? null,
				conc: (() => {
					const spec = specById.get(t.task_id);
					return spec && isParallel(spec) ? spec.concurrency : null;
				})(),
				segs: []
			};
			const agg = node.agg;
			if (par && agg && agg.total > 0) {
				const pct = (n: number) => (n / agg.total) * 100;
				node.segs = [
					{ color: 'var(--green)', pct: pct(agg.success) },
					{ color: '#58a6ff', pct: pct(agg.running), glow: true },
					{ color: 'var(--red)', pct: pct(agg.failed) },
					{ color: 'var(--amber)', pct: pct(agg.dropped) },
					{ color: 'var(--panel3)', pct: pct(agg.queued) }
				].filter((s) => s.pct > 0);
			}
			y += h + GAP;
			return node;
		});
	});

	const canvasH = $derived(nodes.length ? nodes[nodes.length - 1].y + nodes[nodes.length - 1].h + TOP : 0);

	interface GEdge {
		d: string;
		active: boolean;
		done: boolean;
		label: string;
		midX: number;
		midY: number;
	}

	/** Edge label from the producing task's parsed outputs. */
	function outputsLabel(t: TaskRunView): string {
		if (!t.finished_at || !t.outputs) return '';
		return Object.entries(t.outputs)
			.slice(0, 2)
			.map(([name, value]) => (Array.isArray(value) ? `${name}: ${formatNumber(value.length)}` : `outputs.${name}`))
			.join(' · ');
	}

	const edges: GEdge[] = $derived.by(() =>
		nodes.slice(1).map((to, i) => {
			const from = nodes[i];
			const cx = W / 2;
			const y1 = from.y + from.h;
			const y2 = to.y;
			return {
				d: `M ${cx} ${y1} C ${cx} ${y1 + 38} ${cx} ${y2 - 38} ${cx} ${y2}`,
				active: to.t.status === 'running',
				done: from.t.status === 'success' && to.t.status !== 'pending',
				label: outputsLabel(from.t),
				midX: cx,
				midY: (y1 + y2) / 2
			};
		})
	);
</script>

<div class="wrap">
	<div class="canvas" style:width="{W}px" style:height="{canvasH}px">
		<svg width={W} height={canvasH} class="edges">
			{#each edges as e, i (i)}
				<path
					d={e.d}
					class:active={e.active}
					class:done={e.done}
					fill="none"
					stroke-linecap="round"
				></path>
			{/each}
		</svg>

		{#each nodes as n (n.t.task_id)}
			{#if n.par}
				{@const clickable = !!n.agg && n.agg.total > 0}
				<div
					class="node fanout {n.t.status}"
					class:clickable
					style:left="{n.x}px"
					style:top="{n.y}px"
					style:width="{n.w}px"
					style:height="{n.h}px"
					role="button"
					aria-disabled={!clickable}
					tabindex={clickable ? 0 : -1}
					onclick={() => clickable && oninspect(n.t.task_id)}
					onkeydown={(e) => {
						if (clickable && (e.key === 'Enter' || e.key === ' ')) {
							e.preventDefault();
							oninspect(n.t.task_id);
						}
					}}
				>
					<div class="fan-head">
						<span
							class="dot"
							class:pulse={n.t.status === 'running'}
							style="background:{n.dotColor};box-shadow:0 0 8px {n.dotColor}"
						></span>
						<span class="fan-name">{n.t.task_id}</span>
						{#if n.agg && n.agg.total > 0}
							<span class="count-badge">{formatNumber(n.agg.total)} items</span>
						{/if}
						<span class="conc">{n.conc !== null ? `conc ${n.conc}` : 'parallel'}</span>
					</div>
					<div class="fan-sub">{n.subtitle}</div>
					<div class="fan-bar">
						{#if n.segs.length}
							{#each n.segs as s, i (i)}
								<div
									class="seg"
									class:glow={s.glow}
									style:width="{s.pct}%"
									style:background={s.color}
								></div>
							{/each}
						{:else}
							<div class="seg idle"></div>
						{/if}
					</div>
					<div class="fan-stats">
						<span class="statline">
							{#if n.agg && n.agg.total > 0}
								{formatNumber(n.agg.success)} done · {formatNumber(n.agg.running)} running · {formatNumber(
									n.agg.failed
								)} failed
							{:else}
								fan-out not started
							{/if}
						</span>
						{#if n.agg && n.agg.throughput_per_sec > 0}
							<span class="tps">~{Math.round(n.agg.throughput_per_sec)}/s</span>
						{/if}
					</div>
					<div class="cta" class:on={!!n.agg && n.agg.total > 0}>
						{#if n.agg && n.agg.total > 0}
							Inspect {formatNumber(n.agg.total)} items →
						{:else}
							parallel{n.conc !== null ? ` · concurrency ${n.conc}` : ''}
						{/if}
					</div>
				</div>
			{:else}
				<div
					class="node plain {n.t.status}"
					style:left="{n.x}px"
					style:top="{n.y}px"
					style:width="{n.w}px"
					style:height="{n.h}px"
				>
					<span
						class="dot"
						class:pulse={n.t.status === 'running'}
						style="background:{n.dotColor};box-shadow:0 0 8px {n.dotColor}"
					></span>
					<div class="plain-text">
						<div class="plain-name">{n.t.task_id}</div>
						<div class="plain-sub">{n.subtitle}</div>
					</div>
					<span class="chip" style="color:{n.chipColor};background:{n.chipBg};border-color:{n.chipColor}33"
						>{n.chip}</span
					>
				</div>
			{/if}
		{/each}

		{#each edges as e, i (i)}
			{#if e.label}
				<div
					class="edge-label"
					class:active={e.active}
					class:done={e.done}
					style:left="{e.midX}px"
					style:top="{e.midY}px"
				>
					{e.label}
				</div>
			{/if}
		{/each}
	</div>
</div>

<style>
	@keyframes nodePulse {
		0%,
		100% {
			box-shadow:
				0 0 0 1px #58a6ff,
				0 0 16px -3px rgba(88, 166, 255, 0.5);
		}
		50% {
			box-shadow:
				0 0 0 1px #58a6ff,
				0 0 26px 0 rgba(88, 166, 255, 0.8);
		}
	}

	@keyframes dashFlow {
		to {
			stroke-dashoffset: -18;
		}
	}

	.wrap {
		padding: 22px 16px;
		background: radial-gradient(circle at 50% 0%, rgba(88, 166, 255, 0.04), transparent 55%);
	}

	.canvas {
		position: relative;
		margin: 0 auto;
	}

	.edges {
		position: absolute;
		inset: 0;
		overflow: visible;
		pointer-events: none;
	}

	.edges path {
		stroke: var(--border2);
		stroke-width: 1.6;
	}

	.edges path.done {
		stroke: rgba(63, 185, 80, 0.45);
		stroke-width: 1.8;
	}

	.edges path.active {
		stroke: #58a6ff;
		stroke-width: 2;
		stroke-dasharray: 6 6;
		animation: dashFlow 0.7s linear infinite;
	}

	.node {
		position: absolute;
		box-sizing: border-box;
		border-radius: 11px;
		background: var(--panel2);
		border: 1px solid var(--border2);
	}

	.node.plain {
		display: flex;
		align-items: center;
		padding: 0 12px;
	}

	.node.success {
		border-color: rgba(63, 185, 80, 0.4);
		box-shadow: inset 0 0 0 1px rgba(63, 185, 80, 0.16);
	}

	.node.failed {
		border-color: rgba(248, 81, 73, 0.55);
	}

	.node.running {
		border-color: #58a6ff;
		animation: nodePulse 1.7s ease-in-out infinite;
	}

	.node.pending,
	.node.skipped {
		opacity: 0.72;
		background: rgba(16, 20, 26, 0.7);
		border-color: var(--border);
	}

	.node.canceled {
		border-color: rgba(138, 149, 166, 0.4);
		opacity: 0.85;
	}

	.dot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		flex: 0 0 auto;
	}

	.dot.pulse {
		animation: dotPulse 1s ease-in-out infinite;
	}

	.plain-text {
		min-width: 0;
		flex: 1;
		margin: 0 10px;
	}

	.plain-name {
		font: 600 12.5px/1.3 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.plain-sub {
		font: 400 10.5px/1.3 'IBM Plex Mono', monospace;
		color: var(--dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.chip {
		flex: 0 0 auto;
		font: 600 10px 'IBM Plex Mono', monospace;
		padding: 2px 7px;
		border-radius: 6px;
		border: 1px solid transparent;
		white-space: nowrap;
	}

	/* -- parallel fan-out card ------------------------------------------------ */

	.node.fanout {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		padding: 13px 15px;
		border-radius: 13px;
		box-shadow:
			5px 6px 0 -1px var(--bg2),
			5px 6px 0 0 var(--border2),
			10px 12px 0 -1px var(--bg2),
			10px 12px 0 0 var(--border);
	}

	.node.fanout.running {
		border-color: rgba(88, 166, 255, 0.55);
		animation: nodePulse 1.7s ease-in-out infinite;
	}

	.node.fanout.clickable {
		cursor: pointer;
	}

	.node.fanout.clickable:hover {
		border-color: #58a6ff;
	}

	.fan-head {
		display: flex;
		align-items: center;
		gap: 9px;
		width: 100%;
		min-width: 0;
	}

	.fan-name {
		font: 600 13px 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.count-badge {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--muted);
		background: var(--panel3);
		border: 1px solid var(--border2);
		padding: 1px 7px;
		border-radius: 6px;
		white-space: nowrap;
	}

	.conc {
		margin-left: auto;
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		white-space: nowrap;
	}

	.fan-sub {
		font: 400 10.5px/1.3 'IBM Plex Mono', monospace;
		color: var(--dim);
		width: 100%;
		margin-top: 4px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.fan-bar {
		display: flex;
		height: 8px;
		border-radius: 5px;
		overflow: hidden;
		background: var(--panel3);
		width: 100%;
		margin-top: 12px;
	}

	.seg {
		height: 100%;
	}

	.seg.glow {
		box-shadow: 0 0 10px #58a6ff;
	}

	.seg.idle {
		width: 100%;
		background: repeating-linear-gradient(90deg, #232c38 0 7px, transparent 7px 11px);
	}

	.fan-stats {
		display: flex;
		align-items: center;
		width: 100%;
		margin-top: 9px;
		gap: 8px;
	}

	.statline {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.tps {
		margin-left: auto;
		font: 600 10.5px 'IBM Plex Mono', monospace;
		color: var(--cyan);
		white-space: nowrap;
	}

	.cta {
		margin-top: auto;
		font: 600 10.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		white-space: nowrap;
	}

	.cta.on {
		color: var(--accent);
	}

	/* -- edge labels ----------------------------------------------------------- */

	.edge-label {
		position: absolute;
		transform: translate(-50%, -50%);
		z-index: 2;
		pointer-events: none;
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--muted);
		white-space: nowrap;
		padding: 2px 8px 2px 6px;
		border-radius: 6px;
		background: var(--bg2);
		border: 1px solid var(--border2);
		border-left: 2px solid var(--muted);
	}

	.edge-label.done {
		color: var(--green);
		border-color: rgba(63, 185, 80, 0.35);
		border-left-color: var(--green);
	}

	.edge-label.active {
		color: #58a6ff;
		border-color: rgba(88, 166, 255, 0.33);
		border-left-color: #58a6ff;
	}
</style>
