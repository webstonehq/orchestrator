//! Integration tests for the cron scheduler: timezone math, catch-up
//! policies, pause/disable semantics, reconciliation, and the run loop.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use orchestrator::db::Db;
use orchestrator::scheduler::{Clock, RunLauncher, Scheduler};

// ---------------------------------------------------------------------------
// Test doubles & harness
// ---------------------------------------------------------------------------

/// Settable clock shared between the test and the scheduler.
struct MockClock(Mutex<DateTime<Utc>>);

impl MockClock {
    fn at(s: &str) -> Arc<Self> {
        Arc::new(MockClock(Mutex::new(utc(s))))
    }
    fn set(&self, s: &str) {
        *self.0.lock().unwrap() = utc(s);
    }
}

impl Clock for MockClock {
    fn now_utc(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

/// Records every launched run id.
#[derive(Default)]
struct MockLauncher(Mutex<Vec<i64>>);

impl MockLauncher {
    fn ids(&self) -> Vec<i64> {
        self.0.lock().unwrap().clone()
    }
}

impl RunLauncher for MockLauncher {
    fn launch(&self, run_id: i64) {
        self.0.lock().unwrap().push(run_id);
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    db: Db,
    sched: Arc<Scheduler>,
    clock: Arc<MockClock>,
    launcher: Arc<MockLauncher>,
}

fn harness(start: &str) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path().join("test.db")).expect("open db");
    let clock = MockClock::at(start);
    let launcher = Arc::new(MockLauncher::default());
    let sched = Scheduler::new(
        db.clone(),
        Arc::clone(&launcher) as Arc<dyn RunLauncher>,
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    Harness {
        _dir: dir,
        db,
        sched,
        clock,
        launcher,
    }
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("rfc3339")
        .with_timezone(&Utc)
}

/// Store a flow whose definition has the given triggers array.
fn seed_flow(db: &Db, flow_id: &str, triggers: Value) {
    let def = json!({ "name": "Test Flow", "triggers": triggers });
    db.upsert_flow_with_revision(
        flow_id,
        "Test Flow",
        "default",
        "",
        &def.to_string(),
        "test",
    )
    .expect("seed flow");
}

fn trigger(id: &str, cron: &str, tz: &str, catchup: &str) -> Value {
    json!({ "id": id, "type": "schedule", "cron": cron, "timezone": tz, "catchup": catchup })
}

fn row(db: &Db, flow_id: &str, trigger_id: &str) -> orchestrator::db::ScheduleRow {
    db.list_schedules()
        .expect("list schedules")
        .into_iter()
        .find(|r| r.flow_id == flow_id && r.trigger_id == trigger_id)
        .unwrap_or_else(|| panic!("no schedule row for {flow_id}/{trigger_id}"))
}

fn flow_rows(db: &Db, flow_id: &str) -> Vec<orchestrator::db::ScheduleRow> {
    db.list_schedules()
        .expect("list schedules")
        .into_iter()
        .filter(|r| r.flow_id == flow_id)
        .collect()
}

fn scheduled_fors(db: &Db, ids: &[i64]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            db.get_run(*id)
                .expect("get run")
                .expect("run exists")
                .scheduled_for
                .expect("scheduled_for set")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Timezone-correct next occurrence (incl. DST transition)
// ---------------------------------------------------------------------------

#[test]
fn next_fire_is_timezone_correct_across_dst() {
    // 2026-03-08 is the US/Canada spring-forward day: 02:00 EST -> 03:00 EDT.
    // Day before the transition: 03:00 America/Toronto == 08:00Z (EST).
    let h = harness("2026-03-07T05:00:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 3 * * *", "America/Toronto", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-03-07T08:00:00.000Z")
    );

    // On the transition day itself: 03:00 America/Toronto == 07:00Z (EDT).
    let h = harness("2026-03-08T05:00:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 3 * * *", "America/Toronto", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-03-08T07:00:00.000Z")
    );
}

// ---------------------------------------------------------------------------
// 2. Basic fire on due
// ---------------------------------------------------------------------------

#[test]
fn fires_due_schedule_and_advances_state() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-01-01T01:00:00.000Z")
    );

    h.clock.set("2026-01-01T01:00:05.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 1);
    assert_eq!(h.launcher.ids(), ids);

    let run = h.db.get_run(ids[0]).expect("get run").expect("run exists");
    assert_eq!(run.status, "queued");
    assert_eq!(run.trigger, "schedule");
    assert_eq!(run.inputs, "{}");
    assert_eq!(run.flow_rev, 1);
    assert_eq!(
        run.scheduled_for.as_deref(),
        Some("2026-01-01T01:00:00.000Z")
    );

    let state = row(&h.db, "f1", "t1");
    assert_eq!(
        state.last_fired_at.as_deref(),
        Some("2026-01-01T01:00:05.000Z")
    );
    assert_eq!(
        state.next_fire_at.as_deref(),
        Some("2026-01-01T02:00:00.000Z")
    );

    // Nothing further to do until the next occurrence.
    assert!(h.sched.tick_once().expect("tick").is_empty());
}

// ---------------------------------------------------------------------------
// 3–6. Catch-up policies
// ---------------------------------------------------------------------------

#[test]
fn catchup_latest_launches_one_run_for_most_recent_missed() {
    let h = harness("2026-01-01T12:00:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 0 * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // Jump 3 days: missed Jan 2, Jan 3, Jan 4 midnights.
    h.clock.set("2026-01-04T12:00:00.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 1);
    assert_eq!(
        scheduled_fors(&h.db, &ids),
        vec!["2026-01-04T00:00:00.000Z".to_string()]
    );
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-01-05T00:00:00.000Z")
    );
}

#[test]
fn catchup_all_launches_one_run_per_missed_ascending() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "all")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    h.clock.set("2026-01-01T05:30:00.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 5);
    assert_eq!(
        scheduled_fors(&h.db, &ids),
        vec![
            "2026-01-01T01:00:00.000Z",
            "2026-01-01T02:00:00.000Z",
            "2026-01-01T03:00:00.000Z",
            "2026-01-01T04:00:00.000Z",
            "2026-01-01T05:00:00.000Z",
        ]
    );
}

#[test]
fn catchup_all_caps_at_100_most_recent() {
    let h = harness("2026-01-01T00:00:30.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "* * * * *", "UTC", "all")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // Jump 300 minutes: 300 missed minutely occurrences (00:01 ..= 05:00).
    h.clock.set("2026-01-01T05:00:30.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 100);

    // The most recent 100 (03:21 ..= 05:00), oldest first.
    let fors = scheduled_fors(&h.db, &ids);
    assert_eq!(
        fors.first().map(String::as_str),
        Some("2026-01-01T03:21:00.000Z")
    );
    assert_eq!(
        fors.last().map(String::as_str),
        Some("2026-01-01T05:00:00.000Z")
    );
    let mut sorted = fors.clone();
    sorted.sort();
    assert_eq!(fors, sorted, "runs launch in ascending scheduled_for order");
}

#[test]
fn catchup_none_fires_live_occurrence_in_steady_state() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "none")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // Occurrence came due 3s ago: within the live window, so it fires
    // even under catchup:none (it is a live fire, not backlog).
    h.clock.set("2026-01-01T01:00:03.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 1);
    assert_eq!(
        scheduled_fors(&h.db, &ids),
        vec!["2026-01-01T01:00:00.000Z".to_string()]
    );
}

#[test]
fn catchup_none_skips_backlog_after_gap_then_fires_next_live() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "none")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // 3-day gap: every due occurrence is backlog — nothing fires, but the
    // state still advances past now.
    h.clock.set("2026-01-04T05:30:00.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert!(ids.is_empty(), "catchup:none must not fire make-up runs");
    assert!(h.launcher.ids().is_empty());

    let state = row(&h.db, "f1", "t1");
    assert_eq!(
        state.last_fired_at.as_deref(),
        Some("2026-01-04T05:30:00.000Z")
    );
    assert_eq!(
        state.next_fire_at.as_deref(),
        Some("2026-01-04T06:00:00.000Z")
    );

    // The next future occurrence fires normally once it comes due (live).
    h.clock.set("2026-01-04T06:00:02.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 1);
    assert_eq!(
        scheduled_fors(&h.db, &ids),
        vec!["2026-01-04T06:00:00.000Z".to_string()]
    );
}

#[test]
fn subsecond_clock_fraction_does_not_drop_live_fires() {
    // Regression: croner preserves the sub-second fraction of its search
    // origin, so an untruncated basis stored next_fire_at like
    // 06:53:00.447Z and the missed-window comparison then excluded the
    // real occurrence by milliseconds (state advanced, zero runs).
    let h = harness("2026-07-05T06:52:30.447Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "* * * * *", "UTC", "none")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-07-05T06:53:00.000Z"),
        "stored next_fire_at must be truncated to whole seconds"
    );

    h.clock.set("2026-07-05T06:53:00.447Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 1, "fractional now must not drop the live fire");
    assert_eq!(
        scheduled_fors(&h.db, &ids),
        vec!["2026-07-05T06:53:00.000Z".to_string()]
    );
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-07-05T06:54:00.000Z")
    );
}

// ---------------------------------------------------------------------------
// 7. Disabled schedules
// ---------------------------------------------------------------------------

#[test]
fn disabled_schedule_is_skipped_and_reenable_recomputes_from_now() {
    // Enable/disable is driven entirely through the flow definition (the
    // single source of truth), reconciled onto schedule_state.
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "all")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // Disable via the definition.
    let mut off = trigger("t1", "0 * * * *", "UTC", "all");
    off["enabled"] = json!(false);
    seed_flow(&h.db, "f1", json!([off]));
    h.sched.reconcile_flow("f1").expect("reconcile");

    // While disabled: tick skips entirely — no runs, next_fire_at untouched.
    h.clock.set("2026-01-01T05:30:00.000Z");
    assert!(h.sched.tick_once().expect("tick").is_empty());
    let state = row(&h.db, "f1", "t1");
    assert!(!state.enabled);
    assert_eq!(
        state.next_fire_at.as_deref(),
        Some("2026-01-01T01:00:00.000Z"),
        "next_fire_at must not advance while disabled"
    );

    // Re-enable via the definition: next_fire_at recomputed strictly after
    // now — the disabled period is not replayed as a catch-up burst.
    seed_flow(&h.db, "f1", json!([trigger("t1", "0 * * * *", "UTC", "all")]));
    h.sched.reconcile_flow("f1").expect("reconcile");
    let state = row(&h.db, "f1", "t1");
    assert!(state.enabled);
    assert_eq!(
        state.next_fire_at.as_deref(),
        Some("2026-01-01T06:00:00.000Z")
    );
    assert!(h.sched.tick_once().expect("tick").is_empty());
    assert!(h.launcher.ids().is_empty());
}

// ---------------------------------------------------------------------------
// 8. Paused flows
// ---------------------------------------------------------------------------

#[test]
fn paused_flow_advances_state_without_firing() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "all")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    h.db.set_paused("f1", true).expect("pause");

    h.clock.set("2026-01-01T05:30:00.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert!(ids.is_empty());
    assert!(h.launcher.ids().is_empty());

    let state = row(&h.db, "f1", "t1");
    assert_eq!(
        state.next_fire_at.as_deref(),
        Some("2026-01-01T06:00:00.000Z"),
        "lapsed next_fire_at advances while paused (no burst on unpause)"
    );
    assert!(state.last_fired_at.is_none(), "nothing fired while paused");

    // Unpausing does not replay the paused period.
    h.db.set_paused("f1", false).expect("unpause");
    assert!(h.sched.tick_once().expect("tick").is_empty());
}

// ---------------------------------------------------------------------------
// 9. Reconciliation: add/remove triggers, stale rows at tick time
// ---------------------------------------------------------------------------

#[test]
fn reconcile_adds_removes_and_syncs_enabled() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // Definition disables t1 and gains t2: the survivor's enabled follows
    // the definition (now false); t2 is a new enabled row with a computed
    // next_fire_at.
    let mut t1_off = trigger("t1", "0 * * * *", "UTC", "latest");
    t1_off["enabled"] = json!(false);
    seed_flow(
        &h.db,
        "f1",
        json!([t1_off, trigger("t2", "0 0 * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    let rows = flow_rows(&h.db, "f1");
    assert_eq!(rows.len(), 2);
    let t1 = row(&h.db, "f1", "t1");
    assert!(!t1.enabled, "survivor's enabled follows the definition");
    let t2 = row(&h.db, "f1", "t2");
    assert!(t2.enabled);
    assert_eq!(t2.next_fire_at.as_deref(), Some("2026-01-02T00:00:00.000Z"));

    // Definition loses t2: its row is deleted.
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    let rows = flow_rows(&h.db, "f1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].trigger_id, "t1");
}

#[test]
fn reconcile_honors_definition_enabled_false_for_new_rows() {
    let h = harness("2026-01-01T00:30:00.000Z");
    let mut t = trigger("t1", "0 * * * *", "UTC", "latest");
    t["enabled"] = json!(false);
    seed_flow(&h.db, "f1", json!([t]));
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert!(!row(&h.db, "f1", "t1").enabled);
}

#[test]
fn reconcile_syncs_enabled_from_definition_for_survivor() {
    // The flow definition is the single source of truth for `enabled`:
    // flipping a *pre-existing* trigger's `enabled` in the definition and
    // reconciling must propagate to schedule_state (which the scheduler and
    // the /schedules page both read), and it must then fire when due.
    let h = harness("2026-01-01T00:30:00.000Z");
    let mut disabled = trigger("t1", "0 * * * *", "UTC", "latest");
    disabled["enabled"] = json!(false);
    seed_flow(&h.db, "f1", json!([disabled]));
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert!(!row(&h.db, "f1", "t1").enabled, "starts disabled");

    // Enable the survivor trigger via the definition and reconcile again.
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert!(
        row(&h.db, "f1", "t1").enabled,
        "survivor's enabled must follow the definition"
    );

    // ...and it now fires at the next hour.
    h.clock.set("2026-01-01T01:00:00.000Z");
    let ids = h.sched.tick_once().expect("tick");
    assert_eq!(ids.len(), 1, "enabled survivor fires when due");
}

#[test]
fn stale_state_row_is_deleted_at_tick_without_firing() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // Inject a state row for a trigger that is not in the definition, with
    // a due next_fire_at (simulates a missed reconcile).
    h.db.reconcile_schedules(
        "f1",
        &[
            ("t1", Some("2026-01-01T01:00:00.000Z")),
            ("ghost", Some("2026-01-01T00:00:00.000Z")),
        ],
    )
    .expect("inject ghost row");
    assert_eq!(flow_rows(&h.db, "f1").len(), 2);

    let ids = h.sched.tick_once().expect("tick");
    assert!(ids.is_empty(), "stale trigger must not fire");
    let rows = flow_rows(&h.db, "f1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].trigger_id, "t1");
}

// ---------------------------------------------------------------------------
// 10. Reconciliation: cron/timezone changes on survivors
// ---------------------------------------------------------------------------

#[test]
fn reconcile_recomputes_survivor_when_cron_changed() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 3 * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-01-01T03:00:00.000Z")
    );

    // Cron changes; the stored 03:00 no longer matches "0 5 * * *".
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 5 * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-01-01T05:00:00.000Z")
    );
}

#[test]
fn reconcile_preserves_survivor_with_valid_stored_occurrence() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 5 * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");

    // A stored *future* occurrence of the current cron is preserved even
    // when it is not the nearest one.
    h.db.set_schedule_next("f1", "t1", Some("2026-01-04T05:00:00.000Z"))
        .expect("set next");
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-01-04T05:00:00.000Z")
    );

    // A stored *past* occurrence that still matches the cron is preserved
    // too: startup reconcile must not erase the catch-up window.
    h.clock.set("2026-01-10T12:00:00.000Z");
    h.sched.reconcile_flow("f1").expect("reconcile");
    assert_eq!(
        row(&h.db, "f1", "t1").next_fire_at.as_deref(),
        Some("2026-01-04T05:00:00.000Z"),
        "past-but-matching next_fire_at survives reconcile so tick can catch up"
    );
}

// ---------------------------------------------------------------------------
// 11. Run loop: notify wake and shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_loop_wakes_on_notify_and_exits_on_shutdown() {
    let h = harness("2026-01-01T00:30:00.000Z");
    seed_flow(
        &h.db,
        "f1",
        json!([trigger("t1", "0 * * * *", "UTC", "latest")]),
    );
    h.sched.reconcile_flow("f1").expect("reconcile");
    h.clock.set("2026-01-01T01:00:05.000Z"); // schedule is now due

    let shutdown = tokio_util::sync::CancellationToken::new();
    let notify = h.sched.notify_handle();
    let handle = tokio::spawn(Arc::clone(&h.sched).run(shutdown.clone()));

    // Poke the loop; the due schedule should fire well before the 1s tick.
    notify.notify_one();
    let mut fired = false;
    for _ in 0..50 {
        if !h.launcher.ids().is_empty() {
            fired = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(fired, "notify poke should wake the loop before the 1s tick");

    // Shutdown stops the loop promptly.
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("loop exits promptly on shutdown")
        .expect("loop task does not panic");
}
