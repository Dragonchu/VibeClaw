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

use reloopy_ipc::LogErr;

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Git ref used to persist the rollback target across restarts.
const ROLLBACK_REF: &str = "refs/reloopy/rollback";

/// Compile-time path to the peripheral crate source tree.  Used as seed
/// source when initialising the `peripheral/source/` git repo for the
/// first time.  Set by `crates/boot/build.rs`.
const SEED_SOURCE: &str = env!("RELOOPY_SEED_SOURCE");

/// Compile-time path to the IPC crate source tree.  Copied alongside the
/// peripheral crate so the seed workspace can compile independently.
/// Set by `crates/boot/build.rs`.
const SEED_IPC: &str = env!("RELOOPY_SEED_IPC");

/// Root workspace `Cargo.toml`, embedded at compile time.  Used to derive
/// a standalone workspace manifest for the seed `peripheral/source/` repo.
const ROOT_WORKSPACE_TOML: &str = include_str!("../../../Cargo.toml");

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
    /// Isolated worktree directory for building this version
    /// (`peripheral/worktrees/<version>/`).  Populated after
    /// `commit_version_source` and removed by `cleanup_worktree`.
    pub build_dir: PathBuf,
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

    /// Return the deterministic worktree directory for a version.
    fn worktree_dir(&self, version: &str) -> PathBuf {
        self.base_dir.join("worktrees").join(version)
    }

    /// Return the build directory (worktree) for a version.
    /// The worktree is created by `commit_version_source` and should be
    /// cleaned up via `cleanup_worktree` after compilation finishes.
    pub fn build_dir(&self, version: &str) -> PathBuf {
        self.worktree_dir(version)
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

        // Build an independent workspace so the seed source is compilable
        // without the parent workspace.  Copy both crates/peripheral and
        // crates/ipc, then generate a standalone workspace Cargo.toml.
        let seed = Path::new(SEED_SOURCE);
        if !seed.is_dir() {
            return Err(format!(
                "Seed source directory not found at {}; cannot initialise peripheral repo",
                seed.display(),
            ));
        }
        let seed_ipc = Path::new(SEED_IPC);
        if !seed_ipc.is_dir() {
            return Err(format!(
                "Seed IPC directory not found at {}; cannot initialise peripheral repo",
                seed_ipc.display(),
            ));
        }

        let crates_dir = self.source_dir.join("crates");
        copy_dir_recursive(seed, &crates_dir.join("peripheral")).map_err(|e| {
            format!(
                "Failed to copy seed peripheral from {} to {}: {}",
                seed.display(),
                crates_dir.join("peripheral").display(),
                e,
            )
        })?;
        copy_dir_recursive(seed_ipc, &crates_dir.join("ipc")).map_err(|e| {
            format!(
                "Failed to copy seed ipc from {} to {}: {}",
                seed_ipc.display(),
                crates_dir.join("ipc").display(),
                e,
            )
        })?;

        // Write the standalone workspace manifest.
        let workspace_toml = generate_seed_workspace_toml();
        fs::write(self.source_dir.join("Cargo.toml"), workspace_toml)
            .map_err(|e| format!("Failed to write workspace Cargo.toml: {}", e))?;

        let o = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git add (init) failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git add (init) failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        let o = Command::new("git")
            .args(["commit", "-m", "Initial commit with seed source"])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git commit (init) failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git initial commit failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        tracing::info!(source_dir = %self.source_dir.display(), "Git repo initialised with seed source");
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
    /// switching to it.  The caller should then call
    /// `commit_version_source(version_info, source_from)` which checks
    /// out the new branch, copies source files, commits, and restores
    /// the previous branch.
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
            version: version.clone(),
            source_dir: self.source_dir.clone(),
            build_dir: self.worktree_dir(&version),
            binary_path: self.binary_path.clone(),
        })
    }

    /// Create an isolated worktree for the target version branch, copy
    /// source files into it, stage, and commit.
    ///
    /// Unlike the previous design that temporarily checked out the target
    /// branch in the main working directory (non-atomic, and left the
    /// wrong source checked out for the compiler), this uses
    /// `git worktree add` so the main repo HEAD is **never** moved.
    /// The worktree is kept alive for the compiler to build against;
    /// call `cleanup_worktree` once compilation is finished.
    pub fn commit_version_source(
        &self,
        version_info: &VersionInfo,
        source_from: &Path,
    ) -> Result<(), String> {
        let wt_dir = self.worktree_dir(&version_info.version);

        // Prune stale worktree bookkeeping entries left by a previous crash.
        if let Ok(o) = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.source_dir)
            .output()
        {
            if !o.status.success() {
                tracing::debug!(
                    "git worktree prune failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }

        // Remove an existing worktree for this version (previous failed attempt).
        if wt_dir.exists() {
            let remove_ok = Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &wt_dir.to_string_lossy().to_string(),
                ])
                .current_dir(&self.source_dir)
                .output()
                .is_ok_and(|o| o.status.success());
            if !remove_ok && wt_dir.exists() {
                tracing::warn!(
                    path = %wt_dir.display(),
                    "git worktree remove failed; falling back to manual removal"
                );
                fs::remove_dir_all(&wt_dir).warn_err();
                // Prune stale bookkeeping left by the manual removal so
                // `git worktree add` below does not fail with "already
                // checked out" / "already registered" errors.
                Command::new("git")
                    .args(["worktree", "prune"])
                    .current_dir(&self.source_dir)
                    .output()
                    .warn_err();
            }
        }

        // 1. Create a worktree on the version branch.
        let o = Command::new("git")
            .args([
                "worktree",
                "add",
                &wt_dir.to_string_lossy().to_string(),
                &version_info.version,
            ])
            .current_dir(&self.source_dir)
            .output()
            .map_err(|e| format!("git worktree add failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git worktree add {} failed: {}",
                version_info.version,
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        // 2. Copy source into the worktree.
        if let Err(e) = self.copy_source(source_from, &wt_dir) {
            self.cleanup_worktree_internal(&version_info.version);
            return Err(format!("Failed to copy source: {}", e));
        }

        // 3. Stage all changes.
        let add = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&wt_dir)
            .output()
            .map_err(|e| format!("git add failed: {}", e))?;
        if !add.status.success() {
            self.cleanup_worktree_internal(&version_info.version);
            return Err(format!(
                "git add -A failed: {}",
                String::from_utf8_lossy(&add.stderr)
            ));
        }

        // 4. Commit.
        let msg = format!("Version {}", version_info.version);
        let commit = Command::new("git")
            .args(["commit", "--allow-empty", "-m", &msg])
            .current_dir(&wt_dir)
            .output()
            .map_err(|e| format!("git commit failed: {}", e))?;
        if !commit.status.success() {
            self.cleanup_worktree_internal(&version_info.version);
            return Err(format!(
                "git commit failed: {}",
                String::from_utf8_lossy(&commit.stderr)
            ));
        }

        tracing::info!(
            version = %version_info.version,
            worktree = %wt_dir.display(),
            "Source committed via worktree (main HEAD unchanged)"
        );

        // Worktree is kept alive so the compiler can build in it.
        Ok(())
    }

    /// Remove the build worktree for a version.  Safe to call even if the
    /// worktree does not exist.
    pub fn cleanup_worktree(&self, version: &str) {
        self.cleanup_worktree_internal(version);
    }

    /// Internal: best-effort removal of a version's worktree.
    fn cleanup_worktree_internal(&self, version: &str) {
        let wt_dir = self.worktree_dir(version);
        if wt_dir.exists() {
            let remove_ok = Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &wt_dir.to_string_lossy().to_string(),
                ])
                .current_dir(&self.source_dir)
                .output()
                .is_ok_and(|o| o.status.success());
            // Fallback: manual removal if git failed.
            if !remove_ok && wt_dir.exists() {
                tracing::warn!(
                    version = %version,
                    path = %wt_dir.display(),
                    "git worktree remove failed; falling back to manual removal"
                );
                fs::remove_dir_all(&wt_dir).warn_err();
            }
        }
        if let Ok(o) = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.source_dir)
            .output()
        {
            if !o.status.success() {
                tracing::debug!(
                    "git worktree prune failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }
    }

    /// Copy source files from `from` into the target directory `to`.
    ///
    /// Before copying, removes all existing files/directories in `to`
    /// except `.git` (which may be a file in a worktree) and `target`
    /// so that stale files from a previous version do not leak into the
    /// new one.  Then recursively copies from `from`, also skipping
    /// `.git` and `target`.
    fn copy_source(&self, from: &Path, to: &Path) -> Result<(), String> {
        // Clean destination: remove everything except .git and target.
        clean_dir_except(to, &[".git", "target"])
            .map_err(|e| format!("Failed to clean destination {}: {}", to.display(), e))?;

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
    ///
    /// Any lingering build worktree for `version` is removed first so
    /// `git checkout` does not fail with "already checked out" errors.
    /// Untracked files are cleaned explicitly before checkout rather
    /// than relying on the destructive `git checkout -f`.
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

        // Remove the build worktree for this version if it still exists.
        // Git refuses to check out a branch that is already checked out
        // in another worktree.
        self.cleanup_worktree_internal(version);

        // Explicitly clean the working tree before switching branches.
        // 1. Discard modifications to tracked files.
        if let Ok(o) = Command::new("git")
            .args(["checkout", "--", "."])
            .current_dir(&self.source_dir)
            .output()
        {
            if !o.status.success() {
                tracing::warn!(
                    "Failed to discard tracked modifications: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }
        // 2. Remove untracked files (e.g. Cargo.lock from a previous
        //    build).  Preserve `target/` for the incremental build cache.
        if let Ok(o) = Command::new("git")
            .args(["clean", "-fd", "-e", "target/"])
            .current_dir(&self.source_dir)
            .output()
        {
            if !o.status.success() {
                tracing::warn!(
                    "Failed to clean untracked files: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
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

        // Remove any lingering build worktree.
        self.cleanup_worktree_internal(&rollback_version);

        // Explicitly clean the working tree before switching branches.
        if let Ok(o) = Command::new("git")
            .args(["checkout", "--", "."])
            .current_dir(&self.source_dir)
            .output()
        {
            if !o.status.success() {
                tracing::warn!(
                    "Rollback: failed to discard tracked modifications: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }
        if let Ok(o) = Command::new("git")
            .args(["clean", "-fd", "-e", "target/"])
            .current_dir(&self.source_dir)
            .output()
        {
            if !o.status.success() {
                tracing::warn!(
                    "Rollback: failed to clean untracked files: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }

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
        self.binary_path.exists() && self.current_version().as_deref() == Some(version)
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

/// Generate a standalone workspace `Cargo.toml` from the root workspace
/// manifest.  The members list is replaced to contain only `crates/ipc`
/// and `crates/peripheral` so the seed repo can compile independently.
fn generate_seed_workspace_toml() -> String {
    let mut result = String::new();
    let mut in_members = false;
    for line in ROOT_WORKSPACE_TOML.lines() {
        if line.starts_with("members") {
            result.push_str("members = [\n");
            result.push_str("    \"crates/ipc\",\n");
            result.push_str("    \"crates/peripheral\",\n");
            result.push_str("]\n");
            in_members = true;
            continue;
        }
        if in_members {
            // Skip original member entries until closing `]`.
            if line.trim() == "]" {
                in_members = false;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_v1_creates_branch_without_switching() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let info = mgr
            .allocate_version()
            .expect("V1 allocation should succeed");
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
        fs::write(staging.join(".git").join("HEAD"), "ref: refs/heads/fake\n").unwrap();
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

        // Build a staging directory to feed into commit_version_source.
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();

        mgr.commit_version_source(&info, &staging).unwrap();

        let detail = mgr.version_detail("V1").unwrap();
        assert_eq!(detail["version"], "V1");
        assert!(!detail["commit"].as_str().unwrap().is_empty());
    }

    /// Regression test: files present in the staging directory (e.g.
    /// `Cargo.lock`) must not cause an "untracked working tree files
    /// would be overwritten" error during `commit_version_source`,
    /// because the checkout now happens **before** the copy.
    #[test]
    fn commit_version_source_with_new_files_does_not_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let v1 = mgr.allocate_version().expect("V1");

        // Build a staging dir that includes Cargo.lock (not tracked on main).
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::write(staging.join("Cargo.lock"), b"# lock file\n").unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();

        // This used to fail because copy_source was called while still on
        // main, leaving Cargo.lock as untracked, then git checkout V1
        // refused to overwrite it.
        mgr.commit_version_source(&v1, &staging)
            .expect("commit_version_source must succeed even with new files");

        // HEAD should be restored to main.
        let out = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&v1.source_dir)
            .output()
            .unwrap();
        let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(current, "main", "HEAD must be restored to main");

        // V1 branch must contain the committed Cargo.lock.
        let show = Command::new("git")
            .args(["show", "V1:Cargo.lock"])
            .current_dir(&v1.source_dir)
            .output()
            .unwrap();
        assert!(
            show.status.success(),
            "Cargo.lock must be committed on V1 branch"
        );
    }

    #[test]
    fn init_copies_seed_source_with_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        // allocate_version triggers init_repo_if_needed which should copy
        // the peripheral seed source into the repo as a workspace.
        let info = mgr.allocate_version().expect("V1");

        // The workspace root must have a Cargo.toml.
        assert!(
            info.source_dir.join("Cargo.toml").exists(),
            "Workspace Cargo.toml must exist in source dir after init"
        );
        // Peripheral crate must be under crates/peripheral/.
        assert!(
            info.source_dir
                .join("crates")
                .join("peripheral")
                .join("Cargo.toml")
                .exists(),
            "crates/peripheral/Cargo.toml must exist in source dir after init"
        );
        // IPC crate must be under crates/ipc/.
        assert!(
            info.source_dir
                .join("crates")
                .join("ipc")
                .join("Cargo.toml")
                .exists(),
            "crates/ipc/Cargo.toml must exist in source dir after init"
        );
        // Also verify `src/main.rs` was copied (the binary entry point).
        assert!(
            info.source_dir
                .join("crates")
                .join("peripheral")
                .join("src")
                .join("main.rs")
                .exists(),
            "crates/peripheral/src/main.rs must exist in source dir after init"
        );

        // The initial commit should track the copied files.
        let out = Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(&info.source_dir)
            .output()
            .unwrap();
        let msg = String::from_utf8_lossy(&out.stdout);
        assert!(
            msg.contains("seed source"),
            "initial commit message should mention seed source"
        );
    }

    #[test]
    fn seed_workspace_toml_has_correct_members() {
        let toml = generate_seed_workspace_toml();
        // Must contain the two required members.
        assert!(
            toml.contains("\"crates/ipc\""),
            "workspace must list crates/ipc as member"
        );
        assert!(
            toml.contains("\"crates/peripheral\""),
            "workspace must list crates/peripheral as member"
        );
        // Must NOT contain other workspace members from the root.
        assert!(
            !toml.contains("\"crates/boot\""),
            "workspace must not list crates/boot"
        );
        assert!(
            !toml.contains("\"crates/admin\""),
            "workspace must not list crates/admin"
        );
        // Must preserve workspace-level package and dependencies.
        assert!(
            toml.contains("[workspace.package]"),
            "workspace must have [workspace.package]"
        );
        assert!(
            toml.contains("[workspace.dependencies]"),
            "workspace must have [workspace.dependencies]"
        );
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

    /// Untracked files in the working tree (e.g. `Cargo.lock` generated by
    /// `cargo build`) must not block `switch_to` or `rollback`.
    #[test]
    fn switch_to_succeeds_despite_untracked_cargo_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        let v1 = mgr.allocate_version().expect("V1");

        // Commit source that includes Cargo.lock on V1.
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::write(staging.join("Cargo.lock"), b"# lock v1\n").unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();
        mgr.commit_version_source(&v1, &staging).unwrap();

        // HEAD is back on main.  Now drop an *untracked* Cargo.lock into
        // the working tree — simulates `cargo build` having generated it.
        fs::write(v1.source_dir.join("Cargo.lock"), b"# lock untracked\n").unwrap();

        // switch_to must not fail with "untracked working tree files".
        mgr.switch_to("V1")
            .expect("switch_to must succeed even with an untracked Cargo.lock");

        assert_eq!(mgr.current_version(), Some("V1".to_string()));
    }

    /// Untracked files must not block `rollback` either.
    #[test]
    fn rollback_succeeds_despite_untracked_cargo_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        let v1 = mgr.allocate_version().expect("V1");
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::write(staging.join("Cargo.lock"), b"# lock v1\n").unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();
        mgr.commit_version_source(&v1, &staging).unwrap();
        mgr.switch_to("V1").unwrap();

        let v2 = mgr.allocate_version().expect("V2");
        fs::write(staging.join("Cargo.lock"), b"# lock v2\n").unwrap();
        mgr.commit_version_source(&v2, &staging).unwrap();
        mgr.switch_to("V2").unwrap();

        // Drop an untracked Cargo.lock while on V2.
        fs::write(v2.source_dir.join("Cargo.lock"), b"# lock untracked\n").unwrap();

        // Rollback to V1 must not fail.
        mgr.rollback()
            .expect("rollback must succeed even with an untracked Cargo.lock");
    }

    /// Worktree-based commit must not move the main repo HEAD.
    #[test]
    fn commit_via_worktree_preserves_main_head() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let v1 = mgr.allocate_version().expect("V1");

        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();

        // HEAD should be on `main` before and after commit.
        let head_before = git_current_branch(&mgr.source_dir);
        assert_eq!(head_before, "main");

        mgr.commit_version_source(&v1, &staging).unwrap();

        let head_after = git_current_branch(&mgr.source_dir);
        assert_eq!(
            head_after, "main",
            "HEAD must stay on main after worktree commit"
        );

        // Clean up.
        mgr.cleanup_worktree("V1");
    }

    /// The build worktree must contain the committed source — not the
    /// source from the parent branch.  This is the scenario that was
    /// broken in the old temporary-checkout design: the compiler would
    /// receive the PREVIOUS version's files.
    #[test]
    fn build_dir_contains_correct_version_source() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        // V1: source with "v1" marker.
        let v1 = mgr.allocate_version().expect("V1");
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(
            staging.join("Cargo.toml"),
            b"[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"v1\"); }\n",
        )
        .unwrap();
        mgr.commit_version_source(&v1, &staging).unwrap();

        // The build_dir worktree must have V1's source.
        let content = fs::read_to_string(v1.build_dir.join("src").join("main.rs")).unwrap();
        assert!(
            content.contains("v1"),
            "V1 build_dir must contain V1 source, got: {}",
            content
        );

        // Switch to V1 and clean up the worktree.
        mgr.switch_to("V1").unwrap();

        // V2: source with "v2" marker.
        let v2 = mgr.allocate_version().expect("V2");
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"v2\"); }\n",
        )
        .unwrap();
        mgr.commit_version_source(&v2, &staging).unwrap();

        // V2's build_dir must have V2's source, NOT V1's.
        let content = fs::read_to_string(v2.build_dir.join("src").join("main.rs")).unwrap();
        assert!(
            content.contains("v2"),
            "V2 build_dir must contain V2 source, got: {}",
            content
        );

        // Main working directory must still show V1's source.
        let main_content =
            fs::read_to_string(mgr.source_dir().join("src").join("main.rs")).unwrap();
        assert!(
            main_content.contains("v1"),
            "Main source_dir must still show V1 (current), got: {}",
            main_content
        );
        assert_eq!(mgr.current_version(), Some("V1".to_string()));

        mgr.cleanup_worktree("V2");
    }

    /// End-to-end test: allocate → commit → switch, verifying that the
    /// correct source is visible in the main working directory after
    /// switching to a version.
    #[test]
    fn switch_shows_correct_source_after_worktree_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();

        // V1
        fs::write(
            staging.join("Cargo.toml"),
            b"[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"v1\"); }\n",
        )
        .unwrap();
        let v1 = mgr.allocate_version().expect("V1");
        mgr.commit_version_source(&v1, &staging).unwrap();
        mgr.switch_to("V1").unwrap();

        let c1 = fs::read_to_string(mgr.source_dir().join("src").join("main.rs")).unwrap();
        assert!(c1.contains("v1"), "After switch to V1: {}", c1);

        // V2
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"v2\"); }\n",
        )
        .unwrap();
        let v2 = mgr.allocate_version().expect("V2");
        mgr.commit_version_source(&v2, &staging).unwrap();
        mgr.switch_to("V2").unwrap();

        let c2 = fs::read_to_string(mgr.source_dir().join("src").join("main.rs")).unwrap();
        assert!(c2.contains("v2"), "After switch to V2: {}", c2);

        // V3
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"v3\"); }\n",
        )
        .unwrap();
        let v3 = mgr.allocate_version().expect("V3");
        mgr.commit_version_source(&v3, &staging).unwrap();
        mgr.switch_to("V3").unwrap();

        let c3 = fs::read_to_string(mgr.source_dir().join("src").join("main.rs")).unwrap();
        assert!(c3.contains("v3"), "After switch to V3: {}", c3);

        // Rollback to V2.
        let rolled = mgr.rollback().unwrap();
        assert_eq!(rolled, "V2");
        let c_rollback = fs::read_to_string(mgr.source_dir().join("src").join("main.rs")).unwrap();
        assert!(
            c_rollback.contains("v2"),
            "After rollback to V2: {}",
            c_rollback
        );
    }

    /// Switching to a branch and running `cargo check` must succeed.
    /// This is the key "branch switching + compilation" scenario.
    #[test]
    fn switch_branch_then_cargo_check_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = VersionManager::new(tmp.path());

        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(
            staging.join("Cargo.toml"),
            b"[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"v1\"); }\n",
        )
        .unwrap();

        let v1 = mgr.allocate_version().expect("V1");
        mgr.commit_version_source(&v1, &staging).unwrap();
        mgr.switch_to("V1").unwrap();

        // Run `cargo check` in the main source directory after switching.
        let check_output = Command::new("cargo")
            .args(["check"])
            .current_dir(mgr.source_dir())
            .output()
            .expect("cargo check should launch");
        assert!(
            check_output.status.success(),
            "cargo check must succeed after switch_to V1: {}",
            String::from_utf8_lossy(&check_output.stderr)
        );

        // V2 with different source.
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"v2\"); }\n",
        )
        .unwrap();
        let v2 = mgr.allocate_version().expect("V2");
        mgr.commit_version_source(&v2, &staging).unwrap();
        mgr.switch_to("V2").unwrap();

        let check_output2 = Command::new("cargo")
            .args(["check"])
            .current_dir(mgr.source_dir())
            .output()
            .expect("cargo check should launch");
        assert!(
            check_output2.status.success(),
            "cargo check must succeed after switch_to V2: {}",
            String::from_utf8_lossy(&check_output2.stderr)
        );
    }

    /// The build worktree must be compilable independently, so the
    /// compiler service can build in it without affecting the main repo.
    #[test]
    fn build_worktree_is_independently_compilable() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(
            staging.join("Cargo.toml"),
            b"[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            staging.join("src").join("main.rs"),
            b"fn main() { println!(\"hello\"); }\n",
        )
        .unwrap();

        let v1 = mgr.allocate_version().expect("V1");
        mgr.commit_version_source(&v1, &staging).unwrap();

        // Compile in the worktree (build_dir), not in the main source_dir.
        let check = Command::new("cargo")
            .args(["check"])
            .current_dir(&v1.build_dir)
            .output()
            .expect("cargo check should launch");
        assert!(
            check.status.success(),
            "cargo check must succeed in the build worktree: {}",
            String::from_utf8_lossy(&check.stderr)
        );

        // Main HEAD is still on `main`.
        let head = git_current_branch(&mgr.source_dir);
        assert_eq!(
            head, "main",
            "HEAD must remain on main during worktree build"
        );

        mgr.cleanup_worktree("V1");
    }

    /// `cleanup_worktree` must remove the worktree directory and prune
    /// git's internal bookkeeping.
    #[test]
    fn cleanup_worktree_removes_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = VersionManager::new(tmp.path());

        let v1 = mgr.allocate_version().expect("V1");
        let staging = tmp.path().join("staging");
        fs::create_dir_all(staging.join("src")).unwrap();
        fs::write(staging.join("Cargo.toml"), b"[package]\nname=\"test\"\n").unwrap();
        fs::write(staging.join("src").join("main.rs"), b"fn main() {}").unwrap();
        mgr.commit_version_source(&v1, &staging).unwrap();

        assert!(v1.build_dir.exists(), "worktree must exist after commit");

        mgr.cleanup_worktree("V1");

        assert!(
            !v1.build_dir.exists(),
            "worktree must be removed after cleanup"
        );

        // The branch itself should still exist.
        assert!(
            mgr.has_source("V1"),
            "V1 branch must still exist after worktree cleanup"
        );
    }

    // -- helper --

    fn git_current_branch(repo: &Path) -> String {
        let out = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}
