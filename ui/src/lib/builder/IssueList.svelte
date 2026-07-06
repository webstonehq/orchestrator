<!--
	IssueList — compact list of validation issues that could not be mapped
	to an inline field control; rendered at the top of an inspector panel.
-->
<script lang="ts">
	import type { ValidationIssue } from '../api';

	let { issues }: { issues: ValidationIssue[] } = $props();
</script>

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

<style>
	.issues {
		border: 1px solid rgba(248, 81, 73, 0.35);
		border-radius: 9px;
		background: rgba(248, 81, 73, 0.06);
		padding: 8px 12px;
		margin-bottom: 14px;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.issue {
		display: flex;
		align-items: baseline;
		gap: 8px;
		font-size: 11.5px;
		line-height: 1.5;
	}

	.path {
		font: 600 10.5px 'IBM Plex Mono', monospace;
		color: var(--red);
		white-space: nowrap;
	}

	.msg {
		color: var(--muted);
	}
</style>
