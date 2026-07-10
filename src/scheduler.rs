//! Cron scheduler loop with timezone support and catch-up.
//!
//! The scheduler owns the `schedule_state` table: it computes `next_fire_at`
//! values with [`croner`] evaluated in each trigger's IANA timezone, fires
//! due schedules by inserting queued run rows, and applies the trigger's
//! catch-up policy ([`crate::model::Catchup`]) to occurrences missed while
//! the process was down or the schedule lapsed.
//!
//! # Semantics (canonical decisions)
//!
//! - **Live fires vs backlog.** The due occurrences of a schedule are all
//!   fire times of the *current* cron in the window `[next_fire_at ..= now]`.
//!   The newest due occurrence is a **live fire** when it came due within
//!   the last [`LIVE_WINDOW_SECS`] seconds — the normal steady-state path
//!   while the process is up — and a live fire launches under *every*
//!   catch-up policy. Everything older is **backlog**, governed by the
//!   trigger's `catchup`: `none` skips backlog entirely (no make-up runs
//!   after downtime), `latest` fires the newest due occurrence exactly once
//!   whether live or backlog (so after downtime it is the single make-up
//!   run), and `all` fires every due occurrence ascending. Net effect:
//!   steady-state, every policy fires each occurrence as it comes due;
//!   after downtime, `none` waits for the next future occurrence, `latest`
//!   fires one make-up, `all` replays up to 100.
//! - **Cap.** `catchup: all` enumerates occurrences backwards from `now`
//!   with an iteration cap of [`CATCHUP_CAP`]` + 1`, so when more than 100
//!   occurrences are due the *most recent* 100 fire (the oldest are the
//!   least useful) and a WARN is logged. Because enumeration stops at the
//!   cap, the exact dropped count is not computed; the WARN reports the
//!   window start instead. `none`/`latest` enumerate at most one occurrence.
//! - **Second granularity.** All occurrence math is truncated to whole
//!   seconds (`trunc_secs`): croner preserves the sub-second fraction of
//!   its search origin, and fractional timestamps would miss the
//!   window/equality comparisons by milliseconds (a live fire would update
//!   state but launch nothing).
//! - **Paused flows** never launch runs, but a lapsed `next_fire_at` is still
//!   advanced to the next occurrence strictly after now (DEBUG log).
//!   Otherwise unpausing would trigger a stale catch-up burst covering the
//!   whole paused period. `last_fired_at` is left untouched — nothing fired.
//! - **Disabled schedules** are skipped entirely: `next_fire_at` is *not*
//!   advanced while disabled. Re-enabling (via reconcile, below) recomputes
//!   `next_fire_at` strictly after now, so no catch-up burst happens across
//!   the disabled period.
//! - **Enabled is owned by the definition.** The flow definition's
//!   `enabled` flag is the single source of truth; `schedule_state.enabled`
//!   is a mirror the scheduler and the read-only `/schedules` page read.
//!   [`Scheduler::reconcile_flow`] is the only writer.
//! - **Reconcile** ([`Scheduler::reconcile_flow`]) syncs `schedule_state`
//!   with the flow's current definition: new triggers get a fresh row (with
//!   `next_fire_at` = next occurrence after now), removed triggers lose their
//!   row, and every surviving row's `enabled` is set from the definition. A
//!   survivor flipping disabled -> enabled recomputes `next_fire_at` strictly
//!   after now (no catch-up burst across the disabled period). Otherwise a
//!   survivor's stored `next_fire_at` is preserved iff it is still an
//!   occurrence of the *current* cron in the current timezone (checked with
//!   `is_time_matching`); otherwise it is recomputed to the next occurrence
//!   after now. A stored occurrence in the *past* that still matches the cron
//!   is deliberately preserved so that startup reconciliation does not erase
//!   the catch-up window.
//! - **Stale state rows** (trigger id no longer in the definition) found at
//!   tick time are deleted by re-reconciling the flow, without firing.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use chrono_tz::Tz;
use croner::Cron;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::db::{Db, DbError, ScheduleRow};
use crate::model::{Catchup, FlowDefinition, TriggerDef, cron_parser};

/// Maximum number of runs a single `catchup: all` firing may launch.
pub const CATCHUP_CAP: usize = 100;

/// How recently (in seconds) an occurrence must have come due to count as a
/// LIVE fire rather than backlog. Live fires launch under *every* catch-up
/// policy — this is the normal steady-state path while the process is up.
/// Comfortably larger than the 1-second tick cadence so a slow tick never
/// demotes an on-time occurrence to backlog.
pub const LIVE_WINDOW_SECS: i64 = 10;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors surfaced by scheduler operations.
#[derive(Debug)]
pub enum SchedulerError {
    /// Database failure.
    Db(DbError),
    /// A trigger's cron expression failed to parse with the canonical
    /// parser configuration (5-field, seconds and years disallowed).
    BadCron {
        /// Flow owning the trigger.
        flow_id: String,
        /// Offending trigger id.
        trigger_id: String,
        /// Parser error message.
        message: String,
    },
    /// A trigger's timezone is not a known IANA timezone name.
    BadTimezone {
        /// Flow owning the trigger.
        flow_id: String,
        /// Offending trigger id.
        trigger_id: String,
        /// The unparseable timezone string.
        timezone: String,
    },
    /// A flow's stored definition JSON failed to deserialize.
    BadDefinition {
        /// Flow whose definition is broken.
        flow_id: String,
        /// Deserialization error message.
        message: String,
    },
    /// No flow with the given id exists.
    UnknownFlow(String),
    /// The flow exists but its definition has no trigger with this id.
    UnknownTrigger {
        /// Flow that was inspected.
        flow_id: String,
        /// Trigger id that was not found.
        trigger_id: String,
    },
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchedulerError::Db(e) => write!(f, "scheduler database error: {e}"),
            SchedulerError::BadCron {
                flow_id,
                trigger_id,
                message,
            } => write!(
                f,
                "invalid cron for flow `{flow_id}` trigger `{trigger_id}`: {message}"
            ),
            SchedulerError::BadTimezone {
                flow_id,
                trigger_id,
                timezone,
            } => write!(
                f,
                "unknown timezone `{timezone}` for flow `{flow_id}` trigger `{trigger_id}`"
            ),
            SchedulerError::BadDefinition { flow_id, message } => {
                write!(f, "invalid definition for flow `{flow_id}`: {message}")
            }
            SchedulerError::UnknownFlow(id) => write!(f, "unknown flow `{id}`"),
            SchedulerError::UnknownTrigger {
                flow_id,
                trigger_id,
            } => write!(f, "flow `{flow_id}` has no trigger `{trigger_id}`"),
        }
    }
}

impl std::error::Error for SchedulerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SchedulerError::Db(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DbError> for SchedulerError {
    fn from(e: DbError) -> Self {
        SchedulerError::Db(e)
    }
}

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// Launches queued runs; implemented by the engine (wired in task D3).
/// Decouples the scheduler from the engine.
pub trait RunLauncher: Send + Sync {
    /// Called after the scheduler inserts a queued run row.
    fn launch(&self, run_id: i64);
}

/// Injectable time source so tests can drive the clock deterministically.
pub trait Clock: Send + Sync {
    /// The current instant in UTC.
    fn now_utc(&self) -> DateTime<Utc>;
}

/// [`Clock`] backed by the system wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// The cron scheduler: a 1-second tick loop (plus a [`Notify`] poke on
/// schedule mutations) that fires due schedules with catch-up.
pub struct Scheduler {
    db: Db,
    launcher: Arc<dyn RunLauncher>,
    clock: Arc<dyn Clock>,
    notify: Arc<Notify>,
}

impl Scheduler {
    /// Build a scheduler over `db`, launching runs through `launcher` and
    /// reading time from `clock`.
    pub fn new(db: Db, launcher: Arc<dyn RunLauncher>, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Scheduler {
            db,
            launcher,
            clock,
            notify: Arc::new(Notify::new()),
        })
    }

    /// Handle the API layer pokes when schedules mutate, waking the loop
    /// before its next 1-second tick.
    pub fn notify_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// The scheduler loop: waits for the earliest of shutdown, a notify
    /// poke, or a 1-second tick, then runs [`Scheduler::tick_once`]. Errors
    /// are logged; the loop never crashes.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        debug!("scheduler loop started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!("scheduler loop stopping (shutdown)");
                    return;
                }
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            match self.tick_once() {
                Ok(ids) if !ids.is_empty() => {
                    debug!(count = ids.len(), "scheduler launched runs");
                }
                Ok(_) => {}
                Err(e) => error!(error = %e, "scheduler tick failed"),
            }
        }
    }

    /// One scheduler pass: fire every enabled, unpaused, due schedule and
    /// return the launched run ids (ascending `scheduled_for` per trigger).
    ///
    /// Per-row failures (broken definition, bad cron, …) are logged at WARN
    /// and skipped so one bad schedule cannot starve the others; `Err` is
    /// only returned when listing the schedules themselves fails.
    pub fn tick_once(&self) -> Result<Vec<i64>, SchedulerError> {
        let now = self.clock.now_utc();
        let mut launched = Vec::new();
        for row in self.db.list_schedules()? {
            if !row.enabled {
                continue;
            }
            let Some(next_str) = row.next_fire_at.as_deref() else {
                continue;
            };
            let Some(due) = parse_utc(next_str) else {
                warn!(
                    flow = %row.flow_id,
                    trigger = %row.trigger_id,
                    next_fire_at = %next_str,
                    "scheduler: unparseable next_fire_at; skipping row"
                );
                continue;
            };
            if due > now {
                continue;
            }
            if let Err(e) = self.fire_row(&row, due, now, &mut launched) {
                warn!(
                    flow = %row.flow_id,
                    trigger = %row.trigger_id,
                    error = %e,
                    "scheduler: skipping schedule row after error"
                );
            }
        }
        Ok(launched)
    }

    /// Fire one due schedule row: apply the catch-up policy, launch runs,
    /// and advance `next_fire_at` strictly past `now`.
    fn fire_row(
        &self,
        row: &ScheduleRow,
        due: DateTime<Utc>,
        now: DateTime<Utc>,
        launched: &mut Vec<i64>,
    ) -> Result<(), SchedulerError> {
        let flow = self
            .db
            .get_flow(&row.flow_id)?
            .ok_or_else(|| SchedulerError::UnknownFlow(row.flow_id.clone()))?;
        let def = parse_definition(&row.flow_id, &flow.definition)?;
        let Some(trigger) = def.triggers.iter().find(|t| t.id == row.trigger_id) else {
            // Stale state row: the trigger was removed from the definition
            // but the row survived (e.g. reconcile was missed). Re-reconcile
            // to delete it; fire nothing.
            debug!(
                flow = %row.flow_id,
                trigger = %row.trigger_id,
                "scheduler: stale schedule row (trigger gone from definition); reconciling"
            );
            self.reconcile_definition(&row.flow_id, &def, now)?;
            return Ok(());
        };
        let (cron, tz) = self.parse_trigger(&row.flow_id, trigger)?;
        // All occurrence math runs at whole-second granularity: croner
        // preserves the sub-second fraction of its search origin, so an
        // untruncated basis would store fractional next_fire_at values and
        // exclude real occurrences from the missed window by milliseconds.
        let due = trunc_secs(due);
        let now_s = trunc_secs(now);
        let next_after_now = next_occurrence(&cron, tz, now_s, false).map(|t| fmt_utc(&t));

        if row.flow_paused {
            // Advance without firing so unpausing does not replay the
            // paused period as a catch-up burst. `last_fired_at` stays
            // untouched: nothing fired.
            debug!(
                flow = %row.flow_id,
                trigger = %row.trigger_id,
                "scheduler: flow paused; advancing next_fire_at without firing"
            );
            self.db
                .set_schedule_next(&row.flow_id, &row.trigger_id, next_after_now.as_deref())?;
            return Ok(());
        }

        // Due occurrences of the *current* cron in [due ..= now], enumerated
        // newest-first, capped at CATCHUP_CAP + 1 iterations. `none`/`latest`
        // only ever inspect the newest, so they enumerate at most one.
        let due_desc = |limit: usize| -> Vec<DateTime<Utc>> {
            let mut found: Vec<DateTime<Utc>> = Vec::new();
            let mut cursor = now_s.with_timezone(&tz);
            let mut inclusive = true;
            while found.len() < limit {
                let Ok(occ) = cron.find_previous_occurrence(&cursor, inclusive) else {
                    break;
                };
                let occ_utc = occ.with_timezone(&Utc);
                if occ_utc < due {
                    break;
                }
                found.push(occ_utc);
                cursor = occ;
                inclusive = false;
            }
            found
        };
        // The newest due occurrence is a LIVE fire (not backlog) when it came
        // due within the live window; it then fires under every policy.
        let is_live = |occ: &DateTime<Utc>| {
            now_s.signed_duration_since(occ).num_seconds() <= LIVE_WINDOW_SECS
        };

        match trigger.catchup {
            Catchup::None => {
                // Fire only a live newest occurrence; the backlog is skipped.
                if let Some(newest) = due_desc(1).first()
                    && is_live(newest)
                {
                    self.launch_run(&flow.id, flow.current_rev, &def.queue, newest, launched)?;
                }
            }
            Catchup::Latest => {
                // The newest due occurrence fires whether it is live (the
                // steady-state path) or backlog (the single make-up run);
                // at most one run per pass either way.
                if let Some(newest) = due_desc(1).first() {
                    self.launch_run(&flow.id, flow.current_rev, &def.queue, newest, launched)?;
                }
            }
            Catchup::All => {
                // Live/backlog makes no difference: every due occurrence
                // fires, ascending, capped at the most recent CATCHUP_CAP.
                let mut due_all = due_desc(CATCHUP_CAP + 1);
                if due_all.len() > CATCHUP_CAP {
                    warn!(
                        flow = %row.flow_id,
                        trigger = %row.trigger_id,
                        cap = CATCHUP_CAP,
                        window_start = %fmt_utc(&due),
                        "scheduler: catch-up capped; dropping occurrences older than the most recent 100"
                    );
                    due_all.truncate(CATCHUP_CAP);
                }
                for occ in due_all.iter().rev() {
                    self.launch_run(&flow.id, flow.current_rev, &def.queue, occ, launched)?;
                }
            }
        }

        self.db.update_schedule_fired(
            &row.flow_id,
            &row.trigger_id,
            &fmt_utc(&now),
            next_after_now.as_deref(),
        )?;
        Ok(())
    }

    /// Insert one queued schedule run on `queue` and hand it to the launcher.
    fn launch_run(
        &self,
        flow_id: &str,
        flow_rev: i64,
        queue: &str,
        scheduled_for: &DateTime<Utc>,
        launched: &mut Vec<i64>,
    ) -> Result<(), SchedulerError> {
        let run_id = self.db.insert_run(
            flow_id,
            flow_rev,
            "schedule",
            "{}",
            queue,
            Some(&fmt_utc(scheduled_for)),
        )?;
        self.launcher.launch(run_id);
        launched.push(run_id);
        Ok(())
    }

    /// Recompute `schedule_state` for one flow from its current definition.
    ///
    /// Call sites: flow save/import (D1) and startup (D3, via
    /// [`Scheduler::reconcile_all`]).
    pub fn reconcile_flow(&self, flow_id: &str) -> Result<(), SchedulerError> {
        let flow = self
            .db
            .get_flow(flow_id)?
            .ok_or_else(|| SchedulerError::UnknownFlow(flow_id.to_string()))?;
        let def = parse_definition(flow_id, &flow.definition)?;
        self.reconcile_definition(flow_id, &def, self.clock.now_utc())
    }

    /// Reconcile every flow's schedule state. Per-flow failures are logged
    /// at WARN and skipped so one broken flow cannot block startup.
    pub fn reconcile_all(&self) -> Result<(), SchedulerError> {
        for flow in self.db.list_flows()? {
            if let Err(e) = self.reconcile_flow(&flow.id) {
                warn!(flow = %flow.id, error = %e, "scheduler: reconcile failed; skipping flow");
            }
        }
        Ok(())
    }

    /// Reconcile `schedule_state` rows for `flow_id` against `def`.
    ///
    /// See the module docs for the survivor-preservation rule: a stored
    /// `next_fire_at` is kept iff it still matches the current cron in the
    /// current timezone (even when it is in the past, preserving the
    /// catch-up window across restarts); otherwise it is recomputed to the
    /// next occurrence strictly after `now`.
    fn reconcile_definition(
        &self,
        flow_id: &str,
        def: &FlowDefinition,
        now: DateTime<Utc>,
    ) -> Result<(), SchedulerError> {
        // Next occurrence after `now` per trigger, used for new rows and
        // for survivors whose stored value is no longer valid.
        let mut computed: Vec<(String, Option<String>)> = Vec::with_capacity(def.triggers.len());
        for t in &def.triggers {
            let (cron, tz) = self.parse_trigger(flow_id, t)?;
            let next = next_occurrence(&cron, tz, now, false).map(|t| fmt_utc(&t));
            computed.push((t.id.clone(), next));
        }

        let pre_existing: Vec<String> = self
            .db
            .list_schedules()?
            .into_iter()
            .filter(|r| r.flow_id == flow_id)
            .map(|r| r.trigger_id)
            .collect();

        let trigger_pairs: Vec<(&str, Option<&str>)> = computed
            .iter()
            .map(|(id, next)| (id.as_str(), next.as_deref()))
            .collect();
        self.db.reconcile_schedules(flow_id, &trigger_pairs)?;

        for row in self
            .db
            .list_schedules()?
            .into_iter()
            .filter(|r| r.flow_id == flow_id)
        {
            let Some(trigger) = def.triggers.iter().find(|t| t.id == row.trigger_id) else {
                continue; // unreachable: reconcile_schedules just deleted these
            };
            let computed_next = computed
                .iter()
                .find(|(id, _)| *id == row.trigger_id)
                .and_then(|(_, next)| next.as_deref());
            // The definition is the single source of truth for `enabled`:
            // sync the flag onto the row (fresh rows insert enabled=1). A
            // survivor flipping disabled -> enabled recomputes `next_fire_at`
            // strictly after now (like the old toggle path), so the disabled
            // period is never replayed as a catch-up burst.
            let re_enabling = !row.enabled && trigger.enabled;
            if row.enabled != trigger.enabled {
                self.db
                    .set_schedule_enabled(flow_id, &row.trigger_id, trigger.enabled)?;
            }
            if !pre_existing.contains(&row.trigger_id) {
                // Fresh row: next_fire_at was set at insert.
                continue;
            }
            if re_enabling {
                self.db
                    .set_schedule_next(flow_id, &row.trigger_id, computed_next)?;
                continue;
            }
            let (cron, tz) = self.parse_trigger(flow_id, trigger)?;
            let stored_still_valid =
                row.next_fire_at
                    .as_deref()
                    .and_then(parse_utc)
                    .is_some_and(|stored| {
                        cron.is_time_matching(&stored.with_timezone(&tz))
                            .unwrap_or(false)
                    });
            if !stored_still_valid {
                self.db
                    .set_schedule_next(flow_id, &row.trigger_id, computed_next)?;
            }
        }
        Ok(())
    }

    /// Parse a trigger's cron and timezone, with scheduler-flavoured errors.
    fn parse_trigger(
        &self,
        flow_id: &str,
        trigger: &TriggerDef,
    ) -> Result<(Cron, Tz), SchedulerError> {
        let cron = cron_parser()
            .parse(&trigger.cron)
            .map_err(|e| SchedulerError::BadCron {
                flow_id: flow_id.to_string(),
                trigger_id: trigger.id.clone(),
                message: e.to_string(),
            })?;
        let tz: Tz = trigger
            .timezone
            .parse()
            .map_err(|_| SchedulerError::BadTimezone {
                flow_id: flow_id.to_string(),
                trigger_id: trigger.id.clone(),
                timezone: trigger.timezone.clone(),
            })?;
        Ok((cron, tz))
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Format an instant the way the database stores timestamps
/// (RFC3339 UTC, millisecond precision, `Z` suffix — see `db::now_rfc3339`).
fn fmt_utc(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Parse a stored RFC3339 timestamp back into a UTC instant.
fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Truncate an instant to whole-second granularity. All occurrence math
/// operates on truncated instants: croner preserves the sub-second fraction
/// of its search origin, so an untruncated basis would produce fractional
/// occurrence timestamps that miss equality/window comparisons by
/// milliseconds.
fn trunc_secs(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_nanosecond(0).unwrap_or(t)
}

/// Next occurrence of `cron` evaluated in `tz`, converted back to UTC.
/// The search origin is truncated to whole seconds (see [`trunc_secs`]).
/// `inclusive` includes `from` itself when it matches. `None` when croner's
/// search gives up (pattern never matches within its horizon).
fn next_occurrence(
    cron: &Cron,
    tz: Tz,
    from: DateTime<Utc>,
    inclusive: bool,
) -> Option<DateTime<Utc>> {
    cron.find_next_occurrence(&trunc_secs(from).with_timezone(&tz), inclusive)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Deserialize a flow's stored definition JSON.
fn parse_definition(flow_id: &str, definition: &str) -> Result<FlowDefinition, SchedulerError> {
    serde_json::from_str(definition).map_err(|e| SchedulerError::BadDefinition {
        flow_id: flow_id.to_string(),
        message: e.to_string(),
    })
}
