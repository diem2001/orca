//! Own-database mode: one database per instance inside a shared server.
//!
//! Orca knows how to do this for Postgres, MySQL/MariaDB and Redis. The
//! commands run inside the shared container with `sh -c`, reading
//! credentials from the container's own environment (`$POSTGRES_USER` …)
//! so no secret passes through Orca.

use serde::{Deserialize, Serialize};

use super::compose_model::Service;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenancyKind {
    Postgres,
    Mysql,
    Redis,
    Mongo,
    /// A vhost per instance.
    Rabbitmq,
}

impl TenancyKind {
    pub fn label(self) -> &'static str {
        match self {
            TenancyKind::Postgres => "Postgres",
            TenancyKind::Mysql => "MySQL",
            TenancyKind::Redis => "Redis",
            TenancyKind::Mongo => "MongoDB",
            TenancyKind::Rabbitmq => "RabbitMQ",
        }
    }
}

/// What kind of server a shared service is, from its image.
pub fn detect_kind(svc: &Service) -> Option<TenancyKind> {
    let image = svc.image.as_deref()?.to_lowercase();
    let base = image.rsplit('/').next().unwrap_or(&image);
    let base = base.split([':', '@']).next().unwrap_or(base);
    if ["postgres", "postgis", "timescaledb", "pgvector"]
        .iter()
        .any(|k| base.contains(k))
    {
        Some(TenancyKind::Postgres)
    } else if ["mysql", "mariadb", "percona"].iter().any(|k| base.contains(k)) {
        Some(TenancyKind::Mysql)
    } else if ["redis", "valkey", "keydb"].iter().any(|k| base.contains(k)) {
        Some(TenancyKind::Redis)
    } else if base.starts_with("mongo") {
        Some(TenancyKind::Mongo)
    } else if base.contains("rabbitmq") {
        Some(TenancyKind::Rabbitmq)
    } else {
        None
    }
}

/// The database everyone shares, read from the service's environment the
/// way the official images do.
pub fn detect_shared_database(kind: TenancyKind, svc: &Service) -> Option<String> {
    let env = |k: &str| svc.environment.get(k).cloned().flatten().filter(|v| !v.is_empty());
    match kind {
        TenancyKind::Postgres => env("POSTGRES_DB")
            .or_else(|| env("POSTGRES_USER"))
            .or_else(|| Some("postgres".to_string())),
        TenancyKind::Mysql => env("MYSQL_DATABASE").or_else(|| env("MARIADB_DATABASE")),
        TenancyKind::Redis => Some("0".to_string()),
        TenancyKind::Mongo => env("MONGO_INITDB_DATABASE"),
        TenancyKind::Rabbitmq => env("RABBITMQ_DEFAULT_VHOST").or_else(|| Some("/".to_string())),
    }
}

/// Tenant name for a kind: databases get `<db>_<instance>`; RabbitMQ's
/// default vhost `/` gets `vhost_<instance>`.
pub fn tenant_for(kind: TenancyKind, shared: &str, instance: &str) -> String {
    match kind {
        TenancyKind::Rabbitmq if shared == "/" => tenant_name("vhost", instance),
        _ => tenant_name(shared, instance),
    }
}

/// Database name for an instance: `myapp` + `feat-auth` → `myapp_feat_auth`.
/// Postgres and MySQL both allow 63 characters.
pub fn tenant_name(shared_db: &str, instance: &str) -> String {
    let mut name = format!("{shared_db}_{instance}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    name.truncate(63);
    name
}

/// Pick a free Redis DB index (1..=15; 0 is the shared one).
pub fn redis_index(taken: &[u32]) -> anyhow::Result<u32> {
    (1..=15)
        .find(|i| !taken.contains(i))
        .ok_or_else(|| anyhow::anyhow!("all 15 Redis databases are in use by other instances"))
}

/// Single-quote for `sh -c`.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn ident_ok(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || name.len() > 63 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!("'{name}' is not a safe database name (letters, digits and _ only)");
    }
    Ok(())
}

const PG: &str = r#"psql -v ON_ERROR_STOP=1 -U "${POSTGRES_USER:-postgres}""#;
/// mongosh (or the legacy mongo shell), authenticating when the image was
/// given root credentials.
const MONGO: &str = r#"$(command -v mongosh || command -v mongo) --quiet ${MONGO_INITDB_ROOT_USERNAME:+-u "$MONGO_INITDB_ROOT_USERNAME" -p "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin}"#;
const MONGO_AUTH: &str = r#"${MONGO_INITDB_ROOT_USERNAME:+--username "$MONGO_INITDB_ROOT_USERNAME" --password "$MONGO_INITDB_ROOT_PASSWORD" --authenticationDatabase admin}"#;
const RABBIT_USER: &str = r#""${RABBITMQ_DEFAULT_USER:-guest}""#;
// MariaDB 11 images only ship `mariadb` / `mariadb-dump`; MySQL images only
// `mysql` / `mysqldump`.
const MY: &str =
    r#"$(command -v mysql || command -v mariadb) -uroot -p"${MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}""#;
const MYDUMP: &str =
    r#"$(command -v mysqldump || command -v mariadb-dump) -uroot -p"${MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}""#;

/// Shell command that prints `yes` when the tenant exists.
pub fn exists_cmd(kind: TenancyKind, tenant: &str) -> anyhow::Result<String> {
    Ok(match kind {
        TenancyKind::Postgres => {
            ident_ok(tenant)?;
            format!(
                r#"{PG} -d postgres -tAc {} | grep -q 1 && echo yes || echo no"#,
                q(&format!("SELECT 1 FROM pg_database WHERE datname = '{tenant}'"))
            )
        }
        TenancyKind::Mysql => {
            ident_ok(tenant)?;
            format!(
                r#"{MY} -N -e {} 2>/dev/null | grep -q . && echo yes || echo no"#,
                q(&format!("SHOW DATABASES LIKE '{tenant}'"))
            )
        }
        // A Redis index always exists.
        TenancyKind::Redis => "echo yes".to_string(),
        TenancyKind::Mongo => {
            ident_ok(tenant)?;
            format!(r#"{MONGO} --eval "print(db.getMongo().getDBNames().includes('{tenant}') ? 'yes' : 'no')""#)
        }
        TenancyKind::Rabbitmq => {
            ident_ok(tenant)?;
            format!("rabbitmqctl list_vhosts --silent | grep -qx {tenant} && echo yes || echo no")
        }
    })
}

/// Shell command that creates the tenant, copying the shared database's
/// data when `clone_from` is given. The copy streams a dump, so it works
/// while other instances are connected to the source.
pub fn create_cmd(kind: TenancyKind, tenant: &str, clone_from: Option<&str>) -> anyhow::Result<String> {
    Ok(match kind {
        TenancyKind::Postgres => {
            ident_ok(tenant)?;
            // Double quotes so sh expands $POSTGRES_USER inside the SQL.
            let create =
                format!(r#"{PG} -d postgres -c "CREATE DATABASE \"{tenant}\" OWNER \"${{POSTGRES_USER:-postgres}}\"""#);
            match clone_from {
                Some(src) => {
                    ident_ok(src)?;
                    format!(
                        r#"{create} && pg_dump -U "${{POSTGRES_USER:-postgres}}" --no-owner --no-acl -d {src} | {PG} -q -d {tenant} >/dev/null"#
                    )
                }
                None => create,
            }
        }
        TenancyKind::Mysql => {
            ident_ok(tenant)?;
            let grant = r#"if [ -n "$MYSQL_USER" ]; then "#.to_string()
                + MY
                + &format!(r#" -e "GRANT ALL ON \`{tenant}\`.* TO '$MYSQL_USER'@'%'"; fi"#);
            let create = format!(r#"{MY} -e "CREATE DATABASE \`{tenant}\`" && {grant}"#);
            match clone_from {
                Some(src) => {
                    ident_ok(src)?;
                    format!(r#"{create} && {MYDUMP} --single-transaction {src} | {MY} {tenant}"#)
                }
                None => create,
            }
        }
        TenancyKind::Redis => {
            let idx: u32 = tenant
                .parse()
                .map_err(|_| anyhow::anyhow!("bad Redis index '{tenant}'"))?;
            format!("redis-cli -n {idx} FLUSHDB >/dev/null && echo ok")
        }
        TenancyKind::Mongo => {
            ident_ok(tenant)?;
            match clone_from {
                Some(src) => {
                    ident_ok(src)?;
                    format!(
                        r#"mongodump {MONGO_AUTH} --quiet --archive --db={src} | mongorestore {MONGO_AUTH} --quiet --archive --nsFrom='{src}.*' --nsTo='{tenant}.*'"#
                    )
                }
                // Mongo creates databases lazily; a collection makes it exist.
                None => format!(r#"{MONGO} --eval "db.getSiblingDB('{tenant}').createCollection('_orca')""#),
            }
        }
        TenancyKind::Rabbitmq => {
            ident_ok(tenant)?;
            // Queues and exchanges are declared by the app, so the vhost
            // starts empty even when asked to copy.
            format!(
                r#"rabbitmqctl add_vhost {tenant} && rabbitmqctl set_permissions -p {tenant} {RABBIT_USER} ".*" ".*" ".*""#
            )
        }
    })
}

/// Shell command that deletes the tenant.
pub fn drop_cmd(kind: TenancyKind, tenant: &str) -> anyhow::Result<String> {
    Ok(match kind {
        TenancyKind::Postgres => {
            ident_ok(tenant)?;
            format!(r#"{PG} -d postgres -c 'DROP DATABASE IF EXISTS "{tenant}" WITH (FORCE)'"#)
        }
        TenancyKind::Mysql => {
            ident_ok(tenant)?;
            format!(r#"{MY} -e "DROP DATABASE IF EXISTS \`{tenant}\`""#)
        }
        TenancyKind::Redis => {
            let idx: u32 = tenant
                .parse()
                .map_err(|_| anyhow::anyhow!("bad Redis index '{tenant}'"))?;
            format!("redis-cli -n {idx} FLUSHDB >/dev/null && echo ok")
        }
        TenancyKind::Mongo => {
            ident_ok(tenant)?;
            format!(r#"{MONGO} --eval "db.getSiblingDB('{tenant}').dropDatabase()""#)
        }
        TenancyKind::Rabbitmq => {
            ident_ok(tenant)?;
            format!("rabbitmqctl delete_vhost {tenant} || true")
        }
    })
}

/// Shell command that prints the tenant's size in bytes (or key count for Redis).
pub fn size_cmd(kind: TenancyKind, tenant: &str) -> anyhow::Result<String> {
    Ok(match kind {
        TenancyKind::Postgres => {
            ident_ok(tenant)?;
            format!(r#"{PG} -d postgres -tAc "SELECT pg_database_size('{tenant}')""#)
        }
        TenancyKind::Mysql => {
            ident_ok(tenant)?;
            format!(
                r#"{MY} -N -e "SELECT COALESCE(SUM(data_length + index_length), 0) FROM information_schema.tables WHERE table_schema = '{tenant}'" 2>/dev/null"#
            )
        }
        TenancyKind::Redis => {
            let idx: u32 = tenant
                .parse()
                .map_err(|_| anyhow::anyhow!("bad Redis index '{tenant}'"))?;
            format!("redis-cli -n {idx} DBSIZE")
        }
        TenancyKind::Mongo => {
            ident_ok(tenant)?;
            format!(r#"{MONGO} --eval "print(db.getSiblingDB('{tenant}').stats().dataSize)""#)
        }
        // Queue count, not bytes.
        TenancyKind::Rabbitmq => {
            if tenant == "/" {
                "rabbitmqctl list_queues -p / --silent | wc -l".to_string()
            } else {
                ident_ok(tenant)?;
                format!("rabbitmqctl list_queues -p {tenant} --silent | wc -l")
            }
        }
    })
}

/// Rewrite references to the shared database in a service's environment so
/// it uses `tenant` instead. Recognises connection URLs whose host is the
/// shared service (`postgres://u:p@postgres:5432/myapp`) and plain database
/// name variables (`DB_NAME=myapp`). Returns `(key, old, new)` per change.
pub fn rewrite_env(
    env: &std::collections::BTreeMap<String, Option<String>>,
    service_host: &str,
    kind: TenancyKind,
    shared_db: &str,
    tenant: &str,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (k, v) in env {
        let Some(v) = v else { continue };
        if let Some(new) = rewrite_url(v, service_host, kind, shared_db, tenant) {
            out.push((k.clone(), v.clone(), new));
            continue;
        }
        let key = k.to_uppercase();
        let names_db = match kind {
            TenancyKind::Rabbitmq => key.contains("VHOST"),
            _ => key.contains("DB") || key.contains("DATABASE"),
        };
        if kind != TenancyKind::Redis && names_db && v == shared_db && !key.contains("HOST") && !key.contains("URL") {
            out.push((k.clone(), v.clone(), tenant.to_string()));
        }
    }
    out
}

fn rewrite_url(v: &str, host: &str, kind: TenancyKind, shared_db: &str, tenant: &str) -> Option<String> {
    let (scheme, rest) = v.split_once("://")?;
    let scheme_ok = match kind {
        TenancyKind::Postgres => scheme.starts_with("postgres") || scheme.contains("postgresql"),
        TenancyKind::Mysql => scheme.starts_with("mysql") || scheme.starts_with("mariadb"),
        TenancyKind::Redis => scheme.starts_with("redis") || scheme == "rediss",
        TenancyKind::Mongo => scheme.starts_with("mongodb"),
        TenancyKind::Rabbitmq => scheme.starts_with("amqp"),
    };
    if !scheme_ok {
        return None;
    }
    // authority ends at the first '/', '?' or end
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let after = &rest[auth_end..];
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    let h = hostport.split(':').next().unwrap_or(hostport);
    if h != host {
        return None;
    }
    let (path, query) = match after.find('?') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, ""),
    };
    let current = path.trim_start_matches('/');
    match kind {
        // amqp://host/ and amqp://host/%2F both mean the default vhost "/".
        TenancyKind::Rabbitmq => {
            let is_default = current.is_empty() || current == "%2F" || current == "%2f";
            if (shared_db == "/" && is_default) || current == shared_db {
                Some(format!("{scheme}://{authority}/{tenant}{query}"))
            } else {
                None
            }
        }
        TenancyKind::Redis => {
            if current.is_empty() || current == shared_db {
                Some(format!("{scheme}://{authority}/{tenant}{query}"))
            } else {
                None
            }
        }
        _ => {
            if current == shared_db {
                Some(format!("{scheme}://{authority}/{tenant}{query}"))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn svc(image: &str, env: &[(&str, &str)]) -> Service {
        Service {
            image: Some(image.into()),
            environment: env.iter().map(|(k, v)| (k.to_string(), Some(v.to_string()))).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn detects_kinds_and_databases() {
        let pg = svc("postgres:16-alpine", &[("POSTGRES_DB", "myapp")]);
        assert_eq!(detect_kind(&pg), Some(TenancyKind::Postgres));
        assert_eq!(
            detect_shared_database(TenancyKind::Postgres, &pg).as_deref(),
            Some("myapp")
        );
        let pg_default = svc("docker.io/library/postgres", &[]);
        assert_eq!(
            detect_shared_database(TenancyKind::Postgres, &pg_default).as_deref(),
            Some("postgres")
        );
        assert_eq!(detect_kind(&svc("mariadb:11", &[])), Some(TenancyKind::Mysql));
        assert_eq!(detect_kind(&svc("valkey/valkey:8", &[])), Some(TenancyKind::Redis));
        assert_eq!(detect_kind(&svc("nats:2", &[])), None);
    }

    #[test]
    fn tenant_names_are_safe() {
        assert_eq!(tenant_name("myapp", "feat-auth"), "myapp_feat_auth");
        assert!(tenant_name(&"x".repeat(60), "long-instance").len() <= 63);
        assert_eq!(redis_index(&[1, 2]).unwrap(), 3);
        assert!(redis_index(&(1..=15).collect::<Vec<_>>()).is_err());
    }

    #[test]
    fn rewrites_urls_and_db_name_vars() {
        let env: BTreeMap<String, Option<String>> = [
            ("DATABASE_URL", "postgres://app:app@postgres:5432/myapp?sslmode=disable"),
            ("OTHER_URL", "postgres://app:app@otherhost:5432/myapp"),
            ("DB_NAME", "myapp"),
            ("DB_HOST", "postgres"),
            ("APP_NAME", "myapp"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), Some(v.to_string())))
        .collect();
        let changes = rewrite_env(&env, "postgres", TenancyKind::Postgres, "myapp", "myapp_feat");
        let keys: Vec<&str> = changes.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(keys, vec!["DATABASE_URL", "DB_NAME"]);
        assert_eq!(
            changes[0].2,
            "postgres://app:app@postgres:5432/myapp_feat?sslmode=disable"
        );
    }

    #[test]
    fn rewrites_redis_index() {
        let env: BTreeMap<String, Option<String>> =
            [("REDIS_URL", "redis://redis:6379"), ("CACHE", "redis://redis:6379/0")]
                .iter()
                .map(|(k, v)| (k.to_string(), Some(v.to_string())))
                .collect();
        let changes = rewrite_env(&env, "redis", TenancyKind::Redis, "0", "4");
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].2, "redis://redis:6379/4");
        assert_eq!(changes[1].2, "redis://redis:6379/4");
    }

    #[test]
    fn commands_refuse_unsafe_names_and_quote_sql() {
        assert!(create_cmd(TenancyKind::Postgres, "x; drop", None).is_err());
        let c = create_cmd(TenancyKind::Postgres, "myapp_feat", Some("myapp")).unwrap();
        assert!(c.contains(r#"CREATE DATABASE \"myapp_feat\""#), "{c}");
        assert!(c.contains("pg_dump"), "{c}");
        let e = exists_cmd(TenancyKind::Postgres, "myapp_feat").unwrap();
        assert!(e.contains("datname = '\\''myapp_feat'\\''"), "{e}");
        let m = create_cmd(TenancyKind::Mysql, "myapp_feat", None).unwrap();
        assert!(m.contains("CREATE DATABASE \\`myapp_feat\\`"), "{m}");
    }

    #[test]
    fn mongo_and_rabbit() {
        assert_eq!(detect_kind(&svc("mongo:7", &[])), Some(TenancyKind::Mongo));
        assert_eq!(
            detect_kind(&svc("rabbitmq:3-management", &[])),
            Some(TenancyKind::Rabbitmq)
        );
        let r = svc("rabbitmq:3", &[]);
        assert_eq!(detect_shared_database(TenancyKind::Rabbitmq, &r).as_deref(), Some("/"));
        assert_eq!(tenant_for(TenancyKind::Rabbitmq, "/", "feat-x"), "vhost_feat_x");
        let env: BTreeMap<String, Option<String>> = [
            ("AMQP_URL", "amqp://guest:guest@rabbitmq:5672/"),
            ("MONGO_URL", "mongodb://root:pw@mongo:27017/shop?authSource=admin"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), Some(v.to_string())))
        .collect();
        let r = rewrite_env(&env, "rabbitmq", TenancyKind::Rabbitmq, "/", "vhost_feat_x");
        assert_eq!(r[0].2, "amqp://guest:guest@rabbitmq:5672/vhost_feat_x");
        let m = rewrite_env(&env, "mongo", TenancyKind::Mongo, "shop", "shop_feat");
        assert_eq!(m[0].2, "mongodb://root:pw@mongo:27017/shop_feat?authSource=admin");
        assert!(
            create_cmd(TenancyKind::Mongo, "shop_feat", Some("shop"))
                .unwrap()
                .contains("--nsTo='shop_feat.*'")
        );
    }

    /// Runs the real commands against running containers:
    /// `ORCA_TEST_PG=<container> ORCA_TEST_PG_DB=<db> ORCA_TEST_REDIS=<container> cargo test -p orca-core -- --ignored tenancy`
    #[test]
    #[ignore]
    fn commands_work_against_real_servers() {
        let run = |container: &str, cmd: &str| -> String {
            let out = std::process::Command::new("docker")
                .args(["exec", container, "sh", "-c", cmd])
                .output()
                .expect("docker exec");
            assert!(out.status.success(), "{cmd}\n{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        if let (Ok(pg), Ok(db)) = (std::env::var("ORCA_TEST_PG"), std::env::var("ORCA_TEST_PG_DB")) {
            let t = "orca_tenancy_test";
            let _ = run(&pg, &drop_cmd(TenancyKind::Postgres, t).unwrap());
            assert_eq!(run(&pg, &exists_cmd(TenancyKind::Postgres, t).unwrap()), "no");
            run(&pg, &create_cmd(TenancyKind::Postgres, t, Some(&db)).unwrap());
            assert_eq!(run(&pg, &exists_cmd(TenancyKind::Postgres, t).unwrap()), "yes");
            let size: u64 = run(&pg, &size_cmd(TenancyKind::Postgres, t).unwrap()).parse().unwrap();
            assert!(size > 0);
            run(&pg, &drop_cmd(TenancyKind::Postgres, t).unwrap());
            assert_eq!(run(&pg, &exists_cmd(TenancyKind::Postgres, t).unwrap()), "no");
        }
        // Same flow for the other kinds: (env var, kind, shared db, seed SQL-ish setup).
        for (var, kind, shared) in [
            ("ORCA_TEST_MYSQL", TenancyKind::Mysql, "shop"),
            ("ORCA_TEST_MONGO", TenancyKind::Mongo, "shop"),
            ("ORCA_TEST_RABBIT", TenancyKind::Rabbitmq, "/"),
        ] {
            let Ok(c) = std::env::var(var) else { continue };
            let t = "orca_tenancy_test";
            let _ = run(&c, &drop_cmd(kind, t).unwrap());
            assert_eq!(run(&c, &exists_cmd(kind, t).unwrap()), "no", "{var}");
            let copies = !matches!(kind, TenancyKind::Rabbitmq);
            run(&c, &create_cmd(kind, t, copies.then_some(shared)).unwrap());
            assert_eq!(run(&c, &exists_cmd(kind, t).unwrap()), "yes", "{var}");
            let size: u64 = run(&c, &size_cmd(kind, t).unwrap()).trim().parse().unwrap();
            if copies {
                assert!(size > 0, "{var}: copy should have data");
            }
            run(&c, &drop_cmd(kind, t).unwrap());
            assert_eq!(run(&c, &exists_cmd(kind, t).unwrap()), "no", "{var}");
        }
        if let Ok(r) = std::env::var("ORCA_TEST_REDIS") {
            run(&r, "redis-cli -n 9 SET k v");
            assert_eq!(run(&r, &size_cmd(TenancyKind::Redis, "9").unwrap()), "1");
            run(&r, &drop_cmd(TenancyKind::Redis, "9").unwrap());
            assert_eq!(run(&r, &size_cmd(TenancyKind::Redis, "9").unwrap()), "0");
        }
    }
}
