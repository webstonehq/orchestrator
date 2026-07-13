//! Integration tests for the SQLite DB layer.

use orchestrator::db;

use db::{Db, ItemUpdate, RunStatusUpdate, TaskRunFinish};
use serde_json::json;

fn open_temp() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path().join("test.db")).expect("open db");
    (dir, db)
}

#[test]
fn pragmas_are_applied_on_open() {
    let (_dir, db) = open_temp();
    let conn = db.conn().expect("conn");
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal_mode");
    // synchronous: 0=OFF, 1=NORMAL, 2=FULL, 3=EXTRA
    let synchronous: i64 = conn
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .expect("synchronous");
    assert_eq!(journal.to_lowercase(), "wal");
    assert_eq!(synchronous, 1, "synchronous should be NORMAL under WAL");
}

fn ago_hours(h: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::hours(h))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn ago_hours_plus_secs(h: i64, s: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::hours(h) + chrono::Duration::seconds(s))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn seed_flow(db: &Db, id: &str) {
    db.upsert_flow_with_revision(id, id, "default", "", "{\"tasks\":[]}", "initial")
        .expect("seed flow");
}

/// Status-only run update (no error, timestamps preserved).
fn set_run_status(db: &Db, id: i64, status: &str) {
    db.update_run_status(
        id,
        RunStatusUpdate {
            status,
            ..Default::default()
        },
    )
    .expect("update run status");
}

/// Finish a run with explicit timestamps; `error` set only for failures.
fn finish_run(db: &Db, id: i64, status: &str, started_at: &str, finished_at: &str) {
    db.update_run_status(
        id,
        RunStatusUpdate {
            status,
            error: (status == "failed").then_some("boom"),
            started_at: Some(started_at),
            finished_at: Some(finished_at),
        },
    )
    .expect("finish run");
}

#[test]
fn migration_is_idempotent_across_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orch.db");

    let db1 = Db::open(&path).expect("first open");
    seed_flow(&db1, "flow-a");
    drop(db1);

    // Second open on the same file must not re-run migration 001.
    let db2 = Db::open(&path).expect("second open");
    let flow = db2
        .get_flow("flow-a")
        .unwrap()
        .expect("flow survives reopen");
    assert_eq!(flow.id, "flow-a");
    assert_eq!(flow.current_rev, 1);

    // And a third handle while another is still live.
    let db3 = Db::open(&path).expect("third open while db2 alive");
    assert_eq!(db3.list_flows().unwrap().len(), 1);
}

#[test]
fn upsert_flow_bumps_rev_adds_revision_and_preserves_created_at() {
    let (_dir, db) = open_temp();

    let rev1 = db
        .upsert_flow_with_revision("f1", "Flow One", "default", "first", "{\"v\":1}", "create")
        .unwrap();
    assert_eq!(rev1, 1);
    let created = db.get_flow("f1").unwrap().unwrap();
    assert_eq!(created.current_rev, 1);
    assert_eq!(created.name, "Flow One");
    assert!(!created.paused);

    std::thread::sleep(std::time::Duration::from_millis(10));

    let rev2 = db
        .upsert_flow_with_revision("f1", "Flow One v2", "ns2", "second", "{\"v\":2}", "update")
        .unwrap();
    assert_eq!(rev2, 2);

    let updated = db.get_flow("f1").unwrap().unwrap();
    assert_eq!(updated.current_rev, 2);
    assert_eq!(updated.name, "Flow One v2");
    assert_eq!(updated.namespace, "ns2");
    assert_eq!(updated.definition, "{\"v\":2}");
    assert_eq!(
        updated.created_at, created.created_at,
        "created_at preserved"
    );
    assert_ne!(updated.updated_at, created.updated_at, "updated_at bumped");

    let revs = db.list_revisions("f1").unwrap();
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0].rev, 2, "newest first");
    assert_eq!(revs[0].message, "update");
    assert_eq!(revs[1].rev, 1);

    let old = db.get_revision("f1", 1).unwrap().unwrap();
    assert_eq!(old.definition, "{\"v\":1}");
    assert!(db.get_revision("f1", 99).unwrap().is_none());
}

#[test]
fn flow_pause_list_and_delete_cascade() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "a-flow");
    seed_flow(&db, "b-flow");

    db.set_paused("a-flow", true).unwrap();
    assert!(db.get_flow("a-flow").unwrap().unwrap().paused);
    db.set_paused("a-flow", false).unwrap();
    assert!(!db.get_flow("a-flow").unwrap().unwrap().paused);

    assert_eq!(db.list_flows().unwrap().len(), 2);

    db.reconcile_schedules("a-flow", &[("nightly", Some("2026-07-06T03:00:00Z"))])
        .unwrap();
    assert!(db.delete_flow("a-flow").unwrap());
    assert!(!db.delete_flow("a-flow").unwrap(), "already gone");
    assert!(db.get_flow("a-flow").unwrap().is_none());
    assert!(
        db.list_revisions("a-flow").unwrap().is_empty(),
        "revisions cascade"
    );
    assert!(
        db.list_schedules().unwrap().is_empty(),
        "schedule rows removed"
    );
}

#[test]
fn insert_and_get_run_roundtrip() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");

    let id = db
        .insert_run(
            "f1",
            1,
            "schedule",
            "{\"x\":1}",
            "local",
            Some("2026-07-05T03:00:00Z"),
        )
        .unwrap();
    let run = db.get_run(id).unwrap().expect("run exists");
    assert_eq!(run.flow_id, "f1");
    assert_eq!(run.flow_rev, 1);
    assert_eq!(run.status, "queued");
    assert_eq!(run.trigger, "schedule");
    assert_eq!(run.inputs, "{\"x\":1}");
    assert_eq!(run.queue, "local");
    assert_eq!(run.scheduled_for.as_deref(), Some("2026-07-05T03:00:00Z"));
    assert!(run.started_at.is_none());
    assert!(run.error.is_none());
    assert!(db.get_run(9999).unwrap().is_none());
}

#[test]
fn update_run_status_sets_fields_and_preserves_timestamps() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let id = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();

    db.update_run_status(
        id,
        RunStatusUpdate {
            status: "running",
            started_at: Some("2026-07-05T10:00:00Z"),
            ..Default::default()
        },
    )
    .unwrap();
    let run = db.get_run(id).unwrap().unwrap();
    assert_eq!(run.status, "running");
    assert_eq!(run.started_at.as_deref(), Some("2026-07-05T10:00:00Z"));

    // started_at passed as None must not clear the stored value.
    db.update_run_status(
        id,
        RunStatusUpdate {
            status: "failed",
            error: Some("boom"),
            started_at: None,
            finished_at: Some("2026-07-05T10:01:00Z"),
        },
    )
    .unwrap();
    let run = db.get_run(id).unwrap().unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.error.as_deref(), Some("boom"));
    assert_eq!(run.started_at.as_deref(), Some("2026-07-05T10:00:00Z"));
    assert_eq!(run.finished_at.as_deref(), Some("2026-07-05T10:01:00Z"));
}

#[test]
fn list_runs_filters_paginates_and_counts() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    seed_flow(&db, "f2");

    let r1 = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let r2 = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let r3 = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let r4 = db.insert_run("f2", 1, "api", "{}", "local", None).unwrap();
    let r5 = db
        .insert_run("f2", 1, "schedule", "{}", "local", None)
        .unwrap();
    set_run_status(&db, r1, "success");
    set_run_status(&db, r2, "success");
    db.update_run_status(
        r3,
        RunStatusUpdate {
            status: "failed",
            error: Some("x"),
            ..Default::default()
        },
    )
    .unwrap();
    set_run_status(&db, r4, "running");
    // r5 stays queued.

    let (rows, total) = db.list_runs(None, None, None, None, None, 1, 2).unwrap();
    assert_eq!(total, 5);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, r5, "newest first");
    assert_eq!(rows[1].id, r4);

    let (rows, total) = db.list_runs(None, None, None, None, None, 3, 2).unwrap();
    assert_eq!(total, 5);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, r1);

    let (rows, total) = db.list_runs(Some("f1"), None, None, None, None, 1, 10).unwrap();
    assert_eq!(total, 3);
    assert_eq!(rows.len(), 3);

    let (rows, total) = db.list_runs(None, Some("success"), None, None, None, 1, 10).unwrap();
    assert_eq!(total, 2);
    assert_eq!(rows.len(), 2);

    let (rows, total) = db.list_runs(Some("f1"), Some("failed"), None, None, None, 1, 10).unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].id, r3);

    let (rows, total) = db.list_runs(Some("f1"), Some("queued"), None, None, None, 1, 10).unwrap();
    assert_eq!(total, 0);
    assert!(rows.is_empty());

    // Trigger filter: r1/r2/r3 manual, r4 api, r5 schedule.
    let (rows, total) = db.list_runs(None, None, Some("manual"), None, None, 1, 10).unwrap();
    assert_eq!(total, 3);
    assert_eq!(rows.len(), 3);
    let (_rows, total) = db.list_runs(None, None, Some("schedule"), None, None, 1, 10).unwrap();
    assert_eq!(total, 1);
    // Trigger composes with flow: f2 has one api run (r4).
    let (rows, total) = db.list_runs(Some("f2"), Some("running"), Some("api"), None, None, 1, 10).unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].id, r4);

    let counts = db.count_runs_by_status().unwrap();
    assert_eq!(counts.get("success"), Some(&2));
    assert_eq!(counts.get("failed"), Some(&1));
    assert_eq!(counts.get("running"), Some(&1));
    assert_eq!(counts.get("queued"), Some(&1));
    assert_eq!(counts.get("canceled"), None);
}

#[test]
fn list_runs_filters_by_started_at_window() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");

    let old = db.insert_run("f1", 1, "manual", "{}", "local", None).unwrap();
    let mid = db.insert_run("f1", 1, "manual", "{}", "local", None).unwrap();
    let recent = db.insert_run("f1", 1, "manual", "{}", "local", None).unwrap();
    let queued = db.insert_run("f1", 1, "manual", "{}", "local", None).unwrap();
    finish_run(&db, old, "success", "2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z");
    finish_run(&db, mid, "success", "2026-06-01T00:00:00Z", "2026-06-01T00:01:00Z");
    finish_run(&db, recent, "success", "2026-07-10T00:00:00Z", "2026-07-10T00:01:00Z");
    // `queued` never started, so it has no started_at.

    // since only (half-open lower bound).
    let (rows, total) = db
        .list_runs(None, None, None, Some("2026-05-01T00:00:00Z"), None, 1, 10)
        .unwrap();
    assert_eq!(total, 2);
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![recent, mid]);

    // since + until bracket a single run and exclude the started-less run.
    let (rows, total) = db
        .list_runs(
            None,
            None,
            None,
            Some("2026-05-01T00:00:00Z"),
            Some("2026-07-01T00:00:00Z"),
            1,
            10,
        )
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].id, mid);

    // Runs without a started_at are excluded by any lower bound.
    let (_rows, total) = db
        .list_runs(None, None, None, Some("2000-01-01T00:00:00Z"), None, 1, 10)
        .unwrap();
    assert_eq!(total, 3, "the queued run has no started_at and is filtered out");
    let _ = queued;
}

#[test]
fn task_run_upsert_finish_and_list() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let run_id = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();

    let tr1 = db
        .upsert_task_run(run_id, "discover", "running", 1)
        .unwrap();
    let first = &db.list_task_runs(run_id).unwrap()[0];
    let started = first.started_at.clone();
    assert!(started.is_some(), "running task gets started_at");
    let created = first.created_at.clone();
    assert!(created.is_some(), "task run gets created_at on first insert");
    assert_eq!(first.attempt, 1);

    // Retry bumps attempt, keeps the same row and original started_at.
    let tr1b = db
        .upsert_task_run(run_id, "discover", "running", 2)
        .unwrap();
    assert_eq!(tr1, tr1b);
    let rows = db.list_task_runs(run_id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempt, 2);
    assert_eq!(rows[0].started_at, started);
    assert_eq!(rows[0].created_at, created, "created_at is never overwritten");

    db.finish_task_run(
        run_id,
        "discover",
        TaskRunFinish {
            status: "success",
            result: Some("{\"body\":1}"),
            outputs: Some("{\"ids\":[1]}"),
            error: None,
        },
    )
    .unwrap();
    let tr2 = db.upsert_task_run(run_id, "fetch", "pending", 0).unwrap();
    assert_ne!(tr1, tr2);

    let rows = db.list_task_runs(run_id).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].task_id, "discover");
    assert_eq!(rows[0].status, "success");
    assert_eq!(rows[0].result.as_deref(), Some("{\"body\":1}"));
    assert_eq!(rows[0].outputs.as_deref(), Some("{\"ids\":[1]}"));
    assert!(rows[0].finished_at.is_some());
    assert_eq!(rows[1].task_id, "fetch");
    assert!(
        rows[1].started_at.is_none(),
        "pending task has no started_at"
    );
    assert!(
        rows[1].created_at.is_some(),
        "pending task still gets created_at ahead of started_at"
    );
}

#[test]
fn item_aggregates_and_compact_statuses() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let run_id = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let tr = db.upsert_task_run(run_id, "fanout", "running", 1).unwrap();

    let items: Vec<serde_json::Value> = (0..6).map(|i| json!({ "n": i })).collect();
    db.insert_items(tr, &items).unwrap();

    let now = db::now_rfc3339();
    db.update_item(
        tr,
        0,
        ItemUpdate {
            status: "success",
            attempt: 1,
            result: Some("\"ok\""),
            started_at: Some(&now),
            finished_at: Some(&now),
            ..Default::default()
        },
    )
    .unwrap();
    db.update_item(
        tr,
        1,
        ItemUpdate {
            status: "failed",
            attempt: 2,
            error: Some("http 500"),
            started_at: Some(&now),
            finished_at: Some(&now),
            ..Default::default()
        },
    )
    .unwrap();
    db.update_item(
        tr,
        2,
        ItemUpdate {
            status: "running",
            attempt: 1,
            started_at: Some(&now),
            ..Default::default()
        },
    )
    .unwrap();
    db.update_item(
        tr,
        3,
        ItemUpdate {
            status: "dropped",
            ..Default::default()
        },
    )
    .unwrap();
    // idx 4 stays queued.
    db.update_item(
        tr,
        5,
        ItemUpdate {
            status: "canceled",
            attempt: 3,
            started_at: Some(&now),
            finished_at: Some(&now),
            ..Default::default()
        },
    )
    .unwrap();

    let agg = db.item_aggregates(tr).unwrap();
    assert_eq!(agg.total, 6);
    assert_eq!(agg.queued, 1);
    assert_eq!(agg.running, 1);
    assert_eq!(agg.success, 1);
    assert_eq!(agg.failed, 1);
    assert_eq!(agg.dropped, 1);
    assert_eq!(agg.retried, 2, "attempt > 1 counts as retried");

    assert_eq!(db.item_statuses_compact(tr).unwrap(), "sfrdqc");

    let (rows, total) = db.list_items(tr, None, 1, 10).unwrap();
    assert_eq!(total, 6);
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].idx, 0);
    assert_eq!(rows[0].item, "{\"n\":0}");
    assert_eq!(rows[1].error.as_deref(), Some("http 500"));

    let (rows, total) = db.list_items(tr, Some("failed"), 1, 10).unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].idx, 1);

    let (rows, total) = db.list_items(tr, None, 2, 2).unwrap();
    assert_eq!(total, 6);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].idx, 2);
    assert_eq!(rows[1].idx, 3);
}

#[test]
fn logs_append_and_page_after_id() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let run_id = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();

    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(
            db.append_log(run_id, "INFO", "flow", &format!("msg {i}"))
                .unwrap(),
        );
    }
    assert!(ids.windows(2).all(|w| w[0] < w[1]));

    let page1 = db.list_logs(run_id, 0, 2).unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].id, ids[0]);
    assert_eq!(page1[0].message, "msg 0");
    assert_eq!(page1[0].level, "INFO");
    assert_eq!(page1[0].task, "flow");

    let page2 = db.list_logs(run_id, page1[1].id, 2).unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].id, ids[2]);

    let page3 = db.list_logs(run_id, page2[1].id, 10).unwrap();
    assert_eq!(page3.len(), 1);
    assert_eq!(page3[0].id, ids[4]);

    // Other runs' logs are not visible.
    let run2 = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    assert!(db.list_logs(run2, 0, 10).unwrap().is_empty());
}

#[test]
fn reconcile_schedules_preserves_state_and_removes_stale() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");

    db.reconcile_schedules(
        "f1",
        &[
            ("nightly", Some("2026-07-06T03:00:00Z")),
            ("hourly", Some("2026-07-05T13:00:00Z")),
        ],
    )
    .unwrap();

    db.set_schedule_enabled("f1", "nightly", false).unwrap();
    db.update_schedule_fired(
        "f1",
        "nightly",
        "2026-07-05T03:00:00Z",
        Some("2026-07-06T03:00:00Z"),
    )
    .unwrap();

    // Redeploy: "nightly" survives (new next_fire ignored), "hourly" removed,
    // "weekly" added.
    db.reconcile_schedules(
        "f1",
        &[
            ("nightly", Some("2099-01-01T00:00:00Z")),
            ("weekly", Some("2026-07-12T03:00:00Z")),
        ],
    )
    .unwrap();

    let scheds = db.list_schedules().unwrap();
    assert_eq!(scheds.len(), 2);

    let nightly = scheds.iter().find(|s| s.trigger_id == "nightly").unwrap();
    assert!(!nightly.enabled, "enabled flag preserved");
    assert_eq!(
        nightly.last_fired_at.as_deref(),
        Some("2026-07-05T03:00:00Z")
    );
    assert_eq!(
        nightly.next_fire_at.as_deref(),
        Some("2026-07-06T03:00:00Z"),
        "next_fire_at not overwritten for survivors"
    );
    assert_eq!(nightly.flow_name, "f1");
    assert!(!nightly.flow_paused);

    let weekly = scheds.iter().find(|s| s.trigger_id == "weekly").unwrap();
    assert!(weekly.enabled);
    assert_eq!(weekly.next_fire_at.as_deref(), Some("2026-07-12T03:00:00Z"));
    assert!(weekly.last_fired_at.is_none());

    assert!(
        !scheds.iter().any(|s| s.trigger_id == "hourly"),
        "stale trigger deleted"
    );

    db.set_schedule_next("f1", "weekly", Some("2026-07-19T03:00:00Z"))
        .unwrap();
    let scheds = db.list_schedules().unwrap();
    let weekly = scheds.iter().find(|s| s.trigger_id == "weekly").unwrap();
    assert_eq!(weekly.next_fire_at.as_deref(), Some("2026-07-19T03:00:00Z"));
}

#[test]
fn fail_lost_run_fails_active_work_but_not_queued() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");

    let r_queued = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let r_running = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let r_done = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let now = db::now_rfc3339();
    db.update_run_status(
        r_running,
        RunStatusUpdate {
            status: "running",
            started_at: Some(&now),
            ..Default::default()
        },
    )
    .unwrap();
    set_run_status(&db, r_done, "success");

    db.upsert_task_run(r_running, "t1", "running", 1).unwrap();
    db.upsert_task_run(r_running, "t2", "pending", 0).unwrap();
    db.upsert_task_run(r_done, "t1", "success", 1).unwrap();

    let tr = db.upsert_task_run(r_running, "fan", "running", 1).unwrap();
    db.insert_items(tr, &[json!(1), json!(2), json!(3)])
        .unwrap();
    db.update_item(
        tr,
        0,
        ItemUpdate {
            status: "running",
            attempt: 1,
            ..Default::default()
        },
    )
    .unwrap();
    db.update_item(
        tr,
        1,
        ItemUpdate {
            status: "success",
            attempt: 1,
            ..Default::default()
        },
    )
    .unwrap();

    // Only the in-flight (running) run is "lost" — a queued run is not, since
    // under the unified model it simply waits to be claimed.
    let inflight: Vec<i64> = db
        .all_in_flight_runs()
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(inflight, vec![r_running]);

    assert!(db.fail_lost_run(r_running, &now).unwrap());

    // Queued and finished runs are untouched.
    assert_eq!(db.get_run(r_queued).unwrap().unwrap().status, "queued");
    assert_eq!(db.get_run(r_done).unwrap().unwrap().status, "success");

    let r = db.get_run(r_running).unwrap().unwrap();
    assert_eq!(r.status, "failed");
    assert_eq!(r.error.as_deref(), Some("worker lost (lease expired)"));
    assert!(r.finished_at.is_some());

    let trs = db.list_task_runs(r_running).unwrap();
    assert!(trs.iter().all(|t| t.status == "failed"));
    assert_eq!(db.list_task_runs(r_done).unwrap()[0].status, "success");

    // items: idx0 running→canceled, idx1 success stays, idx2 queued→canceled.
    assert_eq!(db.item_statuses_compact(tr).unwrap(), "csc");

    // Guarded: a run that is already terminal is not re-failed.
    assert!(!db.fail_lost_run(r_running, &now).unwrap());
    assert!(!db.fail_lost_run(r_done, &now).unwrap());
}

#[test]
fn dashboard_metrics_on_seeded_data() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    seed_flow(&db, "f2");
    db.set_paused("f2", true).unwrap();

    // Three finished runs inside the last 24h: 2 success + 1 failed,
    // durations 30s / 60s / 90s.
    let specs = [("success", 1, 30), ("success", 2, 60), ("failed", 1, 90)];
    for (status, hours_ago, dur) in specs {
        let id = db
            .insert_run("f1", 1, "manual", "{}", "local", None)
            .unwrap();
        finish_run(
            &db,
            id,
            status,
            &ago_hours(hours_ago),
            &ago_hours_plus_secs(hours_ago, dur),
        );
    }

    // Old success run: outside both the 24h and 30d windows.
    let old = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    finish_run(
        &db,
        old,
        "success",
        &ago_hours(40 * 24),
        &ago_hours_plus_secs(40 * 24, 600),
    );

    let m = db.dashboard_metrics().unwrap();
    assert_eq!(m.active_flows, 1, "paused flow excluded");
    assert_eq!(m.runs_24h.total, 3);
    assert_eq!(m.runs_24h.ok, 2);
    assert_eq!(m.runs_24h.failed, 1);
    assert_eq!(m.runs_24h.running, 0);

    let rate = m.success_rate_30d.expect("has finished runs in 30d");
    assert!((rate - 2.0 / 3.0).abs() < 1e-9, "got {rate}");

    let avg = m.avg_duration_sec_30d.expect("has durations");
    assert!((avg - 60.0).abs() < 0.5, "got {avg}");
}

#[test]
fn dashboard_metrics_empty_db() {
    let (_dir, db) = open_temp();
    let m = db.dashboard_metrics().unwrap();
    assert_eq!(m.active_flows, 0);
    assert_eq!(m.runs_24h.total, 0);
    assert!(m.success_rate_30d.is_none());
    assert!(m.avg_duration_sec_30d.is_none());
}

#[test]
fn flow_run_stats_batched_across_flows() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    seed_flow(&db, "f2");
    seed_flow(&db, "f3"); // never run

    // f1: success (60s) then failed (30s), both within 30d; failed is latest.
    let a = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    finish_run(
        &db,
        a,
        "success",
        &ago_hours(2),
        &ago_hours_plus_secs(2, 60),
    );
    let b = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let b_finished = ago_hours_plus_secs(1, 30);
    finish_run(&db, b, "failed", &ago_hours(1), &b_finished);

    // f2: an old success outside the 30d window, then a queued run (latest).
    let c = db
        .insert_run("f2", 1, "manual", "{}", "local", None)
        .unwrap();
    finish_run(
        &db,
        c,
        "success",
        &ago_hours(40 * 24),
        &ago_hours_plus_secs(40 * 24, 600),
    );
    db.insert_run("f2", 1, "manual", "{}", "local", None)
        .unwrap();

    let stats = db.flow_run_stats().unwrap();
    assert_eq!(stats.len(), 2, "flows without runs get no entry");
    assert!(!stats.contains_key("f3"));

    let f1 = stats.get("f1").expect("f1 stats");
    assert_eq!(f1.last_run_status.as_deref(), Some("failed"));
    assert_eq!(f1.last_run_finished_at.as_deref(), Some(&*b_finished));
    let rate = f1.success_rate_30d.expect("f1 has finished runs in 30d");
    assert!((rate - 0.5).abs() < 1e-9, "got {rate}");
    let avg = f1.avg_duration_sec_30d.expect("f1 has durations");
    assert!((avg - 45.0).abs() < 0.5, "got {avg}");

    let f2 = stats.get("f2").expect("f2 stats");
    assert_eq!(f2.last_run_status.as_deref(), Some("queued"));
    assert!(f2.last_run_finished_at.is_none());
    assert!(
        f2.success_rate_30d.is_none(),
        "only run finished outside 30d window"
    );
    assert!(f2.avg_duration_sec_30d.is_none());
}

#[test]
fn now_rfc3339_is_utc_zulu() {
    let now = db::now_rfc3339();
    assert!(now.ends_with('Z'), "got {now}");
    assert!(chrono::DateTime::parse_from_rfc3339(&now).is_ok());
}

// ---------------------------------------------------------------------------
// Worker leasing: claim / renew / reap / seq
// ---------------------------------------------------------------------------

/// A future RFC3339 timestamp, for reap comparisons.
fn in_secs(s: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(s))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[test]
fn claim_leases_matching_queue_runs_and_skips_others() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let gpu = db.insert_run("f1", 1, "manual", "{}", "gpu", None).unwrap();
    let _local = db
        .insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    let cpu = db.insert_run("f1", 1, "manual", "{}", "cpu", None).unwrap();

    let claimed = db.claim_runs("w1", &["gpu", "cpu"], 10, 30).unwrap();
    let ids: Vec<i64> = claimed.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![gpu, cpu]);
    for row in &claimed {
        assert_eq!(row.status, "leased");
    }
    // The gpu run is now leased with a worker + future lease.
    let run = db.get_run(gpu).unwrap().unwrap();
    assert_eq!(run.status, "leased");
    assert!(run.lease_expires_at.is_some());
}

#[test]
fn claim_respects_capacity_and_never_double_claims() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(db.insert_run("f1", 1, "manual", "{}", "gpu", None).unwrap());
    }
    let first = db.claim_runs("w1", &["gpu"], 2, 30).unwrap();
    assert_eq!(first.len(), 2);
    let second = db.claim_runs("w2", &["gpu"], 10, 30).unwrap();
    assert_eq!(second.len(), 3);
    // No id appears in both claims.
    let a: std::collections::HashSet<i64> = first.iter().map(|r| r.id).collect();
    let b: std::collections::HashSet<i64> = second.iter().map(|r| r.id).collect();
    assert!(a.is_disjoint(&b));
    // All five accounted for.
    assert_eq!(a.len() + b.len(), 5);
}

#[test]
fn claim_returns_empty_when_no_matching_work() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    db.insert_run("f1", 1, "manual", "{}", "local", None)
        .unwrap();
    assert!(db.claim_runs("w1", &["gpu"], 5, 30).unwrap().is_empty());
    assert!(db.claim_runs("w1", &[], 5, 30).unwrap().is_empty());
    assert!(db.claim_runs("w1", &["local"], 0, 30).unwrap().is_empty());
}

#[test]
fn reap_fails_expired_lease_and_spares_fresh_one() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let stale = db.insert_run("f1", 1, "manual", "{}", "gpu", None).unwrap();
    let fresh = db.insert_run("f1", 1, "manual", "{}", "gpu", None).unwrap();
    db.claim_runs("w1", &["gpu"], 10, 30).unwrap();
    // Force the first run's lease into the past; a running task + queued item
    // ride along to prove the cascade.
    let tr = db.upsert_task_run(stale, "t", "running", 1).unwrap();
    db.insert_items(tr, &[json!({"x": 1})]).unwrap();
    {
        let conn = db.conn().unwrap();
        conn.execute(
            "UPDATE runs SET lease_expires_at = ?2 WHERE id = ?1",
            rusqlite::params![stale, ago_hours(1)],
        )
        .unwrap();
    }

    // The reaper's two DB primitives: select expired leases, then fail one.
    let expired: Vec<i64> = db
        .expired_lease_runs(&in_secs(0))
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(expired, vec![stale]);
    assert!(db.fail_lost_run(stale, &in_secs(0)).unwrap());

    let stale_run = db.get_run(stale).unwrap().unwrap();
    assert_eq!(stale_run.status, "failed");
    assert_eq!(
        stale_run.error.as_deref(),
        Some("worker lost (lease expired)")
    );
    assert_eq!(db.list_task_runs(stale).unwrap()[0].status, "failed");
    let (items, _) = db.list_items(tr, None, 1, 10).unwrap();
    assert_eq!(items[0].status, "canceled");

    // The fresh lease is untouched.
    assert_eq!(db.get_run(fresh).unwrap().unwrap().status, "leased");
}

#[test]
fn renew_extends_only_owned_active_runs() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    db.insert_run("f1", 1, "manual", "{}", "gpu", None).unwrap();
    let claimed = db.claim_runs("w1", &["gpu"], 10, 5).unwrap();
    let id = claimed[0].id;
    let before = db.get_run(id).unwrap().unwrap().lease_expires_at;

    let n = db.renew_leases("w1", &[id], 3600).unwrap();
    assert_eq!(n, 1);
    let after = db.get_run(id).unwrap().unwrap().lease_expires_at;
    assert!(
        after > before,
        "lease should extend: {before:?} -> {after:?}"
    );

    // A different worker cannot renew this run.
    assert_eq!(db.renew_leases("w2", &[id], 3600).unwrap(), 0);
}

#[test]
fn bump_seq_accepts_increasing_rejects_stale() {
    let (_dir, db) = open_temp();
    seed_flow(&db, "f1");
    let id = db.insert_run("f1", 1, "manual", "{}", "gpu", None).unwrap();
    assert!(db.bump_seq(id, 1).unwrap());
    assert!(db.bump_seq(id, 2).unwrap());
    assert!(!db.bump_seq(id, 2).unwrap(), "duplicate seq rejected");
    assert!(!db.bump_seq(id, 1).unwrap(), "stale seq rejected");
    assert!(db.bump_seq(id, 5).unwrap(), "gap forward accepted");
}

// ---------------------------------------------------------------------------
// Auth: users & sessions
// ---------------------------------------------------------------------------

#[test]
fn create_first_user_only_succeeds_while_empty() {
    let (_dir, db) = open_temp();
    assert!(!db.has_users().unwrap());

    let id = db.create_first_user("mike", "hash1").unwrap();
    assert!(id.is_some(), "first user is created");
    assert!(db.has_users().unwrap());
    assert_eq!(db.get_user_id("mike").unwrap(), id);
    assert_eq!(db.get_user_hash("mike").unwrap().as_deref(), Some("hash1"));

    // Setup is closed once a user exists: a second attempt is a no-op and does
    // NOT overwrite the existing admin's credentials.
    assert_eq!(db.create_first_user("intruder", "hash2").unwrap(), None);
    assert_eq!(db.get_user_id("intruder").unwrap(), None);
    assert_eq!(db.get_user_hash("mike").unwrap().as_deref(), Some("hash1"));
}

#[test]
fn get_user_id_and_hash_are_none_for_unknown() {
    let (_dir, db) = open_temp();
    assert_eq!(db.get_user_id("nobody").unwrap(), None);
    assert_eq!(db.get_user_hash("nobody").unwrap(), None);
}

#[test]
fn session_create_lookup_delete() {
    let (_dir, db) = open_temp();
    let uid = db.create_first_user("mike", "h").unwrap().unwrap();
    db.create_session("tok", uid, 3600).unwrap();
    assert_eq!(db.session_username("tok").unwrap().as_deref(), Some("mike"));
    db.delete_session("tok").unwrap();
    assert_eq!(db.session_username("tok").unwrap(), None);
}

#[test]
fn expired_session_is_not_returned_and_sweeps() {
    let (_dir, db) = open_temp();
    let uid = db.create_first_user("mike", "h").unwrap().unwrap();
    db.create_session("live", uid, 3600).unwrap();
    db.create_session("dead", uid, -10).unwrap(); // already expired
    assert_eq!(db.session_username("dead").unwrap(), None);
    assert_eq!(
        db.session_username("live").unwrap().as_deref(),
        Some("mike")
    );
    db.sweep_expired_sessions().unwrap();
    assert_eq!(
        db.session_username("live").unwrap().as_deref(),
        Some("mike")
    );
}

#[test]
fn admin_hash_verifies() {
    let (_dir, db) = open_temp();
    let hash = orchestrator::auth::hash_password("s3cret").unwrap();
    db.create_first_user("admin", &hash).unwrap().unwrap();
    let stored = db.get_user_hash("admin").unwrap().unwrap();
    assert!(orchestrator::auth::verify_password("s3cret", &stored));
    assert!(!orchestrator::auth::verify_password("nope", &stored));
}
