//! Task plugin trait, registry, and built-in plugins.
//!
//! A task type in Orchestrator is a *plugin*: a single Rust module that
//! contributes both the execution code and a declarative UI manifest. The
//! frontend renders every task inspector from these manifests (served at
//! `/api/plugins`), so adding a new task type requires zero frontend changes:
//!
//! 1. Add a module under `src/plugins/` and implement [`TaskPlugin`].
//! 2. Register it in [`PluginRegistry::builtin`].
//!
//! That's it — the builder UI, YAML round-trip, validation plumbing, and run
//! views all come for free. See `docs/plans/2026-07-05-orchestrator-design.md`
//! §2 for the full rationale.

pub mod http;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

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

/// Error returned by [`TaskPlugin::execute`].
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

/// A task type: execution code plus a declarative config UI.
///
/// Implement this trait, register the plugin in [`PluginRegistry::builtin`],
/// and Orchestrator takes care of everything else — the task inspector is
/// rendered from [`TaskPlugin::manifest`], configs are checked with
/// [`TaskPlugin::validate`] at save time, and the engine calls
/// [`TaskPlugin::execute`] with a fully rendered config at run time.
#[async_trait::async_trait]
pub trait TaskPlugin: Send + Sync {
    /// Static metadata and the declarative config UI for this task type.
    ///
    /// Called freely and often; must be cheap and deterministic.
    fn manifest(&self) -> PluginManifest;

    /// Validate a task's config JSON as authored (template expressions still
    /// unrendered — do not try to interpret `{{ … }}` contents).
    ///
    /// Returns human-readable problems, one per message (e.g.
    /// `"url is required"`). An empty vec means the config is acceptable.
    fn validate(&self, config: &serde_json::Value) -> Vec<String>;

    /// Execute the task with a fully-rendered config (all `{{ … }}`
    /// expressions already resolved by the engine; rendered values keep
    /// their JSON types).
    ///
    /// Returns a JSON result the task's declared outputs extract from.
    /// Contract:
    /// - Honor `ctx.cancel`: stop work promptly when it fires and return an
    ///   error. The engine determines canceled-ness by consulting the
    ///   cancellation token after `execute` returns — the error a plugin
    ///   returns on cancellation is display-only (`"canceled"` by
    ///   convention).
    /// - Do not impose timeouts or retries — the engine owns both.
    /// - Set [`TaskError::retryable`] honestly; it drives the retry policy.
    async fn execute(
        &self,
        ctx: &TaskContext,
        config: serde_json::Value,
    ) -> Result<serde_json::Value, TaskError>;
}

/// Static description of a plugin, served as JSON at `/api/plugins`.
///
/// The frontend renders the task palette entry and the whole task inspector
/// from this — a plugin never ships frontend code.
#[derive(Serialize, Clone)]
pub struct PluginManifest {
    /// Unique task type id, e.g. `"http.request"`.
    pub type_id: String,
    /// Display name, e.g. `"HTTP request"`.
    pub label: String,
    /// One-line description shown in the task palette.
    pub description: String,
    /// Icon name from the built-in icon set.
    pub icon: String,
    /// Accent color (CSS hex).
    pub color: String,
    /// Config fields, rendered in order in the task inspector.
    pub fields: Vec<FieldSpec>,
}

/// One config field in a plugin manifest.
#[derive(Serialize, Clone)]
pub struct FieldSpec {
    /// Key in the task's `config` object.
    pub key: String,
    /// Display label.
    pub label: String,
    /// Which UI widget renders this field.
    pub widget: Widget,
    /// Whether the field must be set.
    pub required: bool,
    /// Default value (omitted from JSON when null).
    #[serde(skip_serializing_if = "Value::is_null")]
    pub default: Value,
    /// Help text shown under the field.
    pub help: String,
    /// Choices for [`Widget::Select`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Lower bound for [`Widget::Number`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Upper bound for [`Widget::Number`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Whether the field's value supports `{{ … }}` template expressions
    /// (the UI attaches the expression picker automatically).
    pub template: bool,
}

/// Widget vocabulary for [`FieldSpec::widget`] (v1).
///
/// Serialized lowercase (`"select"`, `"keyvalue"`, …).
#[derive(Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Widget {
    /// Cycling chip / dropdown; value is one of `options`.
    Select,
    /// Single-line expression editor; value is a template string.
    Template,
    /// Key + template-value rows; value is `[{key, value}]`.
    Keyvalue,
    /// Numeric stepper (`min` / `max`); value is a number.
    Number,
    /// Seconds input with unit hint; value is a number (seconds).
    Duration,
    /// On/off switch; value is a bool.
    Toggle,
    /// Plain string input, no expressions; value is a string.
    Text,
    /// Multi-line text (raw bodies, prompts); value is a template string.
    Code,
}

/// Compile-time registry of all task plugins, keyed by `type_id`.
pub struct PluginRegistry {
    plugins: BTreeMap<String, Arc<dyn TaskPlugin>>,
}

impl PluginRegistry {
    /// The registry with all built-in plugins. New plugins register here.
    pub fn builtin() -> Self {
        let mut registry = Self {
            plugins: BTreeMap::new(),
        };
        registry.register(Arc::new(http::HttpPlugin::new()));
        registry
    }

    /// Register a plugin.
    ///
    /// # Panics
    /// Panics if a plugin with the same `type_id` is already registered —
    /// that's a programmer error, caught at startup.
    pub fn register(&mut self, p: Arc<dyn TaskPlugin>) {
        let type_id = p.manifest().type_id;
        if self.plugins.contains_key(&type_id) {
            panic!("duplicate plugin type_id: {type_id}");
        }
        self.plugins.insert(type_id, p);
    }

    /// Look up a plugin by its `type_id`.
    pub fn get(&self, type_id: &str) -> Option<Arc<dyn TaskPlugin>> {
        self.plugins.get(type_id).cloned()
    }

    /// All plugin manifests, in stable order (sorted by `type_id`).
    pub fn manifests(&self) -> Vec<PluginManifest> {
        self.plugins.values().map(|p| p.manifest()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyPlugin {
        type_id: &'static str,
    }

    #[async_trait::async_trait]
    impl TaskPlugin for DummyPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                type_id: self.type_id.to_string(),
                label: "Dummy".to_string(),
                description: String::new(),
                icon: "box".to_string(),
                color: "#888888".to_string(),
                fields: vec![],
            }
        }

        fn validate(&self, _config: &Value) -> Vec<String> {
            vec![]
        }

        async fn execute(&self, _ctx: &TaskContext, _config: Value) -> Result<Value, TaskError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn builtin_registry_contains_http_request() {
        let registry = PluginRegistry::builtin();
        let plugin = registry.get("http.request").expect("http.request missing");
        assert_eq!(plugin.manifest().type_id, "http.request");
        assert!(registry.get("no.such.plugin").is_none());
    }

    #[test]
    fn manifests_are_sorted_by_type_id() {
        let mut registry = PluginRegistry::builtin();
        registry.register(Arc::new(DummyPlugin { type_id: "zz.last" }));
        registry.register(Arc::new(DummyPlugin {
            type_id: "aaa.first",
        }));
        let ids: Vec<String> = registry
            .manifests()
            .into_iter()
            .map(|m| m.type_id)
            .collect();
        assert_eq!(ids, vec!["aaa.first", "http.request", "zz.last"]);
    }

    #[test]
    #[should_panic(expected = "duplicate plugin type_id: http.request")]
    fn duplicate_register_panics() {
        let mut registry = PluginRegistry::builtin();
        registry.register(Arc::new(http::HttpPlugin::new()));
    }

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
