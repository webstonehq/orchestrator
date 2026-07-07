//! End-to-end BYOW test: a real control-plane server plus a real worker
//! process (the `worker::run` loop) executing a `gpu`-queue run over HTTP.
//!
//! The server never executes the run itself (it's not on the `local` queue);
//! the worker claims it, runs the engine locally against its own scratch DB
//! and secret store, and streams state back, which the server persists.

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use orchestrator::api::{self, AppState};
use orchestrator::db::Db;
use orchestrator::engine::Engine;
use orchestrator::plugins::PluginRegistry;
use orchestrator::scheduler::{RunLauncher, Scheduler, SystemClock};
use orchestrator::secrets::SecretStore;
use orchestrator::worker::{self, WorkerConfig};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct NoopLauncher;
impl RunLauncher for NoopLauncher {
    fn launch(&self, _run_id: i64) {}
}

struct Server {
    _dir: TempDir,
    db: Db,
    engine: Arc<Engine>,
    app: Router,
}

/// A control-plane server that accepts worker token "secret".
fn server() -> Server {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("server.db");
    let db = Db::open(&db_path).expect("open db");
    let pool = r2d2::Pool::builder()
        .max_size(4)
        .build(r2d2_sqlite::SqliteConnectionManager::file(&db_path))
        .expect("pool");
    let secrets =
        Arc::new(SecretStore::open(&dir.path().join("master.key"), pool).expect("secrets"));
    let registry = Arc::new(PluginRegistry::builtin());
    let engine = Engine::new(db.clone(), Arc::clone(&registry), Arc::clone(&secrets));
    let scheduler = Scheduler::new(db.clone(), Arc::new(NoopLauncher), Arc::new(SystemClock));
    let state = AppState {
        db: db.clone(),
        engine: Arc::clone(&engine),
        registry,
        secrets,
        scheduler,
        worker_tokens: Arc::new(vec!["secret".to_string()]),
    };
    let app = api::router(state);
    Server {
        _dir: dir,
        db,
        engine,
        app,
    }
}

async fn spawn(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(axum::serve(listener, app).into_future());
    addr
}

fn save_flow(db: &Db, id: &str, def: &Value) {
    db.upsert_flow_with_revision(id, id, "default", "", &def.to_string(), "init")
        .expect("save flow");
}

async fn wait_status(db: &Db, run_id: i64, want: &str, max: Duration) -> String {
    let deadline = Instant::now() + max;
    loop {
        let status = db.get_run(run_id).unwrap().unwrap().status;
        if status == want || Instant::now() > deadline {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn worker_claims_executes_and_reports_gpu_run() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ids": [7, 8, 9]})))
        .mount(&upstream)
        .await;

    let srv = server();
    let def = json!({
        "name": "gpu-job",
        "queue": "gpu",
        "tasks": [{
            "id": "fetch",
            "type": "http.request",
            "config": {"method": "GET", "url": format!("{}/ids", upstream.uri())},
            "on_error": "fail",
            "outputs": [{"name": "ids", "type": "ARRAY", "extract": "result.body.ids"}]
        }]
    });
    save_flow(&srv.db, "gpu_job", &def);

    // Create the run: routing leaves it queued (nothing runs it locally).
    let run_id = srv
        .engine
        .create_run("gpu_job", serde_json::Map::new(), "manual", None)
        .expect("create run");
    srv.engine.start(run_id).expect("start (no-op for gpu)");
    assert_eq!(srv.db.get_run(run_id).unwrap().unwrap().status, "queued");

    let addr = spawn(srv.app.clone()).await;

    // Launch a real worker against the server.
    let wdir = tempfile::tempdir().expect("worker dir");
    let cfg = WorkerConfig {
        server_url: format!("http://{addr}"),
        token: "secret".to_string(),
        worker_id: "test-worker".to_string(),
        queues: vec!["gpu".to_string()],
        capacity: 2,
        db_path: wdir.path().join("worker.db"),
        key_path: wdir.path().join("worker.key"),
    };
    let shutdown = CancellationToken::new();
    let worker_task = tokio::spawn(worker::run(cfg, shutdown.clone()));

    // The worker should claim, execute, and report the run to success.
    let status = wait_status(&srv.db, run_id, "success", Duration::from_secs(15)).await;
    assert_eq!(status, "success", "run did not reach success via worker");

    // The task output made it back to the server's database.
    let tasks = srv.db.list_task_runs(run_id).unwrap();
    let fetch = tasks
        .iter()
        .find(|t| t.task_id == "fetch")
        .expect("fetch task");
    assert_eq!(fetch.status, "success");
    let outputs: Value = serde_json::from_str(fetch.outputs.as_deref().unwrap()).unwrap();
    assert_eq!(outputs["ids"], json!([7, 8, 9]));

    // Logs streamed back too.
    let logs = srv.db.list_logs(run_id, 0, 1000).unwrap();
    assert!(logs.iter().any(|l| l.task == "fetch"));

    shutdown.cancel();
    let _ = worker_task.await;
}

#[tokio::test]
async fn worker_registry_reports_status() {
    let srv = server();
    // A claim registers the worker even when it yields no work.
    let assignments = srv
        .engine
        .claim_remote("w-alpha", &["gpu", "cpu"], 3, 30)
        .expect("claim");
    assert!(assignments.is_empty());

    let statuses = srv.engine.worker_statuses().expect("statuses");
    assert_eq!(statuses.len(), 1);
    let w = &statuses[0];
    assert_eq!(w.worker_id, "w-alpha");
    assert_eq!(w.queues, vec!["gpu", "cpu"]);
    assert_eq!(w.capacity, 3);
    assert_eq!(w.in_flight, 0);
    assert!(w.online, "just-claimed worker should be online");
}

#[tokio::test]
async fn workers_endpoint_reports_enabled_and_list() {
    let srv = server(); // has worker token "secret"
    srv.engine
        .claim_remote("w1", &["gpu"], 2, 30)
        .expect("claim");
    let addr = spawn(srv.app.clone()).await;

    let body: Value = reqwest::get(format!("http://{addr}/api/workers"))
        .await
        .expect("get")
        .json()
        .await
        .expect("json");
    assert_eq!(body["enabled"], json!(true));
    let workers = body["workers"].as_array().expect("workers array");
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0]["worker_id"], json!("w1"));
    assert_eq!(workers[0]["queues"], json!(["gpu"]));
    assert_eq!(workers[0]["capacity"], json!(2));
    assert_eq!(workers[0]["online"], json!(true));
}

#[tokio::test]
async fn worker_api_rejects_bad_token() {
    let srv = server();
    let addr = spawn(srv.app.clone()).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/worker/claim"))
        .bearer_auth("wrong")
        .json(&json!({"worker_id": "w", "queues": ["gpu"], "capacity": 1}))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 401);
}
