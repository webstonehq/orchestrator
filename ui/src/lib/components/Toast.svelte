<script lang="ts">
	import { toasts, dismiss } from '$lib/toast';
</script>

{#if $toasts.length > 0}
	<div class="stack" aria-live="polite">
		{#each $toasts as t (t.id)}
			<div class="toast" class:error={t.variant === 'error'} role="status">
				<span class="msg">{t.message}</span>
				<button type="button" class="close" aria-label="Dismiss" onclick={() => dismiss(t.id)}>
					<svg
						width="12"
						height="12"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2.4"
						stroke-linecap="round"
					>
						<line x1="6" y1="6" x2="18" y2="18"></line>
						<line x1="18" y1="6" x2="6" y2="18"></line>
					</svg>
				</button>
			</div>
		{/each}
	</div>
{/if}

<style>
	.stack {
		position: fixed;
		right: 18px;
		bottom: 18px;
		z-index: 100;
		display: flex;
		flex-direction: column;
		gap: 9px;
		max-width: 380px;
	}

	.toast {
		display: flex;
		align-items: center;
		gap: 11px;
		padding: 11px 13px;
		border: 1px solid var(--border2);
		border-left: 3px solid var(--cyan);
		border-radius: 10px;
		background: var(--panel2);
		box-shadow: 0 18px 40px -12px rgba(0, 0, 0, 0.7);
	}

	.toast.error {
		border-left-color: var(--red);
	}

	.msg {
		flex: 1;
		font: 500 12px 'IBM Plex Sans', system-ui, sans-serif;
		color: var(--text);
		overflow-wrap: anywhere;
	}

	.close {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 22px;
		height: 22px;
		padding: 0;
		border: 0;
		border-radius: 6px;
		background: transparent;
		color: var(--dim);
		cursor: pointer;
		flex: 0 0 auto;
	}

	.close:hover {
		color: var(--text);
	}
</style>
