//! Definition-time validation of a [`FlowDefinition`].
//!
//! [`validate`] checks everything that can be known before a run starts:
//! id shapes and uniqueness, cron/timezone syntax, plugin configs (delegated
//! to each plugin), retry/timeout/concurrency bounds, output extract paths,
//! and — most importantly — that every `{{ ... }}` template parses and only
//! references things that exist in its scope.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::expr;
use crate::plugins::{PluginCapability, PluginRegistry};

use super::{FlowDefinition, LOCAL_QUEUE, OutputDef, ParallelTask, PluginTask, TaskKind};

/// One validation problem: a JSON-ish `path` into the definition (e.g.
/// `tasks[2].config.url` or `triggers[0].cron`) plus a human-readable
/// message.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ValidationErr {
    /// Where in the definition the problem is.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

/// THE canonical cron parser configuration: strict 5-field expressions,
/// seconds and years disallowed.
///
/// Definition validation and the scheduler (`crate::scheduler`) must accept
/// exactly the same cron grammar — both obtain their parser from this
/// function, so the configuration can never drift. Change it here and only
/// here.
pub fn cron_parser() -> croner::parser::CronParser {
    croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Disallowed)
        .year(croner::parser::Year::Disallowed)
        .build()
}

/// Immutable inputs shared by the whole validation pass.
struct Ctx<'a> {
    inputs: HashSet<&'a str>,
    vars: HashSet<&'a str>,
    registry: &'a PluginRegistry,
}

/// What templates may reference at a given point in the flow.
struct Scope<'a> {
    ctx: &'a Ctx<'a>,
    /// Outputs of tasks that finished earlier in the scope chain:
    /// task id -> declared output names.
    outputs: &'a HashMap<String, HashSet<String>>,
    /// `{{ taskrun.* }}` is only available inside parallel children.
    taskrun_allowed: bool,
}

/// Validate a flow definition against the plugin registry. Returns all
/// problems found (empty = valid).
pub fn validate(def: &FlowDefinition, registry: &PluginRegistry) -> Vec<ValidationErr> {
    let mut errs = Vec::new();

    if def.name.trim().is_empty() {
        push(&mut errs, "name", "name must not be empty");
    }

    if !is_valid_id(&def.queue) {
        push(
            &mut errs,
            "queue",
            format!(
                "invalid queue `{}`: must match [a-z][a-z0-9_]* (max 64 chars)",
                def.queue
            ),
        );
    }

    let input_ids = validate_inputs(def, &mut errs);
    let var_ids = validate_variables(def, &mut errs);
    validate_triggers(def, &mut errs);
    validate_schedule_input_defaults(def, &mut errs);

    let ctx = Ctx {
        inputs: input_ids,
        vars: var_ids,
        registry,
    };
    validate_input_defaults(def, &ctx, &mut errs);
    validate_tasks(def, &ctx, &mut errs);

    errs
}

/// Worker-coverage findings for a flow routed to a non-`local` queue, split by
/// how they should be handled.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    /// Blocking: a worker on the queue runs a *different version* of the plugin
    /// than the one the server validated against. The authored config may not
    /// match, so the save is refused.
    pub errors: Vec<ValidationErr>,
    /// Advisory: no live worker on the queue currently provides the plugin.
    /// Never blocks — a worker may connect later.
    pub warnings: Vec<ValidationErr>,
}

/// Cross-check a flow's plugin tasks against the workers on its queue.
///
/// - `reference` is the server's authoritative `type_id -> version` map (from
///   its own plugin registry) — the version whose manifest authored the config.
/// - `queue_caps` is what workers on `def.queue` advertise (with versions).
///
/// For each plugin task on a non-`local` queue: if no worker provides its
/// `type_id`, that's a **warning**; if a worker provides it but at a different
/// version than `reference`, that's a blocking **error** (version skew). The
/// `local` queue is served in-process and is skipped entirely.
pub fn coverage_report(
    def: &FlowDefinition,
    reference: &HashMap<String, String>,
    queue_caps: &[PluginCapability],
) -> CoverageReport {
    let mut report = CoverageReport::default();
    if def.queue == LOCAL_QUEUE {
        return report;
    }

    // type_id -> versions advertised by workers on this queue.
    let mut on_queue: HashMap<&str, Vec<Option<&str>>> = HashMap::new();
    for cap in queue_caps {
        on_queue
            .entry(cap.type_id.as_str())
            .or_default()
            .push(cap.version.as_deref());
    }

    // At most one finding per type_id, anchored at its first use.
    let mut seen: HashSet<&str> = HashSet::new();
    for (path, p) in plugin_tasks(def) {
        if !seen.insert(p.type_id.as_str()) {
            continue;
        }
        match on_queue.get(p.type_id.as_str()) {
            None => push(
                &mut report.warnings,
                path,
                format!(
                    "no live worker on queue `{}` provides task type `{}`; \
                     runs will wait until one connects (or fail if it never does)",
                    def.queue, p.type_id
                ),
            ),
            Some(worker_versions) => {
                // Skew only when both sides know a version and they differ.
                if let Some(server_v) = reference.get(&p.type_id)
                    && let Some(worker_v) = worker_versions
                        .iter()
                        .flatten()
                        .find(|wv| **wv != server_v.as_str())
                {
                    push(
                        &mut report.errors,
                        path,
                        format!(
                            "plugin `{}` version mismatch: the server validated against `{}`, \
                             but a worker on queue `{}` runs `{}`; align the plugin versions",
                            p.type_id, server_v, def.queue, worker_v
                        ),
                    );
                }
            }
        }
    }
    report
}

/// Every plugin task in a flow (top-level and parallel children), each paired
/// with its definition path.
fn plugin_tasks(def: &FlowDefinition) -> Vec<(String, &PluginTask)> {
    let mut out = Vec::new();
    for (i, task) in def.tasks.iter().enumerate() {
        match &task.kind {
            TaskKind::Plugin(p) => out.push((format!("tasks[{i}]"), p)),
            TaskKind::Parallel(par) => {
                for (j, child) in par.tasks.iter().enumerate() {
                    if let TaskKind::Plugin(p) = &child.kind {
                        out.push((format!("tasks[{i}].tasks[{j}]"), p));
                    }
                }
            }
        }
    }
    out
}

/// Check input ids (shape + uniqueness) and return the declared set.
fn validate_inputs<'a>(def: &'a FlowDefinition, errs: &mut Vec<ValidationErr>) -> HashSet<&'a str> {
    let mut input_ids: HashSet<&str> = HashSet::new();
    for (i, input) in def.inputs.iter().enumerate() {
        check_id(&input.id, &format!("inputs[{i}].id"), errs);
        if !input_ids.insert(&input.id) {
            push(
                errs,
                format!("inputs[{i}].id"),
                format!("duplicate input id `{}`", input.id),
            );
        }
    }
    input_ids
}

/// Check variable ids (shape + uniqueness) and return the declared set.
fn validate_variables<'a>(
    def: &'a FlowDefinition,
    errs: &mut Vec<ValidationErr>,
) -> HashSet<&'a str> {
    let mut var_ids: HashSet<&str> = HashSet::new();
    for (i, var) in def.variables.iter().enumerate() {
        check_id(&var.id, &format!("variables[{i}].id"), errs);
        if !var_ids.insert(&var.id) {
            push(
                errs,
                format!("variables[{i}].id"),
                format!("duplicate variable id `{}`", var.id),
            );
        }
    }
    var_ids
}

/// Check trigger ids, kind, cron syntax (strict 5-field), and timezone.
fn validate_triggers(def: &FlowDefinition, errs: &mut Vec<ValidationErr>) {
    let mut trigger_ids: HashSet<&str> = HashSet::new();
    for (i, trigger) in def.triggers.iter().enumerate() {
        check_id(&trigger.id, &format!("triggers[{i}].id"), errs);
        if !trigger_ids.insert(&trigger.id) {
            push(
                errs,
                format!("triggers[{i}].id"),
                format!("duplicate trigger id `{}`", trigger.id),
            );
        }
        if trigger.trigger_type != "schedule" {
            push(
                errs,
                format!("triggers[{i}].type"),
                format!(
                    "trigger type must be `schedule`, got `{}`",
                    trigger.trigger_type
                ),
            );
        }
        if let Err(e) = cron_parser().parse(&trigger.cron) {
            push(
                errs,
                format!("triggers[{i}].cron"),
                format!("invalid cron expression `{}`: {e}", trigger.cron),
            );
        }
        if trigger.timezone.parse::<chrono_tz::Tz>().is_err() {
            push(
                errs,
                format!("triggers[{i}].timezone"),
                format!("unknown timezone `{}`", trigger.timezone),
            );
        }
    }
}

/// Scheduled runs supply no inputs, so with an enabled schedule trigger a
/// required input without a default would make every scheduled run fail at
/// start ("input `x` is required"). Catch that at definition time, flagged
/// on each enabled trigger.
fn validate_schedule_input_defaults(def: &FlowDefinition, errs: &mut Vec<ValidationErr>) {
    let missing: Vec<String> = def
        .inputs
        .iter()
        .filter(|input| input.required && input.default.is_none())
        .map(|input| format!("`{}`", input.id))
        .collect();
    if missing.is_empty() {
        return;
    }
    let (noun, verb) = if missing.len() == 1 {
        ("input", "has")
    } else {
        ("inputs", "have")
    };
    let names = missing.join(", ");
    for (i, trigger) in def.triggers.iter().enumerate() {
        if trigger.trigger_type == "schedule" && trigger.enabled {
            push(
                errs,
                format!("triggers[{i}]"),
                format!("scheduled runs will fail: required {noun} {names} {verb} no default"),
            );
        }
    }
}

/// Check input default templates. Defaults render at trigger time:
/// inputs/vars/secrets are in scope, task outputs and taskrun are not.
fn validate_input_defaults(def: &FlowDefinition, ctx: &Ctx<'_>, errs: &mut Vec<ValidationErr>) {
    let no_outputs = HashMap::new();
    let scope = Scope {
        ctx,
        outputs: &no_outputs,
        taskrun_allowed: false,
    };
    for (i, input) in def.inputs.iter().enumerate() {
        if let Some(default) = &input.default {
            check_template(default, &format!("inputs[{i}].default"), &scope, errs);
        }
    }
}

/// Check the task list: global id uniqueness (children included), per-task
/// rules, and template scoping in definition order.
fn validate_tasks(def: &FlowDefinition, ctx: &Ctx<'_>, errs: &mut Vec<ValidationErr>) {
    let mut task_ids: HashSet<String> = HashSet::new();
    let mut outputs_so_far: HashMap<String, HashSet<String>> = HashMap::new();
    for (i, task) in def.tasks.iter().enumerate() {
        let base = format!("tasks[{i}]");
        register_task_id(&task.id, &format!("{base}.id"), &mut task_ids, errs);
        match &task.kind {
            TaskKind::Plugin(p) => {
                let scope = Scope {
                    ctx,
                    outputs: &outputs_so_far,
                    taskrun_allowed: false,
                };
                validate_plugin_task(p, &base, &scope, errs);
                outputs_so_far.insert(task.id.clone(), output_names(&p.outputs));
            }
            TaskKind::Parallel(par) => {
                validate_parallel(par, &base, ctx, &outputs_so_far, &mut task_ids, errs);
                // Downstream tasks reference the parallel task's own outputs.
                outputs_so_far.insert(task.id.clone(), output_names(&par.outputs));
            }
        }
    }
}

fn validate_parallel(
    par: &ParallelTask,
    base: &str,
    ctx: &Ctx<'_>,
    outputs_so_far: &HashMap<String, HashSet<String>>,
    task_ids: &mut HashSet<String>,
    errs: &mut Vec<ValidationErr>,
) {
    let items_path = format!("{base}.items");
    if par.items.trim().is_empty() {
        push(errs, items_path, "items must not be empty");
    } else {
        // `items` renders before any child runs: parent scope, no taskrun.
        let scope = Scope {
            ctx,
            outputs: outputs_so_far,
            taskrun_allowed: false,
        };
        check_template(&par.items, &items_path, &scope, errs);
    }

    if !(1..=256).contains(&par.concurrency) {
        push(
            errs,
            format!("{base}.concurrency"),
            format!(
                "concurrency must be between 1 and 256, got {}",
                par.concurrency
            ),
        );
    }

    if par.tasks.is_empty() {
        push(
            errs,
            format!("{base}.tasks"),
            "parallel task must have at least one child task",
        );
    }

    // Children see the parent scope plus outputs of prior siblings, and may
    // use `taskrun.*`.
    let mut sibling_outputs = outputs_so_far.clone();
    for (j, child) in par.tasks.iter().enumerate() {
        let child_base = format!("{base}.tasks[{j}]");
        register_task_id(&child.id, &format!("{child_base}.id"), task_ids, errs);
        match &child.kind {
            TaskKind::Plugin(p) => {
                let scope = Scope {
                    ctx,
                    outputs: &sibling_outputs,
                    taskrun_allowed: true,
                };
                validate_plugin_task(p, &child_base, &scope, errs);
                sibling_outputs.insert(child.id.clone(), output_names(&p.outputs));
            }
            TaskKind::Parallel(_) => {
                push(
                    errs,
                    format!("{child_base}.type"),
                    "nested parallel tasks are not allowed",
                );
            }
        }
    }

    validate_outputs(&par.outputs, base, errs);
}

fn validate_plugin_task(
    p: &PluginTask,
    base: &str,
    scope: &Scope<'_>,
    errs: &mut Vec<ValidationErr>,
) {
    match scope.ctx.registry.get(&p.type_id) {
        None => push(
            errs,
            format!("{base}.type"),
            format!("unknown task type `{}`", p.type_id),
        ),
        Some(plugin) => {
            for message in plugin.validate(&p.config) {
                push(errs, format!("{base}.config"), message);
            }
        }
    }

    if let Some(retry) = &p.retry {
        if retry.retry_type != "exponential" {
            push(
                errs,
                format!("{base}.retry.type"),
                format!(
                    "retry type must be `exponential`, got `{}`",
                    retry.retry_type
                ),
            );
        }
        if !(1..=20).contains(&retry.max_attempts) {
            push(
                errs,
                format!("{base}.retry.max_attempts"),
                format!(
                    "max_attempts must be between 1 and 20, got {}",
                    retry.max_attempts
                ),
            );
        }
        if !(1..=3600).contains(&retry.base_seconds) {
            push(
                errs,
                format!("{base}.retry.base_seconds"),
                format!(
                    "base_seconds must be between 1 and 3600, got {}",
                    retry.base_seconds
                ),
            );
        }
    }

    if let Some(timeout) = p.timeout_seconds
        && !(1..=3600).contains(&timeout)
    {
        push(
            errs,
            format!("{base}.timeout_seconds"),
            format!("timeout_seconds must be between 1 and 3600, got {timeout}"),
        );
    }

    walk_config(&p.config, &format!("{base}.config"), scope, errs);
    validate_outputs(&p.outputs, base, errs);
}

/// Deep-walk a config value; every string is a template to check.
fn walk_config(
    value: &serde_json::Value,
    path: &str,
    scope: &Scope<'_>,
    errs: &mut Vec<ValidationErr>,
) {
    match value {
        serde_json::Value::String(s) => check_template(s, path, scope, errs),
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk_config(item, &format!("{path}[{i}]"), scope, errs);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                walk_config(item, &format!("{path}.{key}"), scope, errs);
            }
        }
        _ => {}
    }
}

/// Parse a template and check every referenced path against the scope.
fn check_template(template: &str, path: &str, scope: &Scope<'_>, errs: &mut Vec<ValidationErr>) {
    match expr::referenced_paths(template) {
        Err(e) => push(errs, path, format!("invalid template: {e}")),
        Ok(refs) => {
            for reference in refs {
                check_ref(&reference, path, scope, errs);
            }
        }
    }
}

/// Check one canonical reference path (filters already stripped, `now()`
/// already omitted) against the scope.
fn check_ref(ref_path: &str, err_path: &str, scope: &Scope<'_>, errs: &mut Vec<ValidationErr>) {
    let segs = path_segments(ref_path);
    match segs[0].as_str() {
        "inputs" => match segs.get(1) {
            None => push(
                errs,
                err_path,
                format!("incomplete reference `{ref_path}`: expected `inputs.<id>`"),
            ),
            Some(id) if !scope.ctx.inputs.contains(id.as_str()) => push(
                errs,
                err_path,
                format!("unknown input `{id}` in `{ref_path}`"),
            ),
            Some(_) => {}
        },
        "vars" => match segs.get(1) {
            None => push(
                errs,
                err_path,
                format!("incomplete reference `{ref_path}`: expected `vars.<id>`"),
            ),
            Some(id) if !scope.ctx.vars.contains(id.as_str()) => push(
                errs,
                err_path,
                format!("unknown variable `{id}` in `{ref_path}`"),
            ),
            Some(_) => {}
        },
        // Secret names are runtime-checked; here only the shape matters.
        "secrets" => {
            if segs.len() < 2 {
                push(
                    errs,
                    err_path,
                    format!("incomplete reference `{ref_path}`: expected `secrets.<NAME>`"),
                );
            }
        }
        "outputs" => {
            if segs.len() < 3 {
                push(
                    errs,
                    err_path,
                    format!("output references must be `outputs.<task>.<name>`, got `{ref_path}`"),
                );
                return;
            }
            let (task, name) = (&segs[1], &segs[2]);
            match scope.outputs.get(task.as_str()) {
                None => push(
                    errs,
                    err_path,
                    format!(
                        "`{ref_path}` references task `{task}` which is not an earlier task in scope"
                    ),
                ),
                Some(names) if !names.contains(name.as_str()) => push(
                    errs,
                    err_path,
                    format!("task `{task}` has no declared output `{name}`"),
                ),
                Some(_) => {}
            }
        }
        "taskrun" => {
            if !scope.taskrun_allowed {
                push(
                    errs,
                    err_path,
                    format!(
                        "`{ref_path}` is invalid here: taskrun is only available inside parallel child tasks"
                    ),
                );
                return;
            }
            match segs.get(1) {
                None => push(
                    errs,
                    err_path,
                    format!("incomplete reference `{ref_path}`: expected `taskrun.value`"),
                ),
                Some(field) if field != "value" => push(
                    errs,
                    err_path,
                    format!(
                        "unknown taskrun field `{field}` in `{ref_path}`: expected `taskrun.value`"
                    ),
                ),
                Some(_) => {}
            }
        }
        other => push(
            errs,
            err_path,
            format!("unknown reference root `{other}` in `{ref_path}`"),
        ),
    }
}

fn validate_outputs(outputs: &[OutputDef], base: &str, errs: &mut Vec<ValidationErr>) {
    let mut seen: HashSet<&str> = HashSet::new();
    for (k, output) in outputs.iter().enumerate() {
        let name_path = format!("{base}.outputs[{k}].name");
        if !is_valid_id(&output.name) {
            push(
                errs,
                name_path.clone(),
                format!(
                    "invalid output name `{}`: must match [a-z][a-z0-9_]* (max 64 chars)",
                    output.name
                ),
            );
        }
        if !seen.insert(&output.name) {
            push(
                errs,
                name_path,
                format!("duplicate output name `{}`", output.name),
            );
        }

        let extract_path = format!("{base}.outputs[{k}].extract");
        if output.extract.trim().is_empty() {
            push(errs, extract_path, "extract must not be empty");
            continue;
        }
        // Reuse the expression path grammar by parsing `{{ <extract> }}`.
        match expr::parse(&format!("{{{{ {} }}}}", output.extract)) {
            Err(e) => push(
                errs,
                extract_path,
                format!("invalid extract path: {}", e.message),
            ),
            Ok(segments) => match segments.as_slice() {
                [expr::Segment::Ref(re)] if re.filters.is_empty() => {
                    if path_segments(&re.path)[0] != "result" {
                        push(
                            errs,
                            extract_path,
                            format!("extract must start with `result`, got `{}`", re.path),
                        );
                    }
                }
                _ => push(
                    errs,
                    extract_path,
                    "extract must be a single dotted path without filters",
                ),
            },
        }
    }
}

fn register_task_id(
    id: &str,
    path: &str,
    task_ids: &mut HashSet<String>,
    errs: &mut Vec<ValidationErr>,
) {
    check_id(id, path, errs);
    if !task_ids.insert(id.to_string()) {
        push(errs, path, format!("duplicate task id `{id}`"));
    }
}

fn check_id(id: &str, path: &str, errs: &mut Vec<ValidationErr>) {
    if !is_valid_id(id) {
        push(
            errs,
            path,
            format!("invalid id `{id}`: must match [a-z][a-z0-9_]* (max 64 chars)"),
        );
    }
}

/// THE canonical id grammar: `[a-z][a-z0-9_]*`, at most 64 characters.
///
/// Used for every id in a definition (inputs, variables, triggers, tasks,
/// outputs) and by the API layer for flow ids, so the grammar cannot drift.
pub fn is_valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    let mut bytes = id.bytes();
    let first = bytes.next().expect("non-empty");
    first.is_ascii_lowercase()
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Split a canonical expression path into segment names, stripping `[n]`
/// indices (`outputs.a.ids[0].x` -> `["outputs", "a", "ids", "x"]`).
fn path_segments(path: &str) -> Vec<String> {
    path.split('.')
        .map(|seg| seg.split('[').next().unwrap_or_default().to_string())
        .collect()
}

fn output_names(outputs: &[OutputDef]) -> HashSet<String> {
    outputs.iter().map(|o| o.name.clone()).collect()
}

fn push(errs: &mut Vec<ValidationErr>, path: impl Into<String>, message: impl Into<String>) {
    errs.push(ValidationErr {
        path: path.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn def(value: serde_json::Value) -> FlowDefinition {
        serde_json::from_value(value).expect("valid flow definition")
    }

    /// A one-task `email`-queue flow using `slack.notify`.
    fn slack_flow() -> FlowDefinition {
        def(serde_json::json!({
            "name": "f", "queue": "email",
            "tasks": [{ "id": "t", "type": "slack.notify", "config": {} }],
        }))
    }

    fn reference(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn cap(type_id: &str, version: Option<&str>) -> PluginCapability {
        PluginCapability {
            type_id: type_id.to_string(),
            version: version.map(str::to_string),
        }
    }

    #[test]
    fn warns_when_plugin_missing_on_remote_queue() {
        let report = coverage_report(&slack_flow(), &reference(&[]), &[]);
        assert!(report.errors.is_empty());
        assert_eq!(report.warnings.len(), 1, "{report:?}");
        assert_eq!(report.warnings[0].path, "tasks[0]");
        assert!(report.warnings[0].message.contains("slack.notify"));
        assert!(report.warnings[0].message.contains("email"));
    }

    #[test]
    fn silent_when_queue_covers_the_plugin_at_same_version() {
        let report = coverage_report(
            &slack_flow(),
            &reference(&[("slack.notify", "1.0.0")]),
            &[cap("slack.notify", Some("1.0.0"))],
        );
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(report.warnings.is_empty(), "{report:?}");
    }

    #[test]
    fn version_skew_is_a_blocking_error() {
        let report = coverage_report(
            &slack_flow(),
            &reference(&[("slack.notify", "1.0.0")]),
            &[cap("slack.notify", Some("2.0.0"))],
        );
        assert!(report.warnings.is_empty(), "{report:?}");
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert_eq!(report.errors[0].path, "tasks[0]");
        let msg = &report.errors[0].message;
        assert!(msg.contains("1.0.0") && msg.contains("2.0.0"), "{msg}");
    }

    #[test]
    fn no_skew_when_versions_unknown() {
        // Worker advertises no version (e.g. a built-in) → can't be skew.
        let report = coverage_report(
            &slack_flow(),
            &reference(&[("slack.notify", "1.0.0")]),
            &[cap("slack.notify", None)],
        );
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(report.warnings.is_empty(), "{report:?}");
    }

    #[test]
    fn skips_local_queue_entirely() {
        // No queue → defaults to "local"; served in-process, no worker needed.
        let d = def(serde_json::json!({
            "name": "f",
            "tasks": [{ "id": "t", "type": "slack.notify", "config": {} }],
        }));
        let report = coverage_report(&d, &reference(&[]), &[]);
        assert!(report.errors.is_empty() && report.warnings.is_empty());
    }

    #[test]
    fn checks_parallel_children() {
        let d = def(serde_json::json!({
            "name": "f", "queue": "email",
            "tasks": [{
                "id": "p", "type": "parallel",
                "items": "{{ inputs.x }}", "concurrency": 2,
                "tasks": [{ "id": "c", "type": "slack.notify", "config": {} }],
            }],
        }));
        let report = coverage_report(&d, &reference(&[]), &[]);
        assert_eq!(report.warnings.len(), 1, "{report:?}");
        assert_eq!(report.warnings[0].path, "tasks[0].tasks[0]");
    }

    #[test]
    fn deduplicates_repeated_type_ids() {
        let d = def(serde_json::json!({
            "name": "f", "queue": "email",
            "tasks": [
                { "id": "a", "type": "slack.notify", "config": {} },
                { "id": "b", "type": "slack.notify", "config": {} },
            ],
        }));
        // Same missing plugin used twice → one finding, not two.
        let report = coverage_report(&d, &reference(&[]), &[]);
        assert_eq!(report.warnings.len(), 1, "{report:?}");
    }
}
