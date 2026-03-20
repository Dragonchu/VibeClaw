use ignore::{WalkBuilder, gitignore::GitignoreBuilder};
use similar::TextDiff;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_DIFF_LENGTH: usize = 8000;
const MAX_SEARCH_LINE_LENGTH: usize = 300;

#[derive(Debug, Clone)]
pub struct WriteReport {
    pub path: String,
    pub range: Option<(usize, usize)>,
    pub diff: String,
}

impl WriteReport {
    pub fn summary(&self) -> String {
        let mut msg = String::new();
        match self.range {
            Some((start, end)) => {
                let _ = writeln!(msg, "Updated {} (lines {}-{}).", self.path, start, end);
            }
            None => {
                let _ = writeln!(msg, "Updated {}.", self.path);
            }
        }
        msg.push_str(&self.diff);
        msg
    }
}

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
        self.read_file_range(relative_path, None)
    }

    pub fn read_file_range(
        &self,
        relative_path: &str,
        range: Option<(usize, usize)>,
    ) -> Result<String, String> {
        let (_path, content) = self.read_file_internal(relative_path)?;
        if let Some((start, end)) = range {
            if start == 0 || end == 0 {
                return Err("Line numbers must start at 1.".to_string());
            }
            if end < start {
                return Err(format!(
                    "Invalid range: end_line ({}) must be >= start_line ({}).",
                    end, start
                ));
            }
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            if total == 0 {
                return Ok(format!(
                    "File '{}' is empty; no lines to show.",
                    relative_path
                ));
            }
            if start > total {
                return Err(format!(
                    "Requested lines {}-{} but file only has {} lines.",
                    start, end, total
                ));
            }
            let end = end.min(total);
            let width = total.to_string().len();
            let mut out = String::new();
            let _ = writeln!(
                out,
                "Showing lines {}-{} of {} ({}):",
                start, end, total, relative_path
            );
            for (offset, line) in lines[(start - 1)..end].iter().enumerate() {
                let _ = writeln!(out, "{:>width$}: {}", start + offset, line, width = width);
            }
            Ok(out)
        } else {
            Ok(content)
        }
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
        self.write_file_range(relative_path, content, None)
            .map(|_| ())
    }

    /// Write content to a file, optionally targeting an inclusive 1-based line range for precise edits.
    pub fn write_file_range(
        &mut self,
        relative_path: &str,
        content: &str,
        range: Option<(usize, usize)>,
    ) -> Result<WriteReport, String> {
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

        let original = if target.exists() {
            if target.is_dir() {
                return Err(self.directory_hint(relative_path));
            }
            fs::read_to_string(&target)
                .map_err(|e| format!("Read error: {} (resolved to {})", e, target.display()))?
        } else {
            String::new()
        };

        if !target.exists() && range.is_some() {
            return Err(format!(
                "Cannot apply line-range edit because the file does not exist (resolved to {}). \
                 Create the file first or omit start_line/end_line to write a new file.",
                target.display()
            ));
        }

        let new_content = if let Some((start, end)) = range {
            apply_line_range_edit(&original, content, start, end)?
        } else {
            content.to_string()
        };

        fs::write(&target, new_content.as_bytes())
            .map_err(|e| format!("Write error: {} (resolved to {})", e, target.display()))?;
        tracing::info!(path = %relative_path, resolved = %target.display(), "Source file written");

        let diff = summarize_diff(&original, &new_content);
        Ok(WriteReport {
            path: relative_path.to_string(),
            range,
            diff,
        })
    }

    /// Search for a query string within the peripheral workspace respecting .gitignore files.
    pub fn search(
        &self,
        query: &str,
        relative_path: &str,
        max_results: usize,
    ) -> Result<String, String> {
        if query.trim().is_empty() {
            return Err("Search query must not be empty.".to_string());
        }

        let root = self.peripheral_root.join(relative_path);
        if !root.starts_with(&self.peripheral_root) {
            return Err("Path traversal not allowed".to_string());
        }
        if !root.exists() {
            return Err(format!(
                "Search root not found: '{}' (resolved to {})",
                relative_path,
                root.display()
            ));
        }

        let root = match root.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    path = %root.display(),
                    "Using non-canonical search root"
                );
                root
            }
        };

        let mut gitignore_builder = GitignoreBuilder::new(&self.peripheral_root);
        let _ = gitignore_builder.add(self.peripheral_root.join(".gitignore"));
        let gitignore = gitignore_builder
            .build()
            .map_err(|e| format!("Failed to parse .gitignore: {}", e))?;

        let mut results = Vec::new();

        if root.is_file() {
            if gitignore
                .matched_path_or_any_parents(&root, false)
                .is_ignore()
            {
                return Ok("Search root is ignored by .gitignore.".to_string());
            }
            search_file(
                &root,
                &self.peripheral_root,
                query,
                &mut results,
                max_results,
            )?;
        } else {
            let walker = WalkBuilder::new(&root)
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .follow_links(false)
                .build();

            for entry in walker {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "Skipping unreadable path during search");
                        continue;
                    }
                };
                let path = entry.path();
                if path.is_dir() {
                    continue;
                }
                if gitignore
                    .matched_path_or_any_parents(path, false)
                    .is_ignore()
                {
                    continue;
                }
                if results.len() >= max_results {
                    break;
                }
                if let Err(e) = search_file(
                    path,
                    &self.peripheral_root,
                    query,
                    &mut results,
                    max_results,
                ) {
                    tracing::debug!(error = %e, path = %path.display(), "Skipping file during search");
                }
            }
        }

        if results.is_empty() {
            Ok("No matches found.".to_string())
        } else {
            Ok(results.join("\n"))
        }
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

    fn read_file_internal(&self, relative_path: &str) -> Result<(PathBuf, String), String> {
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

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Read error: {} (resolved to {})", e, path.display()))?;

        Ok((path, content))
    }
}

/// Replace an inclusive 1-based line range with new content. Supports appending by
/// setting `start_line = end_line = current_line_count + 1`.
fn apply_line_range_edit(
    original: &str,
    replacement: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String, String> {
    if start_line == 0 || end_line == 0 {
        return Err("Line numbers must start at 1.".to_string());
    }
    if end_line < start_line {
        return Err(format!(
            "Invalid range: end_line ({}) must be >= start_line ({}).",
            end_line, start_line
        ));
    }

    let spans = line_spans(original);
    let line_count = spans.len();
    let append = start_line == line_count + 1;

    if start_line > line_count + 1 {
        return Err(format!(
            "Cannot edit lines {}-{}: file has {} lines. Use start_line=end_line={} to append.",
            start_line,
            end_line,
            line_count,
            line_count + 1
        ));
    }

    if append && end_line != start_line {
        return Err(format!(
            "When appending at end, set start_line=end_line={} (file has {}).",
            line_count + 1,
            line_count
        ));
    }

    if !append && end_line > line_count {
        return Err(format!(
            "end_line {} exceeds file length {}. Use {} to append at end.",
            end_line,
            line_count,
            line_count + 1
        ));
    }

    let start_byte = if append {
        original.len()
    } else {
        spans.get(start_line - 1).map(|(s, _)| *s).unwrap_or(0)
    };

    let end_byte = if append {
        original.len()
    } else {
        spans
            .get(end_line - 1)
            .map(|(_, e)| *e)
            .unwrap_or(original.len())
    };

    let mut new_content = String::with_capacity(original.len() + replacement.len());
    new_content.push_str(&original[..start_byte]);
    new_content.push_str(replacement);
    if !append {
        new_content.push_str(&original[end_byte..]);
    }
    Ok(new_content)
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for line in content.split_inclusive('\n') {
        let end = start + line.len();
        spans.push((start, end));
        start = end;
    }
    if spans.is_empty() && !content.is_empty() {
        spans.push((0, content.len()));
    }
    spans
}

/// Generate a unified diff between the original and updated content and truncate
/// it to `MAX_DIFF_LENGTH` for safety.
fn summarize_diff(original: &str, updated: &str) -> String {
    let diff = TextDiff::from_lines(original, updated)
        .unified_diff()
        .context_radius(3)
        .header("original", "updated")
        .to_string();
    truncate_with_ellipsis(diff.trim(), MAX_DIFF_LENGTH)
}

fn truncate_with_ellipsis(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max_bytes);
        format!("{}...(diff truncated)", &s[..end])
    }
}

/// Search for `query` in `path`, appending matches to `results` until `max_results`
/// is reached. Skips binary or invalid UTF-8 files to avoid noisy output.
fn search_file(
    path: &Path,
    base: &Path,
    query: &str,
    results: &mut Vec<String>,
    max_results: usize,
) -> Result<(), String> {
    if results.len() >= max_results {
        return Ok(());
    }
    let bytes = fs::read(path).map_err(|e| format!("Search read error: {}", e))?;
    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            // Skip binary files and invalid UTF-8 to avoid noisy search results.
            return Ok(());
        }
    };

    for (idx, line) in content.lines().enumerate() {
        if line.contains(query) {
            let trimmed = truncate_with_ellipsis(line.trim_end(), MAX_SEARCH_LINE_LENGTH);
            let rel = path
                .strip_prefix(base)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string_lossy().to_string());
            results.push(format!("{}:{}: {}", rel, idx + 1, trimmed));
            if results.len() >= max_results {
                break;
            }
        }
    }
    Ok(())
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

    #[test]
    fn read_file_range_returns_numbered_lines() {
        let (_dir, mut mgr) = temp_source();
        mgr.write_file("src/lib.rs", "one\ntwo\nthree\n").unwrap();

        let output = mgr
            .read_file_range("src/lib.rs", Some((2, 3)))
            .expect("read range should work");

        assert!(
            output.contains("Showing lines 2-3 of 3"),
            "expected range header, got: {}",
            output
        );
        assert!(output.contains("2: two"));
        assert!(output.contains("3: three"));
    }

    #[test]
    fn write_file_range_replaces_only_target_lines() {
        let (_dir, mut mgr) = temp_source();
        mgr.write_file("src/lib.rs", "alpha\nbeta\ngamma\n")
            .unwrap();

        let report = mgr
            .write_file_range("src/lib.rs", "// replaced\n", Some((2, 2)))
            .expect("write range should succeed");

        let content = mgr.read_file("src/lib.rs").unwrap();
        assert_eq!(content, "alpha\n// replaced\ngamma\n");
        assert!(
            report.diff.contains("@@"),
            "expected diff output, got: {}",
            report.diff
        );
    }

    #[test]
    fn write_file_range_appends_at_end() {
        let (_dir, mut mgr) = temp_source();
        mgr.write_file("src/lib.rs", "a\nb\n").unwrap();

        mgr.write_file_range("src/lib.rs", "c\n", Some((3, 3)))
            .expect("append should succeed");

        let content = mgr.read_file("src/lib.rs").unwrap();
        assert_eq!(content, "a\nb\nc\n");
    }

    #[test]
    fn search_respects_gitignore() {
        let (dir, mut mgr) = temp_source();
        let gitignore = dir
            .path()
            .join("crates")
            .join("peripheral")
            .join(".gitignore");
        fs::write(&gitignore, "ignored.txt\nignored_dir/\n").unwrap();

        mgr.write_file("src/lib.rs", "find me\n").unwrap();
        fs::write(mgr.peripheral_root().join("ignored.txt"), "find me too\n").unwrap();
        fs::create_dir_all(mgr.peripheral_root().join("ignored_dir")).unwrap();
        fs::write(
            mgr.peripheral_root().join("ignored_dir").join("file.rs"),
            "find me three\n",
        )
        .unwrap();

        let result = mgr.search("find", ".", 10).expect("search should succeed");
        assert!(
            result.contains("src/lib.rs"),
            "expected match in src/lib.rs, got: {}",
            result
        );
        assert!(
            !result.contains("ignored.txt"),
            "ignored file should not appear: {}",
            result
        );
        assert!(
            !result.contains("ignored_dir"),
            "ignored directory should not appear: {}",
            result
        );
    }
}
