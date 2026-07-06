<!--
	Builder — the flow builder/editor. One component, two modes:

	- mode="new"  (/flows/new): fresh definition, "Create flow" saves via
	  POST /api/flows then navigates to the editor. Optional YAML import.
	- mode="edit" (/flows/[id]): loads the flow, edits, PUT on save with a
	  revision message; revision history, Run, Export.

	State: one deep $state FlowDefinition in BuilderStore (state.svelte.ts);
	a derived JSON snapshot drives dirty tracking and the debounced (400ms)
	POST /flows/validate whose issues feed the status badge and the inline
	per-path error map. Unsaved changes guard both SPA navigation
	(beforeNavigate) and tab close (beforeunload), best-effort.
-->
<script lang="ts">
	import { beforeNavigate, goto } from '$app/navigation';
	import { api, ApiError, type RevisionInfo } from '../api';
	import { toast } from '../toast';
	import { BuilderStore, isParallel } from './state.svelte';
	import { initDefinition } from './defs';
	import TaskCanvas from './TaskCanvas.svelte';
	import YamlPane from './YamlPane.svelte';
	import InspectorInputs from './InspectorInputs.svelte';
	import InspectorVars from './InspectorVars.svelte';
	import InspectorTrigger from './InspectorTrigger.svelte';
	import InspectorTask from './InspectorTask.svelte';
	import InspectorParallel from './InspectorParallel.svelte';
	import RevisionsPanel from './RevisionsPanel.svelte';
	import RunModal from './RunModal.svelte';
	import ImportModal from './ImportModal.svelte';

	let {
		mode,
		flowId = null,
		autoImport = false
	}: {
		mode: 'new' | 'edit';
		flowId?: string | null;
		autoImport?: boolean;
	} = $props();

	// mode/flowId/autoImport are fixed per mounted instance: the [id] route
	// keys the component by flow id and "new" never changes mode in place.
	// svelte-ignore state_referenced_locally
	const store = new BuilderStore(mode);

	let revisions = $state<RevisionInfo[]>([]);
	let revisionsOpen = $state(false);
	let savePopoverOpen = $state(false);
	let saveMessage = $state('');
	let revChipEl = $state<HTMLButtonElement>();
	let saveBtnEl = $state<HTMLButtonElement>();
	let saveMsgEl = $state<HTMLInputElement>();
	let saving = $state(false);
	let runOpen = $state(false);
	// svelte-ignore state_referenced_locally
	let importOpen = $state(autoImport && mode === 'new');
	let bypassGuard = false;

	// ---- load ---------------------------------------------------------------

	$effect(() => {
		void loadEverything();
	});

	async function loadEverything() {
		try {
			store.plugins = await api.plugins();
		} catch (e) {
			toast.error(`Failed to load plugins: ${e instanceof Error ? e.message : e}`);
		}
		refreshSecrets();
		if (mode === 'edit' && flowId) {
			try {
				const detail = await api.flows.get(flowId);
				store.flowId = detail.id;
				store.currentRev = detail.current_rev;
				store.def = initDefinition(detail.definition);
				store.savedJson = store.json;
				store.loaded = true;
			} catch (e) {
				store.loadError = e instanceof Error ? e.message : String(e);
			}
		} else {
			store.savedJson = store.json;
			store.loaded = true;
		}
	}

	function refreshSecrets() {
		api.secrets
			.list()
			.then((list) => (store.secretNames = list.map((s) => s.name)))
			.catch(() => {
				// non-fatal: picker simply shows no SECRETS group
			});
	}

	// ---- live validation (debounced) ---------------------------------------

	let validateEpoch = 0;
	/** JSON snapshot that produced the current liveIssues (badge staleness). */
	let validatedJson = $state<string | null>(null);
	/** Consecutive validate request failures (3+ = validation unavailable). */
	let validateFailures = $state(0);

	$effect(() => {
		const json = store.json;
		if (!store.loaded) return;
		const epoch = ++validateEpoch;
		const timer = setTimeout(async () => {
			try {
				const res = await api.flows.validate(JSON.parse(json));
				if (epoch === validateEpoch) {
					store.liveIssues = res.errors;
					store.validatedOnce = true;
					validatedJson = json;
					validateFailures = 0;
				}
			} catch {
				// server unreachable / transient — keep the last result, but track
				// consecutive failures so the badge can degrade instead of showing
				// a stale ready/incomplete forever.
				if (epoch === validateEpoch) validateFailures += 1;
			}
		}, 400);
		return () => clearTimeout(timer);
	});

	// Edits invalidate stale 422 save errors: clear them once the definition
	// changes from the snapshot that produced them.
	let saveIssuesJson: string | null = null;

	$effect(() => {
		const json = store.json;
		if (store.saveIssues.length > 0 && saveIssuesJson !== null && json !== saveIssuesJson) {
			store.saveIssues = [];
			saveIssuesJson = null;
		}
	});

	// Badge: "checking…" until the current JSON has a validation result;
	// after 3 consecutive validate failures, degrade to "validation
	// unavailable" rather than showing a stale ready/incomplete.
	const status = $derived(
		validateFailures >= 3
			? 'unavailable'
			: !store.loaded || !store.validatedOnce || store.json !== validatedJson
				? 'checking'
				: store.issues.length > 0
					? 'incomplete'
					: 'ready'
	);

	// ---- dirty guards -------------------------------------------------------

	beforeNavigate(({ cancel }) => {
		if (bypassGuard || !store.dirty) return;
		if (!confirm('Discard unsaved changes to this flow?')) cancel();
	});

	function onBeforeUnload(e: BeforeUnloadEvent) {
		if (store.dirty && !bypassGuard) e.preventDefault();
	}

	// ---- header actions -----------------------------------------------------

	function goBack() {
		void goto('/');
	}

	async function discard() {
		if (mode === 'new') {
			void goto('/');
			return;
		}
		if (store.dirty && !confirm('Discard unsaved changes and reload from the server?')) return;
		if (!flowId) return;
		try {
			const detail = await api.flows.get(flowId);
			store.currentRev = detail.current_rev;
			store.def = initDefinition(detail.definition);
			store.savedJson = store.json;
			store.viewingRev = null;
			store.saveIssues = [];
		} catch (e) {
			toast.error(`Reload failed: ${e instanceof Error ? e.message : e}`);
		}
	}

	async function createFlow() {
		saving = true;
		try {
			const detail = await api.flows.create({ definition: JSON.parse(store.json) });
			bypassGuard = true;
			toast.info(`Flow ${detail.id} created`);
			void goto(`/flows/${encodeURIComponent(detail.id)}`);
		} catch (e) {
			handleSaveError(e);
		} finally {
			saving = false;
		}
	}

	// Focus the revision-message input whenever the save popover opens.
	$effect(() => {
		if (savePopoverOpen) saveMsgEl?.focus();
	});

	function closeSavePopover() {
		savePopoverOpen = false;
		saveBtnEl?.focus();
	}

	async function saveRevision() {
		if (!flowId) return;
		saving = true;
		try {
			const res = await api.flows.update(flowId, {
				definition: JSON.parse(store.json),
				message: saveMessage.trim() || undefined
			});
			store.currentRev = res.current_rev;
			store.savedJson = store.json;
			store.viewingRev = null;
			store.saveIssues = [];
			closeSavePopover();
			saveMessage = '';
			toast.info(`Saved rev ${res.current_rev}`);
		} catch (e) {
			handleSaveError(e);
		} finally {
			saving = false;
		}
	}

	function handleSaveError(e: unknown) {
		if (e instanceof ApiError && e.status === 422 && e.errors) {
			store.saveIssues = e.errors;
			saveIssuesJson = store.json;
			toast.error('Validation failed — see highlighted fields');
		} else {
			toast.error(`Save failed: ${e instanceof Error ? e.message : e}`);
		}
	}

	async function toggleRevisions() {
		if (!flowId) return;
		if (revisionsOpen) {
			revisionsOpen = false;
			return;
		}
		try {
			revisions = await api.flows.revisions(flowId);
			revisionsOpen = true;
		} catch (e) {
			toast.error(`Failed to load revisions: ${e instanceof Error ? e.message : e}`);
		}
	}

	function closeRevisions() {
		revisionsOpen = false;
		revChipEl?.focus();
	}

	async function pickRevision(rev: number) {
		closeRevisions();
		if (!flowId) return;
		if (store.dirty && !confirm(`Load rev ${rev}? Unsaved changes will be replaced.`)) return;
		try {
			const res = await api.flows.revision(flowId, rev);
			store.def = initDefinition(res.definition);
			store.viewingRev = rev === store.currentRev ? null : rev;
			store.saveIssues = [];
		} catch (e) {
			toast.error(`Failed to load rev ${rev}: ${e instanceof Error ? e.message : e}`);
		}
	}

	async function exportYaml() {
		if (!flowId) return;
		try {
			const yaml = await api.flows.exportYaml(flowId);
			const blob = new Blob([yaml], { type: 'text/yaml' });
			const url = URL.createObjectURL(blob);
			const a = document.createElement('a');
			a.href = url;
			a.download = `${flowId}.yaml`;
			a.click();
			URL.revokeObjectURL(url);
		} catch (e) {
			toast.error(`Export failed: ${e instanceof Error ? e.message : e}`);
		}
	}
</script>

<svelte:window onbeforeunload={onBeforeUnload} />

{#if store.loadError}
	<div class="load-error" role="alert">
		<div class="load-error-title">Failed to load flow</div>
		<div class="load-error-msg">{store.loadError}</div>
		<button class="btn" type="button" onclick={goBack}>Back to flows</button>
	</div>
{:else}
	<div class="builder">
		<header class="head">
			<button class="back" type="button" aria-label="Back to flows" onclick={goBack}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<polyline points="15 18 9 12 15 6"></polyline>
				</svg>
			</button>

			<div class="title">
				<div class="title-row">
					<input
						class="name"
						value={store.def.name}
						spellcheck="false"
						aria-label="Flow name"
						oninput={(e) => (store.def.name = e.currentTarget.value)}
					/>
					<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--dim)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
						<path d="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"></path>
					</svg>
					<span class="sep">·</span>
					<input
						class="namespace"
						value={store.def.namespace}
						spellcheck="false"
						aria-label="Namespace"
						oninput={(e) => (store.def.namespace = e.currentTarget.value)}
					/>
				</div>
				<input
					class="desc"
					value={store.def.description}
					spellcheck="false"
					placeholder="Add a description…"
					aria-label="Description"
					oninput={(e) => (store.def.description = e.currentTarget.value)}
				/>
			</div>

			<div class="spacer"></div>

			<div
				class="badge"
				class:ready={status === 'ready'}
				class:incomplete={status === 'incomplete'}
				title={status === 'incomplete'
					? `${store.issues.length} validation issue${store.issues.length === 1 ? '' : 's'}`
					: status === 'unavailable'
						? 'live validation requests are failing — the server may be unreachable'
						: undefined}
			>
				<span class="badge-dot"></span>
				{status === 'checking'
					? 'checking…'
					: status === 'unavailable'
						? 'validation unavailable'
						: status}
			</div>

			{#if store.errorAt('name')}
				<span class="name-err">{store.errorAt('name')}</span>
			{/if}

			{#if mode === 'edit'}
				<div class="rev-anchor">
					<button
						class="chip"
						type="button"
						aria-haspopup="dialog"
						aria-expanded={revisionsOpen}
						bind:this={revChipEl}
						onclick={toggleRevisions}
					>
						<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
							<circle cx="12" cy="12" r="9"></circle>
							<path d="M12 7v5l3 2"></path>
						</svg>
						<span class="chip-strong">rev {store.currentRev}</span>
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<polyline points="6 9 12 15 18 9"></polyline>
						</svg>
					</button>
					{#if revisionsOpen}
						<RevisionsPanel
							{revisions}
							currentRev={store.currentRev}
							onPick={pickRevision}
							onClose={closeRevisions}
						/>
					{/if}
				</div>
				<button class="btn" type="button" onclick={exportYaml}>Export</button>
			{:else}
				<button class="btn" type="button" onclick={() => (importOpen = true)}>Import YAML</button>
			{/if}

			<button class="btn" type="button" onclick={discard}>Discard</button>

			{#if mode === 'new'}
				<button class="btn primary" type="button" disabled={saving} onclick={createFlow}>
					<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
						<polyline points="20 6 9 17 4 12"></polyline>
					</svg>
					{saving ? 'Creating…' : 'Create flow'}
				</button>
			{:else}
				<div class="save-anchor">
					<button
						class="btn"
						class:attention={store.dirty}
						type="button"
						aria-haspopup="dialog"
						aria-expanded={savePopoverOpen}
						bind:this={saveBtnEl}
						onclick={() => (savePopoverOpen = !savePopoverOpen)}
					>
						Save
					</button>
					{#if savePopoverOpen}
						<div class="save-pop" role="dialog" aria-label="Save revision">
							<div class="save-label">Revision message</div>
							<input
								class="save-msg"
								bind:value={saveMessage}
								bind:this={saveMsgEl}
								placeholder="what changed?"
								spellcheck="false"
								onkeydown={(e) => {
									if (e.key === 'Enter') void saveRevision();
									if (e.key === 'Escape') closeSavePopover();
								}}
							/>
							<div class="save-actions">
								<button class="btn" type="button" onclick={closeSavePopover}>
									Cancel
								</button>
								<button class="btn primary" type="button" disabled={saving} onclick={saveRevision}>
									{saving ? 'Saving…' : `Save rev ${store.currentRev + 1}`}
								</button>
							</div>
						</div>
					{/if}
				</div>
				<button
					class="btn primary"
					type="button"
					disabled={store.dirty}
					title={store.dirty ? 'save first' : undefined}
					onclick={() => (runOpen = true)}
				>
					<svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor" stroke="none">
						<polygon points="6,4 20,12 6,20"></polygon>
					</svg>
					Run
				</button>
			{/if}
		</header>

		{#if store.viewingRev !== null}
			<div class="rev-banner">
				<span>
					viewing <strong>rev {store.viewingRev}</strong> — Save to restore it as a new revision
				</span>
				<button class="rev-banner-dismiss" type="button" onclick={discard}>
					back to rev {store.currentRev}
				</button>
			</div>
		{/if}

		<div class="split">
			<div class="left">
				<YamlPane {store} />
			</div>
			<div class="right">
				<div class="canvas-zone">
					<TaskCanvas {store} />
				</div>
				<div class="inspector-zone">
					{#if !store.loaded}
						<div class="loading">loading…</div>
					{:else if store.selection.kind === 'inputs'}
						<InspectorInputs {store} />
					{:else if store.selection.kind === 'vars'}
						<InspectorVars {store} />
					{:else if store.selection.kind === 'trigger'}
						<InspectorTrigger {store} />
					{:else if store.selection.kind === 'task' && store.selectedTask}
						{#if isParallel(store.selectedTask)}
							<InspectorParallel {store} task={store.selectedTask} index={store.selection.index} />
						{:else}
							<InspectorTask {store} task={store.selectedTask} index={store.selection.index} />
						{/if}
					{:else}
						<InspectorInputs {store} />
					{/if}
				</div>
			</div>
		</div>
	</div>
{/if}

<RunModal bind:open={runOpen} flowId={flowId ?? ''} inputs={store.def.inputs} />
<ImportModal bind:open={importOpen} onSuccess={() => (bypassGuard = true)} />

<style>
	.builder {
		height: 100%;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}

	.head {
		padding: 12px 20px;
		border-bottom: 1px solid var(--border);
		display: flex;
		align-items: center;
		gap: 11px;
		flex: 0 0 auto;
		position: relative;
	}

	.back {
		width: 30px;
		height: 30px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--muted);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
	}

	.back:hover {
		color: var(--text);
	}

	.title {
		min-width: 0;
	}

	.title-row {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.name {
		font: 600 15px 'IBM Plex Mono', monospace;
		letter-spacing: -0.2px;
		color: var(--text);
		background: transparent;
		border: none;
		border-bottom: 1px dashed var(--border2);
		outline: none;
		padding: 0 0 2px;
		field-sizing: content;
		min-width: 120px;
		max-width: 340px;
	}

	.name:focus {
		border-bottom-color: var(--accent);
	}

	.sep {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.namespace {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--muted);
		background: var(--panel);
		border: 1px solid var(--border2);
		border-radius: 6px;
		padding: 3px 9px;
		outline: none;
		field-sizing: content;
		min-width: 50px;
		max-width: 200px;
	}

	.namespace:focus {
		border-color: var(--accent);
	}

	.desc {
		display: block;
		width: 100%;
		min-width: 240px;
		font-size: 11px;
		font-family: 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--dim);
		background: transparent;
		border: none;
		outline: none;
		margin-top: 3px;
		padding: 0;
	}

	.desc:focus {
		color: var(--muted);
	}

	.desc::placeholder {
		color: var(--dim);
		opacity: 0.7;
	}

	.spacer {
		flex: 1;
	}

	.badge {
		display: flex;
		align-items: center;
		gap: 7px;
		height: 26px;
		padding: 0 11px;
		border-radius: 999px;
		border: 1px solid var(--border2);
		font: 600 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		white-space: nowrap;
	}

	.badge-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: currentColor;
	}

	.badge.ready {
		color: var(--accent);
		border-color: rgba(126, 231, 135, 0.4);
		background: rgba(126, 231, 135, 0.07);
	}

	.badge.incomplete {
		color: var(--amber);
		border-color: rgba(227, 179, 65, 0.45);
		background: rgba(227, 179, 65, 0.07);
	}

	.name-err {
		font-size: 11px;
		color: var(--red);
		white-space: nowrap;
	}

	.rev-anchor,
	.save-anchor {
		position: relative;
	}

	.chip {
		height: 32px;
		padding: 0 12px;
		border: 1px solid var(--border2);
		border-radius: 8px;
		background: var(--panel);
		display: flex;
		align-items: center;
		gap: 8px;
		cursor: pointer;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--muted);
	}

	.chip:hover {
		color: var(--text);
	}

	.chip-strong {
		color: var(--text);
	}

	.btn {
		height: 32px;
		padding: 0 14px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--text);
		font: 600 12px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 7px;
		white-space: nowrap;
	}

	.btn:hover {
		border-color: var(--dim);
	}

	.btn.attention {
		border-color: var(--accent);
		color: var(--accent);
	}

	.btn.primary {
		border-color: var(--accent);
		background: var(--accent);
		color: #08110a;
	}

	.btn.primary:disabled {
		opacity: 0.55;
		cursor: default;
	}

	.btn.primary:disabled:hover {
		border-color: var(--accent);
	}

	.save-pop {
		position: absolute;
		top: calc(100% + 8px);
		right: 0;
		z-index: 40;
		width: 280px;
		border: 1px solid var(--border2);
		border-radius: 11px;
		background: var(--panel2);
		box-shadow: 0 18px 40px -12px rgba(0, 0, 0, 0.7);
		padding: 13px;
	}

	.save-label {
		font: 600 10px 'IBM Plex Mono', monospace;
		color: var(--dim);
		text-transform: uppercase;
		letter-spacing: 0.5px;
		margin-bottom: 7px;
	}

	.save-msg {
		width: 100%;
		box-sizing: border-box;
		height: 32px;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--bg2);
		padding: 0 10px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	.save-msg:focus {
		border-color: var(--accent);
	}

	.save-actions {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
		margin-top: 11px;
	}

	.rev-banner {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 20px;
		border-bottom: 1px solid rgba(227, 179, 65, 0.35);
		background: rgba(227, 179, 65, 0.07);
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--amber);
	}

	.rev-banner-dismiss {
		margin-left: auto;
		height: 26px;
		padding: 0 10px;
		border-radius: 7px;
		border: 1px solid rgba(227, 179, 65, 0.4);
		background: transparent;
		color: var(--amber);
		font: 600 11px 'IBM Plex Mono', monospace;
		cursor: pointer;
	}

	.split {
		flex: 1;
		display: flex;
		min-height: 0;
	}

	.left {
		flex: 0 0 46%;
		border-right: 1px solid var(--border);
		min-width: 0;
		display: flex;
		flex-direction: column;
	}

	.right {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}

	.canvas-zone {
		flex: 0 0 46%;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}

	.inspector-zone {
		flex: 1;
		min-height: 0;
		border-top: 1px solid var(--border);
		overflow: auto;
		background: var(--bg2);
	}

	.loading {
		padding: 24px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--dim);
	}

	.load-error {
		padding: 60px 40px;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 10px;
	}

	.load-error-title {
		font: 600 15px 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--text);
	}

	.load-error-msg {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--red);
		margin-bottom: 8px;
	}
</style>
