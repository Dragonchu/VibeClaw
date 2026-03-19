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
            return Err(format!(
                "File not found: '{}' (resolved to {})",
                relative_path,
                path.display()
            ));
        }
        if path.is_dir() {
            return Err(self.directory_hint(relative_path));
        }
        fs::read_to_string(&path).map_err(|e| format!("Read error: {}", e))
    }

    pub fn list_files(&self, relative_path: &str) -> Result<Vec<String>, String> {
        let dir = self.peripheral_root.join(relative_path);
        if !dir.starts_with(&self.peripheral_root) {
            return Err("Path traversal not allowed".to_string());
        }
        if !dir.exists() {
            return Err(format!(
                "Directory not found: '{}' (resolved to {})",
                relative_path,
                dir.display()
            ));
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
            return Err(self.directory_hint(relative_path));
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create dirs: {}", e))?;
        }

        fs::write(&target, content).map_err(|e| {
            format!(
                "Write error: {} (resolved to {})",
                e,
                target.display()
            )
        })?;
        tracing::info!(path = %relative_path, resolved = %target.display(), "Source file written");
        Ok(())
    }

    /// Build an error message that tells the agent what files live inside the
    /// directory it accidentally targeted, so it can self-correct.
    fn directory_hint(&self, relative_path: &str) -> String {
        let dir = self.peripheral_root.join(relative_path);
        let mut files = Vec::new();
        let _ = collect_files_recursive(&dir, &self.peripheral_root, &mut files);
        files.sort();

        let mut msg = format!(
            "Path '{}' is a directory, not a file (resolved to {}). \
             All paths are relative to the peripheral crate root: {}. \
             You must specify a file path, e.g. 'src/main.rs'.",
            relative_path,
            dir.display(),
            self.peripheral_root.display(),
        );
        if !files.is_empty() {
            msg.push_str("\nFiles in this directory:\n");
            for f in &files {
                msg.push_str("  ");
                msg.push_str(f);
                msg.push('\n');
            }
        }
        msg
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
    fn write_file_to_existing_directory_returns_error_with_hint() {
        let (_dir, mut mgr) = temp_source();
        // Write a file so the directory listing is non-empty
        mgr.write_file("src/lib.rs", "// lib").unwrap();
        let result = mgr.write_file("src", "hello");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("is a directory"),
            "expected directory error, got: {}",
            err
        );
        assert!(
            err.contains("src/lib.rs"),
            "expected file listing in hint, got: {}",
            err
        );
        // Must include the resolved absolute path so the agent can debug
        assert!(
            err.contains("resolved to"),
            "expected resolved absolute path, got: {}",
            err
        );
        assert!(
            err.contains("peripheral crate root:"),
            "expected peripheral root path, got: {}",
            err
        );
    }

    #[test]
    fn write_file_to_dot_returns_error_with_hint() {
        let (_dir, mut mgr) = temp_source();
        mgr.write_file("Cargo.toml", "[package]").unwrap();
        let result = mgr.write_file(".", "[package]");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("is a directory"),
            "expected directory error, got: {}",
            err
        );
        assert!(
            err.contains("You must specify a file path"),
            "expected guidance in hint, got: {}",
            err
        );
        assert!(
            err.contains("Cargo.toml"),
            "expected file listing in hint, got: {}",
            err
        );
        assert!(
            err.contains("resolved to"),
            "expected resolved absolute path, got: {}",
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
    fn read_file_on_directory_returns_error_with_hint() {
        let (_dir, mgr) = temp_source();
        // Create a file so listing is non-empty
        fs::write(
            mgr.peripheral_root().join("src").join("main.rs"),
            "fn main() {}",
        )
        .unwrap();
        let result = mgr.read_file("src");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("is a directory"),
            "expected directory error, got: {}",
            err
        );
        assert!(
            err.contains("src/main.rs"),
            "expected file listing in hint, got: {}",
            err
        );
        assert!(
            err.contains("resolved to"),
            "expected resolved absolute path, got: {}",
            err
        );
    }

    #[test]
    fn read_file_not_found_includes_resolved_path() {
        let (_dir, mgr) = temp_source();
        let result = mgr.read_file("nonexistent.rs");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("File not found"),
            "expected not found error, got: {}",
            err
        );
        assert!(
            err.contains("resolved to"),
            "expected resolved absolute path, got: {}",
            err
        );
        assert!(
            err.contains("nonexistent.rs"),
            "expected original path in error, got: {}",
            err
        );
    }
}
