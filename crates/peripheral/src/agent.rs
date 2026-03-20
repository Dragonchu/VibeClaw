use std::time::Duration;

use tokio::sync::mpsc;

use crate::deepseek::{ChatMessage, DeepSeekClient, StreamEvent};
use crate::ipc_client;
use crate::memory::MemoryManager;
use crate::source::SourceManager;
use crate::tools::{self, ToolResult};

use reloopy_ipc::messages::{Envelope, msg_types};

const BASE_SYSTEM_PROMPT: &str = r#"You are Reloopy, a self-evolving AI agent written in Rust. You can read and modify your own source code to improve yourself.

## Source Code Tools
 - read_source_file(path, start_line?, end_line?): Read a file or a 1-based inclusive line range. Paths are relative to the peripheral crate root (e.g. "src/main.rs", "Cargo.toml").
- search_source_files(query, path?="."): Search source files using a .gitignore-aware walker. Returns "path:line: snippet" matches.
- list_source_files(path): List files in your source directory. Path is relative to the peripheral crate root (e.g. "src/", ".").
- write_source_file(path, content, start_line?, end_line?): Write changes directly to a file in your working directory. Provide the replacement text for the targeted range or entire file. Use line ranges for precise edits or set start_line=end_line=current_line_count+1 to append.
- submit_update(): Submit the current working directory for compilation and deployment. This tool returns the build/test result. If compilation fails, read the error messages, fix the code with write_source_file, and call submit_update() again.

## Memory Tools
- memory_search(query): Search across all memory files for relevant content.
- memory_get(date): Get a daily log. date = "today" | "yesterday" | "YYYY-MM-DD".
- memory_get_long_term(): Read the full MEMORY.md. Always call this before memory_write to safely merge updates.
- memory_write(content): Overwrite MEMORY.md with updated long-term facts. Always call memory_get_long_term first to read existing content, then merge and rewrite the full document.
- memory_append(content): Append a note to today's daily log.

## Guidelines
- Always read the relevant source files before making changes
 - Use search_source_files and line ranges to keep context focused
- Tool outputs in the UI are truncated for previews; keep requests concise and rely on targeted reads/diffs
- Ensure your changes produce valid, compilable Rust code
- Maintain IPC protocol compatibility (handshake, heartbeat, message handling)
- Make focused, incremental changes
- After writing all modified files, call submit_update() to deploy
- submit_update() returns compilation/test results. If it fails, analyze the errors, fix the code, and re-submit
- Use memory_append() to record important decisions or context during a session
- Use memory_write() to persist key facts that should survive across sessions
"#;

fn build_system_prompt(memory: &MemoryManager) -> String {
    let ctx = memory.load_context();
    if ctx.is_empty() {
        BASE_SYSTEM_PROMPT.to_string()
    } else {
        format!("{}\n## Current Memory\n\n{}", BASE_SYSTEM_PROMPT, ctx)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
pub enum AgentEvent {
    Reasoning(String),
    Content(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgDelta(String),
    ToolResult { name: String, output: String },
    SubmitUpdate { source_path: String },
    Error(String),
    Done,
}

pub enum AgentOutcome {
    Done,
}

pub struct Agent {
    deepseek: DeepSeekClient,
    source: SourceManager,
    memory: MemoryManager,
    conversation: Vec<ChatMessage>,
    ipc_tx: mpsc::Sender<Envelope>,
    update_result_rx: mpsc::Receiver<Envelope>,
}

impl Agent {
    pub fn new(
        deepseek: DeepSeekClient,
        source: SourceManager,
        memory: MemoryManager,
        ipc_tx: mpsc::Sender<Envelope>,
        update_result_rx: mpsc::Receiver<Envelope>,
    ) -> Self {
        let system_prompt = build_system_prompt(&memory);
        Self {
            deepseek,
            source,
            memory,
            conversation: vec![ChatMessage::system(&system_prompt)],
            ipc_tx,
            update_result_rx,
        }
    }

    pub async fn handle_input_stream(
        &mut self,
        user_input: &str,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<AgentOutcome, String> {
        self.conversation.push(ChatMessage::user(user_input));
        let tool_defs = tools::tool_definitions();

        loop {
            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamEvent>(256);

            let messages = self.conversation.clone();
            let tools_ref = tool_defs.as_slice();

            let chat_handle = self
                .deepseek
                .chat_stream(&messages, Some(tools_ref), stream_tx);

            let event_tx_clone = event_tx.clone();
            let forward_handle = tokio::spawn(async move {
                while let Some(ev) = stream_rx.recv().await {
                    let agent_ev = match ev {
                        StreamEvent::Reasoning(s) => AgentEvent::Reasoning(s),
                        StreamEvent::Content(s) => AgentEvent::Content(s),
                        StreamEvent::ToolCallStart { id, name } => {
                            AgentEvent::ToolCallStart { id, name }
                        }
                        StreamEvent::ToolCallArgDelta(s) => AgentEvent::ToolCallArgDelta(s),
                        StreamEvent::Done | StreamEvent::Error(_) => continue,
                    };
                    if event_tx_clone.send(agent_ev).await.is_err() {
                        break;
                    }
                }
            });

            let message = chat_handle.await?;
            let _ = forward_handle.await;

            if let Some(ref tool_calls) = message.tool_calls {
                self.conversation.push(message.clone());

                for tc in tool_calls {
                    tracing::debug!(tool = %tc.function.name, "Executing tool");

                    let result = tools::execute_tool(
                        &tc.function.name,
                        &tc.function.arguments,
                        &mut self.source,
                        &mut self.memory,
                    );

                    match result {
                        ToolResult::Output(output) => {
                            let _ = event_tx
                                .send(AgentEvent::ToolResult {
                                    name: tc.function.name.clone(),
                                    output: truncate(&output, 500),
                                })
                                .await;
                            self.conversation.push(ChatMessage::tool(&output, &tc.id));
                        }
                        ToolResult::SubmitUpdate(source_path) => {
                            let _ = event_tx
                                .send(AgentEvent::SubmitUpdate {
                                    source_path: source_path.clone(),
                                })
                                .await;

                            let result_text =
                                self.submit_and_wait_result(&source_path, &event_tx).await;

                            let _ = event_tx
                                .send(AgentEvent::ToolResult {
                                    name: "submit_update".to_string(),
                                    output: truncate(&result_text, 500),
                                })
                                .await;
                            self.conversation
                                .push(ChatMessage::tool(&result_text, &tc.id));
                        }
                    }
                }
            } else {
                let text = message.content.unwrap_or_default();
                self.conversation.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                });
                let _ = event_tx.send(AgentEvent::Done).await;
                return Ok(AgentOutcome::Done);
            }
        }
    }

    /// Send SubmitUpdate to Boot via IPC and wait for the result.
    async fn submit_and_wait_result(
        &mut self,
        source_path: &str,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> String {
        let submit = ipc_client::make_submit_update(source_path);
        if self.ipc_tx.send(submit).await.is_err() {
            return "Error: Lost connection to Boot".to_string();
        }

        match tokio::time::timeout(Duration::from_secs(300), self.update_result_rx.recv()).await {
            Ok(Some(envelope)) => {
                let result_text = format_update_result(&envelope);

                // If Boot sends SHUTDOWN after acceptance, notify frontend
                if envelope.msg_type == msg_types::SHUTDOWN {
                    let _ = event_tx
                        .send(AgentEvent::Error(
                            "Hot replacement in progress. Shutting down...".into(),
                        ))
                        .await;
                }

                result_text
            }
            Ok(None) => "Error: IPC channel closed".to_string(),
            Err(_) => "Error: Timed out waiting for build result (300s)".to_string(),
        }
    }

    pub fn reset_conversation(&mut self) {
        let system_prompt = build_system_prompt(&self.memory);
        self.conversation = vec![ChatMessage::system(&system_prompt)];
    }
}

/// Format an UPDATE_ACCEPTED or UPDATE_REJECTED envelope into a human-readable string
/// that is injected back into the Agent's conversation.
fn format_update_result(envelope: &Envelope) -> String {
    match envelope.msg_type.as_str() {
        msg_types::UPDATE_ACCEPTED => {
            let version = envelope
                .payload
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            format!(
                "Update ACCEPTED — version {} deployed successfully.",
                version
            )
        }
        msg_types::UPDATE_REJECTED => {
            let reason = envelope
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let errors = envelope
                .payload
                .get("errors")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let suggestion = envelope
                .payload
                .get("suggestion")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let mut msg = format!("Update REJECTED: {}", reason);
            if !errors.is_empty() {
                msg.push_str(&format!("\n\nCompilation errors:\n{}", errors));
            }
            if !suggestion.is_empty() {
                msg.push_str(&format!("\n\nSuggestion: {}", suggestion));
            }
            msg
        }
        _ => format!("Unexpected response from Boot: {}", envelope.msg_type),
    }
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max_bytes);
        format!("{}...(truncated)", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 500), "hello");
    }

    #[test]
    fn truncate_ascii_at_boundary() {
        let s = "a".repeat(600);
        let result = truncate(&s, 500);
        assert!(result.starts_with(&"a".repeat(500)));
        assert!(result.ends_with("...(truncated)"));
    }

    #[test]
    fn truncate_multibyte_utf8_does_not_panic() {
        // "代理" is 2 chars, each 3 bytes in UTF-8; repeated 100 times = 200 chars, 600 bytes.
        // Truncates at a valid UTF-8 boundary when byte limit falls mid-character.
        let s: String = "代理".repeat(100);
        let result = truncate(&s, 500);
        assert!(result.ends_with("...(truncated)"));
        assert!(result.len() < 600);
    }

    #[test]
    fn truncate_mixed_content() {
        // Mix of ASCII and multi-byte characters
        let s = format!("{}{}", "a".repeat(498), "代理代理");
        let result = truncate(&s, 500);
        // Should truncate safely, including 498 ASCII bytes + up to boundary
        assert!(result.ends_with("...(truncated)"));
    }

    #[test]
    fn format_update_accepted() {
        let envelope = Envelope {
            from: "boot".to_string(),
            to: "peripheral".to_string(),
            msg_type: msg_types::UPDATE_ACCEPTED.to_string(),
            id: "test-1".to_string(),
            payload: serde_json::json!({"version": "V3"}),
        };
        let result = format_update_result(&envelope);
        assert!(result.contains("ACCEPTED"));
        assert!(result.contains("V3"));
    }

    #[test]
    fn format_update_rejected_with_errors() {
        let envelope = Envelope {
            from: "boot".to_string(),
            to: "peripheral".to_string(),
            msg_type: msg_types::UPDATE_REJECTED.to_string(),
            id: "test-2".to_string(),
            payload: serde_json::json!({
                "version": "V4",
                "reason": "compilation_failed",
                "errors": "error[E0308]: mismatched types",
                "suggestion": "Check the return type"
            }),
        };
        let result = format_update_result(&envelope);
        assert!(result.contains("REJECTED"));
        assert!(result.contains("compilation_failed"));
        assert!(result.contains("error[E0308]"));
        assert!(result.contains("Check the return type"));
    }

    #[test]
    fn format_update_rejected_minimal() {
        let envelope = Envelope {
            from: "boot".to_string(),
            to: "peripheral".to_string(),
            msg_type: msg_types::UPDATE_REJECTED.to_string(),
            id: "test-3".to_string(),
            payload: serde_json::json!({"version": "V5", "reason": "test_failed"}),
        };
        let result = format_update_result(&envelope);
        assert!(result.contains("REJECTED"));
        assert!(result.contains("test_failed"));
        // No errors or suggestion fields
        assert!(!result.contains("Compilation errors"));
        assert!(!result.contains("Suggestion"));
    }
}
