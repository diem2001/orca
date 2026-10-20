//! Shared bollard-based container runtime logic.
//!
//! All Orca backends (native, macOS, Windows) talk to Docker/Podman
//! through the same Docker-compatible API. This crate provides the
//! shared implementation so each backend only needs to handle
//! platform-specific VM/machine management.

pub mod compose;
pub mod containers;
pub mod environment;
pub mod events;
pub mod export;
pub mod images;
pub mod k8s;
pub mod networks;
pub mod search;
pub mod templates;
pub mod volumes;

/// How long the engine version is reused before asking the engine again.
const VERSION_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// How long `GET /system/df` is reused. It walks every image, container,
/// volume and build-cache record and costs dockerd and containerd several
/// CPU seconds on a busy engine, while the health it feeds is polled every
/// few seconds.
const DISK_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Shared container runtime client backed by bollard.
/// Each platform backend embeds this and delegates trait impls to it.
pub struct BollardRuntime {
    pub docker: bollard::Docker,
    /// Engine address (`unix:///…/docker.sock`) when connected over a socket.
    /// Child `docker`/`podman` CLI processes get it as `DOCKER_HOST`, so they
    /// reach the same engine as the API client whatever the CLI context says.
    pub host: Option<String>,
    version: tokio::sync::Mutex<Option<(std::time::Instant, bollard::system::Version)>>,
    disk: TtlCache<orca_core::environment::DiskUsage>,
}

impl BollardRuntime {
    pub fn new(docker: bollard::Docker) -> Self {
        Self {
            docker,
            host: None,
            version: tokio::sync::Mutex::new(None),
            disk: TtlCache::new(DISK_TTL),
        }
    }

    /// Record the unix socket this runtime is connected through.
    pub fn with_socket(mut self, path: impl AsRef<std::path::Path>) -> Self {
        self.host = Some(format!("unix://{}", path.as_ref().display()));
        self
    }

    /// The engine's `GET /version`, cached for [`VERSION_TTL`]. A reconnect
    /// builds a new runtime, so it starts fresh.
    ///
    /// Don't call `docker.version()` directly: rootless dockerd (29.x, as in
    /// Orca's Lima VM) leaks a connection to rootlesskit's API socket on every
    /// `/version`, about 50 KB each, until the VM's OOM killer takes dockerd.
    /// To check that the engine is reachable, use `docker.ping()`.
    pub async fn server_version(&self) -> Result<bollard::system::Version, bollard::errors::Error> {
        let mut cached = self.version.lock().await;
        if let Some((at, v)) = cached.as_ref()
            && at.elapsed() < VERSION_TTL
        {
            return Ok(v.clone());
        }
        let v = self.docker.version().await?;
        *cached = Some((std::time::Instant::now(), v.clone()));
        Ok(v)
    }

    /// Drop the cached version, e.g. after the engine was updated.
    pub async fn forget_version(&self) {
        *self.version.lock().await = None;
    }

    /// The engine's disk usage (`GET /system/df`), cached for [`DISK_TTL`].
    /// `force` recomputes it. Concurrent callers share one computation, so
    /// several pollers or a burst of refreshes cost the engine one walk.
    pub async fn disk_usage(&self, force: bool) -> Result<orca_core::environment::DiskUsage, bollard::errors::Error> {
        self.disk
            .get(force, || async {
                let df = self.docker.df().await?;
                Ok(environment::summarize_disk_usage(&df))
            })
            .await
    }

    /// Drop the cached disk usage, e.g. after images or volumes were removed.
    pub async fn forget_disk(&self) {
        self.disk.forget().await;
    }

    /// Detect whether we're talking to Docker or Podman.
    pub async fn detect_runtime(&self) -> orca_core::runtime::RuntimeKind {
        if let Ok(version) = self.server_version().await
            && let Some(components) = version.components
        {
            for c in &components {
                if c.name.to_lowercase().contains("podman") {
                    return orca_core::runtime::RuntimeKind::Podman;
                }
            }
        }
        orca_core::runtime::RuntimeKind::Docker
    }
}

/// A value that is reused for `ttl` and computed by one caller at a time.
///
/// The lock is held across the computation: callers that arrive meanwhile
/// wait for it and take its result instead of starting their own, including
/// forced ones, which only recompute a value older than their own request.
struct TtlCache<T> {
    ttl: std::time::Duration,
    slot: tokio::sync::Mutex<Option<(std::time::Instant, T)>>,
}

impl<T: Clone> TtlCache<T> {
    fn new(ttl: std::time::Duration) -> Self {
        Self {
            ttl,
            slot: tokio::sync::Mutex::new(None),
        }
    }

    async fn get<F, Fut, E>(&self, force: bool, compute: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let asked = std::time::Instant::now();
        let mut slot = self.slot.lock().await;
        if let Some((at, v)) = slot.as_ref() {
            let fresh = if force { *at >= asked } else { at.elapsed() < self.ttl };
            if fresh {
                return Ok(v.clone());
            }
        }
        let v = compute().await?;
        *slot = Some((std::time::Instant::now(), v.clone()));
        Ok(v)
    }

    async fn forget(&self) {
        *self.slot.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::TtlCache;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn counting(calls: &Arc<AtomicUsize>) -> impl std::future::Future<Output = Result<usize, ()>> + use<> {
        let calls = calls.clone();
        async move { Ok(calls.fetch_add(1, Ordering::SeqCst) + 1) }
    }

    #[tokio::test]
    async fn reuses_the_value_within_the_ttl() {
        let cache = TtlCache::new(Duration::from_secs(600));
        let calls = Arc::new(AtomicUsize::new(0));
        assert_eq!(cache.get(false, || counting(&calls)).await, Ok(1));
        assert_eq!(cache.get(false, || counting(&calls)).await, Ok(1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn recomputes_after_the_ttl() {
        let cache = TtlCache::new(Duration::from_millis(20));
        let calls = Arc::new(AtomicUsize::new(0));
        cache.get(false, || counting(&calls)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.get(false, || counting(&calls)).await, Ok(2));
    }

    #[tokio::test]
    async fn force_recomputes() {
        let cache = TtlCache::new(Duration::from_secs(600));
        let calls = Arc::new(AtomicUsize::new(0));
        cache.get(false, || counting(&calls)).await.unwrap();
        assert_eq!(cache.get(true, || counting(&calls)).await, Ok(2));
    }

    #[tokio::test]
    async fn forget_invalidates() {
        let cache = TtlCache::new(Duration::from_secs(600));
        let calls = Arc::new(AtomicUsize::new(0));
        cache.get(false, || counting(&calls)).await.unwrap();
        cache.forget().await;
        assert_eq!(cache.get(false, || counting(&calls)).await, Ok(2));
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        let cache = TtlCache::new(Duration::from_secs(600));
        assert_eq!(
            cache.get(false, || async { Err::<usize, _>("down") }).await,
            Err("down")
        );
        assert_eq!(cache.get(false, || async { Ok::<_, &str>(7) }).await, Ok(7));
    }

    /// Concurrent callers, forced or not, share one computation.
    #[tokio::test]
    async fn concurrent_calls_compute_once() {
        let cache = Arc::new(TtlCache::new(Duration::from_secs(600)));
        let calls = Arc::new(AtomicUsize::new(0));
        let slow = |calls: Arc<AtomicUsize>| async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, ()>(calls.fetch_add(1, Ordering::SeqCst) + 1)
        };
        let (a, b, c) = tokio::join!(
            cache.get(false, || slow(calls.clone())),
            cache.get(true, || slow(calls.clone())),
            cache.get(true, || slow(calls.clone())),
        );
        assert_eq!((a, b, c), (Ok(1), Ok(1), Ok(1)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
