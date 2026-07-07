//! Prototype: the persistent-plugin process manager.
//!
//! A `persistent` plugin is one long-lived process that services *many*
//! concurrent task requests, multiplexed by request `id`. This restores warm
//! state (e.g. a pooled `reqwest` client) and eliminates per-task spawn — the
//! reason `http` can't be a spawn-per-task plugin (see
//! `docs/plans/2026-07-07-unified-plugin-protocol-design.md`).
//!
//! This module proves the risky half of that design in isolation, before the
//! workspace/trait-removal refactor:
//!
//! - **Multiplexing** — N concurrent `execute`s share one process; events are
//!   routed back to the right caller by `id`.
//! - **Cancellation** — a `{id, mode:"cancel"}` message stops one request
//!   without touching its siblings.
//! - **Timeout teardown** — a plugin that ignores `cancel` is killed outright
//!   after a grace; in-flight siblings fail *retryable*.
//! - **Crash & restart** — if the process dies, in-flight requests fail
//!   *retryable* and the next request lazily restarts it.
//!
//! Wire protocol (newline-delimited JSON). On start the plugin emits
//! `{"type":"ready"}`. Then, per request, the engine writes
//! `{"id":N,"mode":"execute","run_id":..,"task_id":..,"config":{..}}` (or
//! `{"id":N,"mode":"cancel"}`) and the plugin streams id-tagged events:
//! `{"id":N,"type":"log","level":..,"message":..}` and a terminal
//! `{"id":N,"type":"result","value":..}` / `{"id":N,"type":"error",..}`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc, oneshot};

use super::{LogLevel, TaskContext, TaskError};

/// How long to wait for a freshly-spawned plugin to emit `ready`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Grace between `cancel` and killing the whole process when a plugin ignores
/// cancellation.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// The per-task payload the engine hands to a plugin (the engine adds `id` and
/// `mode` on the wire).
#[derive(Debug, Clone)]
pub struct RequestPayload {
    pub run_id: i64,
    pub task_id: String,
    pub config: Value,
}

/// A managed long-lived plugin process. Lazily (re)started on demand; safe to
/// share behind an `Arc` and call `execute` on concurrently.
pub struct PersistentPlugin {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    grace: Duration,
    handshake_timeout: Duration,
    inner: Mutex<Option<Arc<Process>>>,
}

/// One running plugin process and the state needed to multiplex over it.
struct Process {
    pid: Option<u32>,
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<StdMutex<HashMap<u64, mpsc::UnboundedSender<WireEvent>>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
}

/// One event line from a plugin. `ready` carries no id; the rest are routed by
/// their request `id`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum WireEvent {
    Ready,
    Log {
        id: u64,
        #[serde(default)]
        level: String,
        #[serde(default)]
        message: String,
    },
    Result {
        id: u64,
        #[serde(default)]
        value: Value,
    },
    Error {
        id: u64,
        message: String,
        #[serde(default)]
        retryable: bool,
    },
}

impl WireEvent {
    /// The request this event belongs to, or `None` for `ready`.
    fn request_id(&self) -> Option<u64> {
        match self {
            WireEvent::Ready => None,
            WireEvent::Log { id, .. } | WireEvent::Result { id, .. } | WireEvent::Error { id, .. } => {
                Some(*id)
            }
        }
    }
}

fn log_level(s: &str) -> LogLevel {
    match s {
        "ok" => LogLevel::Ok,
        "warn" => LogLevel::Warn,
        "err" => LogLevel::Err,
        "dbg" => LogLevel::Dbg,
        _ => LogLevel::Info,
    }
}

impl PersistentPlugin {
    /// Manage the plugin launched by `program args` (working dir `cwd`).
    pub fn new(program: PathBuf, args: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            program,
            args,
            cwd,
            grace: CANCEL_GRACE,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            inner: Mutex::new(None),
        }
    }

    /// Override the cancel→kill grace (tests).
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Override the ready-handshake timeout (tests).
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Get the live process, lazily (re)starting it if absent or dead.
    /// Serialized by `inner`, so a burst of first requests starts it once.
    async fn ensure_started(&self) -> Result<Arc<Process>, String> {
        let mut guard = self.inner.lock().await;
        if let Some(p) = guard.as_ref()
            && p.alive.load(Ordering::SeqCst)
        {
            return Ok(Arc::clone(p));
        }
        let p = Process::start(&self.program, &self.args, &self.cwd, self.handshake_timeout).await?;
        *guard = Some(Arc::clone(&p));
        Ok(p)
    }

    /// Dispatch one task to the plugin and await its result, streaming log lines
    /// to `ctx`. Concurrent calls multiplex over the single process.
    pub async fn execute(
        &self,
        req: RequestPayload,
        ctx: &TaskContext,
    ) -> Result<Value, TaskError> {
        let proc = self
            .ensure_started()
            .await
            .map_err(|e| TaskError::retryable(format!("plugin start failed: {e}")))?;

        let id = proc.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, mut rx) = mpsc::unbounded_channel::<WireEvent>();
        proc.pending.lock().unwrap().insert(id, tx);

        let sent = proc.stdin_tx.send(serialize_line(&json!({
            "id": id,
            "mode": "execute",
            "run_id": req.run_id,
            "task_id": req.task_id,
            "config": req.config,
        })));
        if sent.is_err() || !proc.alive.load(Ordering::SeqCst) {
            proc.pending.lock().unwrap().remove(&id);
            return Err(TaskError::retryable("plugin process unavailable"));
        }

        let mut canceled = false;
        let mut grace: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
        let result = loop {
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled(), if !canceled => {
                    canceled = true;
                    let _ = proc.stdin_tx.send(serialize_line(&json!({ "id": id, "mode": "cancel" })));
                    grace = Some(Box::pin(tokio::time::sleep(self.grace)));
                }
                _ = async {
                    match grace.as_mut() {
                        Some(t) => t.await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    // Plugin ignored the cancel: kill the whole process. Siblings
                    // die with it and fail retryable via their closed receivers.
                    proc.teardown();
                    break Err(TaskError::fatal("canceled"));
                }
                ev = rx.recv() => match ev {
                    Some(WireEvent::Log { level, message, .. }) => ctx.log(log_level(&level), message),
                    Some(WireEvent::Result { value, .. }) => break Ok(value),
                    Some(WireEvent::Error { message, retryable, .. }) => {
                        break Err(TaskError { message, retryable });
                    }
                    Some(WireEvent::Ready) => {}
                    // Sender dropped: the reader saw the process die.
                    None => break Err(TaskError::retryable("plugin process ended")),
                }
            }
        };
        proc.pending.lock().unwrap().remove(&id);
        result
    }
}

impl Process {
    /// Spawn the plugin, wire up its stdio, and wait for its `ready` line.
    async fn start(
        program: &std::path::Path,
        args: &[String],
        cwd: &std::path::Path,
        handshake_timeout: Duration,
    ) -> Result<Arc<Process>, String> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn failed: {e}"))?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take();

        let pending: Arc<StdMutex<HashMap<u64, mpsc::UnboundedSender<WireEvent>>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));

        // Writer: serialize all stdin writes through one task.
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(bytes) = stdin_rx.recv().await {
                if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // Drain stderr so a chatty plugin can't deadlock on a full pipe.
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(_)) = lines.next_line().await {}
            });
        }

        // Reader: demux id-tagged events to per-request channels; the first
        // `ready` completes the handshake; EOF/garbage means the process died.
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        {
            let pending = Arc::clone(&pending);
            let alive = Arc::clone(&alive);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                let mut ready_tx = Some(ready_tx);
                // Ends on EOF or a read error (`next_line` yields non-`Ok(Some)`).
                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<WireEvent>(line) {
                        Ok(WireEvent::Ready) => {
                            if let Some(tx) = ready_tx.take() {
                                let _ = tx.send(());
                            }
                        }
                        Ok(ev) => {
                            if let Some(id) = ev.request_id() {
                                let sink = pending.lock().unwrap().get(&id).cloned();
                                if let Some(sink) = sink {
                                    let _ = sink.send(ev);
                                }
                            }
                        }
                        // Unframable output: can't trust the stream — treat as death.
                        Err(_) => break,
                    }
                }
                // Output ended: mark dead and drop every waiter so in-flight
                // execute() calls observe the death and fail retryable.
                alive.store(false, Ordering::SeqCst);
                pending.lock().unwrap().clear();
            });
        }

        // Reap the child so it never lingers as a zombie.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        match tokio::time::timeout(handshake_timeout, ready_rx).await {
            Ok(Ok(())) => Ok(Arc::new(Process {
                pid,
                stdin_tx,
                pending,
                next_id: AtomicU64::new(1),
                alive,
            })),
            _ => {
                Self::kill(pid);
                Err("plugin did not signal ready".to_string())
            }
        }
    }

    /// Kill the whole process (used on timeout teardown / handshake failure).
    fn teardown(&self) {
        self.alive.store(false, Ordering::SeqCst);
        Self::kill(self.pid);
    }

    #[cfg(unix)]
    fn kill(pid: Option<u32>) {
        if let Some(pid) = pid {
            // SAFETY: our own child's pid; kill_on_drop + the reaper collect it.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }

    #[cfg(not(unix))]
    fn kill(_pid: Option<u32>) {}
}

/// Serialize a JSON value to one newline-terminated line.
fn serialize_line(v: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(v).expect("plugin request serialization cannot fail");
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use futures::future::join_all;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::plugins::LogLevel;

    // --- fixtures ---------------------------------------------------------

    /// Concurrent echo: a thread per request, ~200ms of work, honors cancel,
    /// echoes `config.marker`, fails on `marker == "boom"`, and reports the peak
    /// number of simultaneously in-flight requests as `active_peak` — a
    /// timing-independent proof of multiplexing.
    const ECHO: &str = r#"#!/usr/bin/env python3
import sys, json, threading, time
lock = threading.Lock()
active = 0
peak = 0
def emit(o):
    with lock:
        sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
emit({"type": "ready"})
cancelled = set()
def handle(req):
    global active, peak
    rid = req["id"]; cfg = req.get("config", {})
    if cfg.get("marker") == "boom":
        emit({"id": rid, "type": "error", "message": "boom", "retryable": True}); return
    with lock:
        active += 1
        peak = max(peak, active)
    try:
        for _ in range(20):
            if rid in cancelled:
                emit({"id": rid, "type": "error", "message": "canceled", "retryable": False}); return
            time.sleep(0.01)
        emit({"id": rid, "type": "log", "level": "info", "message": "done"})
        with lock: p = peak
        emit({"id": rid, "type": "result", "value": {"echo": cfg.get("marker"), "active_peak": p}})
    finally:
        with lock: active -= 1
while True:
    line = sys.stdin.readline()
    if not line: break
    line = line.strip()
    if not line: continue
    req = json.loads(line); mode = req.get("mode")
    if mode == "cancel": cancelled.add(req["id"])
    elif mode == "execute": threading.Thread(target=handle, args=(req,), daemon=True).start()
"#;

    /// Never honors cancel: every request sleeps 5s. Forces the teardown path.
    const IGNORE_CANCEL: &str = r#"#!/usr/bin/env python3
import sys, json, threading, time
lock = threading.Lock()
def emit(o):
    with lock:
        sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
emit({"type": "ready"})
def handle(req):
    time.sleep(5)
    emit({"id": req["id"], "type": "result", "value": {}})
while True:
    line = sys.stdin.readline()
    if not line: break
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    if req.get("mode") == "execute":
        threading.Thread(target=handle, args=(req,), daemon=True).start()
"#;

    /// Normal ~200ms echo, but hard-exits the whole process on `config.crash`.
    const CRASH: &str = r#"#!/usr/bin/env python3
import sys, json, threading, time, os
lock = threading.Lock()
def emit(o):
    with lock:
        sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
emit({"type": "ready"})
def handle(req):
    cfg = req.get("config", {})
    if cfg.get("crash"): os._exit(1)
    time.sleep(0.2)
    emit({"id": req["id"], "type": "result", "value": {"echo": cfg.get("marker")}})
while True:
    line = sys.stdin.readline()
    if not line: break
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    if req.get("mode") == "execute":
        threading.Thread(target=handle, args=(req,), daemon=True).start()
"#;

    /// Never emits `ready` — exercises the handshake timeout.
    const NO_READY: &str = "#!/usr/bin/env python3\nimport time\ntime.sleep(5)\n";

    #[cfg(unix)]
    fn write_executable(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[cfg(unix)]
    fn plugin(dir: &Path, script: &str) -> PersistentPlugin {
        let path = dir.join("run");
        write_executable(&path, script);
        PersistentPlugin::new(path, vec![], dir.to_path_buf())
    }

    fn ctx(cancel: CancellationToken) -> TaskContext {
        TaskContext::new(1, "t", cancel, Box::new(|_, _| {}))
    }

    type LogLines = Arc<Mutex<Vec<(LogLevel, String)>>>;

    fn logging_ctx(cancel: CancellationToken) -> (TaskContext, LogLines) {
        let logs: LogLines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&logs);
        let ctx = TaskContext::new(
            1,
            "t",
            cancel,
            Box::new(move |level, line| sink.lock().unwrap().push((level, line))),
        );
        (ctx, logs)
    }

    fn req(marker: &str) -> RequestPayload {
        RequestPayload {
            run_id: 1,
            task_id: "t".to_string(),
            config: json!({ "marker": marker }),
        }
    }

    // --- tests ------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn single_request_returns_result_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(dir.path(), ECHO);
        let (ctx, logs) = logging_ctx(CancellationToken::new());
        let out = p.execute(req("hello"), &ctx).await.unwrap();
        assert_eq!(out["echo"], json!("hello"));
        assert!(logs.lock().unwrap().iter().any(|(l, m)| *l == LogLevel::Info && m == "done"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn error_event_maps_to_task_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(dir.path(), ECHO);
        let err = p.execute(req("boom"), &ctx(CancellationToken::new())).await.unwrap_err();
        assert_eq!(err.message, "boom");
        assert!(err.retryable);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_requests_multiplex_on_one_process() {
        let dir = tempfile::tempdir().unwrap();
        let p = Arc::new(plugin(dir.path(), ECHO));
        let start = Instant::now();
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let p = Arc::clone(&p);
                tokio::spawn(async move {
                    let c = ctx(CancellationToken::new());
                    p.execute(req(&format!("m{i}")), &c).await
                })
            })
            .collect();
        let results = join_all(handles).await;
        let mut peak = 0;
        for (i, r) in results.into_iter().enumerate() {
            let out = r.unwrap().unwrap();
            assert_eq!(out["echo"], json!(format!("m{i}")));
            peak = peak.max(out["active_peak"].as_i64().unwrap());
        }
        // The plugin saw multiple requests in flight at once → true multiplexing
        // over one process (timing-independent, so no flake on a cold/slow box).
        assert!(peak >= 2, "requests were serialized; peak in-flight = {peak}");
        // Sanity: nowhere near the ~1s a fully-serialized run would take.
        assert!(start.elapsed() < Duration::from_secs(3), "unexpectedly slow: {:?}", start.elapsed());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_stops_one_request_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(dir.path(), ECHO);
        let token = CancellationToken::new();
        let c = ctx(token.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
        });
        let err = p.execute(req("x"), &c).await.unwrap_err();
        assert_eq!(err.message, "canceled");
        assert!(!err.retryable);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unresponsive_cancel_tears_down_process_and_sibling_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let p = Arc::new(plugin(dir.path(), IGNORE_CANCEL).with_grace(Duration::from_millis(200)));

        let token_a = CancellationToken::new();
        let (pa, ta) = (Arc::clone(&p), token_a.clone());
        let ha = tokio::spawn(async move { pa.execute(req("A"), &ctx(ta)).await });
        let pb = Arc::clone(&p);
        let hb = tokio::spawn(async move { pb.execute(req("B"), &ctx(CancellationToken::new())).await });

        // Let both dispatch, then cancel A (which the plugin ignores).
        tokio::time::sleep(Duration::from_millis(150)).await;
        token_a.cancel();

        let ra = ha.await.unwrap();
        let rb = hb.await.unwrap();
        assert_eq!(ra.unwrap_err().message, "canceled");
        assert!(rb.unwrap_err().retryable, "sibling should fail retryable after teardown");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn crash_fails_inflight_retryable_then_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let p = Arc::new(plugin(dir.path(), CRASH));

        let pn = Arc::clone(&p);
        let hn = tokio::spawn(async move { pn.execute(req("A"), &ctx(CancellationToken::new())).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Crash the process while A is in flight.
        let crash = RequestPayload { run_id: 1, task_id: "t".into(), config: json!({ "crash": true }) };
        let rc = p.execute(crash, &ctx(CancellationToken::new())).await;
        let rn = hn.await.unwrap();
        assert!(rc.unwrap_err().retryable, "crashing request should be retryable");
        assert!(rn.unwrap_err().retryable, "in-flight request should be retryable");

        // A later request lazily restarts the process and succeeds.
        let out = p.execute(req("B"), &ctx(CancellationToken::new())).await.unwrap();
        assert_eq!(out, json!({ "echo": "B" }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_ready_handshake_fails_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(dir.path(), NO_READY).with_handshake_timeout(Duration::from_millis(300));
        let err = p.execute(req("x"), &ctx(CancellationToken::new())).await.unwrap_err();
        assert!(err.retryable, "startup failure should be retryable");
    }
}
