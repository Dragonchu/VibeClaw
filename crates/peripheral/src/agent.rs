use std::{fmt::Write, time::Duration};

use tokio::sync::mpsc;

use crate::deepseek::{ChatMessage, LlmClient, StreamEvent};
use crate::ipc_client;
use crate::memory::MemoryManager;
use crate::source::SourceManager;
use crate::tools::{self, ToolResult};

use reloopy_ipc::messages::{Envelope, msg_types};

const TOOL_TRUNCATE_LINES: usize = 120;

const BASE_SYSTEM_PROMPT: &str = r#"You are Reloopy, a self-evolving AI agent written in Rust. You can read and modify your own source code to improve yourself.

## Source Code Tools
- read_source_file(path, offset?, limit?, start_line?, end_line?): Read a file or a 1-based inclusive line range. Prefer offset+limit to page large files. Paths are relative to the peripheral crate root (e.g. "src/main.rs", "Cargo.toml").
- search_source(query, path?="."): Search source files using a .gitignore-aware walker. Supports regex. Returns "path:line: snippet" matches.
- list_source_files(path): List files in your source directory. Path is relative to the peripheral crate root (e.g. "src/", ".").
- write_source_file(path, content, start_line?, end_line?): Write changes directly to a file in your working directory. Provide the replacement text for the targeted range or entire file. Use line ranges for precise edits or set start_line=end_line=current_line_count+1 to append.
- edit_source_file(path, old_string, new_string): Precisely replace a single matching string in a file. Fails if the match is missing or ambiguous.
- submit_update(): Submit the current working directory for compilation and deployment. This tool returns the build/test result. If compilation fails, read the error messages, fix the code with write_source_file, and call submit_update() again.

## Memory Tools
- memory_search(query): Search across all memory files for relevant content.
- memory_get(date): Get a daily log. date = "today" | "yesterday" | "YYYY-MM-DD".
- memory_get_long_term(): Read the full MEMORY.md. Always call this before memory_write to safely merge updates.
- memory_write(content): Overwrite MEMORY.md with updated long-term facts. Always call memory_get_long_term first to read existing content, then merge and rewrite the full document.
- memory_append(content): Append a note to today's daily log.

## Guidelines
- Always read the relevant source files before making changes
- Use search_source and line ranges to keep context focused
- Tool outputs are line-truncated for previews and include total line counts; keep requests concise and rely on targeted reads/diffs
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

pub struct Agent<L: LlmClient> {
    llm: L,
    source: SourceManager,
    memory: MemoryManager,
    conversation: Vec<ChatMessage>,
    ipc_tx: mpsc::Sender<Envelope>,
    update_result_rx: mpsc::Receiver<Envelope>,
}

impl<L: LlmClient> Agent<L> {
    pub fn new(
        llm: L,
        source: SourceManager,
        memory: MemoryManager,
        ipc_tx: mpsc::Sender<Envelope>,
        update_result_rx: mpsc::Receiver<Envelope>,
    ) -> Self {
        let system_prompt = build_system_prompt(&memory);
        Self {
            llm,
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

            let chat_handle = self.llm.chat_stream(&messages, Some(tools_ref), stream_tx);

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
                            event_tx
                                .send(AgentEvent::ToolResult {
                                    name: tc.function.name.clone(),
                                    output: truncate_lines(&output, TOOL_TRUNCATE_LINES),
                                })
                                .await
                                .ok();
                            self.conversation.push(ChatMessage::tool(&output, &tc.id));
                        }
                        ToolResult::SubmitUpdate(source_path) => {
                            event_tx
                                .send(AgentEvent::SubmitUpdate {
                                    source_path: source_path.clone(),
                                })
                                .await
                                .ok();

                            let result_text =
                                self.submit_and_wait_result(&source_path, &event_tx).await;

                            event_tx
                                .send(AgentEvent::ToolResult {
                                    name: "submit_update".to_string(),
                                    output: truncate_lines(&result_text, TOOL_TRUNCATE_LINES),
                                })
                                .await
                                .ok();
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
                event_tx.send(AgentEvent::Done).await.ok();
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
                    event_tx
                        .send(AgentEvent::Error(
                            "Hot replacement in progress. Shutting down...".into(),
                        ))
                        .await
                        .ok();
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

fn truncate_lines(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let total = lines.len();
    if total <= max_lines {
        return s.to_string();
    }

    let mut out = String::new();
    for line in lines.iter().take(max_lines) {
        out.push_str(line);
        out.push('\n');
    }
    let _ = write!(
        out,
        "... (truncated; showing first {} of {} lines)",
        max_lines, total
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_lines("hello", 500), "hello");
    }

    #[test]
    fn truncate_ascii_at_boundary() {
        let s = "a\n".repeat(10);
        let result = truncate_lines(&s, 3);
        assert!(
            result.contains("truncated; showing first 3 of 10 lines"),
            "unexpected output: {}",
            result
        );
    }

    #[test]
    fn truncate_multibyte_utf8_does_not_panic() {
        // "代理" is 2 chars, each 3 bytes in UTF-8; repeated 100 times = 200 chars, 600 bytes.
        // Truncates at a valid UTF-8 boundary when byte limit falls mid-character.
        let s: String = "代理\n".repeat(20);
        let result = truncate_lines(&s, 10);
        assert!(
            result.contains("truncated; showing first 10 of 20 lines"),
            "unexpected output: {}",
            result
        );
    }

    #[test]
    fn truncate_mixed_content() {
        // Mix of ASCII and multi-byte characters over multiple lines
        let s = format!("line1 {}\nline2 {}\nline3", "a".repeat(10), "代理代理");
        let result = truncate_lines(&s, 2);
        assert!(
            result.contains("truncated; showing first 2 of 3 lines"),
            "unexpected truncation output: {}",
            result
        );
    }

    #[test]
    fn format_update_accepted() {
        let envelope = Envelope {
            from: "boot".to_string(),
            to: "peripheral".to_string(),
            msg_type: msg_types::UPDATE_ACCEPTED.to_string(),
            id: "test-1".to_string(),
            payload: serde_json::json!({"version": "V3"}),
            fds: Vec::new(),
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
            fds: Vec::new(),
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
            fds: Vec::new(),
        };
        let result = format_update_result(&envelope);
        assert!(result.contains("REJECTED"));
        assert!(result.contains("test_failed"));
        // No errors or suggestion fields
        assert!(!result.contains("Compilation errors"));
        assert!(!result.contains("Suggestion"));
    }
}
