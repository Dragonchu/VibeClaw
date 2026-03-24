use crate::deepseek::{FunctionDefinition, ToolDefinition};
use crate::memory::MemoryManager;
use crate::source::SourceManager;

const DEFAULT_MAX_RESULTS: usize = 40;
const ABSOLUTE_MAX_RESULTS: usize = 200;
const DEFAULT_READ_LIMIT: usize = 200;
const ABSOLUTE_READ_LIMIT: usize = 2_000;

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "read_source_file".to_string(),
                description: "Read a file from the agent's source code. Supports optional line windows via offset+limit or explicit 1-based ranges. Path is relative to the peripheral crate root (e.g. 'src/main.rs', 'Cargo.toml')".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative file path within the peripheral crate"
                        },
                        "offset": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Optional 0-based line offset. Use with limit to page through large files."
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Optional number of lines to read. Defaults to 200 and is capped."
                        },
                        "start_line": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Optional start line (1-based, inclusive). Use with end_line to read a slice."
                        },
                        "end_line": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Optional end line (1-based, inclusive). Defaults to start_line when provided."
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "list_source_files".to_string(),
                description: "List all files in a directory of the agent's source code. Path is relative to the peripheral crate root (e.g. 'src/', '.')".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative directory path within the peripheral crate"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "write_source_file".to_string(),
                description: "Write content to a file in the agent's source code working directory. Supports optional 1-based line ranges for precise edits. Path is relative to crates/peripheral/ (e.g. 'src/main.rs'). Provide the full replacement text for the targeted range or entire file.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative file path within the peripheral crate"
                        },
                        "content": {
                            "type": "string",
                            "description": "Full file content to write"
                        },
                        "start_line": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Optional start line (1-based, inclusive). Provide to replace a specific slice."
                        },
                        "end_line": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Optional end line (1-based, inclusive). Defaults to start_line when provided."
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "edit_source_file".to_string(),
                description: "Precisely replace an exact string in a source file. Fails if the old string is missing or ambiguous. Path is relative to the peripheral crate root (e.g. 'src/main.rs').".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative file path within the peripheral crate"
                        },
                        "old_string": {
                            "type": "string",
                            "description": "Exact string to replace. Must appear exactly once."
                        },
                        "new_string": {
                            "type": "string",
                            "description": "Replacement string"
                        }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "search_source".to_string(),
                description: "Search source files for a query (supports regex), respecting .gitignore. Paths are relative to the peripheral crate root.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search term to look for in source files"
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional directory or file path to scope the search. Defaults to '.'."
                        },
                        "max_results": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Maximum number of matches to return (default 40, capped at 200)."
                        }
                    },
                    "required": ["query"]
                }),
            },
        },
        // Backwards compatibility while the model picks up the new name.
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "search_source_files".to_string(),
                description: "Search source files for a query (supports regex), respecting .gitignore. Paths are relative to the peripheral crate root.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search term to look for in source files"
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional directory or file path to scope the search. Defaults to '.'."
                        },
                        "max_results": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Maximum number of matches to return (default 40, capped at 200)."
                        }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "submit_update".to_string(),
                description: "Submit the current working directory for compilation and deployment. Returns the compilation/test result. If compilation fails, read the errors and fix the code, then submit again.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "memory_search".to_string(),
                description: "Search across all memory files (MEMORY.md and daily logs) for relevant content. Returns the top matching snippets ranked by relevance.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Keywords or phrase to search for in memory"
                        }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "memory_get".to_string(),
                description: "Get a memory file by date. Use \"today\", \"yesterday\", or a date in YYYY-MM-DD format. Omit or pass \"today\" for today's daily log.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "date": {
                            "type": "string",
                            "description": "\"today\", \"yesterday\", or \"YYYY-MM-DD\". Defaults to \"today\"."
                        }
                    },
                    "required": []
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "memory_get_long_term".to_string(),
                description: "Read the full contents of MEMORY.md. Call this before memory_write so you can merge new facts with the existing content.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "memory_write".to_string(),
                description: "Overwrite MEMORY.md with new long-term facts. Use this to update curated, persistent knowledge that should survive across sessions.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "Full Markdown content to write to MEMORY.md"
                        }
                    },
                    "required": ["content"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "memory_append".to_string(),
                description: "Append a timestamped entry to today's daily log. Use this for short-term notes, task progress, and session context.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "Markdown content to append to today's daily log"
                        }
                    },
                    "required": ["content"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "diff_version".to_string(),
                description: "Show what changed between two version branches (e.g. V1..V2). Returns a unified diff scoped to the peripheral crate source. Use this after a rollback to understand what the failed version changed.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "base_version": {
                            "type": "string",
                            "description": "Base version branch (e.g. 'V1', 'V2')"
                        },
                        "target_version": {
                            "type": "string",
                            "description": "Target version branch to compare against (e.g. 'V2', 'V3')"
                        },
                        "path": {
                            "type": "string",
                            "description": "Optional path filter relative to peripheral crate (e.g. 'src/agent.rs')"
                        }
                    },
                    "required": ["base_version", "target_version"]
                }),
            },
        },
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "read_version_file".to_string(),
                description: "Read a file from a specific version branch without switching to it. Use this to inspect source code from previous or failed versions.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "version": {
                            "type": "string",
                            "description": "Version branch name (e.g. 'V1', 'V2')"
                        },
                        "path": {
                            "type": "string",
                            "description": "Relative file path within the peripheral crate (e.g. 'src/main.rs')"
                        }
                    },
                    "required": ["version", "path"]
                }),
            },
        },
    ]
}

#[derive(Debug, PartialEq)]
pub enum ToolResult {
    Output(String),
    SubmitUpdate(String),
}

pub fn execute_tool(
    name: &str,
    arguments: &str,
    source: &mut SourceManager,
    memory: &mut MemoryManager,
) -> ToolResult {
    let args: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(tool = %name, raw_arguments = %arguments, error = %e, "Failed to parse tool arguments");
            return ToolResult::Output(format!(
                "Error: Failed to parse arguments as JSON: {}. Raw arguments: {}",
                e, arguments
            ));
        }
    };

    let extract_line_range_from_args =
        |args: &serde_json::Value| -> Result<Option<(usize, usize)>, String> {
            let start = args
                .get("start_line")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let end = args
                .get("end_line")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let offset = args
                .get("offset")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);

            if offset.is_some() || limit.is_some() {
                if start.is_some() || end.is_some() {
                    return Err(
                        "Use either offset/limit or start_line/end_line, not both.".to_string()
                    );
                }
                let offset = offset.unwrap_or(0);
                let limit = limit
                    .unwrap_or(DEFAULT_READ_LIMIT)
                    .clamp(1, ABSOLUTE_READ_LIMIT);
                let start_line = offset + 1;
                let end_line = start_line + limit - 1;
                return Ok(Some((start_line, end_line)));
            }

            match (start, end) {
                (None, None) => Ok(None),
                (Some(s), None) => Ok(Some((s, s))),
                (Some(s), Some(e)) => Ok(Some((s, e))),
                (None, Some(_)) => Err("Provide start_line when end_line is set.".to_string()),
            }
        };

    match name {
        "read_source_file" => {
            let path = args["path"].as_str().unwrap_or("");
            if path.is_empty() {
                tracing::warn!(tool = %name, raw_arguments = %arguments, "Missing required 'path' parameter");
                return ToolResult::Output(
                    "Error: 'path' parameter is required but was empty or missing. \
                     Please provide a relative file path like 'src/main.rs'."
                        .to_string(),
                );
            }
            let range = match extract_line_range_from_args(&args) {
                Ok(r) => r,
                Err(e) => return ToolResult::Output(format!("Error: {}", e)),
            };
            match source.read_file_range(path, range) {
                Ok(content) => ToolResult::Output(content),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "list_source_files" => {
            let path = args["path"].as_str().unwrap_or(".");
            match source.list_files(path) {
                Ok(files) => ToolResult::Output(files.join("\n")),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "write_source_file" => {
            let path = args["path"].as_str().unwrap_or("");
            let content = args["content"].as_str().unwrap_or("");
            if path.is_empty() {
                tracing::warn!(tool = %name, raw_arguments = %arguments, "Missing required 'path' parameter");
                return ToolResult::Output(
                    "Error: 'path' parameter is required but was empty or missing. \
                     Please provide a relative file path like 'src/main.rs'."
                        .to_string(),
                );
            }
            let range = match extract_line_range_from_args(&args) {
                Ok(r) => r,
                Err(e) => return ToolResult::Output(format!("Error: {}", e)),
            };
            match source.write_file_range(path, content, range) {
                Ok(report) => ToolResult::Output(report.summary()),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "edit_source_file" => {
            let path = args["path"].as_str().unwrap_or("");
            let old = args["old_string"].as_str().unwrap_or("");
            let new = args["new_string"].as_str().unwrap_or("");
            if path.is_empty() || old.is_empty() {
                return ToolResult::Output(
                    "Error: 'path' and 'old_string' parameters are required.".to_string(),
                );
            }
            match source.edit_file(path, old, new) {
                Ok(report) => ToolResult::Output(report.summary()),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "search_source" | "search_source_files" => {
            let query = args["query"].as_str().unwrap_or("");
            if query.trim().is_empty() {
                return ToolResult::Output(
                    "Error: 'query' parameter is required but was empty or missing.".to_string(),
                );
            }
            let path = args["path"].as_str().unwrap_or(".");
            let max_results = args["max_results"]
                .as_u64()
                .map(|v| v as usize)
                .unwrap_or(DEFAULT_MAX_RESULTS)
                .clamp(1, ABSOLUTE_MAX_RESULTS);

            match source.search(query, path, max_results) {
                Ok(output) => ToolResult::Output(output),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "submit_update" => {
            ToolResult::SubmitUpdate(source.workspace_root().to_string_lossy().to_string())
        }
        "memory_search" => {
            let query = args["query"].as_str().unwrap_or("");
            match memory.search(query) {
                Ok(result) => ToolResult::Output(result),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "memory_get" => {
            let date = args["date"].as_str().unwrap_or("today");
            match memory.get_daily(date) {
                Ok(result) => ToolResult::Output(result),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "memory_get_long_term" => match memory.get_long_term() {
            Ok(content) if content.is_empty() => {
                ToolResult::Output("MEMORY.md is empty.".to_string())
            }
            Ok(content) => ToolResult::Output(content),
            Err(e) => ToolResult::Output(format!("Error: {}", e)),
        },
        "memory_write" => {
            let content = args["content"].as_str().unwrap_or("");
            match memory.write_long_term(content) {
                Ok(()) => ToolResult::Output("MEMORY.md updated.".to_string()),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "memory_append" => {
            let content = args["content"].as_str().unwrap_or("");
            match memory.append_daily(content) {
                Ok(()) => ToolResult::Output(format!(
                    "Appended to today's log ({}).",
                    MemoryManager::today()
                )),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "diff_version" => {
            let base = args["base_version"].as_str().unwrap_or("");
            let target = args["target_version"].as_str().unwrap_or("");
            if base.is_empty() || target.is_empty() {
                return ToolResult::Output(
                    "Error: 'base_version' and 'target_version' are required.".to_string(),
                );
            }
            let path = args["path"].as_str();
            match source.diff_versions(base, target, path) {
                Ok(diff) => ToolResult::Output(diff),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "read_version_file" => {
            let version = args["version"].as_str().unwrap_or("");
            let path = args["path"].as_str().unwrap_or("");
            if version.is_empty() || path.is_empty() {
                return ToolResult::Output(
                    "Error: 'version' and 'path' are required.".to_string(),
                );
            }
            match source.read_version_file(version, path) {
                Ok(content) => ToolResult::Output(content),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        _ => ToolResult::Output(format!("Unknown tool: {}", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_tools() -> (tempfile::TempDir, SourceManager, MemoryManager) {
        let dir = tempfile::tempdir().expect("tempdir");
        let peripheral = dir.path().join("crates").join("peripheral");
        fs::create_dir_all(peripheral.join("src")).expect("create src dir");
        let source = SourceManager::new(dir.path().to_path_buf());
        let mem_dir = dir.path().join("memory");
        fs::create_dir_all(&mem_dir).expect("create memory dir");
        let memory = MemoryManager::new(&mem_dir);
        (dir, source, memory)
    }

    #[test]
    fn write_source_file_with_empty_arguments_returns_parse_error() {
        let (_dir, mut source, mut memory) = temp_tools();
        let result = execute_tool("write_source_file", "", &mut source, &mut memory);
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("Failed to parse arguments"),
                    "expected JSON parse error, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn write_source_file_with_missing_path_returns_error() {
        let (_dir, mut source, mut memory) = temp_tools();
        let result = execute_tool(
            "write_source_file",
            r#"{"content": "fn main() {}"}"#,
            &mut source,
            &mut memory,
        );
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("'path' parameter is required"),
                    "expected missing path error, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn write_source_file_with_valid_arguments_succeeds() {
        let (_dir, mut source, mut memory) = temp_tools();
        let result = execute_tool(
            "write_source_file",
            r#"{"path": "src/test.rs", "content": "fn main() {}"}"#,
            &mut source,
            &mut memory,
        );
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("Updated src/test.rs"),
                    "expected success, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn write_source_file_with_range_succeeds() {
        let (_dir, mut source, mut memory) = temp_tools();
        source
            .write_file("src/test.rs", "line1\nline2\nline3\n")
            .unwrap();

        let result = execute_tool(
            "write_source_file",
            r#"{"path": "src/test.rs", "content": "// replace\n", "start_line": 2, "end_line": 2}"#,
            &mut source,
            &mut memory,
        );

        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("lines 2-2"),
                    "expected range summary, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn write_source_file_requires_start_line_when_end_line_given() {
        let (_dir, mut source, mut memory) = temp_tools();
        let result = execute_tool(
            "write_source_file",
            r#"{"path": "src/test.rs", "content": "a", "end_line": 3}"#,
            &mut source,
            &mut memory,
        );
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("start_line"),
                    "expected start_line error, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn search_source_returns_match() {
        let (_dir, mut source, mut memory) = temp_tools();
        source.write_file("src/lib.rs", "needle here\n").unwrap();
        let result = execute_tool(
            "search_source",
            r#"{"query": "needle", "path": "src"}"#,
            &mut source,
            &mut memory,
        );
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("src/lib.rs"),
                    "expected search hit, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn read_source_file_range_works() {
        let (_dir, mut source, mut memory) = temp_tools();
        source
            .write_file("src/lib.rs", "one\ntwo\nthree\n")
            .unwrap();
        let result = execute_tool(
            "read_source_file",
            r#"{"path": "src/lib.rs", "offset": 1, "limit": 1}"#,
            &mut source,
            &mut memory,
        );

        match result {
            ToolResult::Output(msg) => {
                assert!(msg.contains("2: two"), "expected line 2, got: {}", msg);
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn edit_source_file_replaces_once() {
        let (_dir, mut source, mut memory) = temp_tools();
        source
            .write_file("src/lib.rs", "alpha\nbeta\ngamma\n")
            .unwrap();

        let result = execute_tool(
            "edit_source_file",
            r#"{"path": "src/lib.rs", "old_string": "beta", "new_string": "BETA"}"#,
            &mut source,
            &mut memory,
        );

        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("Updated src/lib.rs"),
                    "expected success output, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }

        let content = source.read_file("src/lib.rs").unwrap();
        assert!(content.contains("BETA"));
        assert!(!content.contains("beta"));
    }

    #[test]
    fn edit_source_file_rejects_ambiguous_match() {
        let (_dir, mut source, mut memory) = temp_tools();
        source.write_file("src/lib.rs", "foo\nbar\nfoo\n").unwrap();

        let result = execute_tool(
            "edit_source_file",
            r#"{"path": "src/lib.rs", "old_string": "foo", "new_string": "FOO"}"#,
            &mut source,
            &mut memory,
        );

        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("ambiguous"),
                    "expected ambiguity error, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn read_source_file_with_empty_path_returns_error() {
        let (_dir, mut source, mut memory) = temp_tools();
        let result = execute_tool(
            "read_source_file",
            r#"{"path": ""}"#,
            &mut source,
            &mut memory,
        );
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("'path' parameter is required"),
                    "expected missing path error, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn read_source_file_with_missing_path_returns_error() {
        let (_dir, mut source, mut memory) = temp_tools();
        let result = execute_tool("read_source_file", r#"{}"#, &mut source, &mut memory);
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("'path' parameter is required"),
                    "expected missing path error, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }

    #[test]
    fn invalid_json_arguments_returns_parse_error() {
        let (_dir, mut source, mut memory) = temp_tools();
        let result = execute_tool(
            "write_source_file",
            "not valid json",
            &mut source,
            &mut memory,
        );
        match result {
            ToolResult::Output(msg) => {
                assert!(
                    msg.contains("Failed to parse arguments"),
                    "expected parse error, got: {}",
                    msg
                );
                assert!(
                    msg.contains("not valid json"),
                    "expected raw arguments in error, got: {}",
                    msg
                );
            }
            other => panic!("expected Output, got: {:?}", other),
        }
    }
}
