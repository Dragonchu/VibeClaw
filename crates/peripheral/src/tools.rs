use crate::deepseek::{FunctionDefinition, ToolDefinition};
use crate::memory::MemoryManager;
use crate::source::SourceManager;

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            type_: "function".to_string(),
            function: FunctionDefinition {
                name: "read_source_file".to_string(),
                description: "Read a file from the agent's source code. Path is relative to the peripheral crate root (e.g. 'src/main.rs', 'Cargo.toml')".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative file path within the peripheral crate"
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
                description: "Write content to a file in the agent's source code working directory. Changes are written directly to disk. Path is relative to crates/peripheral/ (e.g. 'src/main.rs'). Provide the FULL file content.".to_string(),
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
                        }
                    },
                    "required": ["path", "content"]
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
    ]
}

#[derive(Debug, PartialEq)]
pub enum ToolResult {
    Output(String),
    SubmitUpdate(String),
}

pub fn execute_tool(name: &str, arguments: &str, source: &mut SourceManager, memory: &mut MemoryManager) -> ToolResult {
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

    match name {
        "read_source_file" => {
            let path = args["path"].as_str().unwrap_or("");
            if path.is_empty() {
                tracing::warn!(tool = %name, raw_arguments = %arguments, "Missing required 'path' parameter");
                return ToolResult::Output(
                    "Error: 'path' parameter is required but was empty or missing. \
                     Please provide a relative file path like 'src/main.rs'.".to_string()
                );
            }
            match source.read_file(path) {
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
                     Please provide a relative file path like 'src/main.rs'.".to_string()
                );
            }
            match source.write_file(path, content) {
                Ok(()) => ToolResult::Output(format!("Written: {}", path)),
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
    fn write_source_file_with_empty_arguments_returns_path_error() {
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
                    msg.contains("Written: src/test.rs"),
                    "expected success, got: {}",
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
