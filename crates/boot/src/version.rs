//! Git-branch-based version management.
//!
//! All versions are branches (V1, V2, V3 …) in a single normal git
//! repository at `peripheral/source/`.  The current version is simply
//! the checked-out branch; rollback is a `git checkout` to the
//! previous branch.  Binary is stored at a fixed path
//! `peripheral/binary`.
//! See plan §2.2.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

#[derive(Debug)]
pub struct VersionManager {
    /// `~/.reloopy/peripheral`
    base_dir: PathBuf,
    /// `~/.reloopy/peripheral/source` — the single git repo
    source_dir: PathBuf,
    /// `~/.reloopy/peripheral/binary` — fixed binary path
    binary_path: PathBuf,
    consecutive_failures: u32,
    locked: bool,
    /// Tracks the previous version for rollback (set on `switch_to`).
    rollback_branch: Option<String>,
}

/// Information about a newly allocated version.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub version: String,
    /// The single source directory (`peripheral/source/`).
    pub source_dir: PathBuf,
    /// Fixed binary path (`peripheral/binary`).
    pub binary_path: PathBuf,
}

impl VersionManager {
    pub fn new(base_dir: &Path) -> Self {
        let peripheral = base_dir.join("peripheral");
        let source_dir = peripheral.join("source");
        let binary_path = peripheral.join("binary");
        Self {
            base_dir: peripheral,
            source_dir,
            binary_path,
            consecutive_failures: 0,
            locked: false,
            rollback_branch: None,
        }
    }

    // -- helpers ----------------------------------------------------------

    fn ensure_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.base_dir)
    }

    /// Return the path to the single source repo.
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }

    /// Return the fixed binary path.
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    /// Initialise a normal git repo at `source_dir` if it does not exist.
    /// Also recovers from a partially initialised repo (no commits on `main`).
    fn init_repo_if_needed(&self) -> Result<(), String> {
        let git_internal = self.source_dir.join(".git");
        if git_internal.exists() {
            // Verify `main` branch has at least one commit.
            let check = Command::new("git")
                .args(["rev-parse", "--verify", "refs/heads/main"])
                .current_dir(&self.source_dir)
                .output();
            match check {
                Ok(o) if o.status.success() => return Ok(()),
                Ok(_) => {
                    tracing::warn!(
                        source_dir = %self.source_dir.display(),
                        "Git repo exists but 'main' branch is missing; re-initialising"
                    );
                    fs::remove_dir_all(&self.source_dir).map_err(|e| {
                        format!(
                            "Failed to remove stale repo at {}: {}",
                            self.source_dir.display(),
                            e
                        )
                    })?;
                }
                Err(e) => {
                    tracing::debug!("git rev-parse failed to run: {}", e);
                    fs::remove_dir_all(&self.source_dir).map_err(|e| {
                        format!("Failed to remove stale repo: {}", e)
                    })?;
                }
            }
        }

        fs::create_dir_all(&self.source_dir)
            .map_err(|e| format!("Failed to create source dir: {}", e))?;

        let o = Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&self.source_dir)
            .output()
            .map_err(|e| format!("git init failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git init failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        for (key, value) in [
            ("user.name", "reloopy-boot"),
            ("user.email", "boot@reloopy.local"),
        ] {
            let o = Command::new("git")
                .args(["config", key, value])
                .current_dir(&self.source_dir)
                .output()
                .map_err(|e| format!("git config failed: {}", e))?;
            if !o.status.success() {
                return Err(format!(
                    "git config {} failed: {}",
                    key,
                    String::from_utf8_lossy(&o.stderr)
                ));
            }
        }

        let o = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git commit (init) failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git initial commit failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        tracing::info!(source_dir = %self.source_dir.display(), "Git repo initialised");
        Ok(())
    }

    /// Determine the next V{N} number by inspecting existing branch names.
    fn next_version_number(&self) -> u32 {
        let output = Command::new("git")
            .args(["branch", "--list", "V*", "--format=%(refname:short)"])
            .current_dir(&self.source_dir)
            .output();

        let mut max = 0u32;
        if let Ok(o) = output {
            if o.status.success() {
                let stdout = String::from_utf8_lossy(&o.stdout);
                for line in stdout.lines() {
                    let line = line.trim();
                    if let Some(stripped) = line.strip_prefix('V') {
                        if let Ok(num) = stripped.parse::<u32>() {
                            max = max.max(num);
                        }
                    }
                }
            }
        }
        max + 1
    }

    // -- public API -------------------------------------------------------

    /// Read the current active version from `git branch --show-current`.
    /// Returns `None` if the repo does not exist or the current branch is
    /// not a version branch.
    pub fn current_version(&self) -> Option<String> {
        if !self.source_dir.join(".git").exists() {
            return None;
        }
        let o = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&self.source_dir)
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        let branch = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if let Some(num_str) = branch.strip_prefix('V') {
            if !num_str.is_empty() && num_str.parse::<u32>().is_ok() {
                return Some(branch);
            }
        }
        None
    }

    /// Return the rollback version (the version that was active before the
    /// most recent `switch_to`).
    pub fn rollback_version(&self) -> Option<String> {
        self.rollback_branch.clone()
    }

    /// Allocate a new version branch from the current branch, ready for
    /// staging content.  The caller should then copy source into
    /// `version_info.source_dir` and call `commit_version_source`.
    pub fn allocate_version(&self) -> Result<VersionInfo, String> {
        self.ensure_dirs()
            .map_err(|e| format!("Failed to create peripheral dir: {}", e))?;

        if self.locked {
            return Err(
                "Version manager is locked due to consecutive failures. Human intervention required."
                    .to_string(),
            );
        }

        self.init_repo_if_needed()?;

        let num = self.next_version_number();
        let version = format!("V{}", num);

        // Create new branch from current HEAD.
        let o = Command::new("git")
            .args(["checkout", "-b", &version])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git checkout -b {} failed: {}", version, e))?;
        if !o.status.success() {
            return Err(format!(
                "git checkout -b {} failed: {}",
                version,
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        tracing::info!(version = %version, "New version branch created");

        Ok(VersionInfo {
            version,
            source_dir: self.source_dir.clone(),
            binary_path: self.binary_path.clone(),
        })
    }

    /// Stage all files and commit on the current branch.
    /// Non-fatal: callers should log a warning on error but continue.
    pub fn commit_version_source(&self, version_info: &VersionInfo) -> Result<(), String> {
        let src = &version_info.source_dir;

        let add = Command::new("git")
            .args(["add", "-A"])
            .current_dir(src)
            .output()
            .map_err(|e| format!("git add failed: {}", e))?;
        if !add.status.success() {
            return Err(format!(
                "git add -A failed: {}",
                String::from_utf8_lossy(&add.stderr)
            ));
        }

        let msg = format!("Version {}", version_info.version);
        let commit = Command::new("git")
            .args(["commit", "--allow-empty", "-m", &msg])
            .current_dir(src)
            .output()
            .map_err(|e| format!("git commit failed: {}", e))?;
        if !commit.status.success() {
            return Err(format!(
                "git commit failed: {}",
                String::from_utf8_lossy(&commit.stderr)
            ));
        }

        tracing::info!(version = %version_info.version, "Source committed to git branch");
        Ok(())
    }

    /// Copy source files from `from` into the source repo, skipping `.git`
    /// and `target` directories.
    pub fn copy_source(&self, from: &Path, to: &Path) -> Result<(), String> {
        copy_dir_recursive(from, to).map_err(|e| {
            format!(
                "Failed to copy source from {} to {}: {}",
                from.display(),
                to.display(),
                e
            )
        })
    }

    /// Switch the repo to `version` branch.  Returns the previously active
    /// version (empty string if there was none).
    pub fn switch_to(&mut self, version: &str) -> Result<String, String> {
        // Verify the branch exists.
        let check = Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{}", version)])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git rev-parse failed: {}", e))?;
        if !check.status.success() {
            return Err(format!("Version branch does not exist: {}", version));
        }

        let old_version = self.current_version();

        let o = Command::new("git")
            .args(["checkout", version])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git checkout {} failed: {}", version, e))?;
        if !o.status.success() {
            return Err(format!(
                "git checkout {} failed: {}",
                version,
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        if let Some(ref old) = old_version {
            self.rollback_branch = Some(old.clone());
        }

        self.consecutive_failures = 0;

        tracing::info!(
            version = %version,
            old_version = ?old_version,
            "Version switched"
        );

        Ok(old_version.unwrap_or_default())
    }

    /// Rollback to the previous version.
    pub fn rollback(&mut self) -> Result<String, String> {
        let rollback_version = self
            .rollback_branch
            .clone()
            .ok_or("No rollback version available")?;

        let o = Command::new("git")
            .args(["checkout", &rollback_version])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git checkout {} failed: {}", rollback_version, e))?;
        if !o.status.success() {
            return Err(format!(
                "git checkout {} failed: {}",
                rollback_version,
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        tracing::warn!(version = %rollback_version, "Rolled back to previous version");

        Ok(rollback_version)
    }

    pub fn record_failure(&mut self) -> bool {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            self.locked = true;
            tracing::error!(
                failures = self.consecutive_failures,
                "Version manager LOCKED — consecutive upgrade failures exceeded threshold"
            );
            true
        } else {
            false
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn unlock(&mut self) -> bool {
        let was_locked = self.locked;
        self.locked = false;
        self.consecutive_failures = 0;
        if was_locked {
            tracing::info!("Version manager unlocked by admin");
        }
        was_locked
    }

    /// List all version branches sorted numerically.
    pub fn list_versions(&self) -> Vec<String> {
        if !self.source_dir.join(".git").exists() {
            return Vec::new();
        }

        let output = Command::new("git")
            .args(["branch", "--list", "V*", "--format=%(refname:short)"])
            .current_dir(&self.source_dir)
            .output();

        let mut versions = Vec::new();
        if let Ok(o) = output {
            if o.status.success() {
                let stdout = String::from_utf8_lossy(&o.stdout);
                for line in stdout.lines() {
                    let name = line.trim().to_string();
                    if let Some(stripped) = name.strip_prefix('V') {
                        if stripped.parse::<u32>().is_ok() {
                            versions.push(name);
                        }
                    }
                }
            }
        }
        versions.sort_by_key(|v| {
            v.strip_prefix('V')
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0)
        });
        versions
    }

    /// Return git log metadata for a version branch as a JSON value.
    pub fn version_detail(&self, version: &str) -> Result<serde_json::Value, String> {
        let check = Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{}", version)])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git rev-parse failed: {}", e))?;
        if !check.status.success() {
            return Err(format!("Version branch does not exist: {}", version));
        }

        let log = Command::new("git")
            .args([
                "log",
                "-1",
                "--format=%H%n%ai%n%s",
                &format!("refs/heads/{}", version),
            ])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git log failed: {}", e))?;
        if !log.status.success() {
            return Err(format!(
                "git log failed: {}",
                String::from_utf8_lossy(&log.stderr)
            ));
        }

        let stdout = String::from_utf8_lossy(&log.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        let commit = lines.first().unwrap_or(&"").to_string();
        let date = lines.get(1).unwrap_or(&"").to_string();
        let subject = lines.get(2).unwrap_or(&"").to_string();

        Ok(serde_json::json!({
            "version": version,
            "commit": commit,
            "date": date,
            "subject": subject,
        }))
    }

    /// Check if the fixed binary exists.
    pub fn has_binary(&self, _version: &str) -> bool {
        self.binary_path.exists()
    }

    /// Check if the version branch exists (i.e. it has source).
    pub fn has_source(&self, version: &str) -> bool {
        if !self.source_dir.join(".git").exists() {
            return false;
        }
        let check = Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{}", version)])
            .current_dir(&self.source_dir)
            .output();
        matches!(check, Ok(o) if o.status.success())
    }

    /// Delete old version branches, keeping at most `keep` beyond
    /// current and rollback.
    pub fn cleanup_old_versions(&self, keep: usize) -> Result<Vec<String>, String> {
        let current = self.current_version();
        let rollback = self.rollback_branch.clone();
        let mut all = self.list_versions();

        all.retain(|v| {
            Some(v.as_str()) != current.as_deref() && Some(v.as_str()) != rollback.as_deref()
        });

        if all.len() <= keep {
            return Ok(Vec::new());
        }

        let to_remove = all.len() - keep;
        let removable: Vec<String> = all.into_iter().take(to_remove).collect();
        let mut removed = Vec::new();

        for v in &removable {
            let o = Command::new("git")
                .args(["branch", "-D", v])
                .current_dir(&self.source_dir)
                .output();
            match o {
                Ok(out) if out.status.success() => {
                    removed.push(v.clone());
                    tracing::info!(version = %v, "Old version branch deleted");
                }
                Ok(out) => {
                    tracing::error!(
                        version = %v,
                        "Failed to delete branch: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                Err(e) => {
                    tracing::error!(version = %v, "git branch -D failed to run: {}", e);
                }
            }
        }

        Ok(removed)
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        // Skip .git (preserves repo metadata) and target (build artifacts).
        if name == ".git" || name == "target" {
            continue;
        }
        let target = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_v1_creates_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let info = mgr.allocate_version().expect("V1 allocation should succeed");
        assert_eq!(info.version, "V1");
        assert!(info.source_dir.exists(), "source dir must exist");

        // `main` branch must be resolvable.
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/main"])
            .current_dir(&mgr.source_dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "main branch must exist after init");

        // V1 branch must also exist.
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/V1"])
            .current_dir(&mgr.source_dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "V1 branch must exist");
    }

    #[test]
    fn allocate_v2_based_on_v1() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let v1 = mgr.allocate_version().expect("V1");
        assert_eq!(v1.version, "V1");

        let v2 = mgr.allocate_version().expect("V2");
        assert_eq!(v2.version, "V2");
        assert!(v2.source_dir.exists());
    }

    #[test]
    fn current_version_follows_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        assert_eq!(mgr.current_version(), None);

        mgr.allocate_version().expect("V1");
        assert_eq!(mgr.current_version(), Some("V1".to_string()));

        mgr.allocate_version().expect("V2");
        assert_eq!(mgr.current_version(), Some("V2".to_string()));
    }

    #[test]
    fn switch_and_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        mgr.allocate_version().expect("V1");
        mgr.allocate_version().expect("V2");

        // Switch to V1
        let old = mgr.switch_to("V1").unwrap();
        assert_eq!(old, "V2");
        assert_eq!(mgr.current_version(), Some("V1".to_string()));
        assert_eq!(mgr.rollback_version(), Some("V2".to_string()));

        // Rollback to V2
        let rolled = mgr.rollback().unwrap();
        assert_eq!(rolled, "V2");
        assert_eq!(mgr.current_version(), Some("V2".to_string()));
    }

    #[test]
    fn list_versions_returns_sorted_branches() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        mgr.allocate_version().expect("V1");
        mgr.allocate_version().expect("V2");
        mgr.allocate_version().expect("V3");

        let versions = mgr.list_versions();
        assert_eq!(versions, vec!["V1", "V2", "V3"]);
    }

    #[test]
    fn cleanup_old_versions_deletes_branches() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        mgr.allocate_version().expect("V1");
        mgr.allocate_version().expect("V2");
        mgr.allocate_version().expect("V3");
        // Currently on V3. Switch to V3 so rollback=V2 is preserved.
        // (allocate_version already checks out V3.)

        // Manually set rollback so V2 is protected.
        mgr.rollback_branch = Some("V2".to_string());

        // Keep 0 beyond current (V3) and rollback (V2) → V1 should be removed.
        let removed = mgr.cleanup_old_versions(0).unwrap();
        assert_eq!(removed, vec!["V1"]);

        let remaining = mgr.list_versions();
        assert!(remaining.contains(&"V2".to_string()));
        assert!(remaining.contains(&"V3".to_string()));
        assert!(!remaining.contains(&"V1".to_string()));
    }

    #[test]
    fn stale_repo_is_recovered() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());
        mgr.ensure_dirs().unwrap();

        // Create a repo but corrupt it by removing refs so main is absent.
        fs::create_dir_all(&mgr.source_dir).unwrap();
        let o = Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&mgr.source_dir)
            .output()
            .unwrap();
        assert!(o.status.success());
        // Don't commit so `main` ref doesn't exist.

        // init_repo_if_needed should detect missing `main` and re-init.
        mgr.init_repo_if_needed()
            .expect("should recover from stale repo");

        let out = Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/main"])
            .current_dir(&mgr.source_dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "main must exist after recovery");
    }

    #[test]
    fn copy_source_preserves_git() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let info = mgr.allocate_version().expect("V1 allocation");

        // Build a fake staging directory.
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join(".git").join("objects")).unwrap();
        fs::write(
            staging.join(".git").join("HEAD"),
            "ref: refs/heads/fake\n",
        )
        .unwrap();
        fs::create_dir_all(staging.join("target").join("debug")).unwrap();
        fs::write(
            staging.join("target").join("debug").join("artifact"),
            b"binary",
        )
        .unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();

        mgr.copy_source(&staging, &info.source_dir).unwrap();

        // .git must not have been overwritten.
        assert!(
            info.source_dir.join(".git").exists(),
            ".git must still exist"
        );
        // target must not have been copied.
        assert!(
            !info.source_dir.join("target").exists(),
            "target directory must not be copied"
        );
        // Regular files must have been copied.
        assert!(info.source_dir.join("Cargo.toml").exists());
        assert!(info.source_dir.join("src").join("main.rs").exists());

        // Git operations must still work.
        let out = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&info.source_dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git status must succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn has_source_checks_branch_existence() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        mgr.allocate_version().expect("V1");

        assert!(mgr.has_source("V1"));
        assert!(!mgr.has_source("V99"));
    }

    #[test]
    fn version_detail_returns_commit_info() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let info = mgr.allocate_version().expect("V1");
        mgr.commit_version_source(&info).unwrap();

        let detail = mgr.version_detail("V1").unwrap();
        assert_eq!(detail["version"], "V1");
        assert!(!detail["commit"].as_str().unwrap().is_empty());
    }
}
