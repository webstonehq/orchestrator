<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		open = $bindable(false),
		title = '',
		width = 520,
		onclose,
		children,
		footer
	}: {
		open?: boolean;
		title?: string;
		/** Panel width: number = px, or any CSS width string. */
		width?: number | string;
		onclose?: () => void;
		children?: Snippet;
		footer?: Snippet;
	} = $props();

	const panelWidth = $derived(typeof width === 'number' ? `${width}px` : width);

	let panel: HTMLElement | undefined = $state();

	function close() {
		open = false;
		onclose?.();
	}

	// Focus the dialog on open, remembering the element that triggered it;
	// the cleanup (close or unmount) restores focus to that element.
	$effect(() => {
		if (open && panel) {
			const trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			panel.focus();
			return () => {
				if (trigger && trigger.isConnected) trigger.focus();
			};
		}
	});

	function onWindowKeydown(event: KeyboardEvent) {
		if (!open) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			close();
			return;
		}
		if (event.key === 'Tab' && panel) {
			// Minimal focus trap: keep Tab cycling inside the dialog.
			const focusables = panel.querySelectorAll<HTMLElement>(
				'a[href], button:not([disabled]), textarea, input, select, [tabindex]:not([tabindex="-1"])'
			);
			if (focusables.length === 0) {
				event.preventDefault();
				return;
			}
			const first = focusables[0];
			const last = focusables[focusables.length - 1];
			const active = document.activeElement;
			if (event.shiftKey && (active === first || active === panel)) {
				event.preventDefault();
				last.focus();
			} else if (!event.shiftKey && active === last) {
				event.preventDefault();
				first.focus();
			} else if (active && panel && !panel.contains(active)) {
				event.preventDefault();
				first.focus();
			}
		}
	}
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#if open}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="overlay"
		onclick={(event) => {
			if (event.target === event.currentTarget) close();
		}}
	>
		<div
			class="panel"
			style:width={panelWidth}
			role="dialog"
			aria-modal="true"
			aria-label={title || 'Dialog'}
			tabindex="-1"
			bind:this={panel}
		>
			{#if title}
				<div class="head">
					<span class="title">{title}</span>
					<button type="button" class="close" aria-label="Close" onclick={close}>
						<svg
							width="14"
							height="14"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
						>
							<line x1="6" y1="6" x2="18" y2="18"></line>
							<line x1="18" y1="6" x2="6" y2="18"></line>
						</svg>
					</button>
				</div>
			{/if}
			<div class="body">
				{@render children?.()}
			</div>
			{#if footer}
				<div class="foot">
					{@render footer()}
				</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		inset: 0;
		background: rgba(4, 6, 9, 0.72);
		backdrop-filter: blur(3px);
		z-index: 50;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 24px;
	}

	.panel {
		max-width: 100%;
		max-height: 90vh;
		overflow: auto;
		background: var(--panel2);
		border: 1px solid var(--border2);
		border-radius: 14px;
		box-shadow: 0 30px 70px -20px rgba(0, 0, 0, 0.8);
		outline: none;
	}

	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		padding: 15px 18px;
		border-bottom: 1px solid var(--border);
	}

	.title {
		font: 600 14px 'IBM Plex Sans', system-ui, sans-serif;
		letter-spacing: -0.2px;
	}

	.close {
		width: 28px;
		height: 28px;
		display: flex;
		align-items: center;
		justify-content: center;
		border: 1px solid var(--border2);
		border-radius: 7px;
		background: var(--panel);
		color: var(--muted);
		cursor: pointer;
	}

	.close:hover {
		color: var(--text);
	}

	.body {
		padding: 18px;
	}

	.foot {
		display: flex;
		justify-content: flex-end;
		gap: 9px;
		padding: 14px 18px;
		border-top: 1px solid var(--border);
	}
</style>
