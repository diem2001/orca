//! Parsing `git worktree list --porcelain`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    /// Commit checked out, if any.
    pub head: Option<String>,
    /// Short branch name (`feat/auth`), `None` for a detached HEAD.
    pub branch: Option<String>,
    /// The main working tree (the original clone) is always listed first.
    pub is_main: bool,
    /// Git considers the worktree folder gone.
    pub prunable: bool,
    pub locked: bool,
}

impl Worktree {
    /// Folder name of the worktree.
    pub fn folder_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

/// Parse porcelain output. Bare repositories are skipped: they have no
/// working tree to run.
pub fn parse_porcelain(output: &str) -> Vec<Worktree> {
    let mut result = Vec::new();
    let mut current: Option<Worktree> = None;
    let mut bare = false;

    let finish = |wt: Option<Worktree>, bare: bool, result: &mut Vec<Worktree>| {
        if let Some(wt) = wt
            && !bare
        {
            result.push(wt);
        }
    };

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            finish(current.take(), bare, &mut result);
            bare = false;
            current = Some(Worktree {
                path: PathBuf::from(path),
                head: None,
                branch: None,
                is_main: false,
                prunable: false,
                locked: false,
            });
            continue;
        }
        let Some(wt) = current.as_mut() else { continue };
        if let Some(head) = line.strip_prefix("HEAD ") {
            wt.head = Some(head.to_string());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            wt.branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(branch).to_string());
        } else if line == "bare" {
            bare = true;
        } else if line.starts_with("prunable") {
            wt.prunable = true;
        } else if line.starts_with("locked") {
            wt.locked = true;
        }
    }
    finish(current.take(), bare, &mut result);

    if let Some(first) = result.first_mut() {
        first.is_main = true;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_main_branch_detached_and_prunable() {
        let out = "worktree /home/u/code/myapp\nHEAD 1111\nbranch refs/heads/main\n\n\
                   worktree /home/u/code/myapp-feat\nHEAD 2222\nbranch refs/heads/feat/auth\n\n\
                   worktree /home/u/code/myapp/.claude/worktrees/x\nHEAD 3333\ndetached\n\n\
                   worktree /tmp/gone\nHEAD 4444\nbranch refs/heads/old\nprunable gitdir file points to non-existent location\n";
        let wts = parse_porcelain(out);
        assert_eq!(wts.len(), 4);
        assert!(wts[0].is_main);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[1].branch.as_deref(), Some("feat/auth"));
        assert!(!wts[1].is_main);
        assert_eq!(wts[2].branch, None);
        assert_eq!(wts[2].folder_name(), "x");
        assert!(wts[3].prunable);
    }

    #[test]
    fn skips_bare_repos() {
        let out = "worktree /srv/repo.git\nbare\n\nworktree /srv/wt\nHEAD 1\nbranch refs/heads/main\n";
        let wts = parse_porcelain(out);
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].path, PathBuf::from("/srv/wt"));
        assert!(wts[0].is_main);
    }
}
