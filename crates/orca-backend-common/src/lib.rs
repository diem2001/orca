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

/// Shared container runtime client backed by bollard.
/// Each platform backend embeds this and delegates trait impls to it.
pub struct BollardRuntime {
    pub docker: bollard::Docker,
    /// Engine address (`unix:///…/docker.sock`) when connected over a socket.
    /// Child `docker`/`podman` CLI processes get it as `DOCKER_HOST`, so they
    /// reach the same engine as the API client whatever the CLI context says.
    pub host: Option<String>,
}

impl BollardRuntime {
    pub fn new(docker: bollard::Docker) -> Self {
        Self { docker, host: None }
    }

    /// Record the unix socket this runtime is connected through.
    pub fn with_socket(mut self, path: impl AsRef<std::path::Path>) -> Self {
        self.host = Some(format!("unix://{}", path.as_ref().display()));
        self
    }

    /// Detect whether we're talking to Docker or Podman.
    pub async fn detect_runtime(&self) -> orca_core::runtime::RuntimeKind {
        if let Ok(version) = self.docker.version().await
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
