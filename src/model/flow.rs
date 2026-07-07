//! Flow definition types and their wire (serde) representation.
//!
//! JSON and YAML share one shape (the "FlowDefinition JSON" contract in
//! `docs/plans/2026-07-05-orchestrator-implementation.md`). A [`TaskDef`]
//! serializes *flat*: the task's `id` and discriminating `type` key sit next
//! to the kind-specific fields —
//! `{"id": "...", "type": "parallel", "items": ..., ...}` for a fan-out task,
//! `{"id": "...", "type": "<plugin type_id>", "config": ..., ...}` for a
//! plugin task. Unknown fields are rejected everywhere with an error naming
//! the offending field.

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// A complete flow definition: metadata, inputs, variables, triggers, and
/// the ordered task list. This is the unit stored per flow revision and
/// round-tripped through YAML export/import.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowDefinition {
    /// Display name; must be non-empty (validation rule, not a serde rule).
    pub name: String,
    /// Grouping namespace; defaults to `"default"`.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Free-form description; defaults to empty.
    #[serde(default)]
    pub description: String,
    /// Execution queue label (`[a-z][a-z0-9_]*`). Runs of this flow are
    /// dispatched to a worker subscribed to this queue; the built-in default
    /// `"local"` is served by the server's own in-process executor. Omitted
    /// from serialization when `"local"` so existing flows round-trip
    /// byte-identically.
    #[serde(default = "default_queue", skip_serializing_if = "is_local_queue")]
    pub queue: String,
    /// Run parameters supplied at trigger time.
    #[serde(default)]
    pub inputs: Vec<InputDef>,
    /// Flow-scoped constants referenced as `{{ vars.<id> }}`.
    #[serde(default)]
    pub variables: Vec<VarDef>,
    /// Schedule triggers.
    #[serde(default)]
    pub triggers: Vec<TriggerDef>,
    /// Tasks, executed sequentially in order.
    #[serde(default)]
    pub tasks: Vec<TaskDef>,
}

/// A declared run input, referenced as `{{ inputs.<id> }}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputDef {
    /// Input id (`[a-z][a-z0-9_]*`).
    pub id: String,
    /// Value type, parsed from the raw string at trigger time.
    #[serde(rename = "type")]
    pub input_type: InputType,
    /// Whether a value must be supplied when the flow runs.
    #[serde(default)]
    pub required: bool,
    /// Default value as a template string, rendered at trigger time
    /// (`ARRAY`/`JSON` defaults are JSON text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// Value type of an input or a task output. Serialized UPPERCASE
/// (`"STRING"`, `"ARRAY"`, ...).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum InputType {
    /// Plain string.
    String,
    /// JSON array.
    Array,
    /// ISO-8601 date/datetime string.
    Date,
    /// Integer.
    Int,
    /// Boolean.
    Boolean,
    /// Arbitrary JSON value.
    Json,
}

/// A flow-scoped constant, referenced as `{{ vars.<id> }}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VarDef {
    /// Variable id (`[a-z][a-z0-9_]*`).
    pub id: String,
    /// Literal value.
    pub value: String,
}

/// A schedule trigger (`type: "schedule"` is the only kind in v1).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TriggerDef {
    /// Trigger id (`[a-z][a-z0-9_]*`).
    pub id: String,
    /// Trigger kind; must be `"schedule"`.
    #[serde(rename = "type")]
    pub trigger_type: String,
    /// 5-field cron expression.
    pub cron: String,
    /// IANA timezone name the cron fires in; defaults to `"UTC"`.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// What to do with fire times missed while the scheduler was down.
    #[serde(default)]
    pub catchup: Catchup,
    /// Whether the trigger currently fires.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Catch-up policy for missed schedule fires. Serialized lowercase.
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Catchup {
    /// Skip everything missed.
    None,
    /// Run once for the most recent missed fire (the default).
    #[default]
    Latest,
    /// Run once per missed fire.
    All,
}

/// One task in a flow: an id plus either a plugin invocation or a parallel
/// fan-out. Serializes flat with a discriminating `type` key (see module
/// docs); implemented with custom `Serialize`/`Deserialize`.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskDef {
    /// Task id (`[a-z][a-z0-9_]*`), unique across the whole flow including
    /// parallel children.
    pub id: String,
    /// What the task does.
    pub kind: TaskKind,
}

/// The two task shapes: a plugin invocation or a parallel fan-out.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskKind {
    /// Run a registered [`crate::plugins::TaskPlugin`].
    Plugin(PluginTask),
    /// Fan out child tasks over an items array.
    Parallel(ParallelTask),
}

/// A plugin-backed task.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginTask {
    /// The plugin's registry key (wire field `type`), e.g. `"http.request"`.
    pub type_id: String,
    /// Optional retry policy; absent means a single attempt.
    pub retry: Option<RetryPolicy>,
    /// Per-attempt timeout in seconds (1..=3600); engine default when absent.
    pub timeout_seconds: Option<u64>,
    /// What the run does when this task fails for good.
    pub on_error: OnError,
    /// Plugin config as authored (template strings unrendered).
    pub config: Value,
    /// Values extracted from the plugin result for downstream tasks.
    pub outputs: Vec<OutputDef>,
}

/// Exponential-backoff retry policy (`type: "exponential"` is the only kind
/// in v1). Attempt *n* sleeps `base_seconds * 2^(n-1)` before retrying.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// Policy kind; must be `"exponential"`.
    #[serde(rename = "type")]
    pub retry_type: String,
    /// Total attempts including the first (1..=20).
    pub max_attempts: u32,
    /// Backoff base in seconds (1..=3600).
    pub base_seconds: u64,
}

/// What happens to the run when a task exhausts its attempts. Serialized
/// lowercase.
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    /// Fail the whole run (the default).
    #[default]
    Fail,
    /// Record the failure and keep going.
    Continue,
}

/// A named value extracted from a task's result, referenced downstream as
/// `{{ outputs.<task>.<name> }}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputDef {
    /// Output name (`[a-z][a-z0-9_]*`), unique per task.
    pub name: String,
    /// Declared value type.
    #[serde(rename = "type")]
    pub output_type: InputType,
    /// Dotted path (optional `[n]` indices) into the task result; must start
    /// with `result`, e.g. `result.body.ids`.
    pub extract: String,
}

/// A parallel fan-out task: renders `items` to an array and runs the child
/// chain once per item, at most `concurrency` at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct ParallelTask {
    /// Template that must render to an array at run time.
    pub items: String,
    /// Max children in flight (1..=256).
    pub concurrency: u32,
    /// Child tasks, run as a chain per item; all must be plugin tasks
    /// (enforced by validation, not by the type system).
    pub tasks: Vec<TaskDef>,
    /// Outputs the parallel task itself exposes downstream (extracted from
    /// the aggregated fan-out result).
    pub outputs: Vec<OutputDef>,
}

/// `TaskDef` serializes as a flat discriminated union whose real shape depends
/// on the plugins registered at *run* time, so it can't be derived at compile
/// time. This placeholder puts a `Task` entry in `$defs` that
/// [`crate::model::schema::flow_json_schema`] overwrites with the assembled
/// `oneOf`; the derive on [`FlowDefinition`] references it as `#/$defs/Task`.
impl schemars::JsonSchema for TaskDef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Task".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "orchestrator::model::flow::TaskDef".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({})
    }
}

fn default_namespace() -> String {
    "default".to_string()
}

/// The built-in execution queue served by the server's own in-process
/// executor. A flow with no explicit `queue` runs here, preserving the
/// single-binary default.
pub const LOCAL_QUEUE: &str = "local";

fn default_queue() -> String {
    LOCAL_QUEUE.to_string()
}

/// Whether a queue label is the default `"local"` (used to omit it from
/// serialization so default-queue flows round-trip byte-identically).
fn is_local_queue(queue: &str) -> bool {
    queue == LOCAL_QUEUE
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_true() -> bool {
    true
}

fn default_config() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Wire-side fields of a plugin task (everything except `id` and `type`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginWire {
    #[serde(default)]
    retry: Option<RetryPolicy>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    on_error: OnError,
    #[serde(default = "default_config")]
    config: Value,
    #[serde(default)]
    outputs: Vec<OutputDef>,
}

/// Wire-side fields of a parallel task (everything except `id` and `type`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParallelWire {
    items: String,
    concurrency: u32,
    #[serde(default)]
    tasks: Vec<TaskDef>,
    #[serde(default)]
    outputs: Vec<OutputDef>,
}

impl Serialize for TaskDef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.kind {
            TaskKind::Plugin(p) => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("id", &self.id)?;
                map.serialize_entry("type", &p.type_id)?;
                if let Some(retry) = &p.retry {
                    map.serialize_entry("retry", retry)?;
                }
                if let Some(timeout) = &p.timeout_seconds {
                    map.serialize_entry("timeout_seconds", timeout)?;
                }
                map.serialize_entry("on_error", &p.on_error)?;
                map.serialize_entry("config", &p.config)?;
                map.serialize_entry("outputs", &p.outputs)?;
                map.end()
            }
            TaskKind::Parallel(p) => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("id", &self.id)?;
                map.serialize_entry("type", "parallel")?;
                map.serialize_entry("items", &p.items)?;
                map.serialize_entry("concurrency", &p.concurrency)?;
                map.serialize_entry("tasks", &p.tasks)?;
                map.serialize_entry("outputs", &p.outputs)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for TaskDef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Value::Object(mut fields) = value else {
            return Err(D::Error::custom(
                "task must be a mapping with `id` and `type` fields",
            ));
        };
        let id = match fields.remove("id") {
            Some(Value::String(s)) => s,
            Some(_) => return Err(D::Error::custom("task field `id` must be a string")),
            None => return Err(D::Error::custom("task is missing required field `id`")),
        };
        let type_id = match fields.remove("type") {
            Some(Value::String(s)) => s,
            Some(_) => {
                return Err(D::Error::custom(format!(
                    "task `{id}`: field `type` must be a string"
                )));
            }
            None => {
                return Err(D::Error::custom(format!(
                    "task `{id}` is missing required field `type`"
                )));
            }
        };
        let rest = Value::Object(fields);
        let kind = if type_id == "parallel" {
            let wire: ParallelWire = serde_json::from_value(rest)
                .map_err(|e| D::Error::custom(format!("task `{id}`: {e}")))?;
            TaskKind::Parallel(ParallelTask {
                items: wire.items,
                concurrency: wire.concurrency,
                tasks: wire.tasks,
                outputs: wire.outputs,
            })
        } else {
            let wire: PluginWire = serde_json::from_value(rest)
                .map_err(|e| D::Error::custom(format!("task `{id}`: {e}")))?;
            TaskKind::Plugin(PluginTask {
                type_id,
                retry: wire.retry,
                timeout_seconds: wire.timeout_seconds,
                on_error: wire.on_error,
                config: wire.config,
                outputs: wire.outputs,
            })
        };
        Ok(TaskDef { id, kind })
    }
}
