//! Orchestrator plugin SDK.
//!
//! Two things live here:
//!
//! - **The wire types** ([`PluginManifest`], [`FieldSpec`], [`Widget`],
//!   [`PluginCapability`], [`Lifecycle`]) — the manifest a plugin declares and
//!   the capability it advertises. The orchestrator depends on these to talk to
//!   plugins; they are the shared contract.
//! - **An authoring convenience** ([`Plugin`] + [`run`]) — implement `Plugin`
//!   in Rust and `run` speaks the stdio protocol for you (either lifecycle).
//!   Non-Rust plugins skip this and speak the protocol directly.
//!
//! ## Protocol
//!
//! Newline-delimited JSON. `stdout` is the protocol channel, `stderr` is free
//! diagnostics.
//!
//! **Oneshot** — the engine spawns the plugin per task, writes one request
//! (`{"mode":"execute"|"validate","run_id":..,"task_id":..,"config":{..}}`),
//! and reads events until a terminal one, then the process exits.
//!
//! **Persistent** — the plugin emits `{"type":"ready"}` on start, then services
//! many id-tagged requests (`{"id":N,"mode":"execute"|"cancel",..}`) until stdin
//! closes, streaming id-tagged events per request.
//!
//! Events: `{"type":"log","level":..,"message":..}`, and a terminal
//! `{"type":"result","value":..}` / `{"type":"error","message":..,"retryable":..}`
//! (persistent events also carry `"id"`). Oneshot `validate` replies
//! `{"type":"validation","errors":[..]}`.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
// Re-exported so plugin authors can build a token for [`Ctx::for_test`] without
// depending on `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Wire / manifest types
// ---------------------------------------------------------------------------

/// How the engine runs a plugin's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    /// Spawned fresh per task; cancellation = kill the process. The default.
    #[default]
    Oneshot,
    /// One long-lived process services many concurrent requests, multiplexed
    /// by id; cancellation is a message.
    Persistent,
}

/// Static description of a plugin, served to the UI at `/api/plugins`.
#[derive(Serialize, Deserialize, Clone)]
pub struct PluginManifest {
    /// Unique task type id, e.g. `"http.request"`.
    pub type_id: String,
    /// Display name.
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

/// One config field in a plugin manifest. Optional attributes carry
/// `#[serde(default)]` so plugins can author terse manifests.
#[derive(Serialize, Deserialize, Clone)]
pub struct FieldSpec {
    /// Key in the task's `config` object.
    pub key: String,
    /// Display label.
    pub label: String,
    /// Which UI widget renders this field.
    pub widget: Widget,
    /// Whether the field must be set.
    #[serde(default)]
    pub required: bool,
    /// Default value (omitted from JSON when null).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub default: Value,
    /// Help text shown under the field.
    #[serde(default)]
    pub help: String,
    /// Choices for [`Widget::Select`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Lower bound for [`Widget::Number`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Upper bound for [`Widget::Number`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Whether the field's value supports `{{ … }}` template expressions.
    #[serde(default)]
    pub template: bool,
}

/// Widget vocabulary for [`FieldSpec::widget`]. Serialized lowercase.
#[derive(Serialize, Deserialize, Clone)]
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

/// One task type a worker can execute, advertised to the server so it can warn
/// on missing coverage or version skew.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCapability {
    /// The plugin's `type_id`.
    pub type_id: String,
    /// The plugin's version, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// Authoring surface
// ---------------------------------------------------------------------------

/// Error returned by [`Plugin::execute`]. `retryable` tells the engine whether
/// re-running could plausibly succeed.
#[derive(Debug, Clone)]
pub struct PluginError {
    pub message: String,
    pub retryable: bool,
}

impl PluginError {
    /// A non-retryable error (bad input, definitive rejection).
    pub fn fatal(message: impl Into<String>) -> Self {
        Self { message: message.into(), retryable: false }
    }
    /// A retryable error (transient infrastructure trouble).
    pub fn retryable(message: impl Into<String>) -> Self {
        Self { message: message.into(), retryable: true }
    }
}

/// A task type authored in Rust. Implement this and pass an instance to [`run`].
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Static metadata and the declarative config UI.
    fn manifest(&self) -> PluginManifest;

    /// Validate a task's config as authored (templates unrendered). Returns
    /// human-readable problems; empty means acceptable. Only consulted when the
    /// bundle opts into validate mode.
    fn validate(&self, _config: &Value) -> Vec<String> {
        Vec::new()
    }

    /// Execute with a fully-rendered config. Log via `ctx`, honor
    /// `ctx.cancelled()`, and return the JSON result the task's outputs extract
    /// from.
    async fn execute(&self, ctx: &Ctx, config: Value) -> Result<Value, PluginError>;
}

/// Everything a plugin gets while executing one request: a log sink and
/// cooperative cancellation.
pub struct Ctx {
    id: Option<u64>,
    emitter: Emitter,
    cancel: CancellationToken,
}

impl Ctx {
    /// Emit a log line at `level` (`info`/`ok`/`warn`/`err`/`dbg`).
    pub fn log(&self, level: &str, message: impl Into<String>) {
        let mut event = json!({ "type": "log", "level": level, "message": message.into() });
        if let Some(id) = self.id {
            event["id"] = json!(id);
        }
        self.emitter.emit(&event);
    }
    /// Log at info level.
    pub fn info(&self, m: impl Into<String>) {
        self.log("info", m);
    }
    /// Log a success step.
    pub fn ok(&self, m: impl Into<String>) {
        self.log("ok", m);
    }
    /// Log a warning.
    pub fn warn(&self, m: impl Into<String>) {
        self.log("warn", m);
    }
    /// Log an error.
    pub fn err(&self, m: impl Into<String>) {
        self.log("err", m);
    }
    /// Log verbose diagnostics.
    pub fn dbg(&self, m: impl Into<String>) {
        self.log("dbg", m);
    }
    /// Whether this request has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
    /// Resolves when this request is cancelled. Race long work against it.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    /// A detached context for unit-testing a `Plugin`: log events are captured
    /// in the returned buffer, and `cancel` drives [`Ctx::cancelled`].
    pub fn for_test(cancel: CancellationToken) -> (Self, Arc<Mutex<Vec<u8>>>) {
        let (emitter, buf) = Emitter::buffer();
        (Self { id: None, emitter, cancel }, buf)
    }
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

/// Serialized sink for protocol events. Concurrent persistent requests share
/// one so their lines never interleave mid-line.
#[derive(Clone)]
pub struct Emitter {
    sink: Sink,
}

#[derive(Clone)]
enum Sink {
    Stdout,
    Buffer(Arc<Mutex<Vec<u8>>>),
}

impl Emitter {
    fn stdout() -> Self {
        Self { sink: Sink::Stdout }
    }

    /// A buffer-backed emitter and a handle to inspect what was written (tests).
    pub fn buffer() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        (Self { sink: Sink::Buffer(Arc::clone(&buf)) }, buf)
    }

    fn emit(&self, event: &Value) {
        let mut line = serde_json::to_vec(event).expect("event serialization cannot fail");
        line.push(b'\n');
        match &self.sink {
            Sink::Stdout => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&line);
                let _ = out.flush();
            }
            Sink::Buffer(buf) => buf.lock().unwrap().extend_from_slice(&line),
        }
    }
}

// ---------------------------------------------------------------------------
// Run loops
// ---------------------------------------------------------------------------

/// Serve `plugin` over stdin/stdout using the given `lifecycle`. Typically the
/// whole body of a plugin binary's `main`.
///
/// A `validate` first argument runs a one-shot config check instead — read one
/// `{"config":..}` line, emit `{"type":"validation","errors":[..]}`, exit —
/// regardless of lifecycle, so the engine can validate a config at save time
/// without a persistent process.
pub async fn run<P: Plugin + 'static>(plugin: P, lifecycle: Lifecycle) {
    let reader = BufReader::new(tokio::io::stdin());
    let emitter = Emitter::stdout();
    if std::env::args().nth(1).as_deref() == Some("validate") {
        validate_once(&plugin, reader, &emitter).await;
        return;
    }
    match lifecycle {
        Lifecycle::Oneshot => oneshot_loop(&plugin, reader, &emitter).await,
        Lifecycle::Persistent => persistent_loop(Arc::new(plugin), reader, emitter).await,
    }
}

/// Read one `{"config":..}` request and emit the plugin's validation problems.
async fn validate_once<P: Plugin, R: AsyncBufRead + Unpin>(
    plugin: &P,
    mut reader: R,
    emitter: &Emitter,
) {
    let mut line = String::new();
    let _ = reader.read_line(&mut line).await;
    let config = serde_json::from_str::<Value>(line.trim())
        .ok()
        .and_then(|req| req.get("config").cloned())
        .unwrap_or(Value::Null);
    emitter.emit(&json!({ "type": "validation", "errors": plugin.validate(&config) }));
}

/// Read one request, run it (or validate), emit the terminal event, return.
async fn oneshot_loop<P: Plugin, R: AsyncBufRead + Unpin>(
    plugin: &P,
    mut reader: R,
    emitter: &Emitter,
) {
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let req: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            emitter.emit(&json!({ "type": "error", "message": format!("invalid request: {e}"), "retryable": false }));
            return;
        }
    };
    let config = req.get("config").cloned().unwrap_or(Value::Null);
    if req.get("mode").and_then(Value::as_str) == Some("validate") {
        emitter.emit(&json!({ "type": "validation", "errors": plugin.validate(&config) }));
        return;
    }
    let ctx = Ctx { id: None, emitter: emitter.clone(), cancel: CancellationToken::new() };
    emit_outcome(emitter, None, plugin.execute(&ctx, config).await);
}

/// Emit `ready`, then service id-tagged requests concurrently until EOF.
async fn persistent_loop<P: Plugin + 'static, R: AsyncBufRead + Unpin>(
    plugin: Arc<P>,
    reader: R,
    emitter: Emitter,
) {
    emitter.emit(&json!({ "type": "ready" }));
    let pending: Arc<Mutex<HashMap<u64, CancellationToken>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut tasks = tokio::task::JoinSet::new();
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(line) else { continue };
        let Some(id) = req.get("id").and_then(Value::as_u64) else { continue };
        match req.get("mode").and_then(Value::as_str) {
            Some("cancel") => {
                if let Some(token) = pending.lock().unwrap().get(&id) {
                    token.cancel();
                }
            }
            Some("execute") => {
                let config = req.get("config").cloned().unwrap_or(Value::Null);
                let token = CancellationToken::new();
                pending.lock().unwrap().insert(id, token.clone());
                let plugin = Arc::clone(&plugin);
                let emitter = emitter.clone();
                let pending = Arc::clone(&pending);
                tasks.spawn(async move {
                    let ctx = Ctx { id: Some(id), emitter: emitter.clone(), cancel: token };
                    emit_outcome(&emitter, Some(id), plugin.execute(&ctx, config).await);
                    pending.lock().unwrap().remove(&id);
                });
                // Reap finished requests so handles don't accumulate.
                while tasks.try_join_next().is_some() {}
            }
            _ => {}
        }
    }
    // stdin closed: let in-flight requests finish before exiting.
    while tasks.join_next().await.is_some() {}
}

/// Emit the terminal `result`/`error` event, id-tagged when persistent.
fn emit_outcome(emitter: &Emitter, id: Option<u64>, outcome: Result<Value, PluginError>) {
    let mut event = match outcome {
        Ok(value) => json!({ "type": "result", "value": value }),
        Err(e) => json!({ "type": "error", "message": e.message, "retryable": e.retryable }),
    };
    if let Some(id) = id {
        event["id"] = json!(id);
    }
    emitter.emit(&event);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A test plugin: echoes `config.marker`, logs, honors cancel, and fails on
    /// `marker == "boom"`; `validate` rejects a missing `marker`.
    struct Echo;

    #[async_trait]
    impl Plugin for Echo {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                type_id: "echo".into(),
                label: "Echo".into(),
                description: String::new(),
                icon: "box".into(),
                color: "#888888".into(),
                fields: vec![],
            }
        }
        fn validate(&self, config: &Value) -> Vec<String> {
            match config.get("marker") {
                Some(Value::String(s)) if !s.is_empty() => vec![],
                _ => vec!["marker is required".into()],
            }
        }
        async fn execute(&self, ctx: &Ctx, config: Value) -> Result<Value, PluginError> {
            let marker = config.get("marker").and_then(Value::as_str).unwrap_or("");
            if marker == "boom" {
                return Err(PluginError::retryable("boom"));
            }
            ctx.info("working");
            // Give a cancel a chance to land.
            for _ in 0..40 {
                if ctx.is_cancelled() {
                    return Err(PluginError::fatal("canceled"));
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(json!({ "echo": marker }))
        }
    }

    /// Parse the buffer's newline-delimited JSON events.
    fn events(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<Value> {
        let bytes = buf.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn oneshot_executes_and_emits_result_and_logs() {
        let (emitter, buf) = Emitter::buffer();
        let reader = BufReader::new(&br#"{"mode":"execute","config":{"marker":"hi"}}"#[..]);
        oneshot_loop(&Echo, reader, &emitter).await;
        let evs = events(&buf);
        assert!(evs.iter().any(|e| e["type"] == "log" && e["message"] == "working"));
        let result = evs.iter().find(|e| e["type"] == "result").unwrap();
        assert_eq!(result["value"], json!({ "echo": "hi" }));
    }

    #[tokio::test]
    async fn oneshot_validate_returns_problems() {
        let (emitter, buf) = Emitter::buffer();
        let reader = BufReader::new(&br#"{"mode":"validate","config":{}}"#[..]);
        oneshot_loop(&Echo, reader, &emitter).await;
        let evs = events(&buf);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0]["type"], "validation");
        assert_eq!(evs[0]["errors"][0], "marker is required");
    }

    #[tokio::test]
    async fn validate_once_reports_problems() {
        let (emitter, buf) = Emitter::buffer();
        let reader = BufReader::new(&br#"{"config":{}}"#[..]);
        validate_once(&Echo, reader, &emitter).await;
        let evs = events(&buf);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0]["type"], "validation");
        assert_eq!(evs[0]["errors"][0], "marker is required");
    }

    #[tokio::test]
    async fn oneshot_error_carries_retryable() {
        let (emitter, buf) = Emitter::buffer();
        let reader = BufReader::new(&br#"{"mode":"execute","config":{"marker":"boom"}}"#[..]);
        oneshot_loop(&Echo, reader, &emitter).await;
        let evs = events(&buf);
        let err = evs.iter().find(|e| e["type"] == "error").unwrap();
        assert_eq!(err["message"], "boom");
        assert_eq!(err["retryable"], true);
    }

    #[tokio::test]
    async fn persistent_emits_ready_then_id_tagged_results() {
        let (emitter, buf) = Emitter::buffer();
        let input = concat!(
            r#"{"id":1,"mode":"execute","config":{"marker":"a"}}"#,
            "\n",
            r#"{"id":2,"mode":"execute","config":{"marker":"b"}}"#,
            "\n",
        );
        persistent_loop(Arc::new(Echo), BufReader::new(input.as_bytes()), emitter).await;
        let evs = events(&buf);
        assert_eq!(evs[0]["type"], "ready");
        let r1 = evs.iter().find(|e| e["type"] == "result" && e["id"] == 1).unwrap();
        let r2 = evs.iter().find(|e| e["type"] == "result" && e["id"] == 2).unwrap();
        assert_eq!(r1["value"], json!({ "echo": "a" }));
        assert_eq!(r2["value"], json!({ "echo": "b" }));
    }

    #[tokio::test]
    async fn persistent_cancel_message_stops_one_request() {
        let (emitter, buf) = Emitter::buffer();
        // Feed an execute, then a cancel for the same id after a beat. Keep the
        // reader open via a pipe so the loop doesn't exit before the cancel.
        let (tx, rx) = tokio::io::duplex(1024);
        let (mut w, r) = (tx, BufReader::new(rx));
        let loop_task = tokio::spawn(persistent_loop(Arc::new(Echo), r, emitter));
        use tokio::io::AsyncWriteExt;
        w.write_all(b"{\"id\":7,\"mode\":\"execute\",\"config\":{\"marker\":\"x\"}}\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        w.write_all(b"{\"id\":7,\"mode\":\"cancel\"}\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        drop(w); // EOF ends the loop
        loop_task.await.unwrap();
        let evs = events(&buf);
        let err = evs.iter().find(|e| e["type"] == "error" && e["id"] == 7).unwrap();
        assert_eq!(err["message"], "canceled");
    }
}
