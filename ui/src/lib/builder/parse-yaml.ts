// YAML -> FlowDefinition parser for the writable YAML editor.
//
// The inverse of yaml.ts (flowToYaml): given editor text, produce the
// internal $state FlowDefinition, or a list of syntax problems for editor
// markers. Two concerns only, kept separate:
//
//   1. Is the text valid YAML? — the `yaml` package (eemeli/yaml) reports
//      syntax errors with 1-based line/column via the Document API.
//   2. Is the parsed value structurally loadable as a flow definition? — a
//      tolerant mapping that fills sensible defaults for missing fields and
//      rejects only a top-level shape the builder cannot represent at all
//      (not a mapping, or a collection field that is a bare primitive).
//
// SEMANTIC validity (unknown plugin types, dangling refs, bad cron, …) is
// NOT checked here — the server's POST /flows/validate owns that. The top
// level `id` field is deliberately ignored: the flow id is immutable and is
// never mapped into the definition.

import { parseDocument, type YAMLError } from 'yaml';
import type { FlowDefinition } from '../api';
import { initDefinition } from './defs';

/** A syntax/structure problem with 1-based positions for editor diagnostics. */
export interface YamlProblem {
	message: string;
	startLine: number;
	startCol: number;
	endLine: number;
	endCol: number;
}

export interface ParseResult {
	def: FlowDefinition | null;
	errors: YamlProblem[];
}

const COLLECTION_FIELDS = ['inputs', 'variables', 'env', 'triggers', 'tasks'] as const;

/**
 * Parse editor text into a FlowDefinition (or syntax/structure problems).
 * Never throws.
 */
export function yamlToDefinition(text: string): ParseResult {
	let doc;
	try {
		doc = parseDocument(text, { prettyErrors: false });
	} catch (e) {
		// parseDocument itself does not throw for malformed YAML (it collects
		// errors), but guard anyway so a pathological input can never crash.
		return { def: null, errors: [atStart(e instanceof Error ? e.message : String(e))] };
	}

	if (doc.errors.length > 0) {
		return { def: null, errors: doc.errors.map(problemFromError) };
	}

	let raw: unknown;
	try {
		raw = doc.toJS();
	} catch (e) {
		return { def: null, errors: [atStart(e instanceof Error ? e.message : String(e))] };
	}

	// Empty document (blank / comments only) is not a usable definition.
	if (raw === null || raw === undefined) {
		return {
			def: null,
			errors: [atStart('flow definition must be a mapping with name, tasks, …')]
		};
	}

	if (!isPlainObject(raw)) {
		return {
			def: null,
			errors: [atStart('flow definition must be a mapping with name, tasks, …')]
		};
	}

	// A collection field that is present but a bare primitive (e.g. `tasks: 5`)
	// is unusable — the builder cannot coerce it into a sequence.
	for (const field of COLLECTION_FIELDS) {
		const value = raw[field];
		if (value !== null && value !== undefined && !Array.isArray(value)) {
			return {
				def: null,
				errors: [atStart(`flow definition field '${field}' must be a sequence`)]
			};
		}
	}

	// Tolerant normalization into the wire shape, then the shared initDefinition
	// mapping (single source of truth for the internal $state shape). The top
	// level `id` is intentionally NOT copied. String-ish scalar fields default
	// the way emptyDefinition would.
	const wire: FlowDefinition = {
		name: asString(raw.name, ''),
		namespace: asString(raw.namespace, 'default'),
		description: asString(raw.description, ''),
		inputs: asArray(raw.inputs),
		variables: asArray(raw.variables),
		env: asArray(raw.env).filter((v): v is string => typeof v === 'string'),
		triggers: asArray(raw.triggers),
		tasks: asArray(raw.tasks)
	} as FlowDefinition;

	try {
		return { def: initDefinition(wire), errors: [] };
	} catch (e) {
		// Deeply malformed inner shapes (e.g. an input that is not a mapping)
		// could still trip the mapping; surface as a structure problem rather
		// than throwing.
		return { def: null, errors: [atStart(e instanceof Error ? e.message : String(e))] };
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function isPlainObject(v: unknown): v is Record<string, unknown> {
	return v !== null && typeof v === 'object' && !Array.isArray(v);
}

function asString(v: unknown, fallback: string): string {
	return typeof v === 'string' ? v : fallback;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function asArray(v: unknown): any[] {
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	return Array.isArray(v) ? (v as any[]) : [];
}

/** A problem anchored at the start of the document (line 1, col 1). */
function atStart(message: string): YamlProblem {
	return { message, startLine: 1, startCol: 1, endLine: 1, endCol: 2 };
}

/** Map a yaml YAMLError to a 1-based editor-ready problem. */
function problemFromError(err: YAMLError): YamlProblem {
	// Drop the trailing "\n\n<source snippet>" the library appends; keep the
	// human-readable first line (which itself ends with "at line X, column Y").
	const message = err.message.split('\n\n')[0].trim();
	const linePos = err.linePos;
	if (linePos && linePos[0]) {
		const start = linePos[0];
		const end = linePos[1] ?? start;
		return {
			message,
			startLine: start.line,
			startCol: start.col,
			endLine: end.line,
			endCol: end.col > start.col || end.line > start.line ? end.col : start.col + 1
		};
	}
	return atStart(message);
}
