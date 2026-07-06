//! Run execution: sequential tasks, retries/timeouts, parallel fan-out,
//! cancellation, and event broadcasting.
//!
//! Everything here runs inside the Tokio task spawned by
//! [`crate::engine::Engine::start`]. Database write failures mid-run are
//! logged via `tracing` and execution continues best-effort — the run's
//! terminal status update is the one write that matters, and it is retried
//! nowhere: a dead database means a dead process anyway.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::db::{ItemUpdate, RunRow, RunStatusUpdate, TaskRunFinish, now_rfc3339};
use crate::expr;
use crate::model::{FlowDefinition, OnError, OutputDef, ParallelTask, PluginTask, TaskKind};
use crate::plugins::{LogLevel, TaskContext, TaskError};

use super::sink::RunSink;
use super::{Engine, RunEvent, json_type_name};

/// Interval between throttled `items` progress events.
const ITEMS_EVENT_INTERVAL: Duration = Duration::from_millis(500);

/// Default per-attempt plugin timeout when a task declares none.
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Terminal result of executing one task (or one fan-out child) through all
/// of its retry attempts.
enum TaskOutcome {
    Success {
        /// Redacted plugin result (or the fan-out `{"items": [...]}`).
        result: Value,
        /// Extracted outputs, keyed by output name.
        outputs: Map<String, Value>,
        /// Final attempt number.
        attempt: u32,
    },
    Failed {
        /// Redacted error message.
        error: String,
        /// Final attempt number (0 if no attempt ran).
        attempt: u32,
    },
    Canceled,
}

/// Terminal state of one fan-out item.
enum ItemOutcome {
    /// All children succeeded; carries the last child's (redacted) result.
    Success(Value),
    /// A child with `on_error: continue` failed — item dropped.
    Dropped,
    /// A child with `on_error: fail` failed — fails the whole parallel task.
    Failed(String),
    /// The run was canceled or the fan-out aborted before/while this item ran.
    Canceled,
}

/// Shared per-run environment threaded through execution.
struct RunScope<'a> {
    engine: &'a Engine,
    run_id: i64,
    run_token: &'a CancellationToken,
    /// Where persisted state and live events land (SQLite locally, or the
    /// wire on a worker). An `Arc` so the per-task logger closure (which is
    /// `'static`) can hold its own clone.
    sink: Arc<dyn RunSink>,
    /// Resolved secret values, for redaction.
    secrets: &'a [String],
}

impl RunScope<'_> {
    /// Append a (redacted) log line and broadcast it.
    fn log(&self, level: LogLevel, task: &str, message: &str) {
        emit_log(
            self.sink.as_ref(),
            self.run_id,
            self.secrets,
            level,
            task,
            message,
        );
    }

    fn send_task(&self, task_id: &str, status: &str, attempt: u32) {
        self.sink.emit(RunEvent::Task {
            task_id: task_id.to_string(),
            status: status.to_string(),
            attempt,
        });
    }

    fn send_run(&self, status: &str, finished_at: Option<String>, error: Option<String>) {
        self.sink.emit(RunEvent::Run {
            status: status.to_string(),
            finished_at,
            error,
        });
    }
}

/// Per-fan-out environment shared by all item futures of one parallel task.
struct FanoutScope<'a> {
    scope: &'a RunScope<'a>,
    par: &'a ParallelTask,
    /// Child of the run token; canceling it aborts the whole fan-out.
    fanout: &'a CancellationToken,
    emitter: &'a ItemsEmitter<'a>,
    task_run_id: i64,
}

/// Execute one run to a terminal status. Never panics on task failures; all
/// outcomes end in a `runs.status` update + `run` event.
pub(crate) async fn execute_run(
    engine: &Engine,
    run: RunRow,
    token: CancellationToken,
    sink: Arc<dyn RunSink>,
) {
    let run_id = run.id;

    let def_json = match load_definition(engine, &run) {
        Ok(json) => json,
        Err(msg) => return fail_before_start(sink.as_ref(), run_id, &msg, &[]),
    };
    let def: FlowDefinition = match serde_json::from_str(&def_json) {
        Ok(def) => def,
        Err(e) => {
            return fail_before_start(
                sink.as_ref(),
                run_id,
                &format!("invalid flow definition: {e}"),
                &[],
            );
        }
    };
    let secrets_map = match engine.resolve_secrets() {
        Ok(map) => map,
        Err(e) => {
            return fail_before_start(
                sink.as_ref(),
                run_id,
                &format!("failed to resolve secrets: {e}"),
                &[],
            );
        }
    };
    let secret_values: Vec<String> = secrets_map.values().cloned().collect();

    let stored_inputs: Value =
        serde_json::from_str(&run.inputs).unwrap_or_else(|_| Value::Object(Map::new()));
    let vars: Map<String, Value> = def
        .variables
        .iter()
        .map(|v| (v.id.clone(), Value::String(v.value.clone())))
        .collect();
    let secrets_obj: Map<String, Value> = secrets_map
        .into_iter()
        .map(|(name, value)| (name, Value::String(value)))
        .collect();

    // Finalize inputs with the full context: apply defaults that were not
    // resolvable at create time (e.g. scheduler-inserted `{}` inputs) and
    // late-bind secret-referencing template values. Never written back to
    // the run row.
    let inputs = match finalize_inputs(&def, stored_inputs, &vars, &secrets_obj) {
        Ok(inputs) => inputs,
        Err(msg) => return fail_before_start(sink.as_ref(), run_id, &msg, &secret_values),
    };

    let mut ctx = json!({
        "inputs": inputs,
        "vars": vars,
        "outputs": {},
        "secrets": secrets_obj,
    });

    let scope = RunScope {
        engine,
        run_id,
        run_token: &token,
        sink,
        secrets: &secret_values,
    };

    let started = Instant::now();
    db_try(
        run_id,
        scope.sink.update_run_status(
            run_id,
            RunStatusUpdate {
                status: "running",
                error: None,
                started_at: Some(&now_rfc3339()),
                finished_at: None,
            },
        ),
    );
    scope.send_run("running", None, None);
    scope.log(
        LogLevel::Info,
        "flow",
        &format!("execution #{run_id} started · trigger={}", run.trigger),
    );

    let mut run_error: Option<String> = None;
    let mut canceled = false;

    for task in &def.tasks {
        if canceled {
            break;
        }
        if run_error.is_some() {
            // A prior task failed the run: remaining tasks are skipped.
            db_try(
                run_id,
                scope.sink.upsert_task_run(run_id, &task.id, "skipped", 0),
            );
            scope.send_task(&task.id, "skipped", 0);
            continue;
        }
        if token.is_cancelled() {
            canceled = true;
            break;
        }

        let (outcome, on_error) = match &task.kind {
            TaskKind::Plugin(pt) => (
                run_top_level_plugin(&scope, &task.id, pt, &ctx).await,
                pt.on_error,
            ),
            // A parallel task that fails always fails the run.
            TaskKind::Parallel(par) => (
                run_parallel(&scope, &task.id, par, &ctx).await,
                OnError::Fail,
            ),
        };

        match outcome {
            TaskOutcome::Success {
                result,
                outputs,
                attempt,
            } => {
                let result_json = result.to_string();
                let outputs_json = Value::Object(outputs.clone()).to_string();
                db_try(
                    run_id,
                    scope.sink.finish_task_run(
                        run_id,
                        &task.id,
                        TaskRunFinish {
                            status: "success",
                            result: Some(&result_json),
                            outputs: Some(&outputs_json),
                            error: None,
                        },
                    ),
                );
                ctx["outputs"][task.id.as_str()] = Value::Object(outputs);
                scope.send_task(&task.id, "success", attempt);
                scope.log(LogLevel::Ok, &task.id, "task succeeded");
            }
            TaskOutcome::Failed { error, attempt } => {
                // Ensure the row exists even when no attempt ever ran (e.g.
                // unknown plugin type) — finish_task_run is a bare UPDATE.
                db_try(
                    run_id,
                    scope
                        .sink
                        .upsert_task_run(run_id, &task.id, "failed", i64::from(attempt)),
                );
                db_try(
                    run_id,
                    scope.sink.finish_task_run(
                        run_id,
                        &task.id,
                        TaskRunFinish {
                            status: "failed",
                            result: None,
                            outputs: None,
                            error: Some(&error),
                        },
                    ),
                );
                scope.send_task(&task.id, "failed", attempt);
                scope.log(LogLevel::Err, &task.id, &error);
                match on_error {
                    OnError::Fail => run_error = Some(format!("task {}: {error}", task.id)),
                    OnError::Continue => scope.log(
                        LogLevel::Warn,
                        "flow",
                        &format!("task {} failed; continuing (on_error=continue)", task.id),
                    ),
                }
            }
            TaskOutcome::Canceled => {
                db_try(
                    run_id,
                    scope.sink.finish_task_run(
                        run_id,
                        &task.id,
                        TaskRunFinish {
                            status: "canceled",
                            result: None,
                            outputs: None,
                            error: None,
                        },
                    ),
                );
                scope.send_task(&task.id, "canceled", 0);
                canceled = true;
            }
        }
    }

    let now = now_rfc3339();
    if canceled {
        db_try(
            run_id,
            scope.sink.update_run_status(
                run_id,
                RunStatusUpdate {
                    status: "canceled",
                    error: None,
                    started_at: None,
                    finished_at: Some(&now),
                },
            ),
        );
        scope.log(
            LogLevel::Warn,
            "flow",
            &format!("execution #{run_id} canceled"),
        );
        scope.send_run("canceled", Some(now), None);
    } else if let Some(error) = run_error {
        db_try(
            run_id,
            scope.sink.update_run_status(
                run_id,
                RunStatusUpdate {
                    status: "failed",
                    error: Some(&error),
                    started_at: None,
                    finished_at: Some(&now),
                },
            ),
        );
        scope.log(
            LogLevel::Err,
            "flow",
            &format!("execution #{run_id} failed: {error}"),
        );
        scope.send_run("failed", Some(now), Some(error));
    } else {
        db_try(
            run_id,
            scope.sink.update_run_status(
                run_id,
                RunStatusUpdate {
                    status: "success",
                    error: None,
                    started_at: None,
                    finished_at: Some(&now),
                },
            ),
        );
        scope.log(
            LogLevel::Ok,
            "flow",
            &format!(
                "execution #{run_id} succeeded in {:.1}s",
                started.elapsed().as_secs_f64()
            ),
        );
        scope.send_run("success", Some(now), None);
    }
}

/// Finalize the stored inputs at run start with the full `{vars, secrets}`
/// context:
/// - a declared input missing from the stored object gets its default
///   rendered/coerced/type-checked, or fails the run if it is required with
///   no default (scheduler-created runs insert `{}` and rely on this);
/// - a stored string value that is a template referencing `secrets.*`
///   (late-bound at create time) is rendered/coerced/type-checked here.
///
/// The finalized values are used for execution only — never written back to
/// the run row. Error messages may embed rendered values; the caller redacts.
fn finalize_inputs(
    def: &FlowDefinition,
    stored: Value,
    vars: &Map<String, Value>,
    secrets_obj: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut inputs = match stored {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let ctx = json!({ "vars": vars, "secrets": secrets_obj });
    for input in &def.inputs {
        match inputs.get(&input.id).cloned() {
            None => {
                if let Some(default) = &input.default {
                    let rendered = expr::render(default, &ctx).map_err(|e| {
                        format!("input \"{}\": default template error: {e}", input.id)
                    })?;
                    let value = super::parse_default(input.input_type, rendered)
                        .map_err(|msg| format!("input \"{}\": {msg}", input.id))?;
                    inputs.insert(input.id.clone(), value);
                } else if input.required {
                    return Err(format!(
                        "input \"{}\" is required and has no value",
                        input.id
                    ));
                }
            }
            Some(Value::String(s)) if super::references_secrets(&s) => {
                let rendered = expr::render(&s, &ctx)
                    .map_err(|e| format!("input \"{}\": template error: {e}", input.id))?;
                let value = super::parse_default(input.input_type, rendered)
                    .map_err(|msg| format!("input \"{}\": {msg}", input.id))?;
                inputs.insert(input.id.clone(), value);
            }
            Some(_) => {}
        }
    }
    Ok(inputs)
}

/// The definition the run was created against: its revision's, falling back
/// to the flow's current definition if the revision row is gone.
fn load_definition(engine: &Engine, run: &RunRow) -> Result<String, String> {
    match engine.db.get_revision(&run.flow_id, run.flow_rev) {
        Ok(Some(rev)) => Ok(rev.definition),
        Ok(None) => {
            tracing::warn!(
                run_id = run.id,
                flow_id = %run.flow_id,
                flow_rev = run.flow_rev,
                "revision row missing; falling back to the flow's current definition"
            );
            match engine.db.get_flow(&run.flow_id) {
                Ok(Some(flow)) => Ok(flow.definition),
                Ok(None) => Err(format!("flow \"{}\" not found", run.flow_id)),
                Err(e) => Err(format!("db error: {e}")),
            }
        }
        Err(e) => Err(format!("db error: {e}")),
    }
}

/// Mark a run failed before any task ran (setup errors: definition, secrets,
/// input finalization). `secrets` holds the resolved secret values for
/// redaction — pass `&[]` on paths reached before secrets resolve.
fn fail_before_start(sink: &dyn RunSink, run_id: i64, message: &str, secrets: &[String]) {
    let message = redact_str(message, secrets);
    let now = now_rfc3339();
    db_try(
        run_id,
        sink.update_run_status(
            run_id,
            RunStatusUpdate {
                status: "failed",
                error: Some(&message),
                started_at: Some(&now),
                finished_at: Some(&now),
            },
        ),
    );
    db_try(run_id, sink.append_log(run_id, "ERR", "flow", &message));
    sink.emit(RunEvent::Run {
        status: "failed".to_string(),
        finished_at: Some(now),
        error: Some(message),
    });
}

/// Run one top-level plugin task through its attempts, recording each attempt
/// on its `task_runs` row.
async fn run_top_level_plugin(
    scope: &RunScope<'_>,
    task_id: &str,
    pt: &PluginTask,
    ctx: &Value,
) -> TaskOutcome {
    execute_with_retries(scope, scope.run_token, pt, ctx, task_id, &mut |attempt| {
        db_try(
            scope.run_id,
            scope
                .sink
                .upsert_task_run(scope.run_id, task_id, "running", i64::from(attempt)),
        );
        scope.send_task(task_id, "running", attempt);
    })
    .await
}

/// Execute a plugin task through its retry policy inside `cancel`'s scope.
///
/// `record_attempt` is invoked before every attempt (1-based) so the caller
/// can stamp its `task_runs` or `task_run_items` row. Cancellation is decided
/// by the engine: after an attempt errors, the scope token — not the plugin's
/// error message — determines canceled-ness.
async fn execute_with_retries(
    scope: &RunScope<'_>,
    cancel: &CancellationToken,
    pt: &PluginTask,
    ctx: &Value,
    log_task: &str,
    record_attempt: &mut (dyn FnMut(u32) + Send),
) -> TaskOutcome {
    let Some(plugin) = scope.engine.registry.get(&pt.type_id) else {
        return TaskOutcome::Failed {
            error: format!("unknown task type \"{}\"", pt.type_id),
            attempt: 0,
        };
    };
    let max_attempts = pt.retry.as_ref().map_or(1, |r| r.max_attempts.max(1));
    let base_seconds = pt.retry.as_ref().map_or(0, |r| r.base_seconds);
    let timeout_secs = pt.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    let mut attempt: u32 = 1;
    loop {
        record_attempt(attempt);

        let config = match expr::render_config(&pt.config, ctx) {
            Ok(config) => config,
            Err(e) => {
                return TaskOutcome::Failed {
                    error: redact_str(&format!("config render failed: {e}"), scope.secrets),
                    attempt,
                };
            }
        };

        let task_ctx = TaskContext::new(
            scope.run_id,
            log_task,
            cancel.child_token(),
            make_logger(scope, log_task),
        );
        let attempt_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return TaskOutcome::Canceled,
            result = tokio::time::timeout(timeout, plugin.execute(&task_ctx, config)) => {
                match result {
                    Ok(inner) => inner,
                    Err(_) => Err(TaskError::retryable(format!("timed out after {timeout_secs}s"))),
                }
            }
        };

        match attempt_result {
            Ok(mut result) => {
                expr::redact(&mut result, scope.secrets);
                return match extract_outputs(&result, &pt.outputs) {
                    Ok(outputs) => TaskOutcome::Success {
                        result,
                        outputs,
                        attempt,
                    },
                    Err(error) => TaskOutcome::Failed { error, attempt },
                };
            }
            Err(err) => {
                // The engine decides canceled-ness from its own token; the
                // plugin's error message is display-only.
                if cancel.is_cancelled() {
                    return TaskOutcome::Canceled;
                }
                let error = redact_str(&err.message, scope.secrets);
                if err.retryable && attempt < max_attempts {
                    let exp = 1u64 << u64::from((attempt - 1).min(62));
                    let backoff = Duration::from_secs(base_seconds.saturating_mul(exp));
                    scope.log(
                        LogLevel::Warn,
                        log_task,
                        &format!(
                            "attempt {attempt}/{max_attempts} failed: {error} — retrying in {}s",
                            backoff.as_secs()
                        ),
                    );
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return TaskOutcome::Canceled,
                        _ = (scope.engine.sleeper)(backoff) => {}
                    }
                    attempt += 1;
                    continue;
                }
                return TaskOutcome::Failed { error, attempt };
            }
        }
    }
}

/// Extract every declared output from a (redacted) task result. Any missing
/// path fails the task with an error naming the output and path.
fn extract_outputs(result: &Value, defs: &[OutputDef]) -> Result<Map<String, Value>, String> {
    let mut outputs = Map::new();
    for def in defs {
        match extract_path(result, &def.extract) {
            Some(value) => {
                outputs.insert(def.name.clone(), value.clone());
            }
            None => {
                return Err(format!(
                    "output \"{}\": path \"{}\" not found in task result",
                    def.name, def.extract
                ));
            }
        }
    }
    Ok(outputs)
}

/// Walk an extract path (`result.body.ids[0]`) into a task result. The path
/// is rooted at `result`, which *is* the value: `"result"` alone selects the
/// whole result. Returns `None` on malformed paths or missing keys/indices.
fn extract_path<'a>(result: &'a Value, path: &str) -> Option<&'a Value> {
    let rest = path.strip_prefix("result")?;
    let mut current = result;
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'[' {
                    i += 1;
                }
                if start == i {
                    return None;
                }
                current = current.get(&rest[start..i])?;
            }
            b'[' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                if i == bytes.len() {
                    return None;
                }
                let index: usize = rest[start..i].parse().ok()?;
                i += 1;
                current = current.get(index)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Run a parallel fan-out task. The task fails if `items` doesn't render to
/// an array, if any child with `on_error: fail` exhausts its retries (which
/// also cancels outstanding items), or if an output path is missing; dropped
/// items (`on_error: continue`) do not fail the task.
async fn run_parallel(
    scope: &RunScope<'_>,
    task_id: &str,
    par: &ParallelTask,
    ctx: &Value,
) -> TaskOutcome {
    let task_run_id = match scope
        .sink
        .upsert_task_run(scope.run_id, task_id, "running", 1)
    {
        Ok(id) => id,
        Err(e) => {
            return TaskOutcome::Failed {
                error: format!("db error: {e}"),
                attempt: 1,
            };
        }
    };
    scope.send_task(task_id, "running", 1);

    let items_value = match expr::render(&par.items, ctx) {
        Ok(value) => value,
        Err(e) => {
            return TaskOutcome::Failed {
                error: redact_str(&format!("items render failed: {e}"), scope.secrets),
                attempt: 1,
            };
        }
    };
    let Value::Array(items) = items_value else {
        return TaskOutcome::Failed {
            error: format!(
                "items must render to an array, got {}",
                json_type_name(&items_value)
            ),
            attempt: 1,
        };
    };

    // Stored item payloads are redacted; execution uses the real values.
    let mut stored = items.clone();
    for item in &mut stored {
        expr::redact(item, scope.secrets);
    }
    if let Err(e) = scope.sink.insert_items(task_run_id, &stored) {
        return TaskOutcome::Failed {
            error: format!("db error: {e}"),
            attempt: 1,
        };
    }
    scope.log(
        LogLevel::Info,
        task_id,
        &format!(
            "fanning out over {} item(s), concurrency {}",
            items.len(),
            par.concurrency
        ),
    );

    let emitter = ItemsEmitter::new(scope, task_run_id, task_id);
    emitter.emit(true);

    let fanout = scope.run_token.child_token();
    let fs = FanoutScope {
        scope,
        par,
        fanout: &fanout,
        emitter: &emitter,
        task_run_id,
    };
    let concurrency = par.concurrency.max(1) as usize;
    let mut item_futures = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        item_futures.push(run_item(&fs, ctx, idx, item));
    }
    let outcomes: Vec<(usize, ItemOutcome)> = futures::stream::iter(item_futures)
        .buffer_unordered(concurrency)
        .collect()
        .await;
    emitter.emit(true);

    if scope.run_token.is_cancelled() {
        return TaskOutcome::Canceled;
    }

    // A failed item (child on_error=fail) fails the whole parallel task.
    if let Some((idx, error)) = outcomes
        .iter()
        .filter_map(|(idx, outcome)| match outcome {
            ItemOutcome::Failed(error) => Some((*idx, error)),
            _ => None,
        })
        .min_by_key(|(idx, _)| *idx)
    {
        return TaskOutcome::Failed {
            error: format!("item {idx} failed: {error}"),
            attempt: 1,
        };
    }

    // Task result: per-item final child results in idx order; dropped items
    // are null.
    let mut per_item = vec![Value::Null; items.len()];
    for (idx, outcome) in outcomes {
        if let ItemOutcome::Success(value) = outcome {
            per_item[idx] = value;
        }
    }
    let result = json!({ "items": per_item });
    match extract_outputs(&result, &par.outputs) {
        Ok(outputs) => TaskOutcome::Success {
            result,
            outputs,
            attempt: 1,
        },
        Err(error) => TaskOutcome::Failed { error, attempt: 1 },
    }
}

/// Run one fan-out item's child chain. Item context = run context + the
/// item as `taskrun.value`; child outputs accumulate per item (context is
/// cloned, so sibling items never see each other's outputs).
async fn run_item(
    fs: &FanoutScope<'_>,
    ctx: &Value,
    idx: usize,
    item: &Value,
) -> (usize, ItemOutcome) {
    let scope = fs.scope;
    let db = scope.sink.as_ref();
    let item_idx = idx as i64;

    // Queued items never start once the fan-out (or run) is canceled.
    if fs.fanout.is_cancelled() {
        db_try(
            scope.run_id,
            db.update_item(
                fs.task_run_id,
                item_idx,
                ItemUpdate {
                    status: "canceled",
                    attempt: 0,
                    finished_at: Some(&now_rfc3339()),
                    ..Default::default()
                },
            ),
        );
        fs.emitter.emit(false);
        return (idx, ItemOutcome::Canceled);
    }

    let mut item_ctx = ctx.clone();
    item_ctx["taskrun"] = json!({ "value": item });

    let mut started = false;
    let mut max_attempt: u32 = 1;
    let mut last_result = Value::Null;

    for child in &fs.par.tasks {
        let TaskKind::Plugin(cpt) = &child.kind else {
            // Validation forbids nested parallels; guard anyway.
            let error = format!("child task \"{}\" is not a plugin task", child.id);
            db_try(
                scope.run_id,
                db.update_item(
                    fs.task_run_id,
                    item_idx,
                    ItemUpdate {
                        status: "failed",
                        attempt: i64::from(max_attempt),
                        error: Some(&error),
                        finished_at: Some(&now_rfc3339()),
                        ..Default::default()
                    },
                ),
            );
            fs.fanout.cancel();
            return (idx, ItemOutcome::Failed(error));
        };
        let log_task = format!("{}[{idx}]", child.id);

        let outcome = execute_with_retries(
            scope,
            fs.fanout,
            cpt,
            &item_ctx,
            &log_task,
            &mut |attempt| {
                max_attempt = max_attempt.max(attempt);
                let started_at = (!started).then(now_rfc3339);
                started = true;
                db_try(
                    scope.run_id,
                    db.update_item(
                        fs.task_run_id,
                        item_idx,
                        ItemUpdate {
                            status: "running",
                            attempt: i64::from(attempt),
                            started_at: started_at.as_deref(),
                            ..Default::default()
                        },
                    ),
                );
            },
        )
        .await;

        match outcome {
            TaskOutcome::Success {
                result,
                outputs,
                attempt,
            } => {
                max_attempt = max_attempt.max(attempt);
                item_ctx["outputs"][child.id.as_str()] = Value::Object(outputs);
                last_result = result;
            }
            TaskOutcome::Canceled => {
                db_try(
                    scope.run_id,
                    db.update_item(
                        fs.task_run_id,
                        item_idx,
                        ItemUpdate {
                            status: "canceled",
                            attempt: i64::from(max_attempt),
                            finished_at: Some(&now_rfc3339()),
                            ..Default::default()
                        },
                    ),
                );
                fs.emitter.emit(false);
                return (idx, ItemOutcome::Canceled);
            }
            TaskOutcome::Failed { error, attempt } => {
                max_attempt = max_attempt.max(attempt);
                match cpt.on_error {
                    OnError::Continue => {
                        db_try(
                            scope.run_id,
                            db.update_item(
                                fs.task_run_id,
                                item_idx,
                                ItemUpdate {
                                    status: "dropped",
                                    attempt: i64::from(max_attempt),
                                    error: Some(&error),
                                    finished_at: Some(&now_rfc3339()),
                                    ..Default::default()
                                },
                            ),
                        );
                        scope.log(
                            LogLevel::Warn,
                            &log_task,
                            &format!("item {idx} dropped: {error}"),
                        );
                        fs.emitter.emit(false);
                        return (idx, ItemOutcome::Dropped);
                    }
                    OnError::Fail => {
                        db_try(
                            scope.run_id,
                            db.update_item(
                                fs.task_run_id,
                                item_idx,
                                ItemUpdate {
                                    status: "failed",
                                    attempt: i64::from(max_attempt),
                                    error: Some(&error),
                                    finished_at: Some(&now_rfc3339()),
                                    ..Default::default()
                                },
                            ),
                        );
                        scope.log(
                            LogLevel::Err,
                            &log_task,
                            &format!("item {idx} failed: {error}"),
                        );
                        // Abort the rest of the fan-out.
                        fs.fanout.cancel();
                        fs.emitter.emit(false);
                        return (idx, ItemOutcome::Failed(error));
                    }
                }
            }
        }
    }

    let result_json = last_result.to_string();
    db_try(
        scope.run_id,
        db.update_item(
            fs.task_run_id,
            item_idx,
            ItemUpdate {
                status: "success",
                attempt: i64::from(max_attempt),
                result: Some(&result_json),
                finished_at: Some(&now_rfc3339()),
                ..Default::default()
            },
        ),
    );
    fs.emitter.emit(false);
    (idx, ItemOutcome::Success(last_result))
}

/// Throttled `items` progress event emitter (≥500ms between events, plus
/// forced initial/final emissions).
struct ItemsEmitter<'a> {
    scope: &'a RunScope<'a>,
    task_run_id: i64,
    task_id: String,
    started: Instant,
    last: Mutex<Option<Instant>>,
}

impl<'a> ItemsEmitter<'a> {
    fn new(scope: &'a RunScope<'a>, task_run_id: i64, task_id: &str) -> Self {
        ItemsEmitter {
            scope,
            task_run_id,
            task_id: task_id.to_string(),
            started: Instant::now(),
            last: Mutex::new(None),
        }
    }

    fn emit(&self, force: bool) {
        {
            let mut last = self.last.lock().expect("items emitter lock poisoned");
            if !force && last.is_some_and(|t| t.elapsed() < ITEMS_EVENT_INTERVAL) {
                return;
            }
            *last = Some(Instant::now());
        }
        let agg = match self.scope.sink.item_aggregates(self.task_run_id) {
            Ok(agg) => agg,
            Err(e) => {
                tracing::error!(run_id = self.scope.run_id, "item_aggregates failed: {e}");
                return;
            }
        };
        let completed = (agg.success + agg.failed + agg.dropped) as f64;
        let elapsed = self.started.elapsed().as_secs_f64().max(f64::EPSILON);
        self.scope.sink.emit(RunEvent::Items {
            task_id: self.task_id.clone(),
            agg,
            throughput_per_sec: completed / elapsed,
        });
    }
}

/// Build the logger handed to a plugin's [`TaskContext`]: redacts, persists,
/// and broadcasts every line. Clones the sink `Arc` so the logger can outlive
/// the borrow of `scope` (it is `'static`).
fn make_logger(scope: &RunScope<'_>, task: &str) -> Box<dyn Fn(LogLevel, String) + Send + Sync> {
    let sink = Arc::clone(&scope.sink);
    let run_id = scope.run_id;
    let secrets = scope.secrets.to_vec();
    let task = task.to_string();
    Box::new(move |level, message| {
        emit_log(sink.as_ref(), run_id, &secrets, level, &task, &message);
    })
}

/// Redact, append to the `logs` table, and broadcast one log line.
fn emit_log(
    sink: &dyn RunSink,
    run_id: i64,
    secrets: &[String],
    level: LogLevel,
    task: &str,
    message: &str,
) {
    let message = redact_str(message, secrets);
    let level = level.to_string();
    match sink.append_log(run_id, &level, task, &message) {
        Ok(id) => {
            sink.emit(RunEvent::Log {
                id,
                ts: now_rfc3339(),
                level,
                task: task.to_string(),
                message,
            });
        }
        Err(e) => tracing::error!(run_id, "append_log failed: {e}"),
    }
}

/// Redact secret values out of a plain string.
fn redact_str(s: &str, secrets: &[String]) -> String {
    let mut value = Value::String(s.to_string());
    expr::redact(&mut value, secrets);
    match value {
        Value::String(s) => s,
        _ => unreachable!("redact never changes the JSON shape"),
    }
}

/// Log-and-continue for best-effort mid-run database writes.
fn db_try<T>(run_id: i64, result: Result<T, crate::db::DbError>) {
    if let Err(e) = result {
        tracing::error!(run_id, "db write failed during run execution: {e}");
    }
}
