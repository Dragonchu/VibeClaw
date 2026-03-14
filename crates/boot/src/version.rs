use std::fs;
use std::path::{Path, PathBuf};

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

#[derive(Debug)]
pub struct VersionManager {
    base_dir: PathBuf,
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
        Self {
            base_dir: base_dir.join("peripheral"),
            consecutive_failures: 0,
            locked: false,
        }
    }

    fn ensure_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.base_dir)
    }

    pub fn current_version(&self) -> Option<String> {
        let current_link = self.base_dir.join("current");
        if !current_link.exists() {
            return None;
        }
        fs::read_link(&current_link)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
    }

    pub fn rollback_version(&self) -> Option<String> {
        let rollback_link = self.base_dir.join("rollback");
        if !rollback_link.exists() {
            return None;
        }
        fs::read_link(&rollback_link)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
    }

    fn next_version_number(&self) -> u32 {
        let mut max = 0u32;
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(stripped) = name.strip_prefix('v') {
                    if let Ok(num) = stripped.parse::<u32>() {
                        max = max.max(num);
                    }
                }
            }
        }
        max + 1
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

        let num = self.next_version_number();
        let version = format!("v{:03}", num);
        let dir = self.base_dir.join(&version);

        fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create version dir {}: {}", dir.display(), e))?;

        let source_dir = dir.join("source");
        fs::create_dir_all(&source_dir)
            .map_err(|e| format!("Failed to create source dir: {}", e))?;

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

        let current_link = self.base_dir.join("current");
        let rollback_link = self.base_dir.join("rollback");

        let old_version = self.current_version();

        if let Some(ref old) = old_version {
            let old_dir = self.base_dir.join(old);
            if old_dir.exists() {
                if rollback_link.exists() || rollback_link.is_symlink() {
                    fs::remove_file(&rollback_link)
                        .map_err(|e| format!("Failed to remove old rollback link: {}", e))?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&old_dir, &rollback_link)
                    .map_err(|e| format!("Failed to create rollback symlink: {}", e))?;
            }
        }

        if current_link.exists() || current_link.is_symlink() {
            fs::remove_file(&current_link)
                .map_err(|e| format!("Failed to remove old current link: {}", e))?;
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(&version_dir, &current_link)
            .map_err(|e| format!("Failed to create current symlink: {}", e))?;

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

        let current_link = self.base_dir.join("current");

        if current_link.exists() || current_link.is_symlink() {
            fs::remove_file(&current_link)
                .map_err(|e| format!("Failed to remove current link: {}", e))?;
        }

        let rollback_dir = self.base_dir.join(&rollback_version);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&rollback_dir, &current_link)
            .map_err(|e| format!("Failed to create current symlink for rollback: {}", e))?;

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
                if name.starts_with('v') && entry.path().is_dir() {
                    versions.push(name);
                }
            }
        }
        versions.sort();
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

        all.retain(|v| Some(v.as_str()) != current.as_deref() && Some(v.as_str()) != rollback.as_deref());

        if all.len() <= keep {
            return Ok(Vec::new());
        }

        let to_remove = all.len() - keep;
        let removable: Vec<String> = all.into_iter().take(to_remove).collect();
        let mut removed = Vec::new();

        for v in &removable {
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
