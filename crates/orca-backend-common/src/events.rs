//! Bridge Docker/Podman events to Orca events.

use bollard::models::EventMessageTypeEnum;
use bollard::system::EventsOptions;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;

use orca_core::event::{Event, EventKind};

use crate::BollardRuntime;

/// Container actions that don't change what `GET /containers/json` returns.
/// `exec_*` matter most: a healthcheck runs one every few seconds.
fn changes_container_list(action: &str) -> bool {
    !(action.starts_with("exec_")
        || matches!(
            action,
            "attach" | "detach" | "resize" | "top" | "archive-path" | "extract-to-dir" | "export" | "copy" | "commit"
        ))
}

impl BollardRuntime {
    /// Docker events as Orca events. All subscribers share one event stream,
    /// started on the first call and ended when this runtime is dropped. The
    /// stream also drops the runtime's cached lists when Docker reports a
    /// change, before passing the event on, so a subscriber that refetches on
    /// an event sees the change.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        let rx = self.events.subscribe();
        self.events_started.call_once(|| self.spawn_event_stream());
        rx
    }

    fn spawn_event_stream(&self) {
        let tx = self.events.clone();
        let docker = self.docker.clone();
        let containers = self.containers.clone();
        let images = self.images.clone();
        let disk = self.disk.clone();
        let mut alive = self.alive.subscribe();
        tokio::spawn(async move {
            // Reconnect with exponential backoff. Without this, a single
            // transient error ended the event stream forever and the UI
            // stopped showing live updates until the user restarted the
            // daemon.
            let mut backoff_secs: u64 = 1;
            loop {
                // Changes made while no stream was open went unreported.
                containers.forget();
                images.forget();
                let options = EventsOptions::<String> { ..Default::default() };
                let inspect_docker = docker.clone();
                let mut stream = docker.events(Some(options));
                let mut had_successful_read = false;

                loop {
                    let item = tokio::select! {
                        item = stream.next() => item,
                        // The runtime was dropped (replaced by a reconnect).
                        _ = alive.changed() => return,
                    };
                    let Some(item) = item else { break };
                    match item {
                        Ok(event) => {
                            had_successful_read = true;
                            let typ = event.typ;
                            let action = event.action.as_deref().unwrap_or_default();
                            if typ == Some(EventMessageTypeEnum::CONTAINER) && changes_container_list(action) {
                                containers.forget();
                                if action == "destroy" {
                                    disk.forget();
                                }
                            } else if typ == Some(EventMessageTypeEnum::IMAGE) && !matches!(action, "save" | "push") {
                                images.forget();
                                if matches!(action, "delete" | "untag") {
                                    disk.forget();
                                }
                            } else if typ == Some(EventMessageTypeEnum::VOLUME)
                                && matches!(action, "create" | "destroy")
                            {
                                disk.forget();
                            }
                            // The type's Debug form is upper case (`CONTAINER`), so
                            // compare the enum, not its name.
                            let kind = match (typ, event.action.as_deref()) {
                                (Some(EventMessageTypeEnum::CONTAINER), Some(action)) => {
                                    let id = event.actor.as_ref().and_then(|a| a.id.clone()).unwrap_or_default();
                                    let name = event
                                        .actor
                                        .as_ref()
                                        .and_then(|a| a.attributes.as_ref())
                                        .and_then(|attrs| attrs.get("name").cloned())
                                        .unwrap_or_default();

                                    match action {
                                        "create" => Some(EventKind::ContainerCreated { id, name }),
                                        "start" => Some(EventKind::ContainerStarted { id, name }),
                                        "stop" => Some(EventKind::ContainerStopped { id, name }),
                                        "destroy" => Some(EventKind::ContainerRemoved { id, name }),
                                        "die" => {
                                            let exit_code = event
                                                .actor
                                                .as_ref()
                                                .and_then(|a| a.attributes.as_ref())
                                                .and_then(|attrs| attrs.get("exitCode"))
                                                .and_then(|c| c.parse().ok())
                                                .unwrap_or(-1);
                                            let oom_killed = if id.is_empty() {
                                                false
                                            } else {
                                                inspect_docker
                                                    .inspect_container(&id, None)
                                                    .await
                                                    .ok()
                                                    .and_then(|info| info.state.and_then(|s| s.oom_killed))
                                                    .unwrap_or(false)
                                            };
                                            Some(EventKind::ContainerDied {
                                                id,
                                                name,
                                                exit_code,
                                                oom_killed,
                                            })
                                        }
                                        _ => None,
                                    }
                                }
                                (Some(EventMessageTypeEnum::IMAGE), Some(action)) => {
                                    let id = event.actor.as_ref().and_then(|a| a.id.clone()).unwrap_or_default();

                                    match action {
                                        "pull" => Some(EventKind::ImagePulled {
                                            id: id.clone(),
                                            reference: id,
                                        }),
                                        "delete" | "untag" => Some(EventKind::ImageRemoved { id }),
                                        _ => None,
                                    }
                                }
                                (Some(EventMessageTypeEnum::VOLUME), Some(action)) => {
                                    let name = event.actor.as_ref().and_then(|a| a.id.clone()).unwrap_or_default();

                                    match action {
                                        "create" => Some(EventKind::VolumeCreated { name }),
                                        "destroy" => Some(EventKind::VolumeRemoved { name }),
                                        _ => None,
                                    }
                                }
                                _ => None,
                            };

                            if let Some(kind) = kind {
                                let event = Event {
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                    kind,
                                };
                                // Ignore send errors (no subscribers)
                                let _ = tx.send(event);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Docker event stream error, will reconnect: {e}");
                            break;
                        }
                    }
                }

                // Reset backoff after a run with at least one good read;
                // otherwise grow it so we don't spin on a persistent failure
                // (e.g., Docker daemon down).
                if had_successful_read {
                    backoff_secs = 1;
                } else {
                    backoff_secs = (backoff_secs * 2).min(60);
                }
                tracing::info!("Docker event stream reconnecting in {backoff_secs}s");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
                    _ = alive.changed() => return,
                }
            }
        });
    }
}
