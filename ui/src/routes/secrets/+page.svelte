<script lang="ts">
	import { breadcrumb } from '$lib/breadcrumb';
	import { api, ApiError, type SecretInfo } from '$lib/api';
	import { relativeTime } from '$lib/format';
	import { toast } from '$lib/toast';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import Modal from '$lib/components/Modal.svelte';

	breadcrumb.set(['secrets']);

	const NAME_RE = /^[A-Za-z_][A-Za-z0-9_]{0,127}$/;

	let secrets = $state<SecretInfo[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	// Add/update modal state
	let modalOpen = $state(false);
	let modalMode = $state<'add' | 'update'>('add');
	let nameInput = $state('');
	let valueInput = $state('');
	let showValue = $state(false);
	let saving = $state(false);
	let saveError = $state<string | null>(null);

	// Delete confirm modal state
	let deleteTarget = $state<string | null>(null);
	let deleting = $state(false);

	async function load() {
		loading = true;
		loadError = null;
		try {
			secrets = await api.secrets.list();
		} catch (err) {
			loadError = err instanceof ApiError ? err.message : 'Failed to load secrets.';
		} finally {
			loading = false;
		}
	}

	load();

	function absolute(iso: string): string {
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		return d.toLocaleString();
	}

	const nameValid = $derived(NAME_RE.test(nameInput));
	const nameError = $derived(
		nameInput.length === 0 || nameValid
			? ''
			: 'Use letters, numbers, underscore; must start with a letter or underscore.'
	);
	const canSave = $derived(nameValid && valueInput.length > 0 && !saving);

	function openAdd() {
		modalMode = 'add';
		nameInput = '';
		valueInput = '';
		showValue = false;
		saveError = null;
		modalOpen = true;
	}

	function openUpdate(name: string) {
		modalMode = 'update';
		nameInput = name;
		valueInput = '';
		showValue = false;
		saveError = null;
		modalOpen = true;
	}

	function closeModal() {
		modalOpen = false;
		saveError = null;
		// Never keep a typed secret value around after the modal goes away.
		valueInput = '';
		showValue = false;
	}

	async function save() {
		if (!nameValid || valueInput.length === 0 || saving) return;
		saving = true;
		saveError = null;
		try {
			await api.secrets.put(nameInput, valueInput);
			toast.info(
				modalMode === 'add' ? `Secret "${nameInput}" created.` : `Secret "${nameInput}" updated.`
			);
			closeModal();
			await load();
		} catch (err) {
			saveError = err instanceof ApiError ? err.message : 'Failed to save secret.';
		} finally {
			saving = false;
		}
	}

	function requestDelete(name: string) {
		deleteTarget = name;
	}

	function cancelDelete() {
		deleteTarget = null;
	}

	async function confirmDelete() {
		if (!deleteTarget || deleting) return;
		const name = deleteTarget;
		deleting = true;
		try {
			await api.secrets.delete(name);
			toast.info(`Secret "${name}" deleted.`);
		} catch (err) {
			if (err instanceof ApiError && err.status === 404) {
				toast.info(`Secret "${name}" was already gone.`);
			} else {
				toast.error(err instanceof ApiError ? err.message : 'Failed to delete secret.');
			}
		} finally {
			deleting = false;
			deleteTarget = null;
			await load();
		}
	}
</script>

<svelte:head>
	<title>Secrets · Orchestrator</title>
</svelte:head>

<div class="page">
	<div class="head-row">
		<div>
			<h1 class="page-title">Secrets</h1>
			<p class="page-desc">
				Encrypted values for HTTP auth — reference as <code class="snippet"
					>{'{{ secrets.NAME }}'}</code
				> in any task field.
			</p>
			<p class="page-note">
				These serve runs the server executes (the <code class="snippet">local</code> queue). Runs on a
				worker queue resolve against that worker's own store — set those on the worker box with
				<code class="snippet">orchestrator secrets set</code>.
			</p>
		</div>
		{#if secrets.length > 0}
			<button type="button" class="btn-accent" onclick={openAdd}>+ Add secret</button>
		{/if}
	</div>

	{#if loading}
		<div class="table-wrap">
			<table>
				<thead>
					<tr>
						<th>Name</th>
						<th>Created</th>
						<th>Updated</th>
						<th class="actions-col"></th>
					</tr>
				</thead>
				<tbody>
					{#each [0, 1, 2] as i (i)}
						<tr class="skeleton-row">
							<td><span class="sk sk-name"></span></td>
							<td><span class="sk sk-time"></span></td>
							<td><span class="sk sk-time"></span></td>
							<td></td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{:else if loadError}
		<div class="error-panel" role="alert">
			<div class="error-text">{loadError}</div>
			<button type="button" class="btn-secondary" onclick={load}>Retry</button>
		</div>
	{:else if secrets.length === 0}
		<EmptyState title="No secrets yet" hint={'Add one, then reference it as {{ secrets.NAME }} in a task field.'}>
			{#snippet cta()}
				<button type="button" class="btn-accent" onclick={openAdd}>+ Add secret</button>
			{/snippet}
		</EmptyState>
	{:else}
		<div class="table-wrap">
			<table>
				<thead>
					<tr>
						<th>Name</th>
						<th>Created</th>
						<th>Updated</th>
						<th class="actions-col"></th>
					</tr>
				</thead>
				<tbody>
					{#each secrets as secret (secret.name)}
						<tr>
							<td><span class="name-chip">{secret.name}</span></td>
							<td><span title={absolute(secret.created_at)}>{relativeTime(secret.created_at)}</span></td>
							<td><span title={absolute(secret.updated_at)}>{relativeTime(secret.updated_at)}</span></td>
							<td class="actions-col">
								<div class="row-actions">
									<button type="button" class="btn-ghost" onclick={() => openUpdate(secret.name)}
										>Update value</button
									>
									<button
										type="button"
										class="btn-ghost btn-danger"
										onclick={() => requestDelete(secret.name)}>Delete</button
									>
								</div>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>

<Modal
	bind:open={modalOpen}
	title={modalMode === 'add' ? 'Add secret' : `Update value — ${nameInput}`}
	width={440}
	onclose={closeModal}
>
	<div class="form">
		<label class="field">
			<span class="field-label">Name</span>
			<input
				type="text"
				class="mono-input"
				placeholder="API_TOKEN"
				spellcheck="false"
				bind:value={nameInput}
				readonly={modalMode === 'update'}
				aria-invalid={nameError ? true : undefined}
				aria-describedby={nameError ? 'secret-name-error' : undefined}
			/>
			{#if nameError}
				<span class="field-error" id="secret-name-error">{nameError}</span>
			{/if}
		</label>

		<label class="field">
			<span class="field-label">Value</span>
			<div class="value-row">
				<input
					type={showValue ? 'text' : 'password'}
					class="mono-input"
					placeholder={modalMode === 'update' ? 'new value' : 'secret value'}
					spellcheck="false"
					autocomplete="off"
					bind:value={valueInput}
					aria-invalid={saveError ? true : undefined}
					aria-describedby={saveError ? 'secret-save-error' : undefined}
				/>
				<button
					type="button"
					class="btn-ghost toggle-visibility"
					onclick={() => (showValue = !showValue)}
				>
					{showValue ? 'Hide' : 'Show'}
				</button>
			</div>
			{#if modalMode === 'update'}
				<span class="field-hint">existing value never shown</span>
			{/if}
		</label>

		{#if saveError}
			<div class="field-error form-error" id="secret-save-error">{saveError}</div>
		{/if}

		<!-- Persistent polite live region so validation/save errors are announced
		     when they appear (the visible errors above are conditional, so they
		     cannot act as live regions themselves). -->
		<div class="sr-only" aria-live="polite">{saveError ?? nameError}</div>
	</div>

	{#snippet footer()}
		<button type="button" class="btn-secondary" onclick={closeModal}>Cancel</button>
		<button type="button" class="btn-accent" disabled={!canSave} onclick={save}>
			{saving ? 'Saving…' : 'Save'}
		</button>
	{/snippet}
</Modal>

<Modal open={deleteTarget !== null} title="Delete secret" width={380} onclose={cancelDelete}>
	<p class="confirm-text">
		Delete secret <span class="name-chip">{deleteTarget ?? ''}</span>? Any task field referencing
		<code class="snippet">{`{{ secrets.${deleteTarget ?? ''} }}`}</code> will fail at run time.
	</p>

	{#snippet footer()}
		<button type="button" class="btn-secondary" onclick={cancelDelete}>Cancel</button>
		<button type="button" class="btn-danger-solid" disabled={deleting} onclick={confirmDelete}>
			{deleting ? 'Deleting…' : 'Delete'}
		</button>
	{/snippet}
</Modal>

<style>
	.head-row {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
		margin-bottom: 22px;
	}

	.head-row .page-title {
		margin-bottom: 5px;
	}

	.head-row .page-desc {
		margin-bottom: 0;
	}

	.page-note {
		margin: 8px 0 0;
		max-width: 60ch;
		font-size: 12.5px;
		line-height: 1.5;
		color: var(--dim);
	}

	.snippet {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--accent);
		background: rgba(126, 231, 135, 0.1);
		padding: 1px 6px;
		border-radius: 5px;
	}

	.table-wrap {
		border: 1px solid var(--border);
		border-radius: 12px;
		background: var(--panel);
		overflow: hidden;
	}

	table {
		width: 100%;
		border-collapse: collapse;
	}

	thead th {
		text-align: left;
		font: 600 10px 'IBM Plex Mono', monospace;
		text-transform: uppercase;
		letter-spacing: 0.6px;
		color: var(--dim);
		padding: 11px 16px;
		border-bottom: 1px solid var(--border);
	}

	tbody td {
		padding: 12px 16px;
		font-size: 13px;
		color: var(--muted);
		border-bottom: 1px solid var(--border);
	}

	tbody tr:last-child td {
		border-bottom: none;
	}

	.actions-col {
		text-align: right;
		white-space: nowrap;
	}

	.row-actions {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
	}

	.name-chip {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--accent);
		background: rgba(126, 231, 135, 0.08);
		border: 1px solid rgba(126, 231, 135, 0.25);
		padding: 2px 9px;
		border-radius: 6px;
	}

	.btn-accent {
		height: 34px;
		padding: 0 14px;
		border: 1px solid var(--accent);
		border-radius: 8px;
		background: var(--accent);
		color: #08110a;
		font: 600 12.5px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
		white-space: nowrap;
	}

	.btn-accent:hover {
		filter: brightness(1.06);
	}

	.btn-accent:disabled {
		opacity: 0.45;
		cursor: default;
		filter: none;
	}

	.btn-secondary {
		height: 34px;
		padding: 0 14px;
		border: 1px solid var(--border2);
		border-radius: 8px;
		background: var(--panel2);
		color: var(--text);
		font: 500 12.5px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
	}

	.btn-secondary:hover {
		border-color: var(--accent);
	}

	.btn-ghost {
		height: 28px;
		padding: 0 10px;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: transparent;
		color: var(--muted);
		font: 500 11.5px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
	}

	.btn-ghost:hover {
		color: var(--text);
		border-color: var(--accent);
	}

	.btn-danger {
		color: var(--red);
	}

	.btn-danger:hover {
		color: var(--red);
		border-color: var(--red);
	}

	.btn-danger-solid {
		height: 34px;
		padding: 0 14px;
		border: 1px solid var(--red);
		border-radius: 8px;
		background: var(--red);
		color: #1a0605;
		font: 600 12.5px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
	}

	.btn-danger-solid:hover {
		filter: brightness(1.08);
	}

	.btn-danger-solid:disabled {
		opacity: 0.5;
		cursor: default;
		filter: none;
	}

	.toggle-visibility {
		flex: 0 0 auto;
	}

	.error-panel {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 14px;
		padding: 16px 18px;
		border: 1px solid var(--border2);
		border-left: 3px solid var(--red);
		border-radius: 10px;
		background: var(--panel);
	}

	.error-text {
		font-size: 13px;
		color: var(--muted);
	}

	.skeleton-row td {
		opacity: 0.6;
	}

	.sk {
		display: inline-block;
		height: 12px;
		border-radius: 4px;
		background: var(--panel3);
		animation: skPulse 1.4s ease-in-out infinite;
	}

	.sk-name {
		width: 120px;
	}

	.sk-time {
		width: 70px;
	}

	@keyframes skPulse {
		0%,
		100% {
			opacity: 0.5;
		}
		50% {
			opacity: 1;
		}
	}

	.form {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.field-label {
		font: 600 11px 'IBM Plex Mono', monospace;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--dim);
	}

	.field-hint {
		font-size: 11px;
		color: var(--dim);
	}

	.field-error {
		font-size: 11.5px;
		color: var(--red);
	}

	.form-error {
		padding: 8px 10px;
		border: 1px solid rgba(248, 81, 73, 0.3);
		border-radius: 7px;
		background: rgba(248, 81, 73, 0.08);
	}

	.value-row {
		display: flex;
		gap: 8px;
	}

	.mono-input {
		flex: 1;
		width: 100%;
		box-sizing: border-box;
		height: 34px;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--bg2);
		padding: 0 12px;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		outline: none;
	}

	.mono-input:focus {
		border-color: var(--accent);
	}

	.mono-input:disabled,
	.mono-input[readonly] {
		color: var(--muted);
		opacity: 0.7;
	}

	.mono-input::placeholder {
		color: var(--dim);
	}

	.confirm-text {
		margin: 0;
		font-size: 13px;
		color: var(--muted);
		line-height: 1.5;
	}
</style>
