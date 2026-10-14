//! Instance and project names.
//!
//! Names end up in compose project names (`[a-z0-9][a-z0-9_-]*`) and in DNS
//! labels (max 63 characters, no leading or trailing `-`), so they are
//! slugified to the intersection of both.

/// Names an instance can never take: they would collide with Orca's own
/// compose projects (`<project>-shared`).
const RESERVED: &[&str] = &["shared"];

/// Longest slug we produce. Leaves room for `<project>-` and `<service>.`
/// inside a 63-character DNS label budget per segment.
const MAX_LEN: usize = 40;

/// Turn any branch or folder name into a DNS- and compose-safe slug.
/// `claude/Fix_Login#2` → `claude-fix-login-2`. Returns `None` when nothing
/// usable is left.
pub fn slugify(input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= MAX_LEN {
        return Some(trimmed.to_string());
    }
    // Too long: keep a readable prefix plus a short hash of the full input so
    // two long names with the same prefix stay distinct.
    let hash = short_hash(input);
    let prefix = trimmed[..MAX_LEN - hash.len() - 1].trim_end_matches('-');
    Some(format!("{prefix}-{hash}"))
}

/// Pick `base`, or `base-2`, `base-3`… if it is taken or reserved.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    let is_free = |n: &str| !RESERVED.contains(&n) && !taken.iter().any(|t| t == n);
    if is_free(base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base}-{i}"))
        .find(|n| is_free(n))
        .unwrap_or_else(|| base.to_string())
}

/// Validate a name a user typed (rename): must already be a slug.
pub fn validate_name(name: &str) -> anyhow::Result<()> {
    if slugify(name).as_deref() != Some(name) {
        anyhow::bail!(
            "'{name}' is not a valid name: use lowercase letters, digits and '-', at most {MAX_LEN} characters"
        );
    }
    if RESERVED.contains(&name) {
        anyhow::bail!("'{name}' is reserved");
    }
    Ok(())
}

/// 6 hex characters of FNV-1a. Stable across runs and platforms.
fn short_hash(s: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x01000193);
    }
    format!("{:06x}", h & 0xff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_cleans_branch_names() {
        assert_eq!(slugify("main").as_deref(), Some("main"));
        assert_eq!(slugify("feat/Auth_v2").as_deref(), Some("feat-auth-v2"));
        assert_eq!(slugify("claude/fix-7f3a").as_deref(), Some("claude-fix-7f3a"));
        assert_eq!(slugify("--weird//name--").as_deref(), Some("weird-name"));
        assert_eq!(slugify("æøå"), None);
        assert_eq!(slugify("///"), None);
    }

    #[test]
    fn slugify_truncates_with_hash() {
        let long = "feature/".to_string() + &"very-long-branch-name-".repeat(5);
        let a = slugify(&long).unwrap();
        assert!(a.len() <= MAX_LEN, "{a}");
        assert!(!a.ends_with('-'));
        let b = slugify(&(long.clone() + "x")).unwrap();
        assert_ne!(a, b, "different long names must not collide");
    }

    #[test]
    fn unique_name_suffixes_and_avoids_reserved() {
        let taken = vec!["feat".to_string(), "feat-2".to_string()];
        assert_eq!(unique_name("feat", &taken), "feat-3");
        assert_eq!(unique_name("other", &taken), "other");
        assert_eq!(unique_name("shared", &[]), "shared-2");
    }

    #[test]
    fn validate_name_rules() {
        assert!(validate_name("auth").is_ok());
        assert!(validate_name("Auth").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name("shared").is_err());
    }
}
