<script lang="ts">
	import type { LogLine, LogLevel } from '$lib/api';
	import { formatNumber } from '$lib/format';
	import { MAX_LOG_LINES } from './sse';

	let {
		logs,
		live,
		truncated = false
	}: { logs: LogLine[]; live: boolean; truncated?: boolean } = $props();

	const LEVEL_COLOR: Record<LogLevel, string> = {
		INFO: '#58a6ff',
		OK: '#3fb950',
		WARN: '#e3b341',
		ERR: '#f85149',
		DBG: '#6f7d8f'
	};

	let scroller: HTMLElement | undefined = $state();
	let pinned = $state(true);

	function onScroll() {
		if (!scroller) return;
		pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 48;
	}

	function resume() {
		pinned = true;
		if (scroller) scroller.scrollTop = scroller.scrollHeight;
	}

	// Autoscroll: stay glued to the bottom while pinned.
	$effect(() => {
		void logs.length;
		if (pinned && scroller) scroller.scrollTop = scroller.scrollHeight;
	});

	function fmtTs(ts: string): string {
		const d = new Date(ts);
		if (Number.isNaN(d.getTime())) return ts.slice(11, 19) || ts;
		const p = (n: number) => String(n).padStart(2, '0');
		return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
	}
</script>

<div class="pane">
	<div class="scroll" bind:this={scroller} onscroll={onScroll}>
		{#if truncated}
			<div class="truncated">older lines evicted · showing last {formatNumber(MAX_LOG_LINES)} lines</div>
		{/if}
		{#each logs as line (line.id)}
			<div class="row">
				<span class="ts">{fmtTs(line.ts)}</span>
				<span class="lvl" style:color={LEVEL_COLOR[line.level] ?? 'var(--muted)'}>{line.level}</span>
				<span class="task">{line.task}</span>
				<span class="msg">{line.message}</span>
			</div>
		{/each}
		{#if logs.length === 0}
			<div class="none">no log output yet</div>
		{/if}
		{#if live}
			<div class="tail">
				<span class="tail-bar">▏</span>
				<span class="cursor"></span>
			</div>
		{/if}
	</div>
	{#if !pinned}
		<button type="button" class="resume" onclick={resume}>↓ resume autoscroll</button>
	{/if}
</div>

<style>
	@keyframes blink {
		50% {
			opacity: 0;
		}
	}

	.pane {
		position: relative;
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}

	.scroll {
		flex: 1;
		overflow: auto;
		padding: 12px 4px 30px;
		background: #08090c;
	}

	.row {
		display: flex;
		gap: 12px;
		padding: 2px 16px;
		font: 400 12px/1.6 'IBM Plex Mono', monospace;
	}

	.row:hover {
		background: rgba(255, 255, 255, 0.03);
	}

	.ts {
		color: #3d4653;
		flex: 0 0 auto;
	}

	.lvl {
		font-weight: 600;
		flex: 0 0 34px;
	}

	.task {
		color: #6f7d8f;
		flex: 0 0 auto;
		max-width: 160px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.msg {
		color: #b9c4d2;
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.none {
		padding: 8px 16px;
		font: 400 12px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.truncated {
		padding: 2px 16px 6px;
		font: 400 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		font-style: italic;
	}

	.tail {
		padding: 4px 16px;
		font: 400 12px 'IBM Plex Mono', monospace;
		display: flex;
		gap: 8px;
		align-items: center;
	}

	.tail-bar {
		color: var(--cyan);
	}

	.cursor {
		width: 8px;
		height: 15px;
		background: var(--cyan);
		display: inline-block;
		animation: blink 1s step-end infinite;
	}

	.resume {
		position: absolute;
		bottom: 14px;
		left: 50%;
		transform: translateX(-50%);
		height: 28px;
		padding: 0 13px;
		border-radius: 999px;
		border: 1px solid rgba(88, 166, 255, 0.4);
		background: rgba(13, 16, 21, 0.92);
		color: var(--cyan);
		font: 600 11px 'IBM Plex Mono', monospace;
		cursor: pointer;
		box-shadow: 0 6px 18px -6px rgba(0, 0, 0, 0.8);
	}
</style>
