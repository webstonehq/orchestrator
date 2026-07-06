<!--
	ImportModal — paste a YAML document and POST /api/flows/import; on
	success navigate to the imported flow's editor. 400s show the message,
	422s the structured error list.

	Props:
	- open: bindable boolean
	- onSuccess?: called just before navigating to the imported flow (the
	  Builder uses it to lift its unsaved-changes guard — cancelling the
	  confirm would strand the user while the flow already exists).
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { api, ApiError, type ValidationIssue } from '../api';
	import { toast } from '../toast';
	import Modal from '../components/Modal.svelte';

	let {
		open = $bindable(false),
		onSuccess
	}: {
		open?: boolean;
		onSuccess?: () => void;
	} = $props();

	let yamlText = $state('');
	let message = $state<string | null>(null);
	let issues = $state<ValidationIssue[]>([]);
	let importing = $state(false);

	$effect(() => {
		if (open) {
			message = null;
			issues = [];
		}
	});

	async function doImport() {
		message = null;
		issues = [];
		if (yamlText.trim() === '') {
			message = 'Paste a YAML document first';
			return;
		}
		importing = true;
		try {
			const flow = await api.flows.importYaml(yamlText);
			open = false;
			toast.info(`Imported ${flow.id} (rev ${flow.current_rev})`);
			onSuccess?.();
			void goto(`/flows/${encodeURIComponent(flow.id)}`);
		} catch (e) {
			if (e instanceof ApiError) {
				if (e.errors && e.errors.length > 0) issues = e.errors;
				else message = e.message;
			} else {
				message = e instanceof Error ? e.message : String(e);
			}
		} finally {
			importing = false;
		}
	}
</script>

<Modal bind:open title="Import YAML" width={640}>
	<p class="hint">
		Paste a flow document (the same shape as an export: top-level
		<span class="mono">id</span> plus the definition). Importing upserts by id — an existing
		flow with the same id gains a new revision.
	</p>

	<textarea
		class="yaml"
		bind:value={yamlText}
		spellcheck="false"
		placeholder={'id: my_flow\nname: my-flow\nnamespace: default\n…'}
	></textarea>

	{#if message}
		<div class="error" role="alert">{message}</div>
	{/if}
	{#if issues.length > 0}
		<div class="issues" role="alert">
			{#each issues as issue (issue.path + issue.message)}
				<div class="issue">
					<span class="path">{issue.path}</span>
					<span class="msg">{issue.message}</span>
				</div>
			{/each}
		</div>
	{/if}

	{#snippet footer()}
		<button class="btn" type="button" onclick={() => (open = false)}>Cancel</button>
		<button class="btn primary" type="button" disabled={importing} onclick={doImport}>
			{importing ? 'Importing…' : 'Import flow'}
		</button>
	{/snippet}
</Modal>

<style>
	.hint {
		margin: 0 0 12px;
		font-size: 12px;
		color: var(--muted);
		line-height: 1.5;
	}

	.mono {
		font: 500 11.5px 'IBM Plex Mono', monospace;
		color: #79c0ff;
	}

	.yaml {
		width: 100%;
		box-sizing: border-box;
		min-height: 260px;
		resize: vertical;
		border: 1px solid var(--border2);
		border-radius: 8px;
		background: var(--bg2);
		padding: 10px 12px;
		font: 500 12px / 1.6 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	.yaml:focus {
		border-color: var(--accent);
	}

	.error {
		margin-top: 12px;
		border: 1px solid rgba(248, 81, 73, 0.35);
		border-radius: 9px;
		background: rgba(248, 81, 73, 0.06);
		padding: 9px 12px;
		font-size: 12px;
		color: var(--red);
	}

	.issues {
		margin-top: 12px;
		border: 1px solid rgba(248, 81, 73, 0.35);
		border-radius: 9px;
		background: rgba(248, 81, 73, 0.06);
		padding: 8px 12px;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.issue {
		display: flex;
		align-items: baseline;
		gap: 8px;
		font-size: 11.5px;
	}

	.path {
		font: 600 10.5px 'IBM Plex Mono', monospace;
		color: var(--red);
		white-space: nowrap;
	}

	.msg {
		color: var(--muted);
	}

	.btn {
		height: 32px;
		padding: 0 14px;
		border-radius: 8px;
		border: 1px solid var(--border2);
		background: var(--panel);
		color: var(--muted);
		font: 600 12px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
	}

	.btn:hover {
		color: var(--text);
	}

	.btn.primary {
		border-color: var(--accent);
		background: var(--accent);
		color: #08110a;
	}

	.btn.primary:disabled {
		opacity: 0.6;
		cursor: default;
	}
</style>
