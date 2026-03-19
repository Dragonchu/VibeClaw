use std::fs;
use std::path::{Path, PathBuf};

pub struct SourceManager {
    workspace_root: PathBuf,
    peripheral_root: PathBuf,
}

impl SourceManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        let peripheral_root = workspace_root.join("crates").join("peripheral");
        Self {
            workspace_root,
            peripheral_root,
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn peripheral_root(&self) -> &Path {
        &self.peripheral_root
    }

    pub fn read_file(&self, relative_path: &str) -> Result<String, String> {
        let path = self.peripheral_root.join(relative_path);
        if !path.starts_with(&self.peripheral_root) {
            return Err("Path traversal not allowed".to_string());
        }
        if !path.exists() {
            return Err(format!("File not found: {}", relative_path));
        }
        if path.is_dir() {
            return Err(format!(
                "Path is a directory, not a file: {}",
                relative_path
            ));
        }
        fs::read_to_string(&path).map_err(|e| format!("Read error: {}", e))
    }

    pub fn list_files(&self, relative_path: &str) -> Result<Vec<String>, String> {
        let dir = self.peripheral_root.join(relative_path);
        if !dir.starts_with(&self.peripheral_root) {
            return Err("Path traversal not allowed".to_string());
        }
        if !dir.exists() {
            return Err(format!("Directory not found: {}", relative_path));
        }

        let mut files = Vec::new();
        collect_files_recursive(&dir, &self.peripheral_root, &mut files)
            .map_err(|e| format!("List error: {}", e))?;
        files.sort();
        Ok(files)
    }

    /// Write content directly to a file in the peripheral workspace.
    pub fn write_file(&mut self, relative_path: &str, content: &str) -> Result<(), String> {
        let target = self.peripheral_root.join(relative_path);
        if !target.starts_with(&self.peripheral_root) {
            return Err("Path traversal not allowed".to_string());
        }
        if target.is_dir() {
            return Err(format!(
                "Path is a directory, not a file: {}",
                relative_path
            ));
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create dirs: {}", e))?;
        }

        fs::write(&target, content).map_err(|e| format!("Write error: {}", e))?;
        tracing::info!(path = %relative_path, "Source file written");
        Ok(())
    }
}

fn collect_files_recursive(dir: &Path, base: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_source() -> (tempfile::TempDir, SourceManager) {
        let dir = tempfile::tempdir().expect("tempdir");
        // SourceManager expects workspace_root; peripheral_root = workspace_root/crates/peripheral
        let peripheral = dir.path().join("crates").join("peripheral");
        fs::create_dir_all(peripheral.join("src")).expect("create src dir");
        let mgr = SourceManager::new(dir.path().to_path_buf());
        (dir, mgr)
    }

    #[test]
    fn write_file_to_existing_directory_returns_error() {
        let (_dir, mut mgr) = temp_source();
        // "src" is an existing directory
        let result = mgr.write_file("src", "hello");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("directory"),
            "expected directory error, got: {}",
            err
        );
    }

    #[test]
    fn write_file_to_dot_returns_error() {
        let (_dir, mut mgr) = temp_source();
        // "." resolves to peripheral_root itself, which is a directory
        let result = mgr.write_file(".", "hello");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("directory"),
            "expected directory error, got: {}",
            err
        );
    }

    #[test]
    fn write_file_normal_path_succeeds() {
        let (_dir, mut mgr) = temp_source();
        mgr.write_file("src/test.rs", "fn main() {}")
            .expect("write should succeed");
        let content = mgr.read_file("src/test.rs").expect("read should succeed");
        assert_eq!(content, "fn main() {}");
    }

    #[test]
    fn read_file_on_directory_returns_error() {
        let (_dir, mgr) = temp_source();
        let result = mgr.read_file("src");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("directory"),
            "expected directory error, got: {}",
            err
        );
    }
}
