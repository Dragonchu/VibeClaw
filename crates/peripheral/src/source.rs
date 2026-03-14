use std::fs;
use std::path::{Path, PathBuf};

pub struct SourceManager {
    workspace_root: PathBuf,
    peripheral_root: PathBuf,
    staging_dir: Option<PathBuf>,
}

impl SourceManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        let peripheral_root = workspace_root.join("crates").join("peripheral");
        Self {
            workspace_root,
            peripheral_root,
            staging_dir: None,
        }
    }

    pub fn peripheral_root(&self) -> &Path {
        &self.peripheral_root
    }

    pub fn read_file(&self, relative_path: &str) -> Result<String, String> {
        let path = self.peripheral_root.join(relative_path);
        if !path.exists() {
            return Err(format!("File not found: {}", relative_path));
        }
        if !path.starts_with(&self.peripheral_root) {
            return Err("Path traversal not allowed".to_string());
        }
        fs::read_to_string(&path).map_err(|e| format!("Read error: {}", e))
    }

    pub fn list_files(&self, relative_path: &str) -> Result<Vec<String>, String> {
        let dir = self.peripheral_root.join(relative_path);
        if !dir.exists() {
            return Err(format!("Directory not found: {}", relative_path));
        }
        if !dir.starts_with(&self.peripheral_root) {
            return Err("Path traversal not allowed".to_string());
        }

        let mut files = Vec::new();
        collect_files_recursive(&dir, &self.peripheral_root, &mut files)
            .map_err(|e| format!("List error: {}", e))?;
        files.sort();
        Ok(files)
    }

    pub fn write_staged_file(&mut self, relative_path: &str, content: &str) -> Result<(), String> {
        if self.staging_dir.is_none() {
            self.init_staging()?;
        }

        let staging = self.staging_dir.as_ref().unwrap();
        let target = staging
            .join("crates")
            .join("peripheral")
            .join(relative_path);

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create dirs: {}", e))?;
        }

        fs::write(&target, content).map_err(|e| format!("Write error: {}", e))?;
        tracing::info!(path = %relative_path, "Staged file written");
        Ok(())
    }

    pub fn pack_workspace(&self) -> Result<PathBuf, String> {
        let staging = self
            .staging_dir
            .as_ref()
            .ok_or("No changes staged — call write_source_file first")?;

        let peripheral_staging = staging.join("crates").join("peripheral");
        if !peripheral_staging.exists() {
            return Err("No peripheral files staged".to_string());
        }

        Ok(staging.clone())
    }

    pub fn reset_staging(&mut self) {
        if let Some(ref dir) = self.staging_dir {
            let _ = fs::remove_dir_all(dir);
        }
        self.staging_dir = None;
    }

    fn init_staging(&mut self) -> Result<(), String> {
        let base = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join(".loopy")
            .join("staging");
        fs::create_dir_all(&base).map_err(|e| format!("Failed to create staging base: {}", e))?;

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let staging = base.join(format!("stage-{}", ts));
        fs::create_dir_all(&staging)
            .map_err(|e| format!("Failed to create staging dir: {}", e))?;

        copy_workspace_skeleton(&self.workspace_root, &staging)?;
        self.staging_dir = Some(staging);
        Ok(())
    }
}

fn copy_workspace_skeleton(workspace: &Path, staging: &Path) -> Result<(), String> {
    let workspace_toml = workspace.join("Cargo.toml");
    if workspace_toml.exists() {
        fs::copy(&workspace_toml, staging.join("Cargo.toml"))
            .map_err(|e| format!("Failed to copy workspace Cargo.toml: {}", e))?;
    }

    let ipc_src = workspace.join("crates").join("ipc");
    let ipc_dst = staging.join("crates").join("ipc");
    if ipc_src.exists() {
        copy_dir_recursive(&ipc_src, &ipc_dst)
            .map_err(|e| format!("Failed to copy ipc crate: {}", e))?;
    }

    let peripheral_src = workspace.join("crates").join("peripheral");
    let peripheral_dst = staging.join("crates").join("peripheral");
    if peripheral_src.exists() {
        copy_dir_recursive(&peripheral_src, &peripheral_dst)
            .map_err(|e| format!("Failed to copy peripheral crate: {}", e))?;
    }

    let lock_file = workspace.join("Cargo.lock");
    if lock_file.exists() {
        fs::copy(&lock_file, staging.join("Cargo.lock"))
            .map_err(|e| format!("Failed to copy Cargo.lock: {}", e))?;
    }

    rewrite_workspace_toml(staging)?;

    Ok(())
}

fn rewrite_workspace_toml(staging: &Path) -> Result<(), String> {
    let content = r#"[workspace]
resolver = "3"
members = [
    "crates/ipc",
    "crates/peripheral",
]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
reqwest = { version = "0.12", features = ["json"] }
loopy-ipc = { path = "crates/ipc" }
"#;

    fs::write(staging.join("Cargo.toml"), content)
        .map_err(|e| format!("Failed to write workspace Cargo.toml: {}", e))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
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

fn collect_files_recursive(
    dir: &Path,
    base: &Path,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, base, out)?;
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
    Ok(())
}
