use crate::deepseek::{FunctionDefinition, ToolDefinition};
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
                description: "Write content to a file in the staging area. This stages changes for submission, not the live code. Path is relative to crates/peripheral/ (e.g. 'src/main.rs')".to_string(),
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
                description: "Submit all staged changes for compilation and deployment. Call this after writing all modifications via write_source_file.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
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

pub fn execute_tool(name: &str, arguments: &str, source: &mut SourceManager) -> ToolResult {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();

    match name {
        "read_source_file" => {
            let path = args["path"].as_str().unwrap_or("");
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
            match source.write_staged_file(path, content) {
                Ok(()) => ToolResult::Output(format!("Written: {}", path)),
                Err(e) => ToolResult::Output(format!("Error: {}", e)),
            }
        }
        "submit_update" => match source.pack_workspace() {
            Ok(staging_path) => {
                ToolResult::SubmitUpdate(staging_path.to_string_lossy().to_string())
            }
            Err(e) => ToolResult::Output(format!("Error packing workspace: {}", e)),
        },
        _ => ToolResult::Output(format!("Unknown tool: {}", name)),
    }
}
