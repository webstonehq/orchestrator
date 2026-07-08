//! API tests for run routes: list/detail, cancel, replay, logs, fan-out
//! items (list/heatmap/retry-failed), and the SSE event stream.
//!
//! Plain routes go through `tower::ServiceExt::oneshot`; SSE tests run a real
//! server on an ephemeral port because oneshot cannot read a live stream.

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use orchestrator::api::{AppState, router};
use orchestrator::db::{Db, ItemUpdate, RunStatusUpdate, TaskRunFinish};
use orchestrator::engine::Engine;
use orchestrator::scheduler::{RunLauncher, Scheduler, SystemClock};
use orchestrator::secrets::SecretStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Test environment
// ---------------------------------------------------------------------------

struct NoopLauncher;

impl RunLauncher for NoopLauncher {
    fn launch(&self, _run_id: i64) {}
}

struct Env {
    _dir: TempDir,
    db: Db,
    engine: Arc<Engine>,
    app: Router,
}

fn new_env() -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("orchestrator.db");
    let db = Db::open(&db_path).expect("open db");
    // Seed an authenticated session so the guarded data API is reachable; every
    // request in this suite carries `Cookie: orch_session=test-session`.
    let uid = db
        .create_first_user("tester", "unused-hash")
        .expect("seed user")
        .expect("empty users table");
    db.create_session("test-session", uid, 86_400)
        .expect("seed session");
    let pool = r2d2::Pool::builder()
        .max_size(2)
        .build(r2d2_sqlite::SqliteConnectionManager::file(&db_path))
        .expect("build secrets pool");
    let secrets =
        Arc::new(SecretStore::open(&dir.path().join("master.key"), pool).expect("open secrets"));
    let registry = Arc::new(orchestrator::plugins::testing::http_registry());
    let engine = Engine::new(db.clone(), Arc::clone(&registry), Arc::clone(&secrets));
    let scheduler = Scheduler::new(db.clone(), Arc::new(NoopLauncher), Arc::new(SystemClock));
    let state = AppState {
        db: db.clone(),
        engine: Arc::clone(&engine),
        registry,
        secrets,
        scheduler,
        worker_tokens: Arc::new(vec![]),
    };
    Env {
        _dir: dir,
        db,
        engine,
        app: router(state),
    }
}

fn save_flow(env: &Env, id: &str, definition: Value) {
    env.db
        .upsert_flow_with_revision(id, id, "default", "", &definition.to_string(), "init")
        .expect("save flow");
}

async fn request(env: &Env, method: &str, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", "orch_session=test-session")
        .body(Body::empty())
        .expect("build request");
    let res = env.app.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

async fn get(env: &Env, uri: &str) -> (StatusCode, Value) {
    request(env, "GET", uri).await
}

async fn post(env: &Env, uri: &str) -> (StatusCode, Value) {
    request(env, "POST", uri).await
}

async fn wait_for_finish(env: &Env, run_id: i64) -> orchestrator::db::RunRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let run = env.db.get_run(run_id).expect("get run").expect("run row");
        if matches!(run.status.as_str(), "success" | "failed" | "canceled") {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "run {run_id} did not finish (status: {})",
            run.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Minimal two-task flow definition (no inputs).
fn two_task_def(url: &str) -> Value {
    json!({
        "name": "two",
        "tasks": [
            {"id": "t1", "type": "http.request", "config": {"method": "GET", "url": url}},
            {"id": "t2", "type": "http.request", "config": {"method": "GET", "url": url}}
        ]
    })
}

/// Seed a run row with explicit status/timestamps (no engine involved).
fn seed_run(env: &Env, flow_id: &str, inputs: &str, status: &str) -> i64 {
    let flow = env.db.get_flow(flow_id).expect("get flow").expect("flow");
    let id = env
        .db
        .insert_run(flow_id, flow.current_rev, "manual", inputs, "local", None)
        .expect("insert run");
    if status != "queued" {
        env.db
            .update_run_status(
                id,
                RunStatusUpdate {
                    status,
                    error: None,
                    started_at: Some("2026-07-05T10:00:00.000Z"),
                    finished_at: matches!(status, "success" | "failed" | "canceled")
                        .then_some("2026-07-05T10:01:30.000Z"),
                },
            )
            .expect("update run status");
    }
    id
}

// ---------------------------------------------------------------------------
// GET /runs — list, filters, pagination, counts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_list_filters_pagination_and_counts() {
    let env = new_env();
    save_flow(&env, "f1", two_task_def("http://unused.invalid"));
    save_flow(
        &env,
        "f2",
        json!({
            "name": "f2",
            "tasks": [
                {"id": "only", "type": "http.request",
                 "config": {"method": "GET", "url": "http://unused.invalid"}}
            ]
        }),
    );

    let r1 = seed_run(&env, "f1", r#"{"a":1}"#, "success");
    env.db.upsert_task_run(r1, "t1", "success", 1).unwrap();
    env.db.upsert_task_run(r1, "t2", "success", 1).unwrap();
    let _r2 = seed_run(&env, "f1", "{}", "failed");
    let _r3 = seed_run(&env, "f2", "{}", "running");
    let r4 = seed_run(&env, "f2", "{}", "queued");

    let (status, body) = get(&env, "/api/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));
    assert_eq!(
        body["counts"],
        json!({"all": 4, "running": 1, "success": 1, "degraded": 0, "failed": 1, "queued": 1, "canceled": 0})
    );
    let runs = body["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4);
    assert_eq!(runs[0]["id"], json!(r4), "newest first");

    let first = runs.iter().find(|r| r["id"] == json!(r1)).unwrap();
    assert_eq!(first["inputs"], json!({"a": 1}), "inputs parsed to object");
    assert_eq!(first["duration_sec"], json!(90.0));
    assert_eq!(first["tasks_done"], json!(2));
    assert_eq!(first["tasks_total"], json!(2));
    assert_eq!(first["flow_id"], json!("f1"));
    assert_eq!(first["trigger"], json!("manual"));

    // A run with no timestamps has no duration and zero tasks done.
    let queued = runs.iter().find(|r| r["id"] == json!(r4)).unwrap();
    assert_eq!(queued["duration_sec"], Value::Null);
    assert_eq!(queued["tasks_done"], json!(0));
    assert_eq!(queued["tasks_total"], json!(1));

    // Flow filter.
    let (_, body) = get(&env, "/api/runs?flow=f1").await;
    assert_eq!(body["total"], json!(2));
    assert!(
        body["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["flow_id"] == json!("f1"))
    );

    // Status filter; counts stay global.
    let (_, body) = get(&env, "/api/runs?status=failed").await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["runs"].as_array().unwrap().len(), 1);
    assert_eq!(body["counts"]["all"], json!(4));

    // Empty filter values mean no filter.
    let (_, body) = get(&env, "/api/runs?flow=&status=").await;
    assert_eq!(body["total"], json!(4));

    // Pagination.
    let (_, body) = get(&env, "/api/runs?per=2&page=1").await;
    assert_eq!(body["runs"].as_array().unwrap().len(), 2);
    assert_eq!(body["total"], json!(4));
    let (_, body) = get(&env, "/api/runs?per=2&page=2").await;
    let page2 = body["runs"].as_array().unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[1]["id"], json!(r1), "oldest run on the last page");
}

// ---------------------------------------------------------------------------
// GET /runs/:id — detail shape
// ---------------------------------------------------------------------------

/// Flow with a plugin task followed by a parallel fan-out over `inputs.items`.
fn fanout_flow_def(url: &str) -> Value {
    json!({
        "name": "fan",
        "inputs": [{"id": "items", "type": "ARRAY", "required": true}],
        "tasks": [
            {"id": "t1", "type": "http.request", "config": {"method": "GET", "url": url}},
            {"id": "p1", "type": "parallel", "items": "{{ inputs.items }}", "concurrency": 2,
             "tasks": [
                 {"id": "child", "type": "http.request",
                  "config": {"method": "GET", "url": url}}
             ]}
        ]
    })
}

/// Seed a running fan-out run: t1 finished with result/outputs, p1 running
/// with 4 items (success, success, failed, running-retried).
fn seed_fanout_run(env: &Env) -> i64 {
    save_flow(env, "fan", fanout_flow_def("http://unused.invalid"));
    let run_id = seed_run(env, "fan", r#"{"items":[1,2,3,4]}"#, "running");
    env.db.upsert_task_run(run_id, "t1", "success", 1).unwrap();
    env.db
        .finish_task_run(
            run_id,
            "t1",
            TaskRunFinish {
                status: "success",
                result: Some(r#"{"status":200,"body":{"big":"payload"}}"#),
                outputs: Some(r#"{"code":200}"#),
                error: None,
            },
        )
        .unwrap();
    let p1 = env.db.upsert_task_run(run_id, "p1", "running", 1).unwrap();
    env.db
        .insert_items(p1, &[json!(1), json!(2), json!({"x": 3}), json!(4)])
        .unwrap();
    for (idx, status, attempt, result, error) in [
        (0, "success", 1, Some(r#"{"ok":true}"#), None),
        (1, "success", 1, Some(r#"{"ok":true}"#), None),
        (2, "failed", 1, None, Some("boom")),
        (3, "running", 2, None, None),
    ] {
        env.db
            .update_item(
                p1,
                idx,
                ItemUpdate {
                    status,
                    attempt,
                    result,
                    error,
                    started_at: Some("2026-07-05T10:00:01.000Z"),
                    finished_at: None,
                },
            )
            .unwrap();
    }
    run_id
}

#[tokio::test]
async fn run_detail_shape_and_result_flag() {
    let env = new_env();
    let run_id = seed_fanout_run(&env);

    let (status, body) = get(&env, &format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["run"]["id"], json!(run_id));
    assert_eq!(body["run"]["inputs"], json!({"items": [1, 2, 3, 4]}));
    assert_eq!(body["run"]["status"], json!("running"));
    assert_eq!(body["run"]["tasks_total"], json!(2));
    assert_eq!(body["run"]["tasks_done"], json!(1));

    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    let t1 = tasks.iter().find(|t| t["task_id"] == json!("t1")).unwrap();
    assert_eq!(t1["outputs"], json!({"code": 200}), "outputs parsed");
    assert!(
        t1.as_object().unwrap().get("result").is_none(),
        "result omitted by default"
    );

    assert_eq!(
        body["fanout"]["p1"],
        json!({
            "total": 4, "queued": 0, "running": 1, "success": 2,
            "failed": 1, "dropped": 0, "retried": 1
        })
    );

    // include_result=true adds the parsed result.
    let (_, body) = get(&env, &format!("/api/runs/{run_id}?include_result=true")).await;
    let tasks = body["tasks"].as_array().unwrap();
    let t1 = tasks.iter().find(|t| t["task_id"] == json!("t1")).unwrap();
    assert_eq!(t1["result"]["body"], json!({"big": "payload"}));

    let (status, body) = get(&env, "/api/runs/999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

// ---------------------------------------------------------------------------
// POST /runs/:id/cancel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_active_run_and_404_unknown() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;
    save_flow(
        &env,
        "slow",
        two_task_def(&format!("{}/slow", server.uri())),
    );

    let run_id = env
        .engine
        .create_and_start("slow", serde_json::Map::new(), "manual", None)
        .expect("start run");

    let (status, body) = post(&env, &format!("/api/runs/{run_id}/cancel")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"canceled": true}));
    let run = wait_for_finish(&env, run_id).await;
    assert_eq!(run.status, "canceled");

    // Finished run: known but inactive -> canceled: false.
    let (status, body) = post(&env, &format!("/api/runs/{run_id}/cancel")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"canceled": false}));

    let (status, _) = post(&env, "/api/runs/999999/cancel").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// POST /runs/:id/replay
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_creates_run_with_same_inputs() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    save_flow(
        &env,
        "greet",
        json!({
            "name": "greet",
            "inputs": [{"id": "name", "type": "STRING", "required": true}],
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "GET", "url": format!("{}/ok", server.uri())}}
            ]
        }),
    );
    let original = seed_run(&env, "greet", r#"{"name":"ada"}"#, "success");

    let (status, body) = post(&env, &format!("/api/runs/{original}/replay")).await;
    assert_eq!(status, StatusCode::OK);
    let new_id = body["run_id"].as_i64().expect("run_id");
    assert_ne!(new_id, original);
    let new_run = env.db.get_run(new_id).unwrap().unwrap();
    assert_eq!(new_run.trigger, "manual");
    assert_eq!(
        serde_json::from_str::<Value>(&new_run.inputs).unwrap(),
        json!({"name": "ada"})
    );
    wait_for_finish(&env, new_id).await;

    // Change the flow so the old inputs no longer validate -> 422.
    env.db
        .upsert_flow_with_revision(
            "greet",
            "greet",
            "default",
            "",
            &json!({
                "name": "greet",
                "tasks": [
                    {"id": "t1", "type": "http.request",
                     "config": {"method": "GET", "url": format!("{}/ok", server.uri())}}
                ]
            })
            .to_string(),
            "drop input",
        )
        .unwrap();
    let (status, body) = post(&env, &format!("/api/runs/{original}/replay")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Canonical validation shape (same as flows.rs): {"errors":[{path,message}]}.
    let errors = body["errors"].as_array().expect("errors array");
    assert_eq!(errors[0]["path"], json!("inputs"));
    assert!(errors[0]["message"].as_str().unwrap().contains("name"));

    let (status, _) = post(&env, "/api/runs/999999/replay").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn paused_flow_blocks_replay_and_retry_failed() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    save_flow(
        &env,
        "fan",
        fanout_flow_def(&format!("{}/ok", server.uri())),
    );

    // A finished run with one failed fan-out item (retry-failed material).
    let run_id = seed_run(&env, "fan", r#"{"items":[1]}"#, "failed");
    let p1 = env.db.upsert_task_run(run_id, "p1", "failed", 1).unwrap();
    env.db.insert_items(p1, &[json!(1)]).unwrap();
    env.db
        .update_item(
            p1,
            0,
            ItemUpdate {
                status: "failed",
                attempt: 1,
                ..Default::default()
            },
        )
        .unwrap();

    env.db.set_paused("fan", true).expect("pause flow");

    let (status, body) = post(&env, &format!("/api/runs/{run_id}/replay")).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "replay on paused flow: {body}"
    );
    assert_eq!(body["error"], json!("flow is paused"));

    let (status, body) = post(&env, &format!("/api/runs/{run_id}/tasks/p1/retry-failed")).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "retry-failed on paused flow: {body}"
    );
    assert_eq!(body["error"], json!("flow is paused"));

    // Unpausing lifts the block.
    env.db.set_paused("fan", false).expect("unpause flow");
    let (status, body) = post(&env, &format!("/api/runs/{run_id}/replay")).await;
    assert_eq!(status, StatusCode::OK, "replay after unpause: {body}");
    let new_id = body["run_id"].as_i64().expect("run_id");
    wait_for_finish(&env, new_id).await;
}

// ---------------------------------------------------------------------------
// GET /runs/:id/logs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn logs_page_by_after_id_and_limit() {
    let env = new_env();
    save_flow(&env, "f1", two_task_def("http://unused.invalid"));
    let run_id = seed_run(&env, "f1", "{}", "running");
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(
            env.db
                .append_log(run_id, "INFO", "flow", &format!("line {i}"))
                .unwrap(),
        );
    }

    let (status, body) = get(&env, &format!("/api/runs/{run_id}/logs")).await;
    assert_eq!(status, StatusCode::OK);
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 5);
    assert_eq!(logs[0]["message"], json!("line 0"));
    assert_eq!(logs[0]["level"], json!("INFO"));
    assert_eq!(logs[0]["task"], json!("flow"));
    assert!(logs[0]["ts"].is_string());

    let (_, body) = get(
        &env,
        &format!("/api/runs/{run_id}/logs?after_id={}", ids[1]),
    )
    .await;
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 3);
    assert_eq!(logs[0]["id"], json!(ids[2]));

    let (_, body) = get(&env, &format!("/api/runs/{run_id}/logs?limit=2")).await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 2);

    let (status, _) = get(&env, "/api/runs/999999/logs").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// GET /runs/:id/tasks/:task/items
// ---------------------------------------------------------------------------

#[tokio::test]
async fn items_pagination_status_filter_and_parsing() {
    let env = new_env();
    let run_id = seed_fanout_run(&env);

    let (status, body) = get(&env, &format!("/api/runs/{run_id}/tasks/p1/items")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(4));
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["idx"], json!(0));
    assert_eq!(items[0]["item"], json!(1), "item parsed");
    assert_eq!(items[0]["result"], json!({"ok": true}), "result parsed");
    assert_eq!(items[2]["item"], json!({"x": 3}));
    assert_eq!(items[2]["error"], json!("boom"));
    assert_eq!(items[2]["result"], Value::Null);
    assert_eq!(items[3]["attempt"], json!(2));

    // Status filter.
    let (_, body) = get(
        &env,
        &format!("/api/runs/{run_id}/tasks/p1/items?status=failed"),
    )
    .await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["idx"], json!(2));

    // Pagination.
    let (_, body) = get(
        &env,
        &format!("/api/runs/{run_id}/tasks/p1/items?per=2&page=2"),
    )
    .await;
    let page2 = body["items"].as_array().unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0]["idx"], json!(2));
    assert_eq!(body["total"], json!(4));

    // Unknown task / run -> 404.
    let (status, _) = get(&env, &format!("/api/runs/{run_id}/tasks/nope/items")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&env, "/api/runs/999999/tasks/p1/items").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn items_heatmap_format() {
    let env = new_env();
    let run_id = seed_fanout_run(&env);

    let (status, body) = get(
        &env,
        &format!("/api/runs/{run_id}/tasks/p1/items?format=heatmap"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"statuses": "ssfr", "total": 4}));
}

// ---------------------------------------------------------------------------
// POST /runs/:id/tasks/:task/retry-failed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_failed_launches_new_run_with_failed_item_values() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    save_flow(
        &env,
        "fan",
        fanout_flow_def(&format!("{}/ok", server.uri())),
    );

    let run_id = seed_run(&env, "fan", r#"{"items":[1,2,3,4]}"#, "failed");
    let p1 = env.db.upsert_task_run(run_id, "p1", "failed", 1).unwrap();
    env.db
        .insert_items(p1, &[json!({"v": 1}), json!({"v": 2}), json!({"v": 3})])
        .unwrap();
    for (idx, status) in [(0, "success"), (1, "failed"), (2, "failed")] {
        env.db
            .update_item(
                p1,
                idx,
                ItemUpdate {
                    status,
                    attempt: 1,
                    ..Default::default()
                },
            )
            .unwrap();
    }

    let (status, body) = post(&env, &format!("/api/runs/{run_id}/tasks/p1/retry-failed")).await;
    assert_eq!(status, StatusCode::OK);
    let new_id = body["run_id"].as_i64().expect("run_id");
    let new_run = env.db.get_run(new_id).unwrap().unwrap();
    assert_eq!(new_run.trigger, "manual");
    assert_eq!(
        serde_json::from_str::<Value>(&new_run.inputs).unwrap(),
        json!({"items": [{"v": 2}, {"v": 3}]}),
        "new run receives exactly the failed item values"
    );
    wait_for_finish(&env, new_id).await;

    // Unknown task -> 404.
    let (status, _) = post(&env, &format!("/api/runs/{run_id}/tasks/nope/retry-failed")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Unknown run -> 404.
    let (status, _) = post(&env, "/api/runs/999999/tasks/p1/retry-failed").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn retry_failed_without_items_input_is_422() {
    let env = new_env();
    save_flow(
        &env,
        "noitems",
        json!({
            "name": "noitems",
            "inputs": [{"id": "ids", "type": "ARRAY", "required": true}],
            "tasks": [
                {"id": "p1", "type": "parallel", "items": "{{ inputs.ids }}", "concurrency": 1,
                 "tasks": [
                     {"id": "child", "type": "http.request",
                      "config": {"method": "GET", "url": "http://unused.invalid"}}
                 ]}
            ]
        }),
    );
    let run_id = seed_run(&env, "noitems", r#"{"ids":[1]}"#, "failed");
    let p1 = env.db.upsert_task_run(run_id, "p1", "failed", 1).unwrap();
    env.db.insert_items(p1, &[json!(1)]).unwrap();
    env.db
        .update_item(
            p1,
            0,
            ItemUpdate {
                status: "failed",
                attempt: 1,
                ..Default::default()
            },
        )
        .unwrap();

    let (status, body) = post(&env, &format!("/api/runs/{run_id}/tasks/p1/retry-failed")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Canonical validation shape (same as flows.rs): {"errors":[{path,message}]}.
    let errors = body["errors"].as_array().expect("errors array");
    assert_eq!(errors[0]["path"], json!("inputs"));
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("`items` input"),
        "message explains the missing items input: {body}"
    );
}

#[tokio::test]
async fn retry_failed_with_zero_failed_items_is_400() {
    let env = new_env();
    save_flow(&env, "fan", fanout_flow_def("http://unused.invalid"));
    let run_id = seed_run(&env, "fan", r#"{"items":[1]}"#, "success");
    let p1 = env.db.upsert_task_run(run_id, "p1", "success", 1).unwrap();
    env.db.insert_items(p1, &[json!(1)]).unwrap();
    env.db
        .update_item(
            p1,
            0,
            ItemUpdate {
                status: "success",
                attempt: 1,
                ..Default::default()
            },
        )
        .unwrap();

    let (status, body) = post(&env, &format!("/api/runs/{run_id}/tasks/p1/retry-failed")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("no failed items"));
}

// ---------------------------------------------------------------------------
// SSE — real server + incremental byte-stream reads
// ---------------------------------------------------------------------------

async fn serve(env: &Env) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(axum::serve(listener, env.app.clone()).into_future());
    addr
}

/// Read SSE frames from `url` until an `end` event (or `max` elapses).
/// Returns `(event_name, data)` pairs; comment/keep-alive lines are ignored.
async fn read_sse_frames(url: &str, max: Duration) -> Vec<(String, String)> {
    let deadline = tokio::time::Instant::now() + max;
    let mut resp = reqwest::Client::new()
        .get(url)
        .header("cookie", "orch_session=test-session")
        .send()
        .await
        .expect("connect SSE");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );

    let mut buf = String::new();
    let mut frames: Vec<(String, String)> = Vec::new();
    loop {
        let chunk = tokio::time::timeout_at(deadline, resp.chunk())
            .await
            .expect("timed out waiting for SSE data")
            .expect("stream error");
        let Some(bytes) = chunk else {
            return frames; // server closed
        };
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = buf.find("\n\n") {
            let frame: String = buf.drain(..pos + 2).collect();
            let mut event = String::new();
            let mut data = String::new();
            for line in frame.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim_start());
                }
            }
            if event.is_empty() && data.is_empty() {
                continue; // keep-alive comment
            }
            let is_end = event == "end";
            frames.push((event, data));
            if is_end {
                return frames;
            }
        }
    }
}

#[tokio::test]
async fn sse_live_run_streams_snapshot_events_and_end() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true}))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(&server)
        .await;
    save_flow(
        &env,
        "slow",
        two_task_def(&format!("{}/slow", server.uri())),
    );
    let addr = serve(&env).await;

    let run_id = env
        .engine
        .create_and_start("slow", serde_json::Map::new(), "manual", None)
        .expect("start run");

    let frames = read_sse_frames(
        &format!("http://{addr}/api/runs/{run_id}/events"),
        Duration::from_secs(15),
    )
    .await;

    // First frame: snapshot with the detail payload + last_log_id.
    assert_eq!(frames[0].0, "snapshot");
    let snap: Value = serde_json::from_str(&frames[0].1).expect("snapshot JSON");
    assert_eq!(snap["run"]["id"], json!(run_id));
    assert!(snap["tasks"].is_array());
    assert!(snap["fanout"].is_object());
    assert!(
        snap["last_log_id"].as_i64().is_some(),
        "last_log_id present"
    );

    // Then at least one live engine event with a valid name and JSON data.
    let live = &frames[1..frames.len() - 1];
    assert!(!live.is_empty(), "expected live events, got {frames:?}");
    for (name, data) in live {
        assert!(
            matches!(name.as_str(), "run" | "task" | "items" | "log" | "snapshot"),
            "unexpected event name {name}"
        );
        assert!(serde_json::from_str::<Value>(data).is_ok());
    }
    assert!(
        live.iter().any(|(n, _)| n == "task"),
        "expected a task event"
    );
    let run_success = live
        .iter()
        .filter(|(n, _)| n == "run")
        .map(|(_, d)| serde_json::from_str::<Value>(d).unwrap())
        .any(|v| v["status"] == json!("success"));
    assert!(run_success, "expected a run event with status success");

    // Last frame: end.
    assert_eq!(frames.last().unwrap().0, "end");
    wait_for_finish(&env, run_id).await;
}

#[tokio::test]
async fn sse_finished_run_sends_snapshot_then_end() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    save_flow(&env, "quick", two_task_def(&format!("{}/ok", server.uri())));
    let addr = serve(&env).await;

    let run_id = env
        .engine
        .create_and_start("quick", serde_json::Map::new(), "manual", None)
        .expect("start run");
    wait_for_finish(&env, run_id).await;

    let frames = read_sse_frames(
        &format!("http://{addr}/api/runs/{run_id}/events"),
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(frames.len(), 2, "snapshot then end: {frames:?}");
    assert_eq!(frames[0].0, "snapshot");
    let snap: Value = serde_json::from_str(&frames[0].1).unwrap();
    assert_eq!(snap["run"]["status"], json!("success"));
    assert!(snap["last_log_id"].as_i64().unwrap() > 0);
    assert_eq!(frames[1].0, "end");

    // Unknown run -> 404 before any stream starts (authenticated; an anonymous
    // request would be 401'd by the session guard first).
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/runs/999999/events"))
        .header("cookie", "orch_session=test-session")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
