<script lang="ts">
	import '@fontsource/ibm-plex-sans/latin-400.css';
	import '@fontsource/ibm-plex-sans/latin-500.css';
	import '@fontsource/ibm-plex-sans/latin-600.css';
	import '@fontsource/ibm-plex-sans/latin-700.css';
	import '@fontsource/ibm-plex-mono/latin-400.css';
	import '@fontsource/ibm-plex-mono/latin-500.css';
	import '@fontsource/ibm-plex-mono/latin-600.css';
	import '$lib/theme.css';

	import { page } from '$app/state';
	import { breadcrumb } from '$lib/breadcrumb';
	import { dashboardStore } from '$lib/dashboard';
	import Toast from '$lib/components/Toast.svelte';

	let { children } = $props();

	type NavSection = 'flows' | 'runs' | 'schedules' | 'secrets' | 'workers';

	const active: NavSection = $derived.by(() => {
		const routeId = page.route.id ?? '/';
		if (routeId.startsWith('/runs')) return 'runs';
		if (routeId.startsWith('/schedules')) return 'schedules';
		if (routeId.startsWith('/secrets')) return 'secrets';
		if (routeId.startsWith('/workers')) return 'workers';
		return 'flows'; // '/' and '/flows/*'
	});

	const running = $derived($dashboardStore.data?.runs_24h.running);
	const statusLine = $derived(running !== undefined ? `${running} running` : 'ready');
</script>

<div class="shell">
	<aside class="sidebar">
		<div class="brand">
			<div class="logo">
				<svg
					width="17"
					height="17"
					viewBox="0 0 24 24"
					fill="none"
					stroke="#0a0c10"
					stroke-width="2.2"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"></polygon>
				</svg>
			</div>
			<div class="brand-text">
				<div class="brand-title">Orchestrator</div>
				<div class="brand-sub">workflows</div>
			</div>
		</div>

		<nav class="nav" aria-label="Workspace">
			<div class="nav-label">Workspace</div>
			<a
				href="/"
				class="nav-item"
				class:active={active === 'flows'}
				aria-current={active === 'flows' ? 'page' : undefined}
			>
				<svg
					width="17"
					height="17"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.9"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<circle cx="12" cy="5" r="2.4"></circle>
					<circle cx="6" cy="18" r="2.4"></circle>
					<circle cx="18" cy="18" r="2.4"></circle>
					<path d="M12 7.4v3.1M12 10.5H6.4a1 1 0 0 0-1 1v4M12 10.5h5.6a1 1 0 0 1 1 1v4"></path>
				</svg>
				<span>Flows</span>
			</a>
			<a
				href="/runs"
				class="nav-item"
				class:active={active === 'runs'}
				aria-current={active === 'runs' ? 'page' : undefined}
			>
				<svg
					width="17"
					height="17"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.9"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<path d="M4 6h10M4 12h7M4 18h12"></path>
					<polygon points="17.5,9.5 21,11.7 17.5,13.9" fill="currentColor" stroke="none"></polygon>
				</svg>
				<span>Runs</span>
			</a>
			<a
				href="/schedules"
				class="nav-item"
				class:active={active === 'schedules'}
				aria-current={active === 'schedules' ? 'page' : undefined}
			>
				<svg
					width="17"
					height="17"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.9"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<circle cx="12" cy="12" r="8"></circle>
					<path d="M12 8v4l2.6 2"></path>
				</svg>
				<span>Schedules</span>
			</a>
			<a
				href="/secrets"
				class="nav-item"
				class:active={active === 'secrets'}
				aria-current={active === 'secrets' ? 'page' : undefined}
			>
				<svg
					width="17"
					height="17"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.9"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<path
						d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 1 1-7.778 7.778 5.5 5.5 0 0 1 7.777-7.777zm0 0L15.5 7.5m3 3L22 7l-3-3"
					></path>
				</svg>
				<span>Secrets</span>
			</a>
			<a
				href="/workers"
				class="nav-item"
				class:active={active === 'workers'}
				aria-current={active === 'workers' ? 'page' : undefined}
			>
				<svg
					width="17"
					height="17"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.9"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<rect x="3" y="4" width="18" height="7" rx="1.5"></rect>
					<rect x="3" y="13" width="18" height="7" rx="1.5"></rect>
					<path d="M7 7.5h.01M7 16.5h.01"></path>
				</svg>
				<span>Workers</span>
			</a>
		</nav>

		<div class="side-foot">
			<div class="live-panel">
				<span class="live-dot"></span>
				<div class="live-text">
					<div class="live-name">orchestrator</div>
					<div class="live-sub">{statusLine}</div>
				</div>
			</div>
		</div>
	</aside>

	<main class="main">
		<header class="topbar">
			<div class="crumb">
				<span class="crumb-dim">orchestrator</span>
				{#each $breadcrumb as segment, i (i)}
					<span class="crumb-dim">/</span>
					<span class={i === $breadcrumb.length - 1 ? 'crumb-cur' : 'crumb-dim'}>{segment}</span>
				{/each}
			</div>
		</header>

		<div class="content">
			{@render children()}
		</div>
	</main>
</div>

<Toast />

<style>
	.shell {
		display: flex;
		height: 100vh;
		width: 100%;
		overflow: hidden;
		background: var(--bg);
	}

	.sidebar {
		width: 238px;
		flex: 0 0 238px;
		background: var(--bg2);
		border-right: 1px solid var(--border);
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}

	.brand {
		padding: 18px 18px 14px;
		display: flex;
		align-items: center;
		gap: 11px;
		border-bottom: 1px solid var(--border);
	}

	.logo {
		width: 30px;
		height: 30px;
		border-radius: 8px;
		background: var(--accent);
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
		box-shadow: 0 0 18px -4px var(--accent);
	}

	.brand-text {
		min-width: 0;
	}

	.brand-title {
		font-weight: 700;
		font-size: 14px;
		letter-spacing: 0.2px;
		line-height: 1.1;
	}

	.brand-sub {
		font: 500 10px/1.3 'IBM Plex Mono', monospace;
		color: var(--dim);
		letter-spacing: 0.5px;
		text-transform: uppercase;
	}

	.nav {
		padding: 8px 10px;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.nav-label {
		font: 600 10px/1 'IBM Plex Mono', monospace;
		color: var(--dim);
		letter-spacing: 1px;
		text-transform: uppercase;
		padding: 12px 10px 7px;
	}

	.nav-item {
		display: flex;
		align-items: center;
		gap: 11px;
		height: 36px;
		padding: 0 12px;
		border-radius: 9px;
		cursor: pointer;
		font: 500 13px 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--muted);
		text-decoration: none;
	}

	.nav-item:hover {
		color: var(--text);
	}

	.nav-item.active {
		color: var(--text);
		background: var(--panel2);
		box-shadow: inset 2px 0 0 var(--accent);
	}

	.side-foot {
		margin-top: auto;
		padding: 14px;
		border-top: 1px solid var(--border);
	}

	.live-panel {
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 9px 10px;
		border: 1px solid var(--border2);
		border-radius: 9px;
		background: var(--panel);
	}

	.live-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--green);
		box-shadow: 0 0 8px var(--green);
		flex: 0 0 auto;
		animation: liveDot 2.4s ease-in-out infinite;
	}

	.live-text {
		min-width: 0;
		flex: 1;
	}

	.live-name {
		font: 500 11px/1.2 'IBM Plex Mono', monospace;
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.live-sub {
		font-size: 10px;
		color: var(--dim);
	}

	.main {
		flex: 1;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		min-width: 0;
	}

	.topbar {
		height: 56px;
		flex: 0 0 56px;
		border-bottom: 1px solid var(--border);
		display: flex;
		align-items: center;
		padding: 0 22px;
		gap: 14px;
		background: var(--bg2);
	}

	.crumb {
		font: 500 12px/1 'IBM Plex Mono', monospace;
		color: var(--muted);
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
	}

	.crumb-dim {
		color: var(--dim);
	}

	.crumb-cur {
		color: var(--text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.content {
		flex: 1;
		overflow: auto;
		position: relative;
		background: var(--bg);
	}
</style>
