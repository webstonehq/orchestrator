/**
 * CodeMirror completion for `{{ }}` template expressions in the YAML editor.
 *
 * The visual builder's ExprPicker (see ../picker) is scoped per field — it
 * knows which task the cursor is in and only offers upstream outputs. The raw
 * YAML editor has no reliable notion of the cursor's task, so this offers a
 * GLOBAL scope: every input, variable, secret and task output (including
 * parallel children) is a candidate, grouped by origin. Candidates and their
 * labels come straight from pickerGroups(), so the two UIs stay in lockstep.
 */

import type { Completion, CompletionContext, CompletionResult } from '@codemirror/autocomplete';
import type { EditorView } from '@codemirror/view';
import { isParallel, type FlowDefinition } from '../api';
import { pickerGroups, type PickerGroup } from '../picker';

/** Every `{{ }}` reference available anywhere in the flow, grouped by origin. */
export function expressionGroups(def: FlowDefinition, secretNames: string[]): PickerGroup[] {
	const upstreamTasks: { id: string; outputs: { name: string; type: string }[] }[] = [];
	for (const task of def.tasks) {
		upstreamTasks.push({
			id: task.id,
			outputs: task.outputs.map((o) => ({ name: o.name, type: o.type }))
		});
		if (isParallel(task)) {
			for (const child of task.tasks) {
				upstreamTasks.push({
					id: child.id,
					outputs: child.outputs.map((o) => ({ name: o.name, type: o.type }))
				});
			}
		}
	}
	return pickerGroups({
		inputs: def.inputs,
		variables: def.variables,
		secretNames,
		upstreamTasks
	});
}

/**
 * Pure decision for inserting a reference, normalizing to `{{ ref }}`:
 *   - add the opening space when typed tight against `{{`
 *   - close with ` }}` unless the expression is already closed ahead
 * `charBefore` is the single char before the replaced range; `textAfter` is the
 * rest of the line after it. `cursorOffset` is where to place the cursor
 * relative to the insertion start — right after the ref, so a filter (e.g.
 * ` | dateAdd(...)`) can follow.
 */
export function refInsertion(
	charBefore: string,
	textAfter: string,
	value: string
): { insert: string; cursorOffset: number } {
	const prefix = charBefore === '{' ? ' ' : '';
	const suffix = /^\s*}}/.test(textAfter) ? '' : ' }}';
	return { insert: prefix + value + suffix, cursorOffset: prefix.length + value.length };
}

function applyRef(value: string) {
	return (view: EditorView, _completion: Completion, from: number, to: number) => {
		const doc = view.state.doc;
		const before = from > 0 ? doc.sliceString(from - 1, from) : '';
		const after = doc.sliceString(to, doc.lineAt(to).to);
		const { insert, cursorOffset } = refInsertion(before, after, value);
		view.dispatch({
			changes: { from, to, insert },
			selection: { anchor: from + cursorOffset }
		});
	};
}

/**
 * Build a completion source that fires only inside an open `{{ … ` (no
 * intervening `}}`). Auto-activates after typing `{{` and filters candidates by
 * the path fragment under the cursor. `getGroups` is called per query so it
 * always reflects the live flow definition. Returns null everywhere else, so
 * plain YAML and the schema-driven key completion are unaffected.
 */
export function makeExpressionCompletion(getGroups: () => PickerGroup[]) {
	return (context: CompletionContext): CompletionResult | null => {
		const open = context.matchBefore(/\{\{[^{}]*$/);
		if (!open) return null;
		const word = /[\w.]*$/.exec(open.text.slice(2))?.[0] ?? '';
		const options: Completion[] = [];
		for (const group of getGroups()) {
			for (const item of group.items) {
				options.push({
					label: item.value,
					detail: item.type,
					type: item.value.endsWith(')') ? 'function' : 'variable',
					section: group.title,
					apply: applyRef(item.value)
				});
			}
		}
		if (options.length === 0) return null;
		return { from: context.pos - word.length, options, validFor: /^[\w.]*$/ };
	};
}
