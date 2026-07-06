// Client-side YAML rendering of a flow definition for the builder's
// read-only YAML pane.
//
// Goal: byte-identical output to the server's GET /api/flows/:id/export,
// which is `id: <id>\n` + serde_yaml_ng::to_string(&FlowDefinition)
// (src/model/yaml.rs). serde_yaml_ng drives libyaml's emitter, so this
// module reimplements the relevant subset of its behavior:
//
// - block style throughout; sequence dashes at the SAME indent as their key
//   (serde_yaml does not indent block sequences), nested content +2 spaces;
// - empty collections emit flow style: `[]` / `{}`;
// - scalar style: plain when libyaml's block-context analysis allows it;
//   single-quoted when the string would otherwise parse as a non-string
//   scalar (null/bool/int/float forms) or when plain is not allowed;
//   double-quoted when it contains non-printables; literal block (`|`)
//   when it contains newlines;
// - struct field order matches the Rust serde derives; `config` is a
//   serde_json::Value whose Map is a BTreeMap (no preserve_order feature),
//   so all object keys inside `config` sort alphabetically.
//
// Verified byte-equal against live server exports for the design-doc
// example flow and an edge-case flow (colon-space values, YAML-1.1 bool
// words, trailing spaces, leading indicators, hex/float lookalikes,
// multi-line literal blocks, >80-column strings — serde_yaml_ng never
// wraps long scalars). See yaml.test.ts.

import { isParallel, type FlowDefinition, type TaskSpec } from '../api';

type YamlValue = string | number | boolean | null | YamlValue[] | { [key: string]: YamlValue };

/** Render the full export document: `id:` first, then the definition. */
export function flowToYaml(flowId: string, def: FlowDefinition): string {
	const lines: string[] = [];
	lines.push(`id: ${scalar(flowId)}`);
	emitMapEntries(orderedDefinition(def), 0, lines);
	return lines.join('\n') + '\n';
}

// ---------------------------------------------------------------------------
// Wire ordering (mirrors the serde field order in src/model/flow.rs)
// ---------------------------------------------------------------------------

function orderedDefinition(def: FlowDefinition): Record<string, YamlValue> {
	return {
		name: def.name,
		namespace: def.namespace,
		description: def.description,
		inputs: def.inputs.map((inp) => ({
			id: inp.id,
			type: inp.type,
			required: inp.required ?? false,
			...(inp.default !== undefined && inp.default !== null ? { default: inp.default } : {})
		})),
		variables: def.variables.map((v) => ({ id: v.id, value: v.value })),
		triggers: def.triggers.map((t) => ({
			id: t.id,
			type: t.type,
			cron: t.cron,
			timezone: t.timezone ?? 'UTC',
			catchup: t.catchup ?? 'latest',
			enabled: t.enabled ?? true
		})),
		tasks: def.tasks.map(orderedTask)
	};
}

function orderedTask(task: TaskSpec): Record<string, YamlValue> {
	if (isParallel(task)) {
		return {
			id: task.id,
			type: 'parallel',
			items: task.items,
			concurrency: task.concurrency,
			tasks: task.tasks.map(orderedTask),
			outputs: task.outputs.map((o) => ({ name: o.name, type: o.type, extract: o.extract }))
		};
	}
	return {
		id: task.id,
		type: task.type,
		...(task.retry !== undefined
			? {
					retry: {
						type: task.retry.type,
						max_attempts: task.retry.max_attempts,
						base_seconds: task.retry.base_seconds
					}
				}
			: {}),
		...(task.timeout_seconds !== undefined ? { timeout_seconds: task.timeout_seconds } : {}),
		on_error: task.on_error ?? 'fail',
		config: sortDeep(task.config as YamlValue),
		outputs: task.outputs.map((o) => ({ name: o.name, type: o.type, extract: o.extract }))
	};
}

/** serde_json without preserve_order sorts object keys alphabetically. */
function sortDeep(value: YamlValue): YamlValue {
	if (Array.isArray(value)) return value.map(sortDeep);
	if (value !== null && typeof value === 'object') {
		const out: Record<string, YamlValue> = {};
		for (const key of Object.keys(value).sort()) {
			out[key] = sortDeep((value as Record<string, YamlValue>)[key]);
		}
		return out;
	}
	return value;
}

// ---------------------------------------------------------------------------
// Block emitter
// ---------------------------------------------------------------------------

function pad(indent: number): string {
	return ' '.repeat(indent);
}

function isEmptyMap(v: YamlValue): boolean {
	return v !== null && typeof v === 'object' && !Array.isArray(v) && Object.keys(v).length === 0;
}

function emitMapEntries(map: Record<string, YamlValue>, indent: number, lines: string[]): void {
	for (const [key, value] of Object.entries(map)) {
		const keyText = `${pad(indent)}${scalar(key)}:`;
		if (Array.isArray(value)) {
			if (value.length === 0) {
				lines.push(`${keyText} []`);
			} else {
				lines.push(keyText);
				emitSeqEntries(value, indent, lines);
			}
		} else if (value !== null && typeof value === 'object') {
			if (isEmptyMap(value)) {
				lines.push(`${keyText} {}`);
			} else {
				lines.push(keyText);
				emitMapEntries(value as Record<string, YamlValue>, indent + 2, lines);
			}
		} else if (typeof value === 'string' && value.includes('\n')) {
			emitLiteralBlock(keyText, value, indent + 2, lines);
		} else {
			lines.push(`${keyText} ${scalar(value)}`);
		}
	}
}

/** Sequence dashes sit at the key's own indent (serde_yaml style). */
function emitSeqEntries(seq: YamlValue[], indent: number, lines: string[]): void {
	for (const item of seq) {
		if (Array.isArray(item)) {
			if (item.length === 0) {
				lines.push(`${pad(indent)}- []`);
			} else {
				lines.push(`${pad(indent)}-`);
				emitSeqEntries(item, indent + 2, lines);
			}
		} else if (item !== null && typeof item === 'object') {
			if (isEmptyMap(item)) {
				lines.push(`${pad(indent)}- {}`);
			} else {
				// First entry shares the dash line; the rest indent under it.
				const start = lines.length;
				emitMapEntries(item as Record<string, YamlValue>, indent + 2, lines);
				lines[start] = `${pad(indent)}- ${lines[start].slice(indent + 2)}`;
			}
		} else if (typeof item === 'string' && item.includes('\n')) {
			emitLiteralBlock(`${pad(indent)}-`, item, indent + 2, lines);
		} else {
			lines.push(`${pad(indent)}- ${scalar(item)}`);
		}
	}
}

function emitLiteralBlock(prefix: string, value: string, indent: number, lines: string[]): void {
	// Literal style cannot represent leading spaces on the FIRST line
	// without an indentation indicator; fall back to double quotes there.
	const body = value.replace(/\n+$/, '');
	const trailing = value.length - body.length;
	if (body.startsWith(' ') || body === '') {
		lines.push(`${prefix} ${doubleQuote(value)}`);
		return;
	}
	const chomp = trailing === 0 ? '|-' : trailing === 1 ? '|' : '|+';
	lines.push(`${prefix} ${chomp}`);
	for (const line of body.split('\n')) {
		lines.push(line === '' ? '' : `${pad(indent)}${line}`);
	}
	for (let i = 1; i < trailing; i++) lines.push('');
}

// ---------------------------------------------------------------------------
// Scalar rendering
// ---------------------------------------------------------------------------

function scalar(value: string | number | boolean | null): string {
	if (value === null) return 'null';
	if (typeof value === 'boolean') return value ? 'true' : 'false';
	if (typeof value === 'number') return numberScalar(value);
	return stringScalar(value);
}

function numberScalar(n: number): string {
	if (Number.isInteger(n)) return String(n);
	if (!Number.isFinite(n)) return n > 0 ? '.inf' : Number.isNaN(n) ? '.nan' : '-.inf';
	return String(n);
}

function stringScalar(s: string): string {
	if (hasSpecialCharacters(s)) return doubleQuote(s);
	if (parsesAsNonString(s) || !plainAllowed(s)) return singleQuote(s);
	return s;
}

/** Non-printable characters force double-quoted style. */
function hasSpecialCharacters(s: string): boolean {
	// Control chars other than \n (newlines select literal style upstream).
	// eslint-disable-next-line no-control-regex
	return /[\u0000-\u0008\u000b-\u001f\u007f]/.test(s);
}

/**
 * Would this string parse as a YAML null/bool/int/float? serde_yaml quotes
 * such strings (single-quoted) so they round-trip as strings.
 */
function parsesAsNonString(s: string): boolean {
	if (s === '' || s === '~') return true;
	if (/^(null|Null|NULL)$/.test(s)) return true;
	if (/^(true|True|TRUE|false|False|FALSE)$/.test(s)) return true;
	if (/^[-+]?[0-9]+$/.test(s)) return true;
	if (/^0x[0-9a-fA-F]+$/.test(s) || /^0o[0-7]+$/.test(s)) return true;
	if (/^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$/.test(s) && /[0-9]/.test(s))
		return true;
	if (/^[-+]?(\.inf|\.Inf|\.INF)$/.test(s) || /^(\.nan|\.NaN|\.NAN)$/.test(s)) return true;
	return false;
}

/**
 * libyaml block-context plain-scalar analysis (yaml_emitter_analyze_scalar),
 * reduced to what single-line strings need.
 */
function plainAllowed(s: string): boolean {
	if (s === '') return false;
	if (s[0] === ' ' || s[0] === '\t' || s.endsWith(' ') || s.endsWith('\t')) return false;
	const first = s[0];
	if ('#,[]{}&*!|>\'"%@`'.includes(first)) return false;
	const followedByWs = s.length === 1 || s[1] === ' ' || s[1] === '\t';
	if ((first === '-' || first === '?' || first === ':') && followedByWs) return false;
	for (let i = 1; i < s.length; i++) {
		const c = s[i];
		const nextWs = i + 1 === s.length || s[i + 1] === ' ' || s[i + 1] === '\t';
		if (c === ':' && nextWs) return false;
		if (c === '#' && (s[i - 1] === ' ' || s[i - 1] === '\t')) return false;
		if (c === '\t') return false;
	}
	return true;
}

function singleQuote(s: string): string {
	return `'${s.replace(/'/g, "''")}'`;
}

function doubleQuote(s: string): string {
	let out = '"';
	for (const ch of s) {
		switch (ch) {
			case '"':
				out += '\\"';
				break;
			case '\\':
				out += '\\\\';
				break;
			case '\n':
				out += '\\n';
				break;
			case '\t':
				out += '\\t';
				break;
			case '\r':
				out += '\\r';
				break;
			default: {
				const code = ch.codePointAt(0)!;
				if (code < 0x20 || code === 0x7f) {
					out += '\\x' + code.toString(16).toUpperCase().padStart(2, '0');
				} else if (code === 0x85) {
					out += '\\N';
				} else if (code === 0xa0) {
					out += '\\_';
				} else if (code === 0x2028) {
					out += '\\L';
				} else if (code === 0x2029) {
					out += '\\P';
				} else {
					out += ch;
				}
			}
		}
	}
	return out + '"';
}
