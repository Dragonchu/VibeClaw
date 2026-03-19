//! Git-branch-based version management.
//!
//! All versions are branches (V1, V2, V3 …) in a single normal git
//! repository at `peripheral/source/`.  The current version is simply
//! the checked-out branch; rollback is a `git checkout` to the
//! previous branch.  Binary is stored at a fixed path
//! `peripheral/binary`, with a rollback copy at `peripheral/binary.rollback`.
//! See plan §2.2.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Git ref used to persist the rollback target across restarts.
const ROLLBACK_REF: &str = "refs/reloopy/rollback";

#[derive(Debug)]
pub struct VersionManager {
    /// `~/.reloopy/peripheral`
    base_dir: PathBuf,
    /// `~/.reloopy/peripheral/source` — the single git repo
    source_dir: PathBuf,
    /// `~/.reloopy/peripheral/binary` — fixed binary path
    binary_path: PathBuf,
    /// `~/.reloopy/peripheral/binary.rollback` — previous good binary
    rollback_binary_path: PathBuf,
    consecutive_failures: u32,
    locked: bool,
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
        let rollback_binary_path = peripheral.join("binary.rollback");
        Self {
            base_dir: peripheral,
            source_dir,
            binary_path,
            rollback_binary_path,
            consecutive_failures: 0,
            locked: false,
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

    /// Return the rollback binary path.
    pub fn rollback_binary_path(&self) -> &Path {
        &self.rollback_binary_path
    }

    /// Initialise a normal git repo at `source_dir` if it does not exist.
    /// If the repo exists but `main` is missing, attempts to recreate
    /// `main` from an existing V* branch rather than deleting everything.
    /// Only reinitialises from scratch when no branches exist at all.
    fn init_repo_if_needed(&self) -> Result<(), String> {
        let git_internal = self.source_dir.join(".git");
        if git_internal.exists() {
            // Check if the repo has at least one commit on any branch.
            let check_main = Command::new("git")
                .args(["rev-parse", "--verify", "refs/heads/main"])
                .current_dir(&self.source_dir)
                .output();
            match check_main {
                Ok(o) if o.status.success() => return Ok(()),
                _ => {}
            }

            // `main` is missing. Try to recover from the highest V* branch
            // instead of deleting the entire repo.
            let branch_list = Command::new("git")
                .args(["branch", "--list", "V*", "--format=%(refname:short)"])
                .current_dir(&self.source_dir)
                .output();
            if let Ok(bl) = branch_list {
                if bl.status.success() {
                    let stdout = String::from_utf8_lossy(&bl.stdout);
                    let mut max_branch: Option<(u32, String)> = None;
                    for line in stdout.lines() {
                        let name = line.trim();
                        if let Some(stripped) = name.strip_prefix('V') {
                            if let Ok(num) = stripped.parse::<u32>() {
                                if max_branch.as_ref().is_none_or(|(m, _)| num > *m) {
                                    max_branch = Some((num, name.to_string()));
                                }
                            }
                        }
                    }
                    if let Some((_n, branch)) = max_branch {
                        // Recreate `main` pointing at the same commit as the
                        // highest version branch.
                        let o = Command::new("git")
                            .args(["branch", "main", &branch])
                            .current_dir(&self.source_dir)
                            .output();
                        if let Ok(out) = o {
                            if out.status.success() {
                                tracing::info!(
                                    branch = %branch,
                                    "Recovered 'main' from existing version branch"
                                );
                                return Ok(());
                            }
                        }
                    }
                }
            }

            // No V* branches either — truly stale repo, remove and reinit.
            tracing::warn!(
                source_dir = %self.source_dir.display(),
                "Git repo exists but has no usable branches; re-initialising"
            );
            fs::remove_dir_all(&self.source_dir).map_err(|e| {
                format!(
                    "Failed to remove stale repo at {}: {}",
                    self.source_dir.display(),
                    e
                )
            })?;
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

    /// Persist the rollback target as a symbolic git ref so it survives restarts.
    fn persist_rollback_ref(&self, version: &str) -> Result<(), String> {
        let o = Command::new("git")
            .args([
                "symbolic-ref",
                ROLLBACK_REF,
                &format!("refs/heads/{}", version),
            ])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git symbolic-ref (rollback) failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git symbolic-ref (rollback) failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }
        Ok(())
    }

    /// Read the persisted rollback ref.  Returns the branch name (e.g. "V2")
    /// or `None` if the ref doesn't exist.
    fn read_rollback_ref(&self) -> Option<String> {
        if !self.source_dir.join(".git").exists() {
            return None;
        }
        let o = Command::new("git")
            .args(["symbolic-ref", "--short", ROLLBACK_REF])
            .current_dir(&self.source_dir)
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        let branch = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if let Some(stripped) = branch.strip_prefix('V') {
            if !stripped.is_empty() && stripped.parse::<u32>().is_ok() {
                return Some(branch);
            }
        }
        None
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

    /// Return the rollback version.  Reads from the persisted git ref
    /// `refs/reloopy/rollback` so it survives boot restarts.
    pub fn rollback_version(&self) -> Option<String> {
        self.read_rollback_ref()
    }

    /// Allocate a new version branch from the current HEAD **without**
    /// switching to it.  The caller should then copy source into
    /// `version_info.source_dir` and call `commit_version_source`.
    ///
    /// HEAD remains on the previously active branch so that
    /// `current_version()` continues to report the old version during
    /// compilation and testing.  Use `switch_to` after verification.
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

        // Create new branch from current HEAD without switching to it.
        let o = Command::new("git")
            .args(["branch", &version])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git branch {} failed: {}", version, e))?;
        if !o.status.success() {
            return Err(format!(
                "git branch {} failed: {}",
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

    /// Stage all files and commit on the given version branch.
    ///
    /// This temporarily checks out the target branch, commits, then
    /// restores the previously checked-out branch so HEAD is not left
    /// on the unverified version.
    /// Non-fatal: callers should log a warning on error but continue.
    pub fn commit_version_source(&self, version_info: &VersionInfo) -> Result<(), String> {
        let src = &version_info.source_dir;

        // Remember the current branch so we can return to it afterwards.
        let prev_branch = {
            let out = Command::new("git")
                .args(["branch", "--show-current"])
                .current_dir(src)
                .output()
                .map_err(|e| format!("git branch --show-current failed: {}", e))?;
            if !out.status.success() {
                return Err(format!(
                    "git branch --show-current failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        // Check out the target branch.
        let co = Command::new("git")
            .args(["checkout", &version_info.version])
            .current_dir(src)
            .output()
            .map_err(|e| format!("git checkout {} failed: {}", version_info.version, e))?;
        if !co.status.success() {
            return Err(format!(
                "git checkout {} failed: {}",
                version_info.version,
                String::from_utf8_lossy(&co.stderr)
            ));
        }

        let add = Command::new("git")
            .args(["add", "-A"])
            .current_dir(src)
            .output()
            .map_err(|e| format!("git add failed: {}", e))?;
        if !add.status.success() {
            // Restore before returning error.
            if let Ok(o) = Command::new("git")
                .args(["checkout", &prev_branch])
                .current_dir(src)
                .output()
            {
                if !o.status.success() {
                    tracing::warn!(
                        branch = %prev_branch,
                        "Failed to restore previous branch after git add failure: {}",
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
            }
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
            if let Ok(o) = Command::new("git")
                .args(["checkout", &prev_branch])
                .current_dir(src)
                .output()
            {
                if !o.status.success() {
                    tracing::warn!(
                        branch = %prev_branch,
                        "Failed to restore previous branch after commit failure: {}",
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
            }
            return Err(format!(
                "git commit failed: {}",
                String::from_utf8_lossy(&commit.stderr)
            ));
        }

        tracing::info!(version = %version_info.version, "Source committed to git branch");

        // Restore previous branch.
        if !prev_branch.is_empty() && prev_branch != version_info.version {
            if let Ok(o) = Command::new("git")
                .args(["checkout", &prev_branch])
                .current_dir(src)
                .output()
            {
                if !o.status.success() {
                    tracing::warn!(
                        branch = %prev_branch,
                        "Failed to restore previous branch after commit: {}",
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
            }
        }

        Ok(())
    }

    /// Copy source files from `from` into the source repo.
    ///
    /// Before copying, removes all existing files/directories in `to`
    /// except `.git` and `target` so that stale files from a previous
    /// version do not leak into the new one.  Then recursively copies
    /// from `from`, also skipping `.git` and `target`.
    pub fn copy_source(&self, from: &Path, to: &Path) -> Result<(), String> {
        // Clean destination: remove everything except .git and target.
        clean_dir_except(to, &[".git", "target"]).map_err(|e| {
            format!(
                "Failed to clean destination {}: {}",
                to.display(),
                e
            )
        })?;

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
    ///
    /// Persists the old version as the rollback target (as a git ref) and
    /// copies the current binary to `binary.rollback` before switching.
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

        // Save rollback binary before switching.
        if self.binary_path.exists() {
            if let Err(e) = fs::copy(&self.binary_path, &self.rollback_binary_path) {
                tracing::warn!("Failed to back up binary for rollback: {}", e);
            }
        }

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

        // Persist rollback ref.
        if let Some(ref old) = old_version {
            if let Err(e) = self.persist_rollback_ref(old) {
                tracing::warn!("Failed to persist rollback ref: {}", e);
            }
        }

        self.consecutive_failures = 0;

        tracing::info!(
            version = %version,
            old_version = ?old_version,
            "Version switched"
        );

        Ok(old_version.unwrap_or_default())
    }

    /// Rollback to the previous version.  Restores `binary.rollback` as
    /// the active binary.
    pub fn rollback(&mut self) -> Result<String, String> {
        let rollback_version = self
            .rollback_version()
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

        // Restore the rollback binary.
        if self.rollback_binary_path.exists() {
            if let Err(e) = fs::copy(&self.rollback_binary_path, &self.binary_path) {
                tracing::warn!("Failed to restore rollback binary: {}", e);
            }
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

    /// Check if the binary exists and belongs to the currently active version.
    /// Returns `true` only when `version` matches the current branch and the
    /// binary file is present.
    pub fn has_binary(&self, version: &str) -> bool {
        self.binary_path.exists()
            && self.current_version().as_deref() == Some(version)
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
        let rollback = self.rollback_version();
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

/// Remove everything in `dir` except the listed `keep` names.
fn clean_dir_except(dir: &Path, keep: &[&str]) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_lossy = name.to_string_lossy();
        if keep.contains(&name_lossy.as_ref()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
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
    fn allocate_v1_creates_branch_without_switching() {
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

        // HEAD should still be on `main` (allocate doesn't switch).
        let out = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&mgr.source_dir)
            .output()
            .unwrap();
        let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(current, "main", "HEAD must remain on main after allocate");
    }

    #[test]
    fn allocate_v2_does_not_switch_head() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        let v1 = mgr.allocate_version().expect("V1");
        assert_eq!(v1.version, "V1");
        // Switch to V1 explicitly (simulating successful deployment).
        mgr.switch_to("V1").unwrap();

        let v2 = mgr.allocate_version().expect("V2");
        assert_eq!(v2.version, "V2");
        assert!(v2.source_dir.exists());

        // HEAD must still be on V1, not V2.
        assert_eq!(mgr.current_version(), Some("V1".to_string()));
    }

    #[test]
    fn current_version_unaffected_by_allocate() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        assert_eq!(mgr.current_version(), None);

        mgr.allocate_version().expect("V1");
        // After allocate, HEAD is still on `main` (not a V* branch).
        assert_eq!(mgr.current_version(), None);

        mgr.switch_to("V1").unwrap();
        assert_eq!(mgr.current_version(), Some("V1".to_string()));

        mgr.allocate_version().expect("V2");
        // Still on V1.
        assert_eq!(mgr.current_version(), Some("V1".to_string()));
    }

    #[test]
    fn switch_and_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        mgr.allocate_version().expect("V1");
        mgr.switch_to("V1").unwrap();
        mgr.allocate_version().expect("V2");
        mgr.switch_to("V2").unwrap();

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
    fn rollback_persists_across_manager_instances() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut mgr = VersionManager::new(tmp.path());
            mgr.allocate_version().expect("V1");
            mgr.switch_to("V1").unwrap();
            mgr.allocate_version().expect("V2");
            mgr.switch_to("V2").unwrap();
            // rollback ref should now point at V1.
            assert_eq!(mgr.rollback_version(), Some("V1".to_string()));
        }
        // Create a fresh VersionManager from the same base_dir.
        let mgr2 = VersionManager::new(tmp.path());
        assert_eq!(
            mgr2.rollback_version(),
            Some("V1".to_string()),
            "Rollback ref must survive across manager instances"
        );
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
        mgr.switch_to("V1").unwrap();
        mgr.allocate_version().expect("V2");
        mgr.switch_to("V2").unwrap();
        mgr.allocate_version().expect("V3");
        mgr.switch_to("V3").unwrap();
        // Now on V3, rollback = V2.

        // Keep 0 beyond current (V3) and rollback (V2) → V1 should be removed.
        let removed = mgr.cleanup_old_versions(0).unwrap();
        assert_eq!(removed, vec!["V1"]);

        let remaining = mgr.list_versions();
        assert!(remaining.contains(&"V2".to_string()));
        assert!(remaining.contains(&"V3".to_string()));
        assert!(!remaining.contains(&"V1".to_string()));
    }

    #[test]
    fn stale_repo_without_branches_is_recovered() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());
        mgr.ensure_dirs().unwrap();

        // Create a repo but corrupt it by not committing so main is absent.
        fs::create_dir_all(&mgr.source_dir).unwrap();
        let o = Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&mgr.source_dir)
            .output()
            .unwrap();
        assert!(o.status.success());
        // No commit → no branches at all.

        // init_repo_if_needed should detect missing `main` with no V* branches
        // and re-init.
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
    fn missing_main_recovered_from_existing_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        // Set up a repo with V1 and V2 branches.
        mgr.allocate_version().expect("V1");
        mgr.switch_to("V1").unwrap();
        mgr.allocate_version().expect("V2");
        mgr.switch_to("V2").unwrap();

        // Manually delete `main` to simulate the scenario.
        let del = Command::new("git")
            .args(["branch", "-D", "main"])
            .current_dir(&mgr.source_dir)
            .output()
            .unwrap();
        assert!(del.status.success(), "should delete main");

        // init_repo_if_needed should recreate main from V2 (highest).
        mgr.init_repo_if_needed()
            .expect("should recover main from V2");

        // main now exists.
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/main"])
            .current_dir(&mgr.source_dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "main must exist after recovery");

        // V1 and V2 branches are preserved.
        assert!(mgr.has_source("V1"));
        assert!(mgr.has_source("V2"));
    }

    #[test]
    fn copy_source_cleans_stale_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let info = mgr.allocate_version().expect("V1");
        // Switch to V1 so we can commit there.
        let mut mgr = VersionManager::new(tmp.path());
        mgr.switch_to("V1").unwrap();

        // Write a file that should be removed when new source is copied.
        fs::write(info.source_dir.join("stale.txt"), b"old content").unwrap();

        // Build a staging directory WITHOUT stale.txt.
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();

        mgr.copy_source(&staging, &info.source_dir).unwrap();

        // stale.txt must have been removed.
        assert!(
            !info.source_dir.join("stale.txt").exists(),
            "stale.txt must be removed by copy_source"
        );
        // New files must exist.
        assert!(info.source_dir.join("Cargo.toml").exists());
        assert!(info.source_dir.join("src").join("main.rs").exists());
        // .git must still exist.
        assert!(info.source_dir.join(".git").exists());
    }

    #[test]
    fn copy_source_preserves_git() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let info = mgr.allocate_version().expect("V1 allocation");
        // Switch to V1 for git operations.
        let mut mgr = VersionManager::new(tmp.path());
        mgr.switch_to("V1").unwrap();

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
    fn has_binary_is_version_aware() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());
        mgr.allocate_version().expect("V1");
        mgr.switch_to("V1").unwrap();
        mgr.allocate_version().expect("V2");
        mgr.switch_to("V2").unwrap();

        // Create the binary file.
        fs::write(&mgr.binary_path, b"fake").unwrap();

        // Only reports true for the current version.
        assert!(mgr.has_binary("V2"));
        assert!(!mgr.has_binary("V1"));
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

    #[test]
    fn rollback_restores_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());
        mgr.ensure_dirs().unwrap();

        mgr.allocate_version().expect("V1");
        mgr.switch_to("V1").unwrap();

        // Simulate V1 binary.
        fs::write(&mgr.binary_path, b"v1-binary").unwrap();

        mgr.allocate_version().expect("V2");
        mgr.switch_to("V2").unwrap();
        // binary.rollback should now contain v1-binary.

        // Overwrite active binary with V2 binary.
        fs::write(&mgr.binary_path, b"v2-binary").unwrap();

        // Rollback to V1.
        mgr.rollback().unwrap();
        // Active binary should be restored from rollback.
        let content = fs::read_to_string(&mgr.binary_path).unwrap();
        assert_eq!(content, "v1-binary");
    }
}
