//! End-to-end engine tests: wiremock-backed flows exercising chaining,
//! extraction, retries, timeouts, fan-out, cancellation, redaction, input
//! resolution, recovery, and the live event stream.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orchestrator::db::{Db, RunRow, RunStatusUpdate};
use orchestrator::engine::{Engine, EngineError, RunEvent, Sleeper};
use orchestrator::plugins::PluginRegistry;
use orchestrator::secrets::SecretStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Env {
    _dir: TempDir,
    db: Db,
    engine: Arc<Engine>,
    secrets: Arc<SecretStore>,
}

fn new_env() -> Env {
    new_env_full(None, None)
}

fn new_env_with_sleeper(sleeper: Option<Sleeper>) -> Env {
    new_env_full(sleeper, None)
}

fn new_env_full(sleeper: Option<Sleeper>, registry: Option<PluginRegistry>) -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("orchestrator.db");
    let db = Db::open(&db_path).expect("open db");
    let pool = r2d2::Pool::builder()
        .max_size(2)
        .build(r2d2_sqlite::SqliteConnectionManager::file(&db_path))
        .expect("build secrets pool");
    let secrets =
        Arc::new(SecretStore::open(&dir.path().join("master.key"), pool).expect("open secrets"));
    let registry = Arc::new(registry.unwrap_or_else(orchestrator::plugins::testing::http_registry));
    let engine = match sleeper {
        Some(sleeper) => {
            Engine::new_with_sleeper(db.clone(), registry, Arc::clone(&secrets), sleeper)
        }
        None => Engine::new(db.clone(), registry, Arc::clone(&secrets)),
    };
    Env {
        _dir: dir,
        db,
        engine,
        secrets,
    }
}

fn save_flow(env: &Env, id: &str, definition: Value) {
    env.db
        .upsert_flow_with_revision(id, id, "default", "", &definition.to_string(), "init")
        .expect("save flow");
}

fn create_run(env: &Env, flow_id: &str, inputs: Value) -> Result<i64, EngineError> {
    let Value::Object(map) = inputs else {
        panic!("inputs must be a JSON object")
    };
    env.engine.create_run(flow_id, map, "manual", None)
}

async fn wait_for_finish(env: &Env, run_id: i64) -> RunRow {
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

async fn start_and_wait(env: &Env, run_id: i64) -> RunRow {
    env.engine.start(run_id).expect("start run");
    wait_for_finish(env, run_id).await
}

fn task_run<'a>(
    tasks: &'a [orchestrator::db::TaskRunRow],
    task_id: &str,
) -> &'a orchestrator::db::TaskRunRow {
    tasks
        .iter()
        .find(|t| t.task_id == task_id)
        .unwrap_or_else(|| panic!("no task_run for {task_id}"))
}

fn parse(s: &Option<String>) -> Value {
    serde_json::from_str(s.as_deref().expect("json column present")).expect("valid json")
}

// ---------------------------------------------------------------------------
// 1. Sequential chaining
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_task_chaining_passes_outputs_downstream() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ids": [1, 2]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/post"))
        .and(body_json(json!({"ids": [1, 2]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;

    save_flow(
        &env,
        "chain",
        json!({
            "name": "chain",
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "GET", "url": format!("{}/ids", server.uri())},
                 "outputs": [{"name": "ids", "type": "ARRAY", "extract": "result.body.ids"}]},
                {"id": "t2", "type": "http.request",
                 "config": {"method": "POST", "url": format!("{}/post", server.uri()),
                            "body": [{"key": "ids", "value": "{{ outputs.t1.ids }}"}]}}
            ]
        }),
    );

    let run_id = create_run(&env, "chain", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success");
    assert!(run.started_at.is_some() && run.finished_at.is_some());

    let tasks = env.db.list_task_runs(run_id).unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|t| t.status == "success"));
    assert_eq!(
        parse(&task_run(&tasks, "t1").outputs),
        json!({"ids": [1, 2]})
    );
    let t1_result = parse(&task_run(&tasks, "t1").result);
    assert_eq!(t1_result["body"], json!({"ids": [1, 2]}));

    // Finished runs are inert: not restartable, not subscribed, not active.
    assert!(matches!(
        env.engine.start(run_id),
        Err(EngineError::NotQueued(_))
    ));
    assert!(env.engine.subscribe(run_id).is_none());
    assert!(!env.engine.cancel(run_id));
    assert_eq!(env.engine.active_run_count(), 0);
}

// ---------------------------------------------------------------------------
// 2. Extraction failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn extraction_failure_fails_task_and_skips_downstream() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"other": 1})))
        .mount(&server)
        .await;

    save_flow(
        &env,
        "extract",
        json!({
            "name": "extract",
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "GET", "url": server.uri()},
                 "outputs": [{"name": "ids", "type": "ARRAY", "extract": "result.body.missing"}]},
                {"id": "t2", "type": "http.request",
                 "config": {"method": "GET", "url": server.uri()}}
            ]
        }),
    );

    let run_id = create_run(&env, "extract", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "failed");
    let error = run.error.unwrap();
    assert!(
        error.contains("result.body.missing") && error.contains("t1"),
        "error should name the task and path: {error}"
    );

    let tasks = env.db.list_task_runs(run_id).unwrap();
    assert_eq!(task_run(&tasks, "t1").status, "failed");
    assert!(
        task_run(&tasks, "t1")
            .error
            .as_deref()
            .unwrap()
            .contains("result.body.missing")
    );
    assert_eq!(task_run(&tasks, "t2").status, "skipped");
}

// ---------------------------------------------------------------------------
// 3. Retry with exponential backoff (injected sleeper)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_succeeds_on_third_attempt_with_exponential_backoff() {
    let sleeps: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&sleeps);
    let sleeper: Sleeper = Box::new(move |d| {
        recorded.lock().unwrap().push(d);
        Box::pin(async {})
    });
    let env = new_env_with_sleeper(Some(sleeper));

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_string("flaky"))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;

    save_flow(
        &env,
        "retry",
        json!({
            "name": "retry",
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "retry": {"type": "exponential", "max_attempts": 3, "base_seconds": 1},
                 "config": {"method": "GET", "url": server.uri()}}
            ]
        }),
    );

    let run_id = create_run(&env, "retry", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success");

    let tasks = env.db.list_task_runs(run_id).unwrap();
    assert_eq!(task_run(&tasks, "t1").attempt, 3);
    assert_eq!(task_run(&tasks, "t1").status, "success");

    // base_seconds * 2^(attempt-1): 1s after attempt 1, 2s after attempt 2.
    assert_eq!(
        *sleeps.lock().unwrap(),
        vec![Duration::from_secs(1), Duration::from_secs(2)]
    );

    let logs = env.db.list_logs(run_id, 0, 1000).unwrap();
    assert!(
        logs.iter()
            .any(|l| l.level == "WARN" && l.message.contains("retrying in 1s")),
        "expected a WARN retry log"
    );
}

// ---------------------------------------------------------------------------
// 4. Timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn timeout_fails_task_when_no_retry_policy() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;

    save_flow(
        &env,
        "slow",
        json!({
            "name": "slow",
            "tasks": [
                {"id": "t1", "type": "http.request", "timeout_seconds": 1,
                 "config": {"method": "GET", "url": server.uri()}}
            ]
        }),
    );

    let run_id = create_run(&env, "slow", json!({})).unwrap();
    let started = Instant::now();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "failed");
    assert!(
        run.error.as_deref().unwrap().contains("timed out after 1s"),
        "error: {:?}",
        run.error
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    let tasks = env.db.list_task_runs(run_id).unwrap();
    assert_eq!(task_run(&tasks, "t1").status, "failed");
    assert_eq!(task_run(&tasks, "t1").attempt, 1);
}

// ---------------------------------------------------------------------------
// 5. Fan-out: bounded concurrency, ordered results, downstream extraction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fanout_respects_concurrency_and_orders_results_by_idx() {
    let env = new_env();
    let server = MockServer::start().await;
    for i in 0..10 {
        Mock::given(method("GET"))
            .and(path(format!("/item/{i}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": i}))
                    .set_delay(Duration::from_millis(100)),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/post"))
        .and(body_json(json!({"x": 3})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let item_url = format!("{}/item/", server.uri()) + "{{ taskrun.value }}";
    save_flow(
        &env,
        "fan",
        json!({
            "name": "fan",
            "inputs": [{"id": "ids", "type": "ARRAY", "required": true}],
            "tasks": [
                {"id": "fan", "type": "parallel",
                 "items": "{{ inputs.ids }}", "concurrency": 3,
                 "tasks": [
                     {"id": "fetch", "type": "http.request",
                      "config": {"method": "GET", "url": item_url}}
                 ],
                 "outputs": [{"name": "results", "type": "ARRAY", "extract": "result.items"}]},
                {"id": "after", "type": "http.request",
                 "config": {"method": "POST", "url": format!("{}/post", server.uri()),
                            "body": [{"key": "x", "value": "{{ outputs.fan.results[3].body.id }}"}]}}
            ]
        }),
    );

    let run_id = create_run(&env, "fan", json!({"ids": [0,1,2,3,4,5,6,7,8,9]})).unwrap();
    let started = Instant::now();
    let run = start_and_wait(&env, run_id).await;
    let elapsed = started.elapsed();
    assert_eq!(run.status, "success");

    // 10 items × 100ms at concurrency 3 need at least 4 batches (≥400ms);
    // unbounded concurrency would finish in ~100ms.
    assert!(
        elapsed >= Duration::from_millis(350),
        "fan-out finished too fast for concurrency 3: {elapsed:?}"
    );

    let tasks = env.db.list_task_runs(run_id).unwrap();
    let fan = task_run(&tasks, "fan");
    assert_eq!(fan.status, "success");
    let result = parse(&fan.result);
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 10);
    for (i, item) in items.iter().enumerate() {
        assert_eq!(item["body"]["id"], json!(i), "items out of idx order");
    }

    let agg = env.db.item_aggregates(fan.id).unwrap();
    assert_eq!(agg.total, 10);
    assert_eq!(agg.success, 10);
    assert_eq!(agg.failed + agg.dropped + agg.queued + agg.running, 0);
}

// ---------------------------------------------------------------------------
// 6. Fan-out: child on_error=continue drops the item
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fanout_child_on_error_continue_drops_item() {
    let env = new_env();
    let server = MockServer::start().await;
    for name in ["a", "b", "d", "e"] {
        Mock::given(method("GET"))
            .and(path(format!("/item/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"v": name})))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/item/c"))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;

    let item_url = format!("{}/item/", server.uri()) + "{{ taskrun.value }}";
    save_flow(
        &env,
        "dropfan",
        json!({
            "name": "dropfan",
            "inputs": [{"id": "ids", "type": "ARRAY", "required": true}],
            "tasks": [
                {"id": "fan", "type": "parallel",
                 "items": "{{ inputs.ids }}", "concurrency": 2,
                 "tasks": [
                     {"id": "fetch", "type": "http.request", "on_error": "continue",
                      "config": {"method": "GET", "url": item_url}}
                 ]}
            ]
        }),
    );

    let run_id = create_run(&env, "dropfan", json!({"ids": ["a","b","c","d","e"]})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success");

    let tasks = env.db.list_task_runs(run_id).unwrap();
    let fan = task_run(&tasks, "fan");
    assert_eq!(fan.status, "success");

    let result = parse(&fan.result);
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    assert_eq!(items[2], Value::Null, "dropped item must be null");
    assert_eq!(items[0]["body"], json!({"v": "a"}));
    assert_eq!(items[4]["body"], json!({"v": "e"}));

    let agg = env.db.item_aggregates(fan.id).unwrap();
    assert_eq!(agg.total, 5);
    assert_eq!(agg.success, 4);
    assert_eq!(agg.dropped, 1);
    assert_eq!(agg.failed, 0);

    let logs = env.db.list_logs(run_id, 0, 1000).unwrap();
    assert!(
        logs.iter()
            .any(|l| l.level == "WARN" && l.message.contains("dropped")),
        "expected WARN log for the dropped item"
    );
}

// ---------------------------------------------------------------------------
// 7. Fan-out: child on_error=fail fails the whole parallel task
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fanout_child_on_error_fail_fails_task_and_cancels_outstanding_items() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/item/a"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    for name in ["b", "c"] {
        Mock::given(method("GET"))
            .and(path(format!("/item/{name}")))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
    }

    let item_url = format!("{}/item/", server.uri()) + "{{ taskrun.value }}";
    save_flow(
        &env,
        "failfan",
        json!({
            "name": "failfan",
            "inputs": [{"id": "ids", "type": "ARRAY", "required": true}],
            "tasks": [
                {"id": "fan", "type": "parallel",
                 "items": "{{ inputs.ids }}", "concurrency": 1,
                 "tasks": [
                     {"id": "fetch", "type": "http.request", "on_error": "fail",
                      "config": {"method": "GET", "url": item_url}}
                 ]}
            ]
        }),
    );

    let run_id = create_run(&env, "failfan", json!({"ids": ["a", "b", "c"]})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "failed");
    assert!(
        run.error.as_deref().unwrap().contains("item 0 failed"),
        "error: {:?}",
        run.error
    );

    let tasks = env.db.list_task_runs(run_id).unwrap();
    let fan = task_run(&tasks, "fan");
    assert_eq!(fan.status, "failed");

    let (items, _) = env.db.list_items(fan.id, None, 1, 100).unwrap();
    let statuses: Vec<&str> = items.iter().map(|i| i.status.as_str()).collect();
    assert_eq!(statuses, vec!["failed", "canceled", "canceled"]);
}

// ---------------------------------------------------------------------------
// 8. Fan-out: child chain sees prior child outputs and taskrun.value
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fanout_child_chain_references_prior_child_and_item_value() {
    let env = new_env();
    let server = MockServer::start().await;
    for name in ["x", "y"] {
        Mock::given(method("GET"))
            .and(path(format!("/first/{name}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"tag": format!("t-{name}")})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/second"))
            .and(body_json(
                json!({"prev": format!("t-{name}"), "item": name}),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }

    let first_url = format!("{}/first/", server.uri()) + "{{ taskrun.value }}";
    save_flow(
        &env,
        "chainfan",
        json!({
            "name": "chainfan",
            "inputs": [{"id": "ids", "type": "ARRAY", "required": true}],
            "tasks": [
                {"id": "fan", "type": "parallel",
                 "items": "{{ inputs.ids }}", "concurrency": 2,
                 "tasks": [
                     {"id": "c1", "type": "http.request",
                      "config": {"method": "GET", "url": first_url},
                      "outputs": [{"name": "body", "type": "JSON", "extract": "result.body"}]},
                     {"id": "c2", "type": "http.request",
                      "config": {"method": "POST", "url": format!("{}/second", server.uri()),
                                 "body": [
                                     {"key": "prev", "value": "{{ outputs.c1.body.tag }}"},
                                     {"key": "item", "value": "{{ taskrun.value }}"}
                                 ]}}
                 ]}
            ]
        }),
    );

    let run_id = create_run(&env, "chainfan", json!({"ids": ["x", "y"]})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success");
}

// ---------------------------------------------------------------------------
// 9. Cancellation mid-fan-out
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_mid_fanout_stops_promptly_without_starting_queued_items() {
    let env = new_env();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    save_flow(
        &env,
        "cancelfan",
        json!({
            "name": "cancelfan",
            "inputs": [{"id": "ids", "type": "ARRAY", "required": true}],
            "tasks": [
                {"id": "fan", "type": "parallel",
                 "items": "{{ inputs.ids }}", "concurrency": 2,
                 "tasks": [
                     {"id": "fetch", "type": "http.request",
                      "config": {"method": "GET",
                                 "url": format!("{}/slow", server.uri())}}
                 ]}
            ]
        }),
    );

    let run_id = create_run(&env, "cancelfan", json!({"ids": [1,2,3,4,5,6]})).unwrap();
    env.engine.start(run_id).unwrap();

    // Wait until the first requests are in flight.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !server.received_requests().await.unwrap().is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "no request ever started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let canceled_at = Instant::now();
    assert!(env.engine.cancel(run_id), "run should be active");
    let run = wait_for_finish(&env, run_id).await;
    assert!(
        canceled_at.elapsed() < Duration::from_secs(2),
        "cancellation not prompt: {:?}",
        canceled_at.elapsed()
    );
    assert_eq!(run.status, "canceled");

    // Only the in-flight items (≤ concurrency) ever reached the server.
    assert!(
        server.received_requests().await.unwrap().len() <= 2,
        "queued items must never start"
    );

    let tasks = env.db.list_task_runs(run_id).unwrap();
    let fan = task_run(&tasks, "fan");
    assert_eq!(fan.status, "canceled");
    let (items, _) = env.db.list_items(fan.id, None, 1, 100).unwrap();
    assert_eq!(items.len(), 6);
    assert!(
        items.iter().all(|i| i.status == "canceled"),
        "all items canceled, got: {:?}",
        items.iter().map(|i| i.status.clone()).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 10. Secret redaction end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn secrets_reach_the_wire_but_never_the_database() {
    const SECRET: &str = "s3cr3t-tok";
    let env = new_env();
    env.secrets.set("API_TOKEN", SECRET).unwrap();

    let server = MockServer::start().await;
    // The mock only matches when the REAL secret arrives in URL and header.
    Mock::given(method("GET"))
        .and(path(format!("/leak/{SECRET}")))
        .and(header("authorization", format!("Bearer {SECRET}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": SECRET})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/boom"))
        .respond_with(ResponseTemplate::new(500).set_body_string(format!("denied for {SECRET}")))
        .expect(1)
        .mount(&server)
        .await;

    let leak_url = format!("{}/leak/", server.uri()) + "{{ secrets.API_TOKEN }}";
    save_flow(
        &env,
        "sec",
        json!({
            "name": "sec",
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "GET", "url": leak_url,
                            "headers": [{"key": "Authorization",
                                         "value": "Bearer {{ secrets.API_TOKEN }}"}]}},
                {"id": "t2", "type": "http.request",
                 "config": {"method": "GET", "url": format!("{}/boom", server.uri())}}
            ]
        }),
    );

    let run_id = create_run(&env, "sec", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "failed"); // t2's 500 fails the run

    // The run error is redacted (the 500 body contained the secret).
    let run_error = run.error.unwrap();
    assert!(!run_error.contains(SECRET), "run error leaks: {run_error}");
    assert!(
        run_error.contains("***"),
        "run error not redacted: {run_error}"
    );

    // Grep every persisted surface for the secret: zero occurrences.
    let tasks = env.db.list_task_runs(run_id).unwrap();
    for t in &tasks {
        for text in [&t.result, &t.outputs, &t.error].into_iter().flatten() {
            assert!(
                !text.contains(SECRET),
                "task_run {} column leaks secret: {text}",
                t.task_id
            );
        }
        let (items, _) = env.db.list_items(t.id, None, 1, 1000).unwrap();
        for item in items {
            for text in [Some(&item.item), item.result.as_ref(), item.error.as_ref()]
                .into_iter()
                .flatten()
            {
                assert!(!text.contains(SECRET), "item leaks secret: {text}");
            }
        }
    }
    let logs = env.db.list_logs(run_id, 0, 10_000).unwrap();
    assert!(!logs.is_empty());
    for log in &logs {
        assert!(
            !log.message.contains(SECRET),
            "log leaks secret: {}",
            log.message
        );
    }
    // The GET line for the leak URL must show the mask.
    assert!(
        logs.iter().any(|l| l.message.contains("/leak/***")),
        "expected a redacted request log"
    );
    // The response body echoing the secret is stored masked.
    let t1_result = parse(&task_run(&tasks, "t1").result);
    assert_eq!(t1_result["body"]["token"], json!("***"));
}

// ---------------------------------------------------------------------------
// 10b. Env vars: resolve from the process environment; config, not secrets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn env_values_resolve_and_are_not_redacted() {
    // `env.*` is deployment config: it reaches the wire AND appears verbatim in
    // logs — unlike `secrets.*`, which are masked. Unique var name so the
    // shared, process-global environment can't collide with a parallel test.
    let var = "ORCH_TEST_ENV_REGION_A1";
    const VALUE: &str = "us-west-cfg-9271";
    // SAFETY: unique name, set/removed within this test; no other test reads it.
    unsafe { std::env::set_var(var, VALUE) };
    let env = new_env();

    let server = MockServer::start().await;
    // Matches only if the REAL env value arrives on the wire.
    Mock::given(method("GET"))
        .and(path(format!("/region/{VALUE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/region/{{{{ env.{var} }}}}", server.uri());
    save_flow(
        &env,
        "envcfg",
        json!({
            "name": "envcfg",
            "env": [var],
            "tasks": [
                {"id": "t1", "type": "http.request", "config": {"method": "GET", "url": url}}
            ]
        }),
    );

    let run_id = create_run(&env, "envcfg", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success", "run error: {:?}", run.error);

    // NOT redacted: the request log carries the real value, never a mask.
    let logs = env.db.list_logs(run_id, 0, 10_000).unwrap();
    assert!(
        logs.iter().any(|l| l.message.contains(VALUE)),
        "expected the un-redacted env value in a log, got: {:?}",
        logs.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
    assert!(
        !logs.iter().any(|l| l.message.contains("/region/***")),
        "env value must not be masked like a secret"
    );

    // SAFETY: unique name owned by this test.
    unsafe { std::env::remove_var(var) };
}

#[tokio::test]
async fn declared_env_var_unset_fails_the_run_before_start() {
    let var = "ORCH_TEST_ENV_UNSET_XYZZY";
    // SAFETY: unique name; ensure it is absent regardless of ambient env.
    unsafe { std::env::remove_var(var) };
    let env = new_env();
    save_flow(
        &env,
        "envmissing",
        json!({
            "name": "envmissing",
            "env": [var],
            // Would refuse to connect if ever reached — but it never is.
            "tasks": [{"id": "t1", "type": "http.request", "config": {"url": "http://127.0.0.1:1/never"}}]
        }),
    );

    let run_id = create_run(&env, "envmissing", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "failed");
    let err = run.error.unwrap_or_default();
    assert!(
        err.contains(var) && err.contains("not set"),
        "expected a clear 'declared but not set' error, got: {err}"
    );
    // Failed before start: no task ever ran.
    assert!(
        env.db.list_task_runs(run_id).unwrap().is_empty(),
        "no task should run when a declared env var is unset"
    );
}

// ---------------------------------------------------------------------------
// 11. Input resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_run_validates_and_defaults_inputs() {
    let env = new_env();
    save_flow(
        &env,
        "inp",
        json!({
            "name": "inp",
            "inputs": [
                {"id": "a", "type": "INT", "required": true},
                {"id": "b", "type": "STRING", "default": "{{ vars.x }}"},
                {"id": "c", "type": "ARRAY", "default": "[\"ON\"]"}
            ],
            "variables": [{"id": "x", "value": "hello"}],
            "tasks": []
        }),
    );

    // Typed parsing: INT given a string is rejected, not coerced.
    let err = create_run(&env, "inp", json!({"a": "5"})).unwrap_err();
    let EngineError::InvalidInput(errors) = err else {
        panic!("expected InvalidInput, got {err:?}")
    };
    assert!(
        errors
            .iter()
            .any(|e| e.contains("\"a\"") && e.contains("INT"))
    );

    // Required input missing.
    let err = create_run(&env, "inp", json!({})).unwrap_err();
    let EngineError::InvalidInput(errors) = err else {
        panic!("expected InvalidInput, got {err:?}")
    };
    assert!(
        errors
            .iter()
            .any(|e| e.contains("\"a\"") && e.contains("required"))
    );

    // Unknown key rejected.
    let err = create_run(&env, "inp", json!({"a": 5, "zz": 1})).unwrap_err();
    let EngineError::InvalidInput(errors) = err else {
        panic!("expected InvalidInput, got {err:?}")
    };
    assert!(errors.iter().any(|e| e.contains("unknown input \"zz\"")));

    // Valid: defaults rendered ({{ vars.x }}) and ARRAY default JSON-parsed.
    let run_id = create_run(&env, "inp", json!({"a": 5})).unwrap();
    let run = env.db.get_run(run_id).unwrap().unwrap();
    assert_eq!(run.status, "queued");
    let inputs: Value = serde_json::from_str(&run.inputs).unwrap();
    assert_eq!(inputs, json!({"a": 5, "b": "hello", "c": ["ON"]}));

    // Unknown flow surfaces distinctly.
    assert!(matches!(
        create_run(&env, "nope", json!({})),
        Err(EngineError::UnknownFlow(_))
    ));
}

// ---------------------------------------------------------------------------
// 11b. Queue routing: non-local runs stay queued for a worker
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_local_queue_run_stays_queued_and_starts_nothing() {
    let env = new_env();
    save_flow(
        &env,
        "gpu_flow",
        json!({
            "name": "gpu-flow",
            "queue": "gpu",
            "tasks": [{
                "id": "noop",
                "type": "http.request",
                "config": {"method": "GET", "url": "http://127.0.0.1:9/"},
                "on_error": "fail",
                "outputs": []
            }]
        }),
    );
    let run_id = create_run(&env, "gpu_flow", json!({})).expect("create run");

    // Routing snapshotted the queue onto the run row.
    let run = env.db.get_run(run_id).unwrap().unwrap();
    assert_eq!(run.queue, "gpu");
    assert_eq!(run.status, "queued");

    // start() is a no-op for a non-local queue: nothing is spawned and the
    // run stays queued for a worker to claim.
    env.engine
        .start(run_id)
        .expect("start is a no-op, not an error");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(env.engine.active_run_count(), 0);
    let run = env.db.get_run(run_id).unwrap().unwrap();
    assert_eq!(run.status, "queued");
}

// ---------------------------------------------------------------------------
// 12. Interrupted-run recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recover_interrupted_marks_running_rows_failed() {
    let env = new_env();
    save_flow(&env, "f", json!({"name": "f", "tasks": []}));
    let run_id = env
        .db
        .insert_run("f", 1, "manual", "{}", "local", None)
        .unwrap();
    env.db
        .update_run_status(
            run_id,
            RunStatusUpdate {
                status: "running",
                error: None,
                started_at: Some("2026-07-05T00:00:00Z"),
                finished_at: None,
            },
        )
        .unwrap();
    env.db.upsert_task_run(run_id, "t1", "running", 1).unwrap();

    let changed = env.engine.recover_interrupted().unwrap();
    assert!(changed >= 2, "expected ≥2 rows changed, got {changed}");

    let run = env.db.get_run(run_id).unwrap().unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.error.as_deref(), Some("interrupted by shutdown"));
    let tasks = env.db.list_task_runs(run_id).unwrap();
    assert_eq!(task_run(&tasks, "t1").status, "failed");
}

// ---------------------------------------------------------------------------
// 13. Event stream: subscribe ordering + redacted log events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_streams_ordered_events_with_redacted_logs() {
    const SECRET: &str = "supersecret";
    let env = new_env();
    env.secrets.set("TOK", SECRET).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/q/{SECRET}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/q/", server.uri()) + "{{ secrets.TOK }}";
    save_flow(
        &env,
        "events",
        json!({
            "name": "events",
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "GET", "url": url}}
            ]
        }),
    );

    let run_id = create_run(&env, "events", json!({})).unwrap();
    env.engine.start(run_id).unwrap();
    // Single-threaded test runtime: the spawned run has not been polled yet,
    // so subscribing here cannot miss events.
    let mut rx = env.engine.subscribe(run_id).expect("run active");

    let events = tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        loop {
            match rx.recv().await {
                Ok(ev) => events.push(ev),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        events
    })
    .await
    .expect("event stream did not close");

    // Ordering: Run(running) first, then task lifecycle, Run(success) last.
    assert!(
        matches!(&events[0], RunEvent::Run { status, .. } if status == "running"),
        "first event: {:?}",
        events.first()
    );
    assert!(
        matches!(events.last().unwrap(), RunEvent::Run { status, .. } if status == "success"),
        "last event: {:?}",
        events.last()
    );
    let pos = |want_status: &str| {
        events.iter().position(
            |e| matches!(e, RunEvent::Task { task_id, status, .. } if task_id == "t1" && status == want_status),
        )
    };
    let running = pos("running").expect("task running event");
    let success = pos("success").expect("task success event");
    assert!(running < success);

    // Log events carry redacted text.
    let log_messages: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Log { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert!(!log_messages.is_empty());
    assert!(log_messages.iter().all(|m| !m.contains(SECRET)));
    assert!(
        log_messages.iter().any(|m| m.contains("/q/***")),
        "expected redacted URL in log events: {log_messages:?}"
    );

    // Wait for the engine to drop the finished run from the active set (the
    // channel closes just before removal).
    let run = wait_for_finish(&env, run_id).await;
    assert_eq!(run.status, "success");

    // Serialization shape for D2's SSE bridge.
    let run_ev = events.first().unwrap();
    assert_eq!(run_ev.event_name(), "run");
    assert_eq!(
        serde_json::to_value(run_ev).unwrap(),
        json!({"status": "running"})
    );
}

// ---------------------------------------------------------------------------
// 14. Variables are literals — never template-rendered
// ---------------------------------------------------------------------------

#[tokio::test]
async fn variables_pass_through_verbatim_even_when_they_look_like_templates() {
    let env = new_env();
    let server = MockServer::start().await;
    // The server must receive the variable's raw text, braces and all.
    Mock::given(method("POST"))
        .and(path("/echo"))
        .and(body_json(json!({"u": "{{ inputs.x }}"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    save_flow(
        &env,
        "vars",
        json!({
            "name": "vars",
            "inputs": [{"id": "x", "type": "STRING", "default": "real-value"}],
            "variables": [{"id": "u", "value": "{{ inputs.x }}"}],
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "POST", "url": format!("{}/echo", server.uri()),
                            "body": [{"key": "u", "value": "{{ vars.u }}"}]}}
            ]
        }),
    );

    let run_id = create_run(&env, "vars", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success");
}

// ---------------------------------------------------------------------------
// 15. Secret late-binding: templates stay templates in runs.inputs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn secret_referencing_inputs_stay_templates_until_execution() {
    const SECRET: &str = "tok-xyz-late";
    let env = new_env();
    env.secrets.set("API_TOKEN", SECRET).unwrap();

    let server = MockServer::start().await;
    // Both runs must deliver the REAL secret on the wire.
    Mock::given(method("GET"))
        .and(path(format!("/q/{SECRET}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(2)
        .mount(&server)
        .await;

    let url = format!("{}/q/", server.uri()) + "{{ inputs.tok }}";
    save_flow(
        &env,
        "late",
        json!({
            "name": "late",
            "inputs": [{"id": "tok", "type": "STRING",
                        "default": "{{ secrets.API_TOKEN }}"}],
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "GET", "url": url}}
            ]
        }),
    );

    // Default path: the stored input is the raw template, never the secret.
    let run_id = create_run(&env, "late", json!({})).unwrap();
    let stored = env.db.get_run(run_id).unwrap().unwrap().inputs;
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap(),
        json!({"tok": "{{ secrets.API_TOKEN }}"})
    );
    assert!(
        !stored.contains(SECRET),
        "runs.inputs leaks secret: {stored}"
    );
    let run = wait_for_finish(&env, {
        env.engine.start(run_id).unwrap();
        run_id
    })
    .await;
    assert_eq!(run.status, "success");

    // Replay path: creating a new run from the first run's stored inputs
    // (a PROVIDED secret-referencing template) also stays raw and works.
    let replay_inputs: Value = serde_json::from_str(&run.inputs).unwrap();
    let replay_id = create_run(&env, "late", replay_inputs).unwrap();
    let replay_stored = env.db.get_run(replay_id).unwrap().unwrap().inputs;
    assert_eq!(
        serde_json::from_str::<Value>(&replay_stored).unwrap(),
        json!({"tok": "{{ secrets.API_TOKEN }}"})
    );
    let replay = start_and_wait(&env, replay_id).await;
    assert_eq!(replay.status, "success");
}

// ---------------------------------------------------------------------------
// 16. Scheduler-style run: `{}` inputs, defaults applied at start
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scheduled_run_with_empty_inputs_applies_defaults_at_start() {
    const SECRET: &str = "sched-s3cret";
    let env = new_env();
    env.secrets.set("API_TOKEN", SECRET).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/collect"))
        .and(body_json(json!({"t": SECRET, "p": "hi", "a": [1, 2]})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    save_flow(
        &env,
        "sched",
        json!({
            "name": "sched",
            "inputs": [
                {"id": "tok", "type": "STRING", "default": "{{ secrets.API_TOKEN }}"},
                {"id": "plain", "type": "STRING", "default": "hi"},
                {"id": "arr", "type": "ARRAY", "default": "[1,2]"}
            ],
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "POST", "url": format!("{}/collect", server.uri()),
                            "body": [
                                {"key": "t", "value": "{{ inputs.tok }}"},
                                {"key": "p", "value": "{{ inputs.plain }}"},
                                {"key": "a", "value": "{{ inputs.arr }}"}
                            ]}}
            ]
        }),
    );

    // The scheduler inserts runs directly with `{}` inputs.
    let run_id = env
        .db
        .insert_run("sched", 1, "schedule", "{}", "local", None)
        .unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success");

    // Finalized values are execution-only: the row keeps its `{}`.
    assert_eq!(run.inputs, "{}");
}

// ---------------------------------------------------------------------------
// 17. Required input missing at start fails the run (not stuck queued)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn required_input_missing_at_start_fails_run() {
    let env = new_env();
    save_flow(
        &env,
        "reqd",
        json!({
            "name": "reqd",
            "inputs": [{"id": "req", "type": "STRING", "required": true}],
            "tasks": [
                {"id": "t1", "type": "http.request",
                 "config": {"method": "GET", "url": "http://127.0.0.1:1/never"}}
            ]
        }),
    );

    let run_id = env
        .db
        .insert_run("reqd", 1, "schedule", "{}", "local", None)
        .unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "failed");
    assert!(run.finished_at.is_some());
    let error = run.error.unwrap();
    assert!(
        error.contains("\"req\"") && error.contains("required"),
        "error should name the input: {error}"
    );
    // The task never ran.
    assert!(env.db.list_task_runs(run_id).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// 18. Unknown plugin type still records a failed task_run row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_plugin_type_records_failed_task_run() {
    let env = new_env();
    save_flow(
        &env,
        "nop",
        json!({
            "name": "nop",
            "tasks": [{"id": "t1", "type": "no.such.plugin", "config": {}}]
        }),
    );

    let run_id = create_run(&env, "nop", json!({})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "failed");

    let tasks = env.db.list_task_runs(run_id).unwrap();
    let t1 = task_run(&tasks, "t1");
    assert_eq!(t1.status, "failed");
    assert!(t1.finished_at.is_some());
    assert!(
        t1.error.as_deref().unwrap().contains("unknown task type"),
        "error: {:?}",
        t1.error
    );
}

// ---------------------------------------------------------------------------
// 19. Empty parallel items
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_parallel_items_succeeds_with_empty_result() {
    let env = new_env();
    save_flow(
        &env,
        "emptyfan",
        json!({
            "name": "emptyfan",
            "inputs": [{"id": "ids", "type": "ARRAY", "required": true}],
            "tasks": [
                {"id": "fan", "type": "parallel",
                 "items": "{{ inputs.ids }}", "concurrency": 4,
                 "tasks": [
                     {"id": "fetch", "type": "http.request",
                      "config": {"method": "GET", "url": "http://127.0.0.1:1/never"}}
                 ],
                 "outputs": [{"name": "results", "type": "ARRAY", "extract": "result.items"}]}
            ]
        }),
    );

    let run_id = create_run(&env, "emptyfan", json!({"ids": []})).unwrap();
    let run = start_and_wait(&env, run_id).await;
    assert_eq!(run.status, "success");

    let tasks = env.db.list_task_runs(run_id).unwrap();
    let fan = task_run(&tasks, "fan");
    assert_eq!(fan.status, "success");
    assert_eq!(parse(&fan.result), json!({"items": []}));
    assert_eq!(parse(&fan.outputs), json!({"results": []}));
    assert_eq!(env.db.item_aggregates(fan.id).unwrap().total, 0);
}

// ---------------------------------------------------------------------------
// 20. Panic safety: a panicking plugin never leaks an active-map entry
// ---------------------------------------------------------------------------

/// A run whose task fails must still clean up: close the broadcast channel and
/// drop its active entry. (Previously exercised with an in-process panicking
/// plugin; with subprocess plugins an abnormal failure surfaces as a fatal task
/// error, which must trigger the same cleanup.)
#[tokio::test]
async fn failing_task_cleans_up_active_entry() {
    let env = new_env_full(None, None);
    save_flow(
        &env,
        "boom",
        json!({
            "name": "boom",
            // Connection refused → the http plugin errors; one attempt, then fail.
            "tasks": [{"id": "t1", "type": "http.request", "config": {"url": "http://127.0.0.1:1/"}}]
        }),
    );

    let run_id = create_run(&env, "boom", json!({})).unwrap();
    env.engine.start(run_id).unwrap();
    let mut rx = env.engine.subscribe(run_id).expect("run active");

    // The drop-guard must close the broadcast channel when the run ends.
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(rx.recv().await, Err(broadcast::error::RecvError::Closed)) {}
    })
    .await
    .expect("broadcast channel never closed after failure");

    // ... and remove the run from the active set.
    assert_eq!(env.engine.active_run_count(), 0);
    assert!(env.engine.subscribe(run_id).is_none());
}
