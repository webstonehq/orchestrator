<script lang="ts">
	import { page } from '$app/state';
	import { breadcrumb } from '$lib/breadcrumb';
	import Builder from '$lib/builder/Builder.svelte';

	breadcrumb.set(['flows', 'new']);

	// /flows/new?import=1 opens the YAML import dialog immediately.
	const autoImport = page.url.searchParams.get('import') === '1';
</script>

<svelte:head>
	<title>New flow · Orchestrator</title>
</svelte:head>

<Builder mode="new" {autoImport} />
