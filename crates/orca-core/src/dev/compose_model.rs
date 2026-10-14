//! The resolved compose model, as printed by `docker compose config --format json`.
//!
//! Only the fields Orca reasons about are typed; everything else is ignored.
//! Compose normalizes short syntax (ports, depends_on lists) to long syntax
//! in this output, so there is one shape to handle.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ComposeModel {
    pub name: String,
    pub services: BTreeMap<String, Service>,
    pub networks: BTreeMap<String, Network>,
    pub volumes: BTreeMap<String, Volume>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Service {
    pub image: Option<String>,
    pub build: Option<serde_json::Value>,
    pub container_name: Option<String>,
    pub ports: Vec<Port>,
    pub depends_on: BTreeMap<String, DependsOn>,
    pub networks: BTreeMap<String, Option<serde_json::Value>>,
    pub network_mode: Option<String>,
    pub volumes_from: Vec<String>,
    pub links: Vec<String>,
    pub environment: BTreeMap<String, Option<String>>,
    pub healthcheck: Option<serde_json::Value>,
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Port {
    pub target: u16,
    /// Compose prints this as a string ("8080", or a range "8000-8010").
    pub published: Option<String>,
    pub host_ip: Option<String>,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DependsOn {
    pub condition: Option<String>,
    pub required: Option<bool>,
    pub restart: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    pub name: Option<String>,
    pub external: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Volume {
    pub name: Option<String>,
    pub external: Option<bool>,
}

impl ComposeModel {
    pub fn parse(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("could not read `docker compose config` output: {e}"))
    }
}

impl Service {
    /// Built from source in the repo (so each instance needs its own image).
    pub fn is_built(&self) -> bool {
        self.build.is_some()
    }

    /// The service another service's `network_mode: service:x` points at.
    pub fn network_mode_service(&self) -> Option<&str> {
        self.network_mode.as_deref()?.strip_prefix("service:")
    }

    /// Services named in `volumes_from` (`svc` or `svc:ro`, not `container:x`).
    pub fn volumes_from_services(&self) -> Vec<&str> {
        self.volumes_from
            .iter()
            .filter(|v| !v.starts_with("container:"))
            .map(|v| v.split(':').next().unwrap_or(v))
            .collect()
    }

    /// Services named in legacy `links` (`svc` or `svc:alias`).
    pub fn link_services(&self) -> Vec<&str> {
        self.links.iter().map(|l| l.split(':').next().unwrap_or(l)).collect()
    }
}

/// Container ports that usually speak something other than HTTP. Used to
/// decide between "open in browser" and "port on my machine" when
/// `orca.yaml` doesn't say.
pub const NON_HTTP_PORTS: &[u16] = &[
    22, 25, 53, 389, 465, 587, 1025, 1433, 1521, 2181, 3306, 4222, 5432, 5672, 6379, 6380, 7000, 7001, 7687, 8125,
    9000, 9042, 9092, 9093, 9200, 9229, 9300, 11211, 26257, 27017, 27018, 50051,
];

/// Image names that are usually infrastructure, used to pre-sort services
/// into shared or per-instance when `orca.yaml` has no `dev:` section.
const INFRA_IMAGES: &[&str] = &[
    "postgres",
    "postgis",
    "mysql",
    "mariadb",
    "mongo",
    "redis",
    "valkey",
    "keydb",
    "memcached",
    "rabbitmq",
    "kafka",
    "zookeeper",
    "redpanda",
    "nats",
    "elasticsearch",
    "opensearch",
    "minio",
    "mailpit",
    "mailhog",
    "localstack",
    "clickhouse",
    "cassandra",
    "neo4j",
    "cockroach",
    "timescaledb",
    "meilisearch",
    "typesense",
    "qdrant",
    "jaeger",
    "otel-collector",
    "keycloak",
];

/// Why a service was suggested as shared or per-instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Suggestion {
    pub service: String,
    pub shared: bool,
    pub reason: String,
}

/// Pre-sort services: infrastructure images that are not built from the
/// repo are suggested as shared, everything else per instance.
pub fn suggest_shared(model: &ComposeModel) -> Vec<Suggestion> {
    model
        .services
        .iter()
        .map(|(name, svc)| {
            if svc.is_built() {
                return Suggestion {
                    service: name.clone(),
                    shared: false,
                    reason: "built from this repo, so it runs per worktree".to_string(),
                };
            }
            let image = svc.image.as_deref().unwrap_or_default();
            // `docker.io/library/postgres:16` → `postgres`
            let base = image.rsplit('/').next().unwrap_or(image);
            let base = base.split([':', '@']).next().unwrap_or(base);
            match INFRA_IMAGES.iter().find(|i| base.contains(*i)) {
                Some(kind) => Suggestion {
                    service: name.clone(),
                    shared: true,
                    reason: format!("{kind} image, usually shared between worktrees"),
                },
                None => Suggestion {
                    service: name.clone(),
                    shared: false,
                    reason: "not a known infrastructure image, so it runs per worktree".to_string(),
                },
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Trimmed real output of `docker compose -p myapp-feat config --format json`.
    pub const SAMPLE: &str = r#"{
      "name": "myapp-feat",
      "networks": {"default": {"name": "myapp-feat_default", "ipam": {}}},
      "services": {
        "api": {
          "build": {"context": "/src/api", "dockerfile": "Dockerfile"},
          "container_name": "myapp-api",
          "depends_on": {
            "postgres": {"condition": "service_healthy", "required": true},
            "worker": {"condition": "service_started", "required": true}
          },
          "environment": {"DATABASE_URL": "postgres://app:app@postgres:5432/myapp"},
          "image": "myapp/api:dev",
          "networks": {"default": null},
          "ports": [
            {"mode": "ingress", "target": 8080, "published": "8080", "protocol": "tcp"},
            {"mode": "ingress", "target": 9229, "published": "9229", "protocol": "tcp"}
          ]
        },
        "postgres": {
          "environment": {"POSTGRES_PASSWORD": "app"},
          "healthcheck": {"test": ["CMD", "pg_isready"], "interval": "2s"},
          "image": "postgres:16",
          "networks": {"default": null},
          "ports": [{"mode": "ingress", "target": 5432, "published": "5432", "protocol": "tcp"}],
          "volumes": [{"type": "volume", "source": "pgdata", "target": "/var/lib/postgresql/data"}]
        },
        "redis": {"image": "redis:7", "networks": {"default": null}},
        "web": {
          "image": "nginx:alpine",
          "networks": {"default": null},
          "ports": [{"mode": "ingress", "target": 80, "published": "3000", "protocol": "tcp"}]
        },
        "worker": {
          "command": ["sleep", "infinity"],
          "depends_on": {"redis": {"condition": "service_started", "required": true}},
          "image": "busybox",
          "networks": {"default": null}
        }
      },
      "volumes": {"pgdata": {"name": "myapp-feat_pgdata"}}
    }"#;

    #[test]
    fn parses_real_output() {
        let m = ComposeModel::parse(SAMPLE).unwrap();
        assert_eq!(m.services.len(), 5);
        let api = &m.services["api"];
        assert!(api.is_built());
        assert_eq!(api.ports[1].target, 9229);
        assert_eq!(api.ports[0].published.as_deref(), Some("8080"));
        assert_eq!(api.depends_on["postgres"].condition.as_deref(), Some("service_healthy"));
        assert_eq!(m.volumes["pgdata"].name.as_deref(), Some("myapp-feat_pgdata"));
    }

    #[test]
    fn suggests_infra_as_shared() {
        let m = ComposeModel::parse(SAMPLE).unwrap();
        let s: BTreeMap<String, bool> = suggest_shared(&m).into_iter().map(|s| (s.service, s.shared)).collect();
        assert!(s["postgres"]);
        assert!(s["redis"]);
        assert!(!s["api"]);
        assert!(!s["web"]);
        assert!(!s["worker"]);
    }

    #[test]
    fn cross_service_references() {
        let svc = Service {
            network_mode: Some("service:vpn".into()),
            volumes_from: vec!["data:ro".into(), "container:abc".into()],
            links: vec!["db:database".into()],
            ..Default::default()
        };
        assert_eq!(svc.network_mode_service(), Some("vpn"));
        assert_eq!(svc.volumes_from_services(), vec!["data"]);
        assert_eq!(svc.link_services(), vec!["db"]);
    }
}
