//! Flow definition model: types, definition-time validation, and YAML
//! round-trip.
//!
//! A flow is authored (in the UI or as YAML) as a [`FlowDefinition`]; JSON
//! and YAML share one wire shape, with YAML adding a top-level `id` key
//! ([`to_yaml`] / [`from_yaml`]). [`validate`] checks a definition against
//! the plugin registry before it is saved.

mod flow;
mod schema;
mod validate;
mod yaml;

use std::fmt;

pub use flow::{
    Catchup, FlowDefinition, InputDef, InputType, LOCAL_QUEUE, OnError, OutputDef, ParallelTask,
    PluginTask, RetryPolicy, TaskDef, TaskKind, TriggerDef, VarDef,
};
pub use schema::flow_json_schema;
pub use validate::{
    CoverageReport, ValidationErr, coverage_report, cron_parser, is_valid_id, validate,
};
pub use yaml::{from_yaml, to_yaml};

/// Error from model serialization/deserialization ([`to_yaml`] /
/// [`from_yaml`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError {
    /// Human-readable description of what went wrong.
    pub message: String,
}

impl ModelError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        ModelError {
            message: message.into(),
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}
