use tokio::sync::mpsc;

use crate::deepseek::{ChatMessage, DeepSeekClient, StreamEvent};
use crate::memory::MemoryManager;
use crate::source::SourceManager;
use crate::tools::{self, ToolResult};

const BASE_SYSTEM_PROMPT: &str = r#"You are Reloopy, a self-evolving AI agent written in Rust. You can read and modify your own source code to improve yourself.

## Source Code Tools
- read_source_file(path): Read a file from your source code. Path is relative to the peripheral crate root (e.g. "src/main.rs", "Cargo.toml")
- list_source_files(path): List files in your source directory. Path is relative to the peripheral crate root (e.g. "src/", ".")
- write_source_file(path, content): Stage changes to a file. Path is relative to crates/peripheral/ (e.g. "src/main.rs"). Provide the FULL file content.
- submit_update(): Submit all staged changes for compilation and deployment.

## Memory Tools
- memory_search(query): Search across all memory files for relevant content.
- memory_get(date): Get a daily log. date = "today" | "yesterday" | "YYYY-MM-DD".
- memory_write(content): Overwrite MEMORY.md with updated long-term facts. Always call memory_get or memory_search first to read existing content, then merge and rewrite the full document.
- memory_append(content): Append a note to today's daily log.

## Guidelines
- Always read the relevant source files before making changes
- Ensure your changes produce valid, compilable Rust code
- Maintain IPC protocol compatibility (handshake, heartbeat, message handling)
- Make focused, incremental changes
- After writing all modified files, call submit_update() to deploy
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
    SubmitUpdate(String),
}

pub struct Agent {
    deepseek: DeepSeekClient,
    source: SourceManager,
    memory: MemoryManager,
    conversation: Vec<ChatMessage>,
}

impl Agent {
    pub fn new(deepseek: DeepSeekClient, source: SourceManager, memory: MemoryManager) -> Self {
        let system_prompt = build_system_prompt(&memory);
        Self {
            deepseek,
            source,
            memory,
            conversation: vec![ChatMessage::system(&system_prompt)],
        }
    }

    pub fn source_mut(&mut self) -> &mut SourceManager {
        &mut self.source
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

                let mut submit_path = None;

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
                        ToolResult::SubmitUpdate(path) => {
                            let _ = event_tx
                                .send(AgentEvent::SubmitUpdate {
                                    source_path: path.clone(),
                                })
                                .await;
                            self.conversation.push(ChatMessage::tool(
                                "Update packaged and ready for submission.",
                                &tc.id,
                            ));
                            submit_path = Some(path);
                        }
                    }
                }

                if let Some(path) = submit_path {
                    let _ = event_tx.send(AgentEvent::Done).await;
                    return Ok(AgentOutcome::SubmitUpdate(path));
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

    pub fn reset_conversation(&mut self) {
        let system_prompt = build_system_prompt(&self.memory);
        self.conversation = vec![ChatMessage::system(&system_prompt)];
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...(truncated)", &s[..max])
    }
}
