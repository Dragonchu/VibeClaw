use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

#[derive(Debug)]
pub struct VersionManager {
    base_dir: PathBuf,
    git_dir: PathBuf,
    consecutive_failures: u32,
    locked: bool,
}

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub version: String,
    pub dir: PathBuf,
    pub source_dir: PathBuf,
    pub binary_path: PathBuf,
    pub manifest_path: PathBuf,
}

impl VersionManager {
    pub fn new(base_dir: &Path) -> Self {
        let peripheral = base_dir.join("peripheral");
        let git_dir = peripheral.join("git");
        Self {
            base_dir: peripheral,
            git_dir,
            consecutive_failures: 0,
            locked: false,
        }
    }

    fn ensure_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.base_dir)
    }

    /// Read the current active version name from the `current` text file.
    pub fn current_version(&self) -> Option<String> {
        let current_file = self.base_dir.join("current");
        if !current_file.exists() {
            return None;
        }
        fs::read_to_string(&current_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Read the rollback version name from the `rollback` text file.
    pub fn rollback_version(&self) -> Option<String> {
        let rollback_file = self.base_dir.join("rollback");
        if !rollback_file.exists() {
            return None;
        }
        fs::read_to_string(&rollback_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn next_version_number(&self) -> u32 {
        let mut max = 0u32;
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(stripped) = name.strip_prefix('V') {
                    if let Ok(num) = stripped.parse::<u32>() {
                        max = max.max(num);
                    }
                }
            }
        }
        max + 1
    }

    /// Initialise the bare git repo at `base_dir/git/` if it does not already exist.
    fn init_git_repo_if_needed(&self) -> Result<(), String> {
        if self.git_dir.join("HEAD").exists() {
            return Ok(());
        }

        fs::create_dir_all(&self.git_dir)
            .map_err(|e| format!("Failed to create git dir: {}", e))?;

        let out = Command::new("git")
            .args(["init", "--bare"])
            .arg(&self.git_dir)
            .output()
            .map_err(|e| format!("Failed to run git init --bare: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "git init --bare failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }

        // Configure identity so commits succeed in CI / headless environments.
        for (key, value) in [
            ("user.name", "loopy-boot"),
            ("user.email", "boot@loopy.local"),
        ] {
            let o = Command::new("git")
                .args(["config", key, value])
                .arg("--file")
                .arg(self.git_dir.join("config"))
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

        // Create an empty initial commit on `main` so later branches have a base.
        let tmp = self.base_dir.join(".git_init_tmp");
        let _ = fs::remove_dir_all(&tmp);
        let o = Command::new("git")
            .args(["worktree", "add", "--orphan", "-b", "main"])
            .arg(&tmp)
            .arg("--")
            .env("GIT_DIR", &self.git_dir)
            .output()
            .map_err(|e| format!("git worktree add (init) failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git worktree add for init failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        let o = Command::new("git")
            .args(["commit", "--allow-empty", "-m", "Initial commit"])
            .current_dir(&tmp)
            .env("GIT_DIR", self.git_dir.join(".git").as_os_str().to_os_string().to_string_lossy().as_ref())
            .output()
            .map_err(|e| format!("git commit (init) failed: {}", e))?;

        // Remove temp worktree regardless of commit outcome.
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&tmp)
            .env("GIT_DIR", &self.git_dir)
            .output();
        let _ = fs::remove_dir_all(&tmp);

        if !o.status.success() {
            return Err(format!(
                "git initial commit failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        tracing::info!(git_dir = %self.git_dir.display(), "Git bare repo initialised");
        Ok(())
    }

    /// Commit all source files in `version_info.source_dir` to branch `V{N}`.
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

    pub fn allocate_version(&self) -> Result<VersionInfo, String> {
        self.ensure_dirs()
            .map_err(|e| format!("Failed to create peripheral dir: {}", e))?;

        if self.locked {
            return Err(
                "Version manager is locked due to consecutive failures. Human intervention required."
                    .to_string(),
            );
        }

        self.init_git_repo_if_needed()?;

        let num = self.next_version_number();
        let version = format!("V{}", num);
        let dir = self.base_dir.join(&version);

        fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create version dir {}: {}", dir.display(), e))?;

        let source_dir = dir.join("source");

        // Create git worktree for this version's branch.
        let base_branch = if num == 1 {
            "main".to_string()
        } else {
            format!("V{}", num - 1)
        };

        let o = Command::new("git")
            .args(["worktree", "add", "-b", &version])
            .arg(&source_dir)
            .arg(&base_branch)
            .env("GIT_DIR", &self.git_dir)
            .output()
            .map_err(|e| format!("git worktree add failed: {}", e))?;
        if !o.status.success() {
            return Err(format!(
                "git worktree add for {} failed: {}",
                version,
                String::from_utf8_lossy(&o.stderr)
            ));
        }

        tracing::info!(version = %version, "Git worktree created for new version");

        Ok(VersionInfo {
            version,
            source_dir,
            binary_path: dir.join("binary"),
            manifest_path: dir.join("manifest.json"),
            dir,
        })
    }

    pub fn switch_to(&mut self, version: &str) -> Result<String, String> {
        let version_dir = self.base_dir.join(version);
        if !version_dir.exists() {
            return Err(format!("Version directory does not exist: {}", version));
        }

        let old_version = self.current_version();

        if let Some(ref old) = old_version {
            fs::write(self.base_dir.join("rollback"), old.as_bytes())
                .map_err(|e| format!("Failed to write rollback file: {}", e))?;
        }

        fs::write(self.base_dir.join("current"), version.as_bytes())
            .map_err(|e| format!("Failed to write current file: {}", e))?;

        self.consecutive_failures = 0;

        tracing::info!(
            version = %version,
            old_version = ?old_version,
            "Version switched"
        );

        Ok(old_version.unwrap_or_default())
    }

    pub fn rollback(&mut self) -> Result<String, String> {
        let rollback_version = self
            .rollback_version()
            .ok_or("No rollback version available")?;

        fs::write(self.base_dir.join("current"), rollback_version.as_bytes())
            .map_err(|e| format!("Failed to write current file during rollback: {}", e))?;

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

    pub fn list_versions(&self) -> Vec<String> {
        let mut versions = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(stripped) = name.strip_prefix('V') {
                    if stripped.parse::<u32>().is_ok() && entry.path().is_dir() {
                        versions.push(name);
                    }
                }
            }
        }
        versions.sort_by_key(|v| v.strip_prefix('V').and_then(|n| n.parse::<u32>().ok()).unwrap_or(0));
        versions
    }

    pub fn version_detail(&self, version: &str) -> Result<serde_json::Value, String> {
        let version_dir = self.base_dir.join(version);
        if !version_dir.exists() {
            return Err(format!("Version directory does not exist: {}", version));
        }

        let manifest_path = version_dir.join("manifest.json");
        let manifest = if manifest_path.exists() {
            let content = fs::read_to_string(&manifest_path)
                .map_err(|e| format!("Failed to read manifest: {}", e))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse manifest: {}", e))?
        } else {
            serde_json::Value::Null
        };

        Ok(manifest)
    }

    pub fn has_binary(&self, version: &str) -> bool {
        self.base_dir.join(version).join("binary").exists()
    }

    pub fn has_source(&self, version: &str) -> bool {
        self.base_dir.join(version).join("source").is_dir()
    }

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
            let source_dir = self.base_dir.join(v).join("source");

            // Remove git worktree before deleting the directory.
            let wt_out = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&source_dir)
                .env("GIT_DIR", &self.git_dir)
                .output();
            if let Ok(o) = &wt_out {
                if !o.status.success() {
                    tracing::warn!(
                        version = %v,
                        "git worktree remove failed (may already be gone): {}",
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
            }

            let dir = self.base_dir.join(v);
            if dir.exists() {
                if let Err(e) = fs::remove_dir_all(&dir) {
                    tracing::error!(version = %v, "Failed to remove version directory: {}", e);
                    continue;
                }
                removed.push(v.clone());
                tracing::info!(version = %v, "Old version cleaned up");
            }
        }

        Ok(removed)
    }

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

    pub fn install_binary(
        &self,
        built_binary: &Path,
        version_info: &VersionInfo,
    ) -> Result<(), String> {
        if !built_binary.exists() {
            return Err(format!(
                "Built binary not found: {}",
                built_binary.display()
            ));
        }
        fs::copy(built_binary, &version_info.binary_path)
            .map_err(|e| format!("Failed to copy binary: {}", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&version_info.binary_path, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("Failed to set binary permissions: {}", e))?;
        }

        Ok(())
    }

    pub fn write_manifest(&self, version_info: &VersionInfo) -> Result<(), String> {
        let manifest = serde_json::json!({
            "version": version_info.version,
            "created_at": chrono_now_iso(),
            "source_dir": version_info.source_dir.to_string_lossy(),
            "binary_path": version_info.binary_path.to_string_lossy(),
        });

        let content = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
        fs::write(&version_info.manifest_path, content)
            .map_err(|e| format!("Failed to write manifest: {}", e))?;

        Ok(())
    }
}

fn chrono_now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", now.as_secs())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
