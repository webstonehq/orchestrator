<script lang="ts">
	import { api, ApiError } from '$lib/api';

	let { onAuthed }: { onAuthed: () => void } = $props();

	let username = $state('');
	let password = $state('');
	let error = $state('');
	let pending = $state(false);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		if (pending) return;
		error = '';
		pending = true;
		try {
			await api.auth.login(username, password);
			onAuthed();
		} catch (err) {
			error =
				err instanceof ApiError && err.status === 401
					? 'Invalid username or password.'
					: err instanceof Error
						? err.message
						: 'Sign-in failed.';
		} finally {
			pending = false;
		}
	}
</script>

<div class="gate">
	<form class="card" onsubmit={submit}>
		<div class="head">
			<div class="logo" aria-hidden="true">
				<svg
					width="18"
					height="18"
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
			<div class="titles">
				<div class="title">Orchestrator</div>
				<div class="sub">sign in to continue</div>
			</div>
		</div>

		<label class="field">
			<span class="label">Username</span>
			<!-- svelte-ignore a11y_autofocus -->
			<input
				class="input"
				type="text"
				name="username"
				autocomplete="username"
				autocapitalize="off"
				autocorrect="off"
				spellcheck="false"
				autofocus
				bind:value={username}
			/>
		</label>

		<label class="field">
			<span class="label">Password</span>
			<input
				class="input"
				type="password"
				name="password"
				autocomplete="current-password"
				bind:value={password}
			/>
		</label>

		{#if error}
			<div class="error" role="alert">{error}</div>
		{/if}

		<button class="submit" type="submit" disabled={pending}>
			{pending ? 'Signing in…' : 'Sign in'}
		</button>
	</form>
</div>

<style>
	.gate {
		min-height: 100vh;
		display: grid;
		place-items: center;
		padding: 24px;
		background:
			radial-gradient(120% 80% at 50% -10%, rgba(126, 231, 135, 0.06), transparent 60%),
			var(--bg);
	}

	.card {
		width: 100%;
		max-width: 360px;
		background: var(--panel);
		border: 1px solid var(--border);
		border-radius: 14px;
		padding: 26px 24px 24px;
		display: flex;
		flex-direction: column;
		gap: 15px;
		box-shadow: 0 24px 60px -30px rgba(0, 0, 0, 0.8);
	}

	.head {
		display: flex;
		align-items: center;
		gap: 12px;
		margin-bottom: 4px;
	}

	.logo {
		width: 34px;
		height: 34px;
		border-radius: 9px;
		background: var(--accent);
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
		box-shadow: 0 0 20px -4px var(--accent);
	}

	.title {
		font-weight: 700;
		font-size: 16px;
		letter-spacing: 0.2px;
		line-height: 1.1;
	}

	.sub {
		font: 500 10.5px/1.3 'IBM Plex Mono', monospace;
		color: var(--dim);
		letter-spacing: 0.4px;
		text-transform: uppercase;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.label {
		font: 600 10.5px 'IBM Plex Mono', monospace;
		color: var(--dim);
		letter-spacing: 0.6px;
		text-transform: uppercase;
	}

	.input {
		height: 38px;
		padding: 0 12px;
		border-radius: 9px;
		border: 1px solid var(--border2);
		background: var(--bg2);
		color: var(--text);
		font: 500 13px 'IBM Plex Sans', system-ui, sans-serif;
		outline: none;
	}

	.input:focus {
		border-color: var(--accent);
		box-shadow: 0 0 0 3px rgba(126, 231, 135, 0.14);
	}

	.error {
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--red);
		background: rgba(248, 81, 73, 0.08);
		border: 1px solid rgba(248, 81, 73, 0.35);
		border-radius: 8px;
		padding: 8px 11px;
	}

	.submit {
		height: 40px;
		margin-top: 4px;
		border: none;
		border-radius: 9px;
		background: var(--accent);
		color: #06210a;
		font: 600 13px 'IBM Plex Sans', system-ui, sans-serif;
		cursor: pointer;
	}

	.submit:hover:not(:disabled) {
		filter: brightness(1.06);
	}

	.submit:disabled {
		opacity: 0.55;
		cursor: default;
	}
</style>
