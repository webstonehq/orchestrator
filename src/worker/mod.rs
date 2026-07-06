//! The worker: the same engine in a worker role.
//!
//! A worker dials the control-plane server, claims queued runs off its queues,
//! and executes them with a real [`Engine`] against a local *scratch* database
//! and its **own** secret store — plaintext secrets never leave the worker.
//! Each run's state is streamed back as [`RunUpdate`](crate::engine::RunUpdate)
//! batches for the server to persist and rebroadcast.
//!
//! Transport is plain authenticated HTTP (the worker always dials out, so it
//! traverses NAT without inbound reachability): it polls `claim`, POSTs
//! `updates`, and `heartbeat`s to renew leases and learn about cancellations.
//! The heavy lifting — sequencing, retries, fan-out, redaction — is the shared
//! engine loop; only the sink differs from an in-process run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::db::Db;
use crate::engine::{Assignment, Engine, LocalSink, RemoteSink, RunUpdate, SeqUpdate};
use crate::plugins::PluginRegistry;
use crate::secrets::SecretStore;

/// How often the worker polls for work and heartbeats its in-flight runs.
/// Comfortably below the server's lease length so a live run never lapses.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Resolved worker configuration.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Control-plane base URL, e.g. `http://127.0.0.1:4400`.
    pub server_url: String,
    /// Bearer token presented on every request.
    pub token: String,
    /// Stable id identifying this worker to the server (lease owner).
    pub worker_id: String,
    /// Queues this worker serves.
    pub queues: Vec<String>,
    /// Maximum runs executed concurrently.
    pub capacity: u32,
    /// Scratch database path (throwaway; the server holds authoritative state).
    pub db_path: PathBuf,
    /// This worker's own secrets key file.
    pub key_path: PathBuf,
}

/// Shared handle to in-flight runs: server run id -> its cancellation token.
type Inflight = Arc<Mutex<HashMap<i64, CancellationToken>>>;

/// Run the worker loop until `shutdown` fires.
pub async fn run(
    cfg: WorkerConfig,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let db = Db::open(&cfg.db_path)?;
    let manager = r2d2_sqlite::SqliteConnectionManager::file(&cfg.db_path).with_init(|conn| {
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    });
    let pool = r2d2::Pool::builder().max_size(4).build(manager)?;
    let secrets = Arc::new(SecretStore::open(&cfg.key_path, pool)?);
    let registry = Arc::new(PluginRegistry::builtin());
    let engine = Engine::new(db.clone(), registry, secrets);
    let client = reqwest::Client::new();
    let inflight: Inflight = Arc::new(Mutex::new(HashMap::new()));

    info!(
        server = %cfg.server_url,
        worker_id = %cfg.worker_id,
        queues = ?cfg.queues,
        capacity = cfg.capacity,
        "worker started"
    );

    loop {
        heartbeat(&client, &cfg, &inflight).await;

        // Claim whenever we have spare slots. We send our TOTAL capacity; the
        // server leases only up to the slots we're not already holding, so the
        // status panel sees a stable capacity, not our shrinking free count.
        let held = inflight.lock().expect("inflight poisoned").len() as u32;
        if held < cfg.capacity {
            match claim(&client, &cfg).await {
                Ok(assignments) => {
                    for assignment in assignments {
                        let token = CancellationToken::new();
                        inflight
                            .lock()
                            .expect("inflight poisoned")
                            .insert(assignment.run_id, token.clone());
                        let (engine, db, client, cfg, inflight) = (
                            Arc::clone(&engine),
                            db.clone(),
                            client.clone(),
                            cfg.clone(),
                            Arc::clone(&inflight),
                        );
                        tokio::spawn(async move {
                            let run_id = assignment.run_id;
                            run_assignment(&engine, &db, &client, &cfg, assignment, token).await;
                            inflight.lock().expect("inflight poisoned").remove(&run_id);
                        });
                    }
                }
                Err(e) => warn!(error = %e, "claim failed; will retry"),
            }
        }

        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("worker shutting down");
                return Ok(());
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// Execute one assigned run and stream its updates back.
async fn run_assignment(
    engine: &Arc<Engine>,
    db: &Db,
    client: &reqwest::Client,
    cfg: &WorkerConfig,
    assignment: Assignment,
    token: CancellationToken,
) {
    let server_run_id = assignment.run_id;
    debug!(server_run_id, flow = %assignment.flow_id, "executing assigned run");

    // Seed the scratch DB so the engine's definition lookup finds the flow,
    // then create a local run row to execute against.
    if let Err(e) = db.seed_flow_revision(
        &assignment.flow_id,
        assignment.flow_rev,
        &assignment.flow_id,
        &assignment.definition,
    ) {
        warn!(server_run_id, error = %e, "failed to seed scratch flow");
        return;
    }
    let local_run_id = match db.insert_run(
        &assignment.flow_id,
        assignment.flow_rev,
        &assignment.trigger,
        &assignment.inputs,
        &assignment.queue,
        None,
    ) {
        Ok(id) => id,
        Err(e) => {
            warn!(server_run_id, error = %e, "failed to create scratch run");
            return;
        }
    };
    let run = match db.get_run(local_run_id) {
        Ok(Some(run)) => run,
        _ => return,
    };

    let (utx, urx) = mpsc::unbounded_channel::<RunUpdate>();
    let (btx, _brx) = tokio::sync::broadcast::channel(1024);
    let sink = Arc::new(RemoteSink::new(LocalSink::new(db.clone(), btx), utx));

    // Reporter drains updates to the server; it exits when the sink is
    // dropped (execute_to_sink consumes the only Arc below).
    let reporter = tokio::spawn(report_loop(
        client.clone(),
        cfg.clone(),
        server_run_id,
        urx,
        token.clone(),
    ));

    engine.execute_to_sink(run, token, sink).await;
    let _ = reporter.await;
    debug!(server_run_id, "assigned run finished");
}

/// Drain the update channel, POSTing batches to the server and honoring any
/// cancellation it reports.
async fn report_loop(
    client: reqwest::Client,
    cfg: WorkerConfig,
    server_run_id: i64,
    mut urx: mpsc::UnboundedReceiver<RunUpdate>,
    token: CancellationToken,
) {
    let url = format!("{}/api/worker/updates", cfg.server_url);
    let mut seq: i64 = 0;
    while let Some(first) = urx.recv().await {
        let mut batch = Vec::new();
        seq += 1;
        batch.push(SeqUpdate {
            seq,
            update: first,
        });
        while let Ok(update) = urx.try_recv() {
            seq += 1;
            batch.push(SeqUpdate { seq, update });
        }
        let body = json!({
            "worker_id": cfg.worker_id,
            "run_id": server_run_id,
            "updates": batch,
        });
        match client
            .post(&url)
            .bearer_auth(&cfg.token)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(parsed) = resp.json::<UpdatesResponse>().await
                    && parsed.canceled
                {
                    token.cancel();
                }
            }
            Err(e) => warn!(server_run_id, error = %e, "failed to post updates"),
        }
    }
}

/// Claim runs off the server, declaring this worker's total capacity (the
/// server leases only the slots we don't already hold).
async fn claim(
    client: &reqwest::Client,
    cfg: &WorkerConfig,
) -> Result<Vec<Assignment>, reqwest::Error> {
    let url = format!("{}/api/worker/claim", cfg.server_url);
    let body = json!({
        "worker_id": cfg.worker_id,
        "queues": cfg.queues,
        "capacity": cfg.capacity,
    });
    let resp: ClaimResponse = client
        .post(&url)
        .bearer_auth(&cfg.token)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.assignments)
}

/// Heartbeat all in-flight runs; cancel any the server flags.
async fn heartbeat(client: &reqwest::Client, cfg: &WorkerConfig, inflight: &Inflight) {
    let ids: Vec<i64> = inflight
        .lock()
        .expect("inflight poisoned")
        .keys()
        .copied()
        .collect();
    if ids.is_empty() {
        return;
    }
    let url = format!("{}/api/worker/heartbeat", cfg.server_url);
    let body = json!({ "worker_id": cfg.worker_id, "run_ids": ids });
    match client
        .post(&url)
        .bearer_auth(&cfg.token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(parsed) = resp.json::<HeartbeatResponse>().await {
                let guard = inflight.lock().expect("inflight poisoned");
                for id in parsed.canceled {
                    if let Some(token) = guard.get(&id) {
                        token.cancel();
                    }
                }
            }
        }
        Err(e) => warn!(error = %e, "heartbeat failed"),
    }
}

#[derive(Deserialize)]
struct ClaimResponse {
    assignments: Vec<Assignment>,
}

#[derive(Deserialize)]
struct UpdatesResponse {
    canceled: bool,
}

#[derive(Deserialize)]
struct HeartbeatResponse {
    canceled: Vec<i64>,
}
