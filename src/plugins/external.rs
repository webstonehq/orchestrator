//! Plugin bundle discovery, oneshot execution, and save-time validation.
//!
//! A plugin is a *bundle* directory with a `plugin.json` (metadata + a
//! declarative manifest) plus an executable. [`load_bundles`] reads each
//! `plugin.json` at startup — without executing anything — into a
//! [`PluginEntry`](super::PluginEntry). Oneshot bundles are spawned per task by
//! [`oneshot_execute`]; persistent bundles run through the
//! [`PersistentPlugin`](super::persistent::PersistentPlugin) manager. Save-time
//! validation is schema-derived, plus an opt-in `<binary> validate` subprocess.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use super::persistent::PersistentPlugin;
use super::{Executor, Lifecycle, LogLevel, PluginEntry, PluginManifest, TaskContext, TaskError};

/// Wire protocol version sent to plugins in each execute request.
const PROTOCOL_VERSION: u32 = 1;
/// Highest `schema_version` this build understands in a `plugin.json`.
const SUPPORTED_SCHEMA_VERSION: u32 = 1;
/// Manifest filename inside a bundle directory.
const MANIFEST_FILE: &str = "plugin.json";
/// Grace period between `SIGTERM` and `SIGKILL` when cancelling a oneshot task.
const TERM_GRACE: Duration = Duration::from_secs(3);
/// Upper bound on a plugin-authored save-time validation call.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(3);

/// A parsed `plugin.json`: bundle metadata plus the declarative manifest.
#[derive(Clone, Deserialize)]
struct PluginBundle {
    /// Bundle format version; must be `<= SUPPORTED_SCHEMA_VERSION`.
    schema_version: u32,
    /// Plugin name (for diagnostics; the `type_id` lives in `manifest`).
    name: String,
    /// Plugin version, reported for worker capability advertisement.
    version: String,
    /// Executable filename within the bundle directory.
    entrypoint: String,
    /// Whether the entrypoint implements the `validate` subcommand.
    #[serde(default)]
    supports_validate: bool,
    /// How the engine runs the process: `oneshot` (default) or `persistent`.
    #[serde(default)]
    lifecycle: Lifecycle,
    /// The declarative UI/config manifest.
    manifest: PluginManifest,
}

/// One newline-delimited JSON event a oneshot plugin emits on stdout.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum PluginEvent {
    Log {
        level: EventLevel,
        #[serde(default)]
        message: String,
    },
    Result {
        #[serde(default)]
        value: Value,
    },
    Error {
        message: String,
        #[serde(default)]
        retryable: bool,
    },
}

/// Terminal event of a `validate` call: the plugin's own config problems.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ValidateEvent {
    Validation {
        #[serde(default)]
        errors: Vec<String>,
    },
}

/// Log severity on the wire, mapped onto [`LogLevel`].
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum EventLevel {
    Info,
    Ok,
    Warn,
    Err,
    Dbg,
}

impl From<EventLevel> for LogLevel {
    fn from(l: EventLevel) -> Self {
        match l {
            EventLevel::Info => LogLevel::Info,
            EventLevel::Ok => LogLevel::Ok,
            EventLevel::Warn => LogLevel::Warn,
            EventLevel::Err => LogLevel::Err,
            EventLevel::Dbg => LogLevel::Dbg,
        }
    }
}

/// Scan `dir` for plugin bundles, one [`PluginEntry`] per valid one. Never
/// fails: unreadable, malformed, unsupported-version, or missing-entrypoint
/// bundles are skipped with a warning. Absent `dir` → empty.
pub(super) fn load_bundles(dir: &Path) -> Vec<PluginEntry> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %e, "cannot read plugins dir");
            }
            return Vec::new();
        }
    };

    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let bundle_dir = entry.path();
        if !bundle_dir.is_dir() {
            continue;
        }
        let manifest_path = bundle_dir.join(MANIFEST_FILE);
        let raw = match std::fs::read_to_string(&manifest_path) {
            Ok(raw) => raw,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %manifest_path.display(), error = %e, "cannot read plugin manifest");
                }
                continue;
            }
        };
        let bundle: PluginBundle = match serde_json::from_str(&raw) {
            Ok(bundle) => bundle,
            Err(e) => {
                tracing::warn!(path = %manifest_path.display(), error = %e, "skipping malformed plugin.json");
                continue;
            }
        };
        if bundle.schema_version > SUPPORTED_SCHEMA_VERSION {
            tracing::warn!(
                name = %bundle.name,
                schema_version = bundle.schema_version,
                supported = SUPPORTED_SCHEMA_VERSION,
                "skipping plugin: unsupported schema_version"
            );
            continue;
        }
        let entrypoint = bundle_dir.join(&bundle.entrypoint);
        if !entrypoint.is_file() {
            tracing::warn!(
                name = %bundle.name,
                entrypoint = %entrypoint.display(),
                "skipping plugin: entrypoint not found"
            );
            continue;
        }
        let executor = match bundle.lifecycle {
            Lifecycle::Persistent => {
                Executor::Persistent(PersistentPlugin::new(entrypoint.clone(), Vec::new(), bundle_dir.clone()))
            }
            Lifecycle::Oneshot => Executor::Oneshot,
        };
        plugins.push(PluginEntry {
            manifest: bundle.manifest,
            version: Some(bundle.version),
            supports_validate: bundle.supports_validate,
            program: entrypoint,
            args: Vec::new(),
            cwd: bundle_dir,
            term_grace: TERM_GRACE,
            validate_timeout: VALIDATE_TIMEOUT,
            executor,
        });
    }
    plugins
}

/// Spawn a oneshot plugin fresh for one task and read its protocol events.
pub(super) async fn oneshot_execute(
    program: &Path,
    args: &[String],
    cwd: &Path,
    term_grace: Duration,
    ctx: &TaskContext,
    config: Value,
) -> Result<Value, TaskError> {
    let request = json!({
        "protocol_version": PROTOCOL_VERSION,
        "mode": "execute",
        "run_id": ctx.run_id,
        "task_id": ctx.task_id,
        "config": config,
    });
    let mut request_bytes = serde_json::to_vec(&request)
        .map_err(|e| TaskError::fatal(format!("failed to encode plugin request: {e}")))?;
    request_bytes.push(b'\n');

    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TaskError::fatal(format!("failed to spawn plugin '{}': {e}", program.display())))?;

    // Deliver the request on stdin — never argv/env — so rendered secrets never
    // appear in a process listing. Closing stdin signals EOF.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    if let Err(e) = stdin.write_all(&request_bytes).await {
        return Err(TaskError::fatal(format!("failed to write plugin request: {e}")));
    }
    drop(stdin);

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut lines = BufReader::new(stdout).lines();

    // Read events until a terminal result/error, EOF, or cancellation.
    let terminal: Result<Value, TaskError> = loop {
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                return Err(terminate(child, term_grace).await);
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<PluginEvent>(&line) {
                            Ok(PluginEvent::Log { level, message }) => ctx.log(level.into(), message),
                            Ok(PluginEvent::Result { value }) => break Ok(value),
                            Ok(PluginEvent::Error { message, retryable }) => {
                                break Err(TaskError { message, retryable });
                            }
                            Err(e) => {
                                let _ = child.start_kill();
                                return Err(TaskError::fatal(format!(
                                    "plugin protocol error: could not parse output line: {e}"
                                )));
                            }
                        }
                    }
                    Ok(None) => return Err(no_result_error(child).await),
                    Err(e) => {
                        let _ = child.start_kill();
                        return Err(TaskError::fatal(format!("reading plugin output: {e}")));
                    }
                }
            }
        }
    };

    // Result already decided; reap the child so it doesn't linger.
    let _ = child.wait().await;
    terminal
}

/// Cancel a oneshot process: `SIGTERM` for a chance to clean up, escalating to
/// `SIGKILL` after `term_grace`. Always reaps. Returns the canonical error.
async fn terminate(mut child: Child, term_grace: Duration) -> TaskError {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SAFETY: `pid` is this child's, still un-reaped.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            if tokio::time::timeout(term_grace, child.wait()).await.is_err() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        } else {
            let _ = child.start_kill();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    TaskError::fatal("canceled")
}

/// Read a failed child's stderr and exit status into a fatal error, used when
/// stdout closed before a terminal event.
async fn no_result_error(mut child: Child) -> TaskError {
    let mut stderr = String::new();
    if let Some(mut handle) = child.stderr.take() {
        let _ = handle.read_to_string(&mut stderr).await;
    }
    let status_desc = match child.wait().await {
        Ok(status) => status
            .code()
            .map(|c| format!("exit status {c}"))
            .unwrap_or_else(|| "terminated by signal".to_string()),
        Err(e) => format!("unknown status ({e})"),
    };
    let stderr = stderr.trim();
    if stderr.is_empty() {
        TaskError::fatal(format!("plugin exited without a result ({status_desc})"))
    } else {
        TaskError::fatal(format!("plugin exited without a result ({status_desc}): {stderr}"))
    }
}

/// Schema-derived validation: every `required` manifest field must be present
/// and non-null in the authored config.
pub(super) fn validate_against_manifest(manifest: &PluginManifest, config: &Value) -> Vec<String> {
    let Some(obj) = config.as_object() else {
        return vec!["config must be an object".to_string()];
    };
    let mut errs = Vec::new();
    for field in &manifest.fields {
        // A field that isn't required, or that has a default (which fills in
        // when absent), can never be "missing".
        if !field.required || !field.default.is_null() {
            continue;
        }
        let missing = match obj.get(&field.key) {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            Some(_) => false,
        };
        if missing {
            errs.push(format!("{} is required", field.key));
        }
    }
    errs
}

/// Run the plugin's `validate` subcommand: spawn `program [args] validate`, send
/// the *authored* config, and collect the config problems it reports. Bounded by
/// `timeout` (a watchdog SIGKILLs an overrunning process). Synchronous — safe to
/// call from the sync validate path. `Err` signals infra trouble, not config
/// problems, so the caller can fall back to schema-only checks.
pub(super) fn run_plugin_validate(
    program: &Path,
    args: &[String],
    cwd: &Path,
    config: &Value,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    use std::io::Write;

    // The `validate` subcommand runs a one-shot check regardless of lifecycle (a
    // persistent binary can't be validated over its request stream).
    let request = json!({ "config": config });
    let mut bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    bytes.push(b'\n');

    let mut child = std::process::Command::new(program)
        .args(args)
        .arg("validate")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(&bytes)
        .map_err(|e| format!("write failed: {e}"))?;

    // Watchdog thread: SIGKILL the child if it overruns `timeout`.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(unix)]
    let watchdog = {
        let done = std::sync::Arc::clone(&done);
        let pid = child.id();
        std::thread::spawn(move || {
            let step = Duration::from_millis(20);
            let mut waited = Duration::ZERO;
            while waited < timeout {
                if done.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            if !done.load(std::sync::atomic::Ordering::Relaxed) {
                // SAFETY: `pid` is our just-spawned child, not yet reaped.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        })
    };

    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait failed: {e}"))?;
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    #[cfg(unix)]
    let _ = watchdog.join();

    for line in output.stdout.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(ValidateEvent::Validation { errors }) = serde_json::from_slice(line) {
            return Ok(errors);
        }
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "no validation result (status {:?}){}",
        output.status.code(),
        if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        }
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::plugins::LogLevel;

    /// A manifest with one required `url` field, as a JSON snippet.
    const ECHO_MANIFEST: &str = r##"{
        "type_id": "test.echo",
        "label": "Echo",
        "description": "test plugin",
        "icon": "box",
        "color": "#888888",
        "fields": [
            { "key": "url", "label": "URL", "widget": "template", "required": true }
        ]
    }"##;

    fn plugin_json(entrypoint: &str, manifest: &str) -> String {
        format!(
            r#"{{ "schema_version": 1, "name": "echo", "version": "0.1.0",
                  "entrypoint": "{entrypoint}", "manifest": {manifest} }}"#
        )
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    /// Create `<root>/<dir_name>/plugin.json` and, if given, an entrypoint.
    fn make_bundle(root: &Path, dir_name: &str, plugin_json: &str, entrypoint: Option<(&str, &str)>) {
        let bundle = root.join(dir_name);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("plugin.json"), plugin_json).unwrap();
        if let Some((name, script)) = entrypoint {
            write_executable(&bundle.join(name), script);
        }
    }

    type LogLines = Arc<Mutex<Vec<(LogLevel, String)>>>;

    fn logging_ctx() -> (TaskContext, LogLines) {
        let logs: LogLines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&logs);
        let ctx = TaskContext::new(
            1,
            "t1",
            CancellationToken::new(),
            Box::new(move |level, line| sink.lock().unwrap().push((level, line))),
        );
        (ctx, logs)
    }

    // --- bundle parsing ---------------------------------------------------

    #[test]
    fn bundle_deserializes_with_nested_manifest() {
        let raw = plugin_json("run", ECHO_MANIFEST);
        let bundle: PluginBundle = serde_json::from_str(&raw).unwrap();
        assert_eq!(bundle.schema_version, 1);
        assert_eq!(bundle.name, "echo");
        assert_eq!(bundle.version, "0.1.0");
        assert_eq!(bundle.entrypoint, "run");
        assert_eq!(bundle.manifest.type_id, "test.echo");
        assert!(bundle.manifest.fields[0].required);
    }

    // --- discovery --------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn discovers_valid_bundle() {
        let dir = tempfile::tempdir().unwrap();
        make_bundle(dir.path(), "echo", &plugin_json("run", ECHO_MANIFEST), Some(("run", "#!/bin/sh\n")));
        let plugins = load_bundles(dir.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest().type_id, "test.echo");
        assert_eq!(plugins[0].version().as_deref(), Some("0.1.0"));
        assert!(plugins[0].program.ends_with("run"));
    }

    #[cfg(unix)]
    #[test]
    fn skips_malformed_json_but_keeps_valid() {
        let dir = tempfile::tempdir().unwrap();
        make_bundle(dir.path(), "broken", "{ not json", Some(("run", "#!/bin/sh\n")));
        make_bundle(dir.path(), "echo", &plugin_json("run", ECHO_MANIFEST), Some(("run", "#!/bin/sh\n")));
        let plugins = load_bundles(dir.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest().type_id, "test.echo");
    }

    #[cfg(unix)]
    #[test]
    fn skips_bundle_with_missing_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        make_bundle(dir.path(), "echo", &plugin_json("run", ECHO_MANIFEST), None);
        assert!(load_bundles(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn skips_unsupported_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let raw = format!(
            r#"{{ "schema_version": 999, "name": "echo", "version": "0.1.0",
                  "entrypoint": "run", "manifest": {ECHO_MANIFEST} }}"#
        );
        make_bundle(dir.path(), "echo", &raw, Some(("run", "#!/bin/sh\n")));
        assert!(load_bundles(dir.path()).is_empty());
    }

    #[test]
    fn absent_dir_yields_no_plugins() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_bundles(&dir.path().join("does-not-exist")).is_empty());
    }

    // --- validation -------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn validate_reports_missing_required_field() {
        let dir = tempfile::tempdir().unwrap();
        make_bundle(dir.path(), "echo", &plugin_json("run", ECHO_MANIFEST), Some(("run", "#!/bin/sh\n")));
        let plugins = load_bundles(dir.path());
        assert!(plugins[0].validate(&json!({})).iter().any(|e| e.contains("url")));
        assert!(plugins[0].validate(&json!({ "url": "http://x" })).is_empty());
    }

    /// Load a single bundle that opts into (or out of) validate mode.
    #[cfg(unix)]
    fn validate_plugin(dir: &Path, supports_validate: bool, script: &str) -> PluginEntry {
        let pj = format!(
            r#"{{ "schema_version": 1, "name": "echo", "version": "0.1.0",
                  "entrypoint": "run", "supports_validate": {supports_validate},
                  "manifest": {ECHO_MANIFEST} }}"#
        );
        make_bundle(dir, "echo", &pj, Some(("run", script)));
        let mut plugins = load_bundles(dir);
        assert_eq!(plugins.len(), 1, "expected exactly one loaded plugin");
        plugins.pop().unwrap()
    }

    /// A validate-mode plugin (non-SDK) that rejects `name == "bad"`.
    #[cfg(unix)]
    const PY_VALIDATE: &str = r#"#!/usr/bin/env python3
import sys, json
req = json.load(sys.stdin)
errs = []
if len(sys.argv) > 1 and sys.argv[1] == "validate":
    if req.get("config", {}).get("name") == "bad":
        errs.append("name must not be bad")
    print(json.dumps({"type": "validation", "errors": errs}), flush=True)
"#;

    /// A minimal persistent plugin: emits `ready`, then echoes `config.marker`.
    #[cfg(unix)]
    const PY_PERSISTENT: &str = r#"#!/usr/bin/env python3
import sys, json
def emit(o): print(json.dumps(o), flush=True)
emit({"type": "ready"})
while True:
    line = sys.stdin.readline()
    if not line: break
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    if req.get("mode") == "execute":
        emit({"id": req["id"], "type": "result", "value": {"echo": req.get("config", {}).get("marker")}})
"#;

    #[cfg(unix)]
    #[test]
    fn validate_invokes_plugin_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = validate_plugin(dir.path(), true, PY_VALIDATE);
        let errs = plugin.validate(&json!({ "url": "x", "name": "bad" }));
        assert!(errs.iter().any(|e| e == "name must not be bad"), "not surfaced: {errs:?}");
        assert!(plugin.validate(&json!({ "url": "x", "name": "fine" })).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn validate_still_reports_schema_errors_alongside_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = validate_plugin(dir.path(), true, PY_VALIDATE);
        let errs = plugin.validate(&json!({ "name": "bad" }));
        assert!(errs.iter().any(|e| e.contains("url")), "missing schema error: {errs:?}");
        assert!(errs.iter().any(|e| e == "name must not be bad"), "missing plugin error: {errs:?}");
    }

    #[cfg(unix)]
    #[test]
    fn validate_does_not_run_binary_when_not_supported() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = validate_plugin(dir.path(), false, "#!/bin/sh\ntouch ran\n");
        assert!(plugin.validate(&json!({ "url": "x" })).is_empty());
        assert!(!plugin.cwd.join("ran").exists(), "binary ran despite supports_validate=false");
    }

    #[cfg(unix)]
    #[test]
    fn validate_falls_back_to_schema_when_plugin_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut plugin =
            validate_plugin(dir.path(), true, "#!/bin/sh\ncat > /dev/null\nwhile true; do sleep 0.1; done\n");
        plugin.validate_timeout = Duration::from_millis(200);
        let start = Instant::now();
        let errs = plugin.validate(&json!({}));
        assert_eq!(errs, vec!["url is required".to_string()]);
        assert!(start.elapsed() < Duration::from_secs(2), "validate hung: {:?}", start.elapsed());
    }

    // --- execution --------------------------------------------------------

    /// Load a single oneshot bundle whose entrypoint runs `script`.
    #[cfg(unix)]
    fn echo_plugin(dir: &Path, script: &str) -> PluginEntry {
        make_bundle(dir, "echo", &plugin_json("run", ECHO_MANIFEST), Some(("run", script)));
        let mut plugins = load_bundles(dir);
        assert_eq!(plugins.len(), 1, "expected exactly one loaded plugin");
        plugins.pop().unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_bundle_executes_via_manager() {
        let dir = tempfile::tempdir().unwrap();
        let pj = format!(
            r#"{{ "schema_version": 1, "name": "echo", "version": "0.1.0",
                  "entrypoint": "run", "lifecycle": "persistent", "manifest": {ECHO_MANIFEST} }}"#
        );
        make_bundle(dir.path(), "echo", &pj, Some(("run", PY_PERSISTENT)));
        let mut plugins = load_bundles(dir.path());
        let plugin = plugins.pop().unwrap();
        assert!(matches!(plugin.executor, Executor::Persistent(_)), "should get a manager");

        let (ctx, _logs) = logging_ctx();
        let out = plugin.execute(&ctx, json!({ "url": "x", "marker": "hello" })).await.unwrap();
        assert_eq!(out, json!({ "echo": "hello" }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_streams_logs_and_returns_result() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = echo_plugin(
            dir.path(),
            r#"#!/bin/sh
cat > /dev/null
echo '{"type":"log","level":"info","message":"starting"}'
echo '{"type":"log","level":"ok","message":"done"}'
echo '{"type":"result","value":{"ok":true}}'
"#,
        );
        let (ctx, logs) = logging_ctx();
        let out = plugin.execute(&ctx, json!({ "url": "http://x" })).await.unwrap();
        assert_eq!(out, json!({ "ok": true }));
        let logs = logs.lock().unwrap();
        assert!(logs.iter().any(|(l, m)| *l == LogLevel::Info && m == "starting"));
        assert!(logs.iter().any(|(l, m)| *l == LogLevel::Ok && m == "done"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_passes_rendered_config_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = echo_plugin(
            dir.path(),
            r#"#!/bin/sh
input=$(cat)
printf '{"type":"result","value":%s}\n' "$input"
"#,
        );
        let (ctx, _logs) = logging_ctx();
        let out = plugin.execute(&ctx, json!({ "url": "http://example.com", "n": 5 })).await.unwrap();
        assert_eq!(out["protocol_version"], json!(PROTOCOL_VERSION));
        assert_eq!(out["task_id"], json!("t1"));
        assert_eq!(out["config"], json!({ "url": "http://example.com", "n": 5 }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_maps_error_event_to_task_error() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = echo_plugin(
            dir.path(),
            "#!/bin/sh\ncat > /dev/null\necho '{\"type\":\"error\",\"message\":\"boom\",\"retryable\":true}'\n",
        );
        let (ctx, _logs) = logging_ctx();
        let err = plugin.execute(&ctx, json!({ "url": "x" })).await.unwrap_err();
        assert_eq!(err.message, "boom");
        assert!(err.retryable);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_without_terminal_event_is_fatal_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = echo_plugin(
            dir.path(),
            "#!/bin/sh\ncat > /dev/null\necho \"diagnostic detail\" 1>&2\nexit 3\n",
        );
        let (ctx, _logs) = logging_ctx();
        let err = plugin.execute(&ctx, json!({ "url": "x" })).await.unwrap_err();
        assert!(!err.retryable);
        assert!(err.message.contains("diagnostic detail"), "got: {}", err.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_protocol_violation_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = echo_plugin(
            dir.path(),
            "#!/bin/sh\ncat > /dev/null\necho 'this is not json'\necho '{\"type\":\"result\",\"value\":{}}'\n",
        );
        let (ctx, _logs) = logging_ctx();
        let err = plugin.execute(&ctx, json!({ "url": "x" })).await.unwrap_err();
        assert!(!err.retryable);
        assert!(err.message.contains("protocol"), "got: {}", err.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_cancel_sends_sigterm_for_graceful_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = echo_plugin(
            dir.path(),
            r#"#!/usr/bin/env python3
import signal, sys, time
def handler(signum, frame):
    open("term_caught", "w").write("caught")
    sys.exit(0)
signal.signal(signal.SIGTERM, handler)
open("ready", "w").write("y")
while True:
    time.sleep(0.05)
"#,
        );
        let marker = plugin.cwd.join("term_caught");
        let ready = plugin.cwd.join("ready");
        let (ctx, _logs) = logging_ctx();
        let token = ctx.cancel.clone();
        let watcher = tokio::spawn(async move {
            for _ in 0..400 {
                if ready.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            token.cancel();
        });
        let err = plugin.execute(&ctx, json!({ "url": "x" })).await.unwrap_err();
        let _ = watcher.await;
        assert_eq!(err.message, "canceled");
        let mut seen = false;
        for _ in 0..50 {
            if marker.exists() {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(seen, "plugin was not given a chance to handle SIGTERM");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_escalates_to_sigkill_when_sigterm_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut plugin = echo_plugin(
            dir.path(),
            "#!/bin/sh\ntrap '' TERM\ncat > /dev/null\nwhile true; do sleep 0.05; done\n",
        );
        plugin.term_grace = Duration::from_millis(150);
        let (ctx, _logs) = logging_ctx();
        let token = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.cancel();
        });
        let start = Instant::now();
        let err = plugin.execute(&ctx, json!({ "url": "x" })).await.unwrap_err();
        assert_eq!(err.message, "canceled");
        assert!(start.elapsed() < Duration::from_secs(2), "did not escalate: {:?}", start.elapsed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_kills_process_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = echo_plugin(
            dir.path(),
            "#!/bin/sh\ncat > /dev/null\nsleep 30\necho '{\"type\":\"result\",\"value\":{}}'\n",
        );
        let (ctx, _logs) = logging_ctx();
        let token = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.cancel();
        });
        let start = Instant::now();
        let err = plugin.execute(&ctx, json!({ "url": "x" })).await.unwrap_err();
        assert_eq!(err.message, "canceled");
        assert!(start.elapsed() < Duration::from_secs(2), "did not return promptly: {:?}", start.elapsed());
    }
}
