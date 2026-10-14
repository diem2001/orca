//! Worktree dev instances.
//!
//! One git repo (a *project*) can run as many isolated *instances* at once,
//! one per git worktree, while infrastructure such as databases runs once in
//! a shared stack. This module holds the pure logic: the `dev:` section of
//! `orca.yaml`, instance naming, `git worktree` parsing, the resolved compose
//! model, and generation of the per-instance compose override. The daemon
//! does all I/O (git, compose, Docker) and persists state.

pub mod compose_model;
pub mod hostnames;
pub mod naming;
pub mod override_gen;
pub mod schema;
pub mod state;
pub mod tenancy;
pub mod worktree;
pub mod yaml_out;

pub use compose_model::ComposeModel;
pub use schema::{DataMode, DevConfig, OrcaYaml};
pub use state::{DevInstanceState, DevProject, PortAssignment};
pub use worktree::Worktree;

/// Label put on every container Orca starts for a dev instance or shared stack.
pub const LABEL_PROJECT: &str = "orca.dev.project";
pub const LABEL_INSTANCE: &str = "orca.dev.instance";
pub const LABEL_ROLE: &str = "orca.dev.role";
pub const LABEL_WORKTREE: &str = "orca.dev.worktree";

/// Folder inside each worktree where Orca writes generated files.
pub const ORCA_DIR: &str = ".orca";
pub const OVERRIDE_FILE: &str = "compose.override.yaml";
pub const RESOLVED_FILE: &str = "compose.resolved.yaml";
pub const ENV_FILE: &str = "env";
pub const INSTANCE_FILE: &str = "instance.json";

/// Host ports handed out to instances come from this range.
pub const PORT_RANGE: std::ops::RangeInclusive<u16> = 41000..=41999;

/// Compose project name of a project's shared stack.
pub fn shared_compose_project(project: &str) -> String {
    format!("{project}-shared")
}

/// Compose project name of one instance.
pub fn instance_compose_project(project: &str, instance: &str) -> String {
    format!("{project}-{instance}")
}
