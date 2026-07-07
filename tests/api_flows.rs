//! Integration tests for the D1 API surface: flows CRUD/validate/revisions/
//! pause/import/export/run plus plugins, dashboard, schedules, and secrets.
//!
//! Each test builds the real `/api` router over a temp database, a real
//! engine (wiremock-backed where a run actually executes), a real scheduler
//! (no tick loop driven; a no-op launcher), and a real secret store on a
//! temp keyfile.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use orchestrator::api::{self, AppState};
use orchestrator::db::{Db, RunStatusUpdate, now_rfc3339};
use orchestrator::engine::Engine;
use orchestrator::scheduler::{RunLauncher, Scheduler, SystemClock};
use orchestrator::secrets::SecretStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Scheduler launcher that does nothing: no test drives scheduler ticks.
struct NoopLauncher;

impl RunLauncher for NoopLauncher {
    fn launch(&self, _run_id: i64) {}
}

struct Env {
    _dir: TempDir,
    db: Db,
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
        engine,
        registry,
        secrets,
        scheduler,
        worker_tokens: Arc::new(vec![]),
    };
    // Merge the UI router like `main` does: it owns the JSON 404 fallback
    // for unknown /api paths.
    let app = api::router(state).merge(orchestrator::ui::router());
    Env { _dir: dir, db, app }
}

/// Send a request with an optional raw body; returns status, headers, text.
async fn raw(
    app: &Router,
    method: &str,
    uri: &str,
    content_type: Option<&str>,
    body: Option<String>,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, "orch_session=test-session");
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    let request = builder
        .body(body.map(Body::from).unwrap_or_else(Body::empty))
        .expect("build request");
    let response = app.clone().oneshot(request).await.expect("send request");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf-8 body");
    (status, headers, text)
}

/// Send a JSON request; returns status and parsed JSON body (Null if empty).
async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let content_type = body.is_some().then_some("application/json");
    let (status, _, text) = raw(app, method, uri, content_type, body.map(|v| v.to_string())).await;
    let value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("non-JSON body: {text}"))
    };
    (status, value)
}

/// Minimal valid definition with one daily schedule trigger.
fn sample_definition(name: &str) -> Value {
    json!({
        "name": name,
        "namespace": "default",
        "description": "a sample flow",
        "inputs": [],
        "variables": [],
        "triggers": [{"id": "daily", "type": "schedule", "cron": "0 9 * * *"}],
        "tasks": [
            {"id": "t1", "type": "http.request",
             "config": {"method": "GET", "url": "http://127.0.0.1:1/never-called"}}
        ]
    })
}

/// Valid definition with no triggers and no tasks (runs finish instantly).
fn trivial_definition(name: &str) -> Value {
    json!({ "name": name, "tasks": [] })
}

async fn create_flow(env: &Env, id: &str, definition: Value) {
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows",
        Some(json!({ "id": id, "definition": definition })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
}

async fn wait_for_finish(env: &Env, run_id: i64) -> orchestrator::db::RunRow {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let run = env.db.get_run(run_id).expect("get run").expect("run row");
        if matches!(run.status.as_str(), "success" | "failed" | "canceled") {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} did not finish (status: {})",
            run.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// Flow creation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_flow_with_explicit_id_returns_detail() {
    let env = new_env();
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows",
        Some(json!({ "id": "my_flow", "definition": sample_definition("Sample") })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["id"], "my_flow");
    assert_eq!(body["current_rev"], 1);
    assert_eq!(body["paused"], false);
    assert!(body["definition"].is_object(), "definition must be parsed");
    assert!(body["updated_at"].is_string());
    // Saving reconciled schedule state for the trigger.
    let schedules = env.db.list_schedules().unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].trigger_id, "daily");
    assert!(schedules[0].next_fire_at.is_some());
}

#[tokio::test]
async fn create_flow_slugifies_name_when_id_absent() {
    let env = new_env();
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows",
        Some(json!({ "definition": trivial_definition("  My Fancy Flow!  ") })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["id"], "my_fancy_flow");
}

#[tokio::test]
async fn create_duplicate_flow_conflicts() {
    let env = new_env();
    create_flow(&env, "dupe", trivial_definition("Dupe")).await;
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows",
        Some(json!({ "id": "dupe", "definition": trivial_definition("Dupe") })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("dupe"));
}

#[tokio::test]
async fn create_and_update_reject_invalid_definitions_with_422() {
    let env = new_env();
    // Empty name + config missing its required url.
    let bad = json!({
        "name": "",
        "tasks": [{"id": "t1", "type": "http.request", "config": {}}]
    });
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows",
        Some(json!({ "id": "bad", "definition": bad })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let errors = body["errors"].as_array().expect("errors array");
    assert!(!errors.is_empty());
    for err in errors {
        assert!(err["path"].is_string(), "issue has a path: {err}");
        assert!(err["message"].is_string(), "issue has a message: {err}");
    }
    assert!(errors.iter().any(|e| e["path"] == "name"));
    assert!(errors.iter().any(|e| e["path"] == "tasks[0].config"));
    // Nothing was saved.
    assert_eq!(send(&env.app, "GET", "/api/flows/bad", None).await.0, 404);

    // PUT returns the same shape.
    create_flow(&env, "good", trivial_definition("Good")).await;
    let (status, body) = send(
        &env.app,
        "PUT",
        "/api/flows/good",
        Some(json!({ "definition": {"name": "", "tasks": []} })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["errors"].as_array().is_some_and(|e| !e.is_empty()));

    // A definition that does not even deserialize also 422s.
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows",
        Some(json!({ "definition": {"name": "X", "bogus_field": 1} })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["path"], "definition");
}

// ---------------------------------------------------------------------------
// Detail, update, delete, revisions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_flow_detail_parses_definition_to_object() {
    let env = new_env();
    create_flow(&env, "detailed", sample_definition("Detailed")).await;
    let (status, body) = send(&env.app, "GET", "/api/flows/detailed", None).await;
    assert_eq!(status, StatusCode::OK);
    let tasks = body["definition"]["tasks"]
        .as_array()
        .expect("definition.tasks is a JSON array, not a string");
    assert_eq!(tasks[0]["id"], "t1");
    assert_eq!(body["definition"]["triggers"][0]["cron"], "0 9 * * *");

    let (status, body) = send(&env.app, "GET", "/api/flows/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn update_bumps_revision_and_revisions_are_listable() {
    let env = new_env();
    create_flow(&env, "revs", trivial_definition("Original")).await;
    let (status, body) = send(
        &env.app,
        "PUT",
        "/api/flows/revs",
        Some(json!({ "definition": trivial_definition("Renamed"), "message": "second" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_rev"], 2);

    // Newest first, with messages.
    let (status, body) = send(&env.app, "GET", "/api/flows/revs/revisions", None).await;
    assert_eq!(status, StatusCode::OK);
    let revs = body.as_array().expect("array");
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0]["rev"], 2);
    assert_eq!(revs[0]["message"], "second");
    assert_eq!(revs[1]["rev"], 1);
    assert_eq!(revs[1]["message"], "create");
    assert!(revs[0]["created_at"].is_string());

    // Fetch one revision: parsed definition.
    let (status, body) = send(&env.app, "GET", "/api/flows/revs/revisions/1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["definition"]["name"], "Original");

    // Missing revision and missing flow both 404.
    let (status, _) = send(&env.app, "GET", "/api/flows/revs/revisions/99", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&env.app, "GET", "/api/flows/ghost/revisions", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_flow_removes_it_and_its_schedules() {
    let env = new_env();
    create_flow(&env, "victim", sample_definition("Victim")).await;
    assert_eq!(env.db.list_schedules().unwrap().len(), 1);

    let (status, _) = send(&env.app, "DELETE", "/api/flows/victim", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(env.db.list_schedules().unwrap().len(), 0);
    assert_eq!(
        send(&env.app, "GET", "/api/flows/victim", None).await.0,
        404
    );

    let (status, _) = send(&env.app, "DELETE", "/api/flows/victim", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_nonexistent_flow_is_404() {
    let env = new_env();
    let (status, body) = send(
        &env.app,
        "PUT",
        "/api/flows/no_such_flow",
        Some(json!({ "definition": trivial_definition("Ghost") })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("no_such_flow"));
}

// ---------------------------------------------------------------------------
// Flows list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flows_list_has_summary_shape_and_stats() {
    let env = new_env();
    create_flow(&env, "scheduled", sample_definition("Scheduled")).await;
    create_flow(&env, "manual_only", trivial_definition("Manual Only")).await;

    // Seed a finished run for the scheduled flow.
    let run_id = env
        .db
        .insert_run("scheduled", 1, "manual", "{}", "local", None)
        .unwrap();
    let now = now_rfc3339();
    env.db
        .update_run_status(
            run_id,
            RunStatusUpdate {
                status: "success",
                error: None,
                started_at: Some(&now),
                finished_at: Some(&now),
            },
        )
        .unwrap();

    let (status, body) = send(&env.app, "GET", "/api/flows", None).await;
    assert_eq!(status, StatusCode::OK);
    let flows = body.as_array().expect("array");
    assert_eq!(flows.len(), 2);

    let scheduled = flows.iter().find(|f| f["id"] == "scheduled").unwrap();
    assert_eq!(scheduled["name"], "Scheduled");
    assert_eq!(scheduled["namespace"], "default");
    assert_eq!(scheduled["paused"], false);
    assert_eq!(scheduled["schedule_human"], "daily · 09:00");
    assert_eq!(scheduled["last_run"]["status"], "success");
    assert!(scheduled["last_run"]["finished_at"].is_string());
    assert_eq!(scheduled["success_rate_30d"], 1.0);
    assert!(scheduled["avg_duration_sec"].is_number());
    assert_eq!(scheduled["current_rev"], 1);

    let manual = flows.iter().find(|f| f["id"] == "manual_only").unwrap();
    assert_eq!(manual["schedule_human"], "manual");
    assert!(manual["last_run"].is_null());
    assert!(manual["success_rate_30d"].is_null());
}

// ---------------------------------------------------------------------------
// Pause + run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pause_blocks_manual_runs_until_unpaused() {
    let env = new_env();
    create_flow(&env, "pausable", trivial_definition("Pausable")).await;

    let (status, _) = send(
        &env.app,
        "POST",
        "/api/flows/pausable/pause",
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, detail) = send(&env.app, "GET", "/api/flows/pausable", None).await;
    assert_eq!(detail["paused"], true);

    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows/pausable/run",
        Some(json!({ "inputs": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "flow is paused");

    let (status, _) = send(
        &env.app,
        "POST",
        "/api/flows/pausable/pause",
        Some(json!({ "paused": false })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows/pausable/run",
        Some(json!({ "inputs": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unpaused run rejected: {body}");
    assert!(body["run_id"].is_i64());
}

#[tokio::test]
async fn run_endpoint_whitelists_trigger_values() {
    let env = new_env();
    create_flow(&env, "triggered", trivial_definition("Triggered")).await;

    // Only `manual` and `api` are accepted from clients.
    for trigger in ["schedule", "replay", "", "MANUAL"] {
        let (status, body) = send(
            &env.app,
            "POST",
            "/api/flows/triggered/run",
            Some(json!({ "inputs": {}, "trigger": trigger })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "trigger `{trigger}`: {body}"
        );
        assert!(
            body["error"].as_str().unwrap().contains("trigger"),
            "error names the trigger field: {body}"
        );
    }

    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows/triggered/run",
        Some(json!({ "inputs": {}, "trigger": "api" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "api trigger rejected: {body}");
    let run_id = body["run_id"].as_i64().expect("run_id");
    let run = wait_for_finish(&env, run_id).await;
    assert_eq!(run.trigger, "api");
}

#[tokio::test]
async fn run_endpoint_executes_flow_against_wiremock() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/greet"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"greeting": "hi"})))
        .expect(1)
        .mount(&server)
        .await;

    create_flow(
        &env,
        "greeter",
        json!({
            "name": "Greeter",
            "tasks": [
                {"id": "hello", "type": "http.request",
                 "config": {"method": "GET", "url": format!("{}/greet", server.uri())},
                 "outputs": [{"name": "greeting", "type": "STRING",
                              "extract": "result.body.greeting"}]}
            ]
        }),
    )
    .await;

    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows/greeter/run",
        Some(json!({ "inputs": {}, "trigger": "manual" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "run failed: {body}");
    let run_id = body["run_id"].as_i64().expect("run_id");

    let run = wait_for_finish(&env, run_id).await;
    assert_eq!(run.status, "success");
    assert_eq!(run.trigger, "manual");
    let tasks = env.db.list_task_runs(run_id).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, "success");
}

#[tokio::test]
async fn run_endpoint_maps_invalid_inputs_and_unknown_flow() {
    let env = new_env();
    create_flow(
        &env,
        "needs_city",
        json!({
            "name": "Needs City",
            "inputs": [{"id": "city", "type": "STRING", "required": true}],
            "tasks": []
        }),
    )
    .await;

    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows/needs_city/run",
        Some(json!({ "inputs": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let errors = body["errors"].as_array().expect("errors array");
    assert!(!errors.is_empty());
    assert!(errors[0]["message"].as_str().unwrap().contains("city"));

    let (status, _) = send(
        &env.app,
        "POST",
        "/api/flows/no_such_flow/run",
        Some(json!({ "inputs": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Export / import / validate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_then_reimport_bumps_revision_instead_of_duplicating() {
    let env = new_env();
    create_flow(&env, "porter", sample_definition("Porter Flow")).await;

    let (status, headers, yaml) =
        raw(&env.app, "GET", "/api/flows/porter/export", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let content_type = headers[header::CONTENT_TYPE].to_str().unwrap();
    assert!(content_type.starts_with("text/yaml"), "got {content_type}");
    let disposition = headers[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(disposition.contains("attachment"));
    assert!(disposition.contains("porter.yaml"));
    assert!(yaml.starts_with("id: porter\n"));

    // Round-trip with a rename: same id imports as a new revision.
    let renamed = yaml.replace("name: Porter Flow", "name: Ported Flow");
    let (status, _, body) = raw(
        &env.app,
        "POST",
        "/api/flows/import",
        Some("text/yaml"),
        Some(renamed.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reimport failed: {body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["id"], "porter");
    assert_eq!(body["current_rev"], 2);
    assert_eq!(body["definition"]["name"], "Ported Flow");
    let (_, flows) = send(&env.app, "GET", "/api/flows", None).await;
    assert_eq!(flows.as_array().unwrap().len(), 1, "no duplicate flow");

    // A fresh id imports as a brand-new flow -> 201.
    let fresh = renamed.replace("id: porter", "id: porter_two");
    let (status, _, body) = raw(
        &env.app,
        "POST",
        "/api/flows/import",
        Some("text/yaml"),
        Some(fresh),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "fresh import failed: {body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["id"], "porter_two");
    assert_eq!(body["current_rev"], 1);

    let (status, _) = send(&env.app, "GET", "/api/flows/missing/export", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn import_rejects_bad_yaml_and_invalid_definitions() {
    let env = new_env();
    let (status, _, body) = raw(
        &env.app,
        "POST",
        "/api/flows/import",
        Some("text/yaml"),
        Some("id: [oops".to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert!(!body["error"].as_str().unwrap().is_empty());

    // Parseable YAML, invalid definition -> 422 with issues.
    let (status, _, body) = raw(
        &env.app,
        "POST",
        "/api/flows/import",
        Some("text/yaml"),
        Some("id: invalid_import\nname: ''\ntasks: []\n".to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert!(body["errors"].as_array().is_some_and(|e| !e.is_empty()));
}

#[tokio::test]
async fn validate_endpoint_always_200_with_errors_in_body() {
    let env = new_env();
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows/validate",
        Some(json!({ "definition": sample_definition("Valid") })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["errors"], json!([]));

    let (status, body) = send(
        &env.app,
        "POST",
        "/api/flows/validate",
        Some(json!({ "definition": {
            "name": "X",
            "tasks": [{"id": "t", "type": "no.such.plugin", "config": {}}]
        }})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let errors = body["errors"].as_array().unwrap();
    assert!(errors.iter().any(|e| e["path"] == "tasks[0].type"
        && e["message"].as_str().unwrap().contains("no.such.plugin")));
}

// ---------------------------------------------------------------------------
// Plugins, dashboard, schedules, secrets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flow_schema_endpoint_serves_a_json_schema_covering_plugins() {
    let env = new_env();
    let (status, headers, text) = raw(&env.app, "GET", "/api/flow.schema.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let schema: Value = serde_json::from_str(&text).expect("JSON schema body");
    assert_eq!(schema["type"], json!("object"));
    // The tasks union reflects installed plugins.
    let branches = schema["$defs"]["Task"]["oneOf"]
        .as_array()
        .expect("Task oneOf");
    assert!(
        branches
            .iter()
            .any(|b| b["properties"]["type"]["const"] == json!("http.request")),
        "http.request task branch present"
    );
    // The top-level YAML `id` key is part of the schema.
    assert_eq!(schema["properties"]["id"]["type"], json!("string"));
}

#[tokio::test]
async fn plugins_endpoint_lists_builtin_manifests() {
    let env = new_env();
    let (status, body) = send(&env.app, "GET", "/api/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    let manifests = body.as_array().expect("array");
    assert!(
        manifests.iter().any(|m| m["type_id"] == "http.request"),
        "http.request manifest present"
    );
    assert!(manifests[0]["fields"].is_array());
}

#[tokio::test]
async fn dashboard_reports_metrics_and_next_scheduled() {
    let env = new_env();

    // Empty database: zeros and nulls.
    let (status, body) = send(&env.app, "GET", "/api/dashboard", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active_flows"], 0);
    assert_eq!(body["runs_24h"]["total"], 0);
    assert!(body["success_rate_30d"].is_null());
    assert!(body["avg_duration_sec"].is_null());
    assert!(body["next_scheduled"].is_null());

    // One scheduled flow + one success and one failed run.
    create_flow(&env, "dash", sample_definition("Dash")).await;
    let now = now_rfc3339();
    for status_str in ["success", "failed"] {
        let run_id = env
            .db
            .insert_run("dash", 1, "manual", "{}", "local", None)
            .unwrap();
        env.db
            .update_run_status(
                run_id,
                RunStatusUpdate {
                    status: status_str,
                    error: None,
                    started_at: Some(&now),
                    finished_at: Some(&now),
                },
            )
            .unwrap();
    }

    let (status, body) = send(&env.app, "GET", "/api/dashboard", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["active_flows"], 1);
    assert_eq!(body["runs_24h"]["total"], 2);
    assert_eq!(body["runs_24h"]["ok"], 1);
    assert_eq!(body["runs_24h"]["failed"], 1);
    assert_eq!(body["runs_24h"]["running"], 0);
    assert_eq!(body["success_rate_30d"], 0.5);
    assert!(body["avg_duration_sec"].is_number());
    assert_eq!(body["next_scheduled"]["flow_id"], "dash");
    assert!(body["next_scheduled"]["at"].is_string());

    // A paused flow's schedule no longer counts toward next_scheduled.
    let (status, _) = send(
        &env.app,
        "POST",
        "/api/flows/dash/pause",
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&env.app, "GET", "/api/dashboard", None).await;
    assert!(body["next_scheduled"].is_null());
    assert_eq!(body["active_flows"], 0);
}

#[tokio::test]
async fn schedules_list_joins_definition_and_toggle_flips_enabled() {
    let env = new_env();
    create_flow(&env, "cron_flow", sample_definition("Cron Flow")).await;

    let (status, body) = send(&env.app, "GET", "/api/schedules", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["flow_id"], "cron_flow");
    assert_eq!(row["flow_name"], "Cron Flow");
    assert_eq!(row["trigger_id"], "daily");
    assert_eq!(row["cron"], "0 9 * * *");
    assert_eq!(row["timezone"], "UTC");
    assert_eq!(row["human"], "daily · 09:00");
    assert_eq!(row["catchup"], "latest");
    assert_eq!(row["enabled"], true);
    assert!(row["next_fire_at"].is_string());
    assert!(row["last_fired_at"].is_null());
    assert!(row["last_run_status"].is_null());

    // Toggle off, then back on.
    let (status, _) = send(
        &env.app,
        "POST",
        "/api/schedules/cron_flow/daily/toggle",
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&env.app, "GET", "/api/schedules", None).await;
    assert_eq!(body[0]["enabled"], false);

    let (status, _) = send(
        &env.app,
        "POST",
        "/api/schedules/cron_flow/daily/toggle",
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&env.app, "GET", "/api/schedules", None).await;
    assert_eq!(body[0]["enabled"], true);
    assert!(body[0]["next_fire_at"].is_string());

    // Unknown flow / unknown trigger both 404.
    let (status, body) = send(
        &env.app,
        "POST",
        "/api/schedules/ghost/daily/toggle",
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());
    let (status, _) = send(
        &env.app,
        "POST",
        "/api/schedules/cron_flow/ghost/toggle",
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn secrets_lifecycle_never_exposes_values() {
    let env = new_env();
    let (status, _) = send(
        &env.app,
        "PUT",
        "/api/secrets/API_TOKEN",
        Some(json!({ "value": "hush-hush" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, text) = raw(&env.app, "GET", "/api/secrets", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!text.contains("hush-hush"), "value leaked: {text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    let list = body.as_array().expect("array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "API_TOKEN");
    assert!(list[0]["created_at"].is_string());
    assert!(list[0]["updated_at"].is_string());
    assert!(list[0].get("value").is_none(), "no value key in listing");

    // Invalid name -> 400 with the secrets error message.
    let (status, body) = send(
        &env.app,
        "PUT",
        "/api/secrets/bad-name",
        Some(json!({ "value": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("invalid secret name")
    );

    let (status, _) = send(&env.app, "DELETE", "/api/secrets/API_TOKEN", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&env.app, "DELETE", "/api/secrets/API_TOKEN", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, body) = send(&env.app, "GET", "/api/secrets", None).await;
    assert_eq!(body, json!([]));
}

// ---------------------------------------------------------------------------
// Fallback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_api_path_gets_json_404() {
    let env = new_env();
    let (status, body) = send(&env.app, "GET", "/api/definitely/not/here", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({ "error": "not found" }));
}
