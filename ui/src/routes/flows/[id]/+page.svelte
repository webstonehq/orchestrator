<script lang="ts">
	import { page } from '$app/state';
	import { breadcrumb } from '$lib/breadcrumb';
	import Builder from '$lib/builder/Builder.svelte';

	const flowId = $derived(page.params.id ?? '');

	$effect(() => {
		breadcrumb.set(['flows', flowId]);
	});
</script>

<svelte:head>
	<title>{flowId} · Orchestrator</title>
</svelte:head>

<!-- Key by id: navigating between flows must remount the editor (fresh
     store, fresh load) rather than reuse the previous flow's state. -->
{#key flowId}
	<Builder mode="edit" {flowId} />
{/key}
