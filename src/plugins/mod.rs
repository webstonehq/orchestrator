//! The plugin registry and host.
//!
//! A task type is a *plugin*: an external binary bundle discovered from a
//! plugins directory (see [`external`]). Each bundle's `plugin.json` carries a
//! declarative manifest — the frontend renders every task inspector from these
//! (served at `/api/plugins`), so a new task type needs zero frontend changes.
//!
//! The engine talks to plugins only through [`PluginEntry`] (via the stdio
//! protocol): [`PluginEntry::execute`] runs a task (spawn-per-task for
//! `oneshot`, a long-lived process for `persistent`) and [`PluginEntry::validate`]
//! checks a config at save time. There is no in-process plugin code. Rust
//! plugins are authored against the `plugin-sdk` crate; see
//! `docs/plans/2026-07-07-unified-plugin-protocol-design.md`.

pub mod external;
pub mod persistent;
#[doc(hidden)]
pub mod testing;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Severity of a log line emitted by a task during execution.
///
/// Rendered in run views with the shown uppercase tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Routine progress ("INFO").
    Info,
    /// Successful step ("OK").
    Ok,
    /// Something unexpected but non-fatal ("WARN").
    Warn,
    /// An error ("ERR").
    Err,
    /// Verbose diagnostics ("DBG").
    Dbg,
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LogLevel::Info => "INFO",
            LogLevel::Ok => "OK",
            LogLevel::Warn => "WARN",
            LogLevel::Err => "ERR",
            LogLevel::Dbg => "DBG",
        };
        f.write_str(s)
    }
}

/// Everything a plugin gets from the engine while executing one task run.
///
/// Plugins should log progress through [`TaskContext::log`] or the
/// level-named shorthands ([`TaskContext::info`], [`TaskContext::ok`], …) —
/// lines show up live in the run view — and honor [`TaskContext::cancel`] so
/// that a run can be stopped promptly. The engine owns retries and timeouts —
/// plugins should *not* implement their own.
pub struct TaskContext {
    /// Database id of the flow run this task belongs to.
    pub run_id: i64,
    /// The task's id within the flow (e.g. `"scrape_minutes_index"`).
    pub task_id: String,
    /// Cooperative cancellation. Long-running work (network calls, loops)
    /// must race against this token and bail out when it fires.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Sink for log lines; call through [`TaskContext::log`] and friends.
    logger: Box<dyn Fn(LogLevel, String) + Send + Sync>,
}

impl TaskContext {
    /// Create a context for one task run. The engine is the only caller in
    /// production; tests pass a capturing or no-op `logger`.
    pub fn new(
        run_id: i64,
        task_id: impl Into<String>,
        cancel: tokio_util::sync::CancellationToken,
        logger: Box<dyn Fn(LogLevel, String) + Send + Sync>,
    ) -> Self {
        Self {
            run_id,
            task_id: task_id.into(),
            cancel,
            logger,
        }
    }

    /// Emit a log line for this task run at the given level.
    pub fn log(&self, level: LogLevel, msg: impl Into<String>) {
        (self.logger)(level, msg.into());
    }

    /// Log at [`LogLevel::Info`].
    pub fn info(&self, msg: impl Into<String>) {
        self.log(LogLevel::Info, msg);
    }

    /// Log at [`LogLevel::Ok`].
    pub fn ok(&self, msg: impl Into<String>) {
        self.log(LogLevel::Ok, msg);
    }

    /// Log at [`LogLevel::Warn`].
    pub fn warn(&self, msg: impl Into<String>) {
        self.log(LogLevel::Warn, msg);
    }

    /// Log at [`LogLevel::Err`].
    pub fn err(&self, msg: impl Into<String>) {
        self.log(LogLevel::Err, msg);
    }

    /// Log at [`LogLevel::Dbg`].
    pub fn dbg(&self, msg: impl Into<String>) {
        self.log(LogLevel::Dbg, msg);
    }
}

/// Error returned by `PluginEntry::execute`.
///
/// `retryable` tells the engine whether re-running the task could plausibly
/// succeed (e.g. connection refused, HTTP 5xx) or not (bad config, HTTP 4xx).
/// The engine consults it together with the task's retry policy.
#[derive(Debug, Clone)]
pub struct TaskError {
    /// Human-readable description, shown in run views.
    pub message: String,
    /// Whether the engine may retry this task.
    pub retryable: bool,
}

impl TaskError {
    /// A non-retryable error (bad input, definitive rejection).
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }

    /// A retryable error (transient infrastructure trouble).
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TaskError {}

// The manifest and capability wire types are the shared plugin contract; they
// live in `plugin-sdk` (which plugins depend on) and are re-exported here so the
// rest of the app keeps referring to `crate::plugins::{PluginManifest, …}`.
pub use plugin_sdk::{FieldSpec, Lifecycle, PluginCapability, PluginManifest, Widget};

/// How a registered plugin's process is driven.
pub(crate) enum Executor {
    /// Spawned fresh for each task.
    Oneshot,
    /// One long-lived process multiplexing many concurrent requests.
    Persistent(persistent::PersistentPlugin),
}

/// A registered task type: its manifest plus how to run and validate it. Built
/// only from `plugin.json` bundles — there are no in-process plugins.
pub struct PluginEntry {
    manifest: PluginManifest,
    version: Option<String>,
    supports_validate: bool,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    term_grace: Duration,
    validate_timeout: Duration,
    executor: Executor,
}

impl PluginEntry {
    /// The plugin's declarative manifest.
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// The plugin's advertised version, if any.
    pub fn version(&self) -> Option<String> {
        self.version.clone()
    }

    /// Execute one task with a fully-rendered `config`, streaming logs to `ctx`
    /// and honoring `ctx.cancel`. Dispatches by lifecycle.
    pub async fn execute(
        &self,
        ctx: &TaskContext,
        config: serde_json::Value,
    ) -> Result<serde_json::Value, TaskError> {
        match &self.executor {
            Executor::Persistent(mgr) => {
                let req = persistent::RequestPayload {
                    run_id: ctx.run_id,
                    task_id: ctx.task_id.clone(),
                    config,
                };
                mgr.execute(req, ctx).await
            }
            Executor::Oneshot => {
                external::oneshot_execute(
                    &self.program,
                    &self.args,
                    &self.cwd,
                    self.term_grace,
                    ctx,
                    config,
                )
                .await
            }
        }
    }

    /// Validate an authored `config` at save time: schema-derived checks, plus a
    /// one-shot `<binary> validate` call when the bundle opts in.
    pub fn validate(&self, config: &serde_json::Value) -> Vec<String> {
        let mut errs = external::validate_against_manifest(&self.manifest, config);
        if self.supports_validate {
            match external::run_plugin_validate(
                &self.program,
                &self.args,
                &self.cwd,
                config,
                self.validate_timeout,
            ) {
                Ok(mut plugin_errs) => errs.append(&mut plugin_errs),
                Err(reason) => tracing::warn!(
                    type_id = %self.manifest.type_id,
                    reason,
                    "plugin validate failed; using schema checks only"
                ),
            }
        }
        errs
    }
}

/// Registry of task plugins, keyed by `type_id`, discovered from bundle
/// directories. The engine's sole handle to plugins.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, PluginEntry>,
}

impl PluginRegistry {
    /// An empty registry. Plugins are added via [`PluginRegistry::load_external`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Discover and register plugin bundles from a directory. Malformed,
    /// unsupported, or colliding bundles are skipped with a warning — discovery
    /// never fails the process. Absent `dir` is a no-op.
    pub fn load_external(&mut self, dir: &Path) {
        for entry in external::load_bundles(dir) {
            match self.plugins.entry(entry.manifest.type_id.clone()) {
                std::collections::btree_map::Entry::Occupied(e) => {
                    tracing::warn!(type_id = %e.key(), "skipping plugin: type_id already registered");
                }
                std::collections::btree_map::Entry::Vacant(e) => {
                    tracing::info!(type_id = %e.key(), "loaded plugin");
                    e.insert(entry);
                }
            }
        }
    }

    /// Register a pre-built entry (used by tests / bundle staging).
    pub fn insert(&mut self, entry: PluginEntry) {
        self.plugins.insert(entry.manifest.type_id.clone(), entry);
    }

    /// Look up a plugin by its `type_id`.
    pub fn get(&self, type_id: &str) -> Option<&PluginEntry> {
        self.plugins.get(type_id)
    }

    /// All plugin manifests, in stable order (sorted by `type_id`).
    pub fn manifests(&self) -> Vec<PluginManifest> {
        self.plugins.values().map(|e| e.manifest.clone()).collect()
    }

    /// Capabilities of every registered plugin, in stable order — what a worker
    /// advertises so the server knows which task types this node can run.
    pub fn capabilities(&self) -> Vec<PluginCapability> {
        self.plugins
            .values()
            .map(|e| PluginCapability {
                type_id: e.manifest.type_id.clone(),
                version: e.version.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn log_level_display_tags() {
        let tags: Vec<String> = [
            LogLevel::Info,
            LogLevel::Ok,
            LogLevel::Warn,
            LogLevel::Err,
            LogLevel::Dbg,
        ]
        .iter()
        .map(|l| l.to_string())
        .collect();
        assert_eq!(tags, vec!["INFO", "OK", "WARN", "ERR", "DBG"]);
    }

    #[test]
    fn widget_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(Widget::Keyvalue).unwrap(),
            Value::String("keyvalue".to_string())
        );
        assert_eq!(
            serde_json::to_value(Widget::Select).unwrap(),
            Value::String("select".to_string())
        );
    }

    #[test]
    fn field_spec_omits_null_default_and_none_options() {
        let spec = FieldSpec {
            key: "k".to_string(),
            label: "K".to_string(),
            widget: Widget::Text,
            required: false,
            default: Value::Null,
            help: String::new(),
            options: None,
            min: None,
            max: None,
            template: false,
        };
        let v = serde_json::to_value(&spec).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("default"));
        assert!(!obj.contains_key("options"));
        assert!(!obj.contains_key("min"));
        assert!(!obj.contains_key("max"));
    }
}
