<!--
	YamlPane — a WRITABLE CodeMirror 6 YAML editor with two-way sync to the builder.

	`store.def` (a deep $state FlowDefinition) is the single source of truth.
	Two guarded flows keep the editor and the visual builder in sync:

	  builder -> editor: an $effect on flowToYaml(displayId, store.def) pushes
	    canonical text into the doc, but ONLY when the editor is not focused
	    (never fight the user's cursor). The write is a full-document replace
	    transaction tagged with `syncAnnotation` so the update listener ignores
	    it, and it no-ops when the text already matches — that (plus the focus
	    guard) prevents echo loops. A changes transaction (not a fresh
	    EditorState) preserves the undo history across frequent inspector edits.

	  editor -> builder: EditorView.updateListener on docChanged (ignoring
	    transactions carrying syncAnnotation) debounces ~300ms, then parses via
	    yamlToDefinition. Syntax/structure errors become lint diagnostics and
	    leave store.def untouched (builder keeps its last good state); a clean
	    parse clears diagnostics and assigns store.def only when the JSON
	    actually differs, letting Builder's existing dirty/validate effects
	    react for free.

	  blur: flush the pending parse, then (if valid) re-render canonical YAML so
	    formatting/quoting normalizes to exactly what the builder would emit.

	CodeMirror is constructed in onMount (browser only) and disposed on unmount.
	Extensions are composed minimally (no `codemirror` meta-package, no worker)
	to keep the single-file build small. Highlighting maps the @lezer/highlight
	tags emitted by @codemirror/lang-yaml to the app palette.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { EditorState, Annotation, Transaction } from '@codemirror/state';
	import {
		EditorView,
		keymap,
		lineNumbers,
		highlightActiveLine,
		highlightActiveLineGutter,
		hoverTooltip
	} from '@codemirror/view';
	import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
	import { autocompletion, completionKeymap } from '@codemirror/autocomplete';
	import { syntaxHighlighting, HighlightStyle } from '@codemirror/language';
	import { yaml, yamlLanguage } from '@codemirror/lang-yaml';
	import { setDiagnostics, type Diagnostic } from '@codemirror/lint';
	import { yamlSchemaLinter, yamlSchemaHover, yamlCompletion } from 'codemirror-json-schema/yaml';
	import { stateExtensions, updateSchema } from 'codemirror-json-schema';
	import { tags } from '@lezer/highlight';
	import type { BuilderStore } from './state.svelte';
	import { expressionGroups, makeExpressionCompletion } from './expr-complete';
	import { flowToYaml } from './yaml';
	import { yamlToDefinition, type YamlProblem } from './parse-yaml';
	import { loadFlowSchema } from './flow-schema';
	import { slugifyFlowId } from './defs';

	let { store }: { store: BuilderStore } = $props();

	const displayId = $derived(store.flowId ?? slugifyFlowId(store.def.name) ?? 'new_flow');

	let container: HTMLDivElement;
	let view: EditorView | null = null;
	// Marks transactions WE dispatch so the update listener ignores them.
	const syncAnnotation = Annotation.define<boolean>();
	let debounceTimer: ReturnType<typeof setTimeout> | undefined;
	// Schema-driven lint source (reads the loaded schema from editor state);
	// its diagnostics are surfaced as advisory warnings, never blocking sync.
	const schemaLint = yamlSchemaLinter();

	let ready = $state(false);
	/** True when the current editor text has YAML syntax/structure errors. */
	let parseError = $state(false);

	const valid = $derived(store.validatedOnce && store.issues.length === 0 && !parseError);

	// -- editor -> builder ----------------------------------------------------

	function onDocChanged() {
		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(applyEditorText, 300);
	}

	function applyEditorText() {
		if (!view) return;
		const text = view.state.doc.toString();
		const { def, errors } = yamlToDefinition(text);
		if (errors.length > 0) {
			view.dispatch(setDiagnostics(view.state, errors.map((e) => toDiagnostic(e))));
			parseError = true;
			return; // keep the builder on its last good definition
		}
		parseError = false;
		// Clean parse: the only diagnostics left are advisory schema warnings
		// (empty until the schema loads). They never set parseError, so builder
		// sync below always proceeds.
		view.dispatch(setDiagnostics(view.state, schemaWarnings()));
		if (def && JSON.stringify(def) !== store.json) {
			// Assigning triggers Builder's dirty + debounced validate effects.
			store.def = def;
		}
	}

	/**
	 * Schema-validation diagnostics for the current doc, downgraded to
	 * non-blocking warnings. Returns [] until a schema is loaded (the lint
	 * source reads it from editor state) or if linting throws.
	 */
	function schemaWarnings(): Diagnostic[] {
		if (!view) return [];
		try {
			return schemaLint(view).map((d) => ({ ...d, severity: 'warning' as const }));
		} catch {
			return [];
		}
	}

	/** Recompute schema warnings after a programmatic (canonical) doc replace. */
	function refreshSchemaDiagnostics() {
		if (!view || parseError) return; // parse errors own the diagnostics meanwhile
		view.dispatch(setDiagnostics(view.state, schemaWarnings()));
	}

	/** Convert a 1-based YamlProblem to a CM6 diagnostic (0-based offsets). */
	function toDiagnostic(p: YamlProblem): Diagnostic {
		const from = offsetOf(p.startLine, p.startCol);
		const to = Math.max(offsetOf(p.endLine, p.endCol), from + 1);
		return { from, to, severity: 'error', message: p.message };
	}

	function offsetOf(line: number, col: number): number {
		const doc = view!.state.doc;
		const clamped = Math.min(Math.max(line, 1), doc.lines);
		const l = doc.line(clamped);
		return Math.min(l.from + Math.max(col - 1, 0), l.to);
	}

	function onBlur() {
		clearTimeout(debounceTimer);
		applyEditorText(); // flush any pending edit before normalizing
		if (parseError) return; // leave invalid text + diagnostics for the user to fix
		renderCanonical();
	}

	// -- builder -> editor ----------------------------------------------------

	/**
	 * Full-document replace, tagged programmatic. `addToHistory: false` keeps
	 * these builder-driven normalizations out of the undo stack (unlike a fresh
	 * EditorState, this preserves the user's own typed history), so Cmd+Z steps
	 * through the user's edits rather than each canonical re-render.
	 */
	function pushToEditor(text: string) {
		if (!view) return;
		view.dispatch({
			changes: { from: 0, to: view.state.doc.length, insert: text },
			annotations: [syncAnnotation.of(true), Transaction.addToHistory.of(false)]
		});
		// Canonical text is valid by construction, so re-lint against the schema
		// to keep any warning ranges aligned with the new document.
		refreshSchemaDiagnostics();
	}

	function renderCanonical() {
		if (!view) return;
		const text = flowToYaml(displayId, store.def);
		if (view.state.doc.toString() === text) return;
		pushToEditor(text);
	}

	// Push builder changes into the editor whenever the canonical text changes,
	// but never while the user is typing (focused) and never redundantly. Read
	// the derived text first so this effect tracks displayId + store.def.
	$effect(() => {
		const text = flowToYaml(displayId, store.def);
		if (!ready || !view) return;
		if (view.hasFocus) return;
		if (view.state.doc.toString() === text) return;
		pushToEditor(text);
	});

	// -- {{ }} expression autocomplete ----------------------------------------

	// Reads store.def + store.secretNames live on every query, so newly added
	// tasks/vars/secrets show up without rebuilding the editor.
	const expressionCompletion = makeExpressionCompletion(() =>
		expressionGroups(store.def, store.secretNames)
	);

	// -- theme + highlighting (app palette) -----------------------------------

	const highlightStyle = HighlightStyle.define([
		// Mapping keys.
		{ tag: [tags.definition(tags.propertyName), tags.propertyName], color: '#79c0ff' },
		// Quoted strings + block-literal headers, and plain scalar values: the
		// YAML lexer emits every plain scalar as `content` without type analysis,
		// so numbers/booleans share the string color here.
		{ tag: [tags.string, tags.special(tags.string), tags.content], color: '#a5d6ff' },
		// Defensive: rarely emitted for block YAML, but keep them on-palette.
		{ tag: [tags.number, tags.bool, tags.keyword, tags.atom], color: '#f69d50' },
		{ tag: [tags.comment, tags.lineComment], color: '#5f7566', fontStyle: 'italic' },
		{
			tag: [
				tags.separator,
				tags.punctuation,
				tags.meta,
				tags.brace,
				tags.squareBracket,
				tags.labelName,
				tags.typeName
			],
			color: '#566270'
		}
	]);

	const theme = EditorView.theme(
		{
			'&': {
				height: '100%',
				color: 'var(--text)',
				backgroundColor: 'var(--bg2)',
				fontSize: '12.5px'
			},
			'&.cm-focused': { outline: 'none' },
			'.cm-scroller': {
				fontFamily: "'IBM Plex Mono', ui-monospace, monospace",
				lineHeight: '20px',
				overflow: 'auto'
			},
			'.cm-content': {
				padding: '10px 0 40px',
				caretColor: '#e7edf5'
			},
			'.cm-content ::selection': { backgroundColor: '#2a3646' },
			'.cm-line::selection': { backgroundColor: '#2a3646' },
			'.cm-gutters': {
				backgroundColor: 'var(--bg2)',
				color: '#3b4552',
				border: 'none'
			},
			'.cm-lineNumbers .cm-gutterElement': { minWidth: '3ch', padding: '0 6px 0 8px' },
			'.cm-activeLine': { backgroundColor: '#12161b' },
			'.cm-activeLineGutter': { backgroundColor: 'transparent', color: '#566270' },
			'.cm-cursor, .cm-dropCursor': { borderLeftColor: '#e7edf5' }
		},
		{ dark: true }
	);

	// -- mount ----------------------------------------------------------------

	onMount(() => {
		const state = EditorState.create({
			doc: flowToYaml(displayId, store.def),
			extensions: [
				lineNumbers(),
				highlightActiveLine(),
				highlightActiveLineGutter(),
				history(),
				keymap.of([...defaultKeymap, ...historyKeymap, ...completionKeymap]),
				yaml(),
				// Schema-driven autocomplete + hover docs. The schema is loaded
				// asynchronously and installed via updateSchema() below; these read
				// it from editor state (no-ops until then). Diagnostics from the
				// schema are surfaced via applyEditorText, not a linter() extension,
				// so the existing manual setDiagnostics stays the single owner.
				autocompletion({ activateOnTyping: true }),
				yamlLanguage.data.of({ autocomplete: yamlCompletion() }),
				// `{{ }}` expression refs (vars/inputs/secrets/outputs). Registered as
				// a second language-data source so it coexists with schema key
				// completion above — CM6 queries every source at the cursor.
				yamlLanguage.data.of({ autocomplete: expressionCompletion }),
				hoverTooltip(yamlSchemaHover()),
				stateExtensions(),
				syntaxHighlighting(highlightStyle),
				theme,
				EditorState.tabSize.of(2),
				EditorView.editable.of(true),
				EditorView.updateListener.of((update) => {
					if (!update.docChanged) return;
					if (update.transactions.some((tr) => tr.annotation(syncAnnotation))) return;
					onDocChanged();
				}),
				EditorView.domEventHandlers({
					blur: () => {
						onBlur();
					}
				})
			]
		});
		view = new EditorView({ state, parent: container });
		ready = true;

		// Load the schema the backend assembles from its live plugin registry,
		// then light up autocomplete/hover/warnings. A failure leaves the editor
		// as plain YAML — autocomplete is a progressive enhancement.
		loadFlowSchema().then((schema) => {
			if (!schema || !view) return;
			updateSchema(view, schema);
			refreshSchemaDiagnostics();
		});

		return () => {
			clearTimeout(debounceTimer);
			view?.destroy();
			view = null;
		};
	});
</script>

<div class="pane">
	<div class="tabs">
		<div class="tab">
			<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path>
				<polyline points="14 2 14 8 20 8"></polyline>
			</svg>
			flow.yaml
		</div>
		<div class="status">
			{#if parseError}
				<span class="parse-err">
					<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
						<path d="M12 9v4M12 17h.01"></path>
						<path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z"></path>
					</svg>
					yaml error
				</span>
			{:else if valid}
				<span class="valid">
					<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
						<polyline points="20 6 9 17 4 12"></polyline>
					</svg>
					valid
				</span>
			{/if}
			<span
				class="caption"
				title="Edit this YAML directly — changes sync into the visual builder, and builder edits render back here"
			>
				editable · synced with the visual builder
			</span>
		</div>
	</div>
	<div class="editor" bind:this={container}>
		{#if !ready}
			<div class="fallback">loading editor…</div>
		{/if}
	</div>
</div>

<style>
	.pane {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-width: 0;
		min-height: 0;
		background: var(--bg2);
	}

	.tabs {
		height: 38px;
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 2px;
		padding: 0 8px;
		border-bottom: 1px solid var(--border);
		background: var(--panel);
	}

	.tab {
		display: flex;
		align-items: center;
		gap: 8px;
		height: 30px;
		padding: 0 13px;
		border-radius: 7px 7px 0 0;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--text);
		border-bottom: 2px solid var(--accent);
		align-self: flex-end;
	}

	.status {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 12px;
		padding-right: 8px;
	}

	.valid {
		display: flex;
		align-items: center;
		gap: 6px;
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--green);
	}

	.parse-err {
		display: flex;
		align-items: center;
		gap: 6px;
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--red, #e5534b);
	}

	.caption {
		font: 500 11px 'IBM Plex Mono', monospace;
		color: var(--dim);
		cursor: help;
	}

	.editor {
		flex: 1;
		min-height: 0;
		position: relative;
		overflow: hidden;
	}

	/* CodeMirror fills the remaining height. */
	.editor :global(.cm-editor) {
		height: 100%;
	}

	.fallback {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		font: 500 12px 'IBM Plex Mono', monospace;
		color: var(--dim);
		pointer-events: none;
	}
</style>
