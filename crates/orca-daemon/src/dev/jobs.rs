//! Background jobs for instance operations, with a step list the UI and CLI
//! render as progress ("Start shared services ✓, Build and start …").

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use tokio::sync::Mutex;

/// Keep at most this many finished jobs around for inspection.
const MAX_JOBS: usize = 50;
/// Cap log lines per job so a chatty build can't grow memory without bound.
const MAX_LOG_LINES: usize = 4000;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub key: &'static str,
    pub label: &'static str,
    pub status: StepStatus,
    /// One line of result or error, shown under the step.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub project: String,
    pub instance: Option<String>,
    pub action: String,
    pub status: JobStatus,
    pub steps: Vec<Step>,
    pub log: Vec<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Arbitrary result for the caller (URLs, env path …).
    pub result: Option<serde_json::Value>,
}

type Registry = Mutex<Vec<Arc<Mutex<Job>>>>;

fn registry() -> &'static Registry {
    static JOBS: OnceLock<Registry> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Handle a running operation uses to report progress.
#[derive(Clone)]
pub struct JobHandle(Arc<Mutex<Job>>);

impl JobHandle {
    pub async fn create(
        project: &str,
        instance: Option<&str>,
        action: &str,
        steps: &[(&'static str, &'static str)],
    ) -> Self {
        let id = format!("{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff);
        let job = Job {
            id,
            project: project.to_string(),
            instance: instance.map(str::to_string),
            action: action.to_string(),
            status: JobStatus::Running,
            steps: steps
                .iter()
                .map(|(key, label)| Step {
                    key,
                    label,
                    status: StepStatus::Pending,
                    detail: None,
                })
                .collect(),
            log: Vec::new(),
            error: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            result: None,
        };
        let handle = Arc::new(Mutex::new(job));
        let mut reg = registry().lock().await;
        reg.push(handle.clone());
        // Drop the oldest finished jobs beyond the cap.
        while reg.len() > MAX_JOBS {
            let Some(pos) = futures::future::join_all(reg.iter().map(|j| async { j.lock().await.status }))
                .await
                .iter()
                .position(|s| *s != JobStatus::Running)
            else {
                break;
            };
            reg.remove(pos);
        }
        JobHandle(handle)
    }

    pub async fn snapshot(&self) -> Job {
        self.0.lock().await.clone()
    }

    pub async fn start(&self, key: &str) {
        self.set(key, StepStatus::Running, None).await;
    }

    pub async fn done(&self, key: &str, detail: Option<String>) {
        self.set(key, StepStatus::Done, detail).await;
    }

    pub async fn skip(&self, key: &str, detail: &str) {
        self.set(key, StepStatus::Skipped, Some(detail.to_string())).await;
    }

    async fn set(&self, key: &str, status: StepStatus, detail: Option<String>) {
        let mut job = self.0.lock().await;
        if let Some(step) = job.steps.iter_mut().find(|s| s.key == key) {
            step.status = status;
            if detail.is_some() {
                step.detail = detail;
            }
        }
    }

    pub async fn log(&self, line: impl Into<String>) {
        let mut job = self.0.lock().await;
        job.log.push(line.into());
        if job.log.len() > MAX_LOG_LINES {
            let excess = job.log.len() - MAX_LOG_LINES;
            job.log.drain(..excess);
        }
    }

    /// Mark the running step failed and the job finished with an error.
    pub async fn fail(&self, err: &anyhow::Error) {
        let msg = format!("{err:#}");
        let mut job = self.0.lock().await;
        if let Some(step) = job.steps.iter_mut().find(|s| s.status == StepStatus::Running) {
            step.status = StepStatus::Failed;
            step.detail = Some(msg.clone());
        }
        for step in job.steps.iter_mut().filter(|s| s.status == StepStatus::Pending) {
            step.status = StepStatus::Skipped;
        }
        job.status = JobStatus::Failed;
        job.error = Some(msg);
        job.finished_at = Some(chrono::Utc::now().to_rfc3339());
    }

    pub async fn succeed(&self, result: Option<serde_json::Value>) {
        let mut job = self.0.lock().await;
        job.status = JobStatus::Succeeded;
        job.result = result;
        job.finished_at = Some(chrono::Utc::now().to_rfc3339());
    }
}

pub async fn get(id: &str) -> Option<Job> {
    let reg = registry().lock().await;
    for j in reg.iter() {
        let job = j.lock().await;
        if job.id == id {
            return Some(job.clone());
        }
    }
    None
}

/// Jobs for a project, newest first, without their logs.
pub async fn list(project: Option<&str>) -> Vec<Job> {
    let reg = registry().lock().await;
    let mut out = Vec::new();
    for j in reg.iter().rev() {
        let job = j.lock().await;
        if project.is_none_or(|p| job.project == p) {
            let mut copy = job.clone();
            copy.log.clear();
            out.push(copy);
        }
    }
    out
}

/// The running job for an instance, if any.
pub async fn running_for(project: &str, instance: &str) -> Option<String> {
    let reg = registry().lock().await;
    for j in reg.iter().rev() {
        let job = j.lock().await;
        if job.status == JobStatus::Running && job.project == project && job.instance.as_deref() == Some(instance) {
            return Some(job.id.clone());
        }
    }
    None
}

/// Per-key async locks (per project, per instance).
pub struct Locks(Mutex<HashMap<String, Arc<Mutex<()>>>>);

impl Locks {
    pub fn get() -> &'static Locks {
        static LOCKS: OnceLock<Locks> = OnceLock::new();
        LOCKS.get_or_init(|| Locks(Mutex::new(HashMap::new())))
    }

    pub async fn lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let m = {
            let mut map = self.0.lock().await;
            map.entry(key.to_string()).or_default().clone()
        };
        m.lock_owned().await
    }
}
