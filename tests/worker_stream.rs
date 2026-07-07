//! Proves the worker-reporting core: running a flow through a `RemoteSink`
//! produces a `RunUpdate` stream that, re-applied via `apply_update` against a
//! fresh server database, reproduces the same run state a local execution
//! writes. This is the whole "worker = same engine, reporting over a stream"
//! design, exercised without any networking.

use std::sync::Arc;

use orchestrator::db::Db;
use orchestrator::engine::{Engine, LocalSink, RemoteSink, RunEvent, RunUpdate, apply_update};
use orchestrator::plugins::PluginRegistry;
use orchestrator::secrets::SecretStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A throwaway engine over a fresh temp database (the worker's local scratch).
struct Scratch {
    _dir: TempDir,
    db: Db,
    engine: Arc<Engine>,
}

fn scratch() -> Scratch {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("scratch.db");
    let db = Db::open(&db_path).expect("open db");
    let pool = r2d2::Pool::builder()
        .max_size(2)
        .build(r2d2_sqlite::SqliteConnectionManager::file(&db_path))
        .expect("pool");
    let secrets =
        Arc::new(SecretStore::open(&dir.path().join("master.key"), pool).expect("secrets"));
    let engine = Engine::new(db.clone(), Arc::new(PluginRegistry::builtin()), secrets);
    Scratch {
        _dir: dir,
        db,
        engine,
    }
}

fn save_flow(db: &Db, id: &str, def: &Value) {
    db.upsert_flow_with_revision(id, id, "default", "", &def.to_string(), "init")
        .expect("save flow");
}

/// Run `flow` on a scratch engine through a `RemoteSink`, returning the
/// scratch db (the reference "local" state) and the captured update stream.
async fn run_and_capture(flow_id: &str, def: Value) -> (Db, Vec<RunUpdate>) {
    let s = scratch();
    save_flow(&s.db, flow_id, &def);
    let run_id =
        s.db.insert_run(flow_id, 1, "manual", "{}", "gpu", None)
            .expect("insert run");
    let run = s.db.get_run(run_id).unwrap().unwrap();

    let (btx, _brx) = broadcast::channel::<RunEvent>(1024);
    let (utx, mut urx) = mpsc::unbounded_channel::<RunUpdate>();
    let sink = Arc::new(RemoteSink::new(LocalSink::new(s.db.clone(), btx), utx));

    s.engine
        .execute_to_sink(run, CancellationToken::new(), sink)
        .await;

    let mut updates = Vec::new();
    while let Ok(u) = urx.try_recv() {
        updates.push(u);
    }
    (s.db, updates)
}

/// Rebuild a server-side db from a captured stream and return it plus the
/// server run id.
fn apply_to_server(flow_id: &str, def: &Value, updates: &[RunUpdate]) -> (TempDir, Db, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path().join("server.db")).expect("open server db");
    save_flow(&db, flow_id, def);
    let run_id = db
        .insert_run(flow_id, 1, "manual", "{}", "gpu", None)
        .expect("insert run");
    let (tx, _rx) = broadcast::channel::<RunEvent>(1024);
    for update in updates {
        apply_update(&db, &tx, run_id, update.clone()).expect("apply");
    }
    (dir, db, run_id)
}

#[tokio::test]
async fn stream_reproduces_sequential_run_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ids": [1, 2, 3]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sink"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;

    let def = json!({
        "name": "chain",
        "queue": "gpu",
        "tasks": [
            {
                "id": "fetch",
                "type": "http.request",
                "config": {"method": "GET", "url": format!("{}/ids", server.uri())},
                "on_error": "fail",
                "outputs": [{"name": "ids", "type": "ARRAY", "extract": "result.body.ids"}]
            },
            {
                "id": "push",
                "type": "http.request",
                "config": {
                    "method": "GET",
                    "url": format!("{}/sink?n={{{{ outputs.fetch.ids[0] }}}}", server.uri())
                },
                "on_error": "fail",
                "outputs": []
            }
        ]
    });

    let (local_db, updates) = run_and_capture("chain", def.clone()).await;
    assert!(!updates.is_empty(), "expected a non-empty update stream");

    // Reference local run succeeded.
    let local_run = local_db.get_run(1).unwrap().unwrap();
    assert_eq!(local_run.status, "success");

    let (_dir, server_db, srv_run) = apply_to_server("chain", &def, &updates);

    // Run status matches.
    let sr = server_db.get_run(srv_run).unwrap().unwrap();
    assert_eq!(sr.status, "success");

    // Task rows match on status + outputs.
    let local_tasks = local_db.list_task_runs(1).unwrap();
    let server_tasks = server_db.list_task_runs(srv_run).unwrap();
    assert_eq!(local_tasks.len(), server_tasks.len());
    for (l, s) in local_tasks.iter().zip(&server_tasks) {
        assert_eq!(l.task_id, s.task_id);
        assert_eq!(l.status, s.status, "task {} status", l.task_id);
        assert_eq!(l.outputs, s.outputs, "task {} outputs", l.task_id);
    }

    // The fetch task's declared output survived the round trip.
    let fetch = server_tasks.iter().find(|t| t.task_id == "fetch").unwrap();
    let outputs: Value = serde_json::from_str(fetch.outputs.as_deref().unwrap()).unwrap();
    assert_eq!(outputs["ids"], json!([1, 2, 3]));

    // Logs were reproduced (server assigns its own ids).
    let logs = server_db.list_logs(srv_run, 0, 1000).unwrap();
    assert!(
        logs.iter().any(|l| l.message.contains("succeeded")),
        "expected a success log line, got: {:?}",
        logs.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn stream_reproduces_fanout_items() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;

    let def = json!({
        "name": "fan",
        "queue": "gpu",
        "inputs": [{"id": "xs", "type": "ARRAY", "default": "[1,2,3,4]"}],
        "tasks": [{
            "id": "spread",
            "type": "parallel",
            "items": "{{ inputs.xs }}",
            "concurrency": 2,
            "tasks": [{
                "id": "hit",
                "type": "http.request",
                "config": {"method": "GET", "url": format!("{}/item", server.uri())},
                "on_error": "fail",
                "outputs": []
            }],
            "outputs": []
        }]
    });

    let (local_db, updates) = run_and_capture("fan", def.clone()).await;
    let (_dir, server_db, srv_run) = apply_to_server("fan", &def, &updates);

    assert_eq!(
        server_db.get_run(srv_run).unwrap().unwrap().status,
        "success"
    );

    // The parallel task's items were reproduced with matching statuses.
    let local_tr = &local_db.list_task_runs(1).unwrap()[0];
    let server_tr = &server_db.list_task_runs(srv_run).unwrap()[0];
    let local_agg = local_db.item_aggregates(local_tr.id).unwrap();
    let server_agg = server_db.item_aggregates(server_tr.id).unwrap();
    assert_eq!(server_agg.total, 4);
    assert_eq!(server_agg.success, 4);
    assert_eq!(local_agg, server_agg);
}
