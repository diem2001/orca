//! Instance activity and idle stop.
//!
//! An instance counts as used when its URLs get requests through the gateway
//! or when someone runs Orca commands for it (UI, CLI, MCP). Projects that
//! set `idle_stop` in orca.yaml have instances stopped after that long
//! without use; nothing is deleted and a start brings them back as they were.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use tokio::sync::Mutex;

use super::{deploy, jobs, project::ProjectCtx};
use crate::state::AppState;

type Map = Mutex<HashMap<String, DateTime<Utc>>>;

fn map() -> &'static Map {
    static M: OnceLock<Map> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn started() -> DateTime<Utc> {
    static S: OnceLock<DateTime<Utc>> = OnceLock::new();
    *S.get_or_init(Utc::now)
}

/// Record that an instance was used now.
pub async fn touch(project: &str, instance: &str) {
    map().lock().await.insert(format!("{project}/{instance}"), Utc::now());
}

pub async fn last_seen(project: &str, instance: &str) -> Option<DateTime<Utc>> {
    map().lock().await.get(&format!("{project}/{instance}")).copied()
}

/// Start the background loops. Call once at daemon startup.
pub fn spawn(state: Arc<AppState>) {
    let _ = started();
    // Fast loop: wake sleeping instances when their URL is requested.
    let wake_state = state.clone();
    tokio::spawn(async move {
        let mut since = Utc::now().timestamp();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let now = Utc::now().timestamp();
            if let Err(e) = wake_requested(&wake_state, since).await {
                tracing::debug!("wake on request: {e:#}");
            }
            since = now;
        }
    });
    tokio::spawn(async move {
        let mut since = Utc::now().timestamp();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let now = Utc::now().timestamp();
            if let Err(e) = scan_gateway(&state, since).await {
                tracing::debug!("gateway activity scan: {e:#}");
            }
            since = now;
            if let Err(e) = stop_idle(&state).await {
                tracing::warn!("idle stop: {e:#}");
            }
        }
    });
}

/// Count requests per instance from the gateway's access log since `since`.
async fn scan_gateway(state: &AppState, since: i64) -> anyhow::Result<()> {
    if !crate::gateway::is_running(state).await.unwrap_or(false) {
        return Ok(());
    }
    let hosts: HashMap<String, (String, String)> = {
        let cfg = state.config.lock().await;
        cfg.gateway
            .routes
            .iter()
            .filter_map(|r| {
                let owner = r.owner.as_deref()?.strip_prefix("dev:")?;
                // Host-process routes are owned by "dev:<p>:<i>#host".
                let owner = owner.split('#').next().unwrap_or(owner);
                let (p, i) = owner.split_once(':')?;
                Some((r.hostname.clone(), (p.to_string(), i.to_string())))
            })
            .collect()
    };
    if hosts.is_empty() {
        return Ok(());
    }
    let seen = requested_hosts(state, since, &hosts).await?;
    for (p, i) in seen {
        touch(&p, &i).await;
    }
    Ok(())
}

/// Start sleeping instances whose URL was requested since `since`.
async fn wake_requested(state: &Arc<AppState>, since: i64) -> anyhow::Result<()> {
    let sleeping: HashMap<String, (String, String)> = {
        let cfg = state.config.lock().await;
        cfg.gateway
            .routes
            .iter()
            .filter(|r| r.sleeping)
            .filter_map(|r| {
                let owner = r.owner.as_deref()?.strip_prefix("dev:")?;
                let (p, i) = owner.split_once(':')?;
                Some((r.hostname.clone(), (p.to_string(), i.to_string())))
            })
            .collect()
    };
    if sleeping.is_empty() || !crate::gateway::is_running(state).await.unwrap_or(false) {
        return Ok(());
    }
    for (project, instance) in requested_hosts(state, since, &sleeping).await? {
        if jobs::running_for(&project, &instance).await.is_some() {
            continue;
        }
        tracing::info!("waking {project}/{instance}: its URL was requested");
        touch(&project, &instance).await;
        let opts = deploy::UpOptions {
            build: Some(false),
            source: Some("wake".into()),
            ..Default::default()
        };
        if let Err(e) = deploy::up(state.clone(), project.clone(), instance.clone(), opts).await {
            tracing::warn!("could not wake {project}/{instance}: {e:#}");
        }
    }
    Ok(())
}

/// Instances (from `hosts`) with gateway requests since `since`.
async fn requested_hosts(
    state: &AppState,
    since: i64,
    hosts: &HashMap<String, (String, String)>,
) -> anyhow::Result<std::collections::HashSet<(String, String)>> {
    let rt = state.rt().await;
    let mut stream = rt.docker.logs(
        "orca-gateway",
        Some(bollard::container::LogsOptions::<String> {
            stdout: true,
            stderr: true,
            since,
            ..Default::default()
        }),
    );
    let mut seen = std::collections::HashSet::new();
    while let Some(Ok(chunk)) = stream.next().await {
        let line = chunk.to_string();
        if !line.contains("handled request") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(host) = v.pointer("/request/host").and_then(|h| h.as_str()) else {
            continue;
        };
        let host = host.split(':').next().unwrap_or(host);
        if let Some(owner) = hosts.get(host) {
            seen.insert(owner.clone());
        }
    }
    Ok(seen)
}

async fn stop_idle(state: &Arc<AppState>) -> anyhow::Result<()> {
    let ids: Vec<String> = state
        .config
        .lock()
        .await
        .dev_projects
        .iter()
        .map(|p| p.id.clone())
        .collect();
    for id in ids {
        let Ok(ctx) = ProjectCtx::load(state, &id).await else {
            continue;
        };
        let Some(idle) = ctx.dev.instance.idle_stop_duration().ok().flatten() else {
            continue;
        };
        let view = super::project::build_view(state, &ctx, false).await;
        for inst in view
            .instances
            .iter()
            .filter(|i| i.status == "running" || i.status == "partial")
        {
            if jobs::running_for(&id, &inst.name).await.is_some() {
                continue;
            }
            let started_at = inst
                .last_started
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            let last = [last_seen(&id, &inst.name).await, started_at, Some(started())]
                .into_iter()
                .flatten()
                .max()
                .unwrap_or_else(Utc::now);
            let quiet = Utc::now() - last;
            if quiet.to_std().unwrap_or_default() < idle {
                continue;
            }
            tracing::info!(
                "stopping idle instance {id}/{} (no use for {} min)",
                inst.name,
                quiet.num_minutes()
            );
            {
                let mut cfg = state.config.lock().await;
                if let Some(p) = cfg.dev_projects.iter_mut().find(|p| p.id == id)
                    && let Some(s) = p.instance_by_path_mut(&inst.path)
                {
                    s.idle_stopped_at = Some(Utc::now().to_rfc3339());
                }
                let _ = cfg.save();
            }
            deploy::down(state.clone(), id.clone(), inst.name.clone(), deploy::DownMode::Sleep).await?;
        }
    }
    Ok(())
}
