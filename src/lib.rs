//! Orchestrator library: everything the `orchestrator` binary is built from.
//!
//! The crate ships as a single binary, but the modules live in a lib target so
//! integration tests (`tests/`) can use them directly and so contributors get
//! rustdoc for the plugin API (`plugins::TaskPlugin`).

pub mod api;
pub mod config;
pub mod db;
pub mod engine;
pub mod expr;
pub mod model;
pub mod plugins;
pub mod scheduler;
pub mod secrets;
pub mod ui;
pub mod worker;
