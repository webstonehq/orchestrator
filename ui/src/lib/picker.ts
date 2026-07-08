/**
 * Builds the grouped item list for the expression picker (ExprPicker).
 *
 * Group order: INPUTS, OUTPUTS · <task> (one per upstream task, in the order
 * given), VARIABLES, ENV, SECRETS, FUNCTIONS, then ITERATION and
 * PRIOR STEPS · <id> when applicable. Empty groups are skipped.
 *
 * Scoping (which tasks count as "upstream", which siblings are "prior") is
 * the caller's responsibility — this function renders exactly what it is
 * given, in the order given.
 */

export interface PickerItem {
	label: string;
	type: string;
	value: string;
}

export interface PickerGroup {
	title: string;
	items: PickerItem[];
}

export interface PickerOptions {
	inputs: { id: string; type: string }[];
	variables: { id: string }[];
	/** Env var names declared in the flow's `env:` list (referenced as `env.<NAME>`). */
	envNames: string[];
	secretNames: string[];
	/** Tasks earlier in the graph, already scoped by the caller. */
	upstreamTasks: { id: string; outputs: { name: string; type: string }[] }[];
	/** Inside a parallel child: adds the ITERATION group. */
	iteration?: boolean;
	/** Prior sibling child steps (inside a parallel child), already scoped. */
	priorSteps?: { id: string; outputs: { name: string; type: string }[] }[];
}

export function pickerGroups(opts: PickerOptions): PickerGroup[] {
	const groups: PickerGroup[] = [];

	if (opts.inputs.length > 0) {
		groups.push({
			title: 'INPUTS',
			items: opts.inputs.map((inp) => ({
				label: `inputs.${inp.id}`,
				type: inp.type,
				value: `inputs.${inp.id}`
			}))
		});
	}

	for (const task of opts.upstreamTasks) {
		if (task.outputs.length === 0) continue;
		groups.push({
			title: `OUTPUTS · ${task.id}`,
			items: task.outputs.map((o) => ({
				label: o.name,
				type: o.type,
				value: `outputs.${task.id}.${o.name}`
			}))
		});
	}

	if (opts.variables.length > 0) {
		groups.push({
			title: 'VARIABLES',
			items: opts.variables.map((v) => ({
				label: `vars.${v.id}`,
				type: 'STRING',
				value: `vars.${v.id}`
			}))
		});
	}

	if (opts.envNames.length > 0) {
		groups.push({
			title: 'ENV',
			items: opts.envNames.map((name) => ({
				label: `env.${name}`,
				type: 'STRING',
				value: `env.${name}`
			}))
		});
	}

	if (opts.secretNames.length > 0) {
		groups.push({
			title: 'SECRETS',
			items: opts.secretNames.map((name) => ({
				label: `secrets.${name}`,
				type: 'STRING',
				value: `secrets.${name}`
			}))
		});
	}

	groups.push({
		title: 'FUNCTIONS',
		items: [{ label: 'now()', type: 'DATE', value: 'now()' }]
	});

	if (opts.iteration) {
		groups.push({
			title: 'ITERATION',
			items: [
				{ label: 'taskrun.value', type: 'JSON', value: 'taskrun.value' },
				{ label: 'taskrun.value.id', type: 'STRING', value: 'taskrun.value.id' }
			]
		});
	}

	for (const step of opts.priorSteps ?? []) {
		if (step.outputs.length === 0) continue;
		groups.push({
			title: `PRIOR STEPS · ${step.id}`,
			items: step.outputs.map((o) => ({
				label: o.name,
				type: o.type,
				value: `outputs.${step.id}.${o.name}`
			}))
		});
	}

	return groups;
}
