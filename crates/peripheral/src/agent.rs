use crate::deepseek::{ChatMessage, DeepSeekClient};
use crate::source::SourceManager;
use crate::tools::{self, ToolResult};

const SYSTEM_PROMPT: &str = r#"You are Loopy, a self-evolving AI agent written in Rust. You can read and modify your own source code to improve yourself.

## Available Tools
- read_source_file(path): Read a file from your source code. Path is relative to the peripheral crate root (e.g. "src/main.rs", "Cargo.toml")
- list_source_files(path): List files in your source directory. Path is relative to the peripheral crate root (e.g. "src/", ".")
- write_source_file(path, content): Stage changes to a file. Path is relative to crates/peripheral/ (e.g. "src/main.rs"). Provide the FULL file content.
- submit_update(): Submit all staged changes for compilation and deployment.

## Guidelines
- Always read the relevant source files before making changes
- Ensure your changes produce valid, compilable Rust code
- Maintain IPC protocol compatibility (handshake, heartbeat, message handling)
- Make focused, incremental changes
- After writing all modified files, call submit_update() to deploy
"#;

pub struct Agent {
    deepseek: DeepSeekClient,
    source: SourceManager,
    conversation: Vec<ChatMessage>,
}

pub enum AgentOutput {
    Done,
    SubmitUpdate(String),
}

impl Agent {
    pub fn new(deepseek: DeepSeekClient, source: SourceManager) -> Self {
        Self {
            deepseek,
            source,
            conversation: vec![ChatMessage::system(SYSTEM_PROMPT)],
        }
    }

    pub fn source_mut(&mut self) -> &mut SourceManager {
        &mut self.source
    }

    pub async fn handle_input(&mut self, user_input: &str) -> Result<AgentOutput, String> {
        self.conversation.push(ChatMessage::user(user_input));
        let tool_defs = tools::tool_definitions();

        loop {
            let response = self
                .deepseek
                .chat(&self.conversation, Some(&tool_defs))
                .await?;

            let choice = response
                .choices
                .into_iter()
                .next()
                .ok_or("Empty response from DeepSeek")?;

            let message = choice.message;

            if let Some(ref tool_calls) = message.tool_calls {
                self.conversation.push(message.clone());

                let mut submit_path = None;

                for tc in tool_calls {
                    tracing::debug!(tool = %tc.function.name, "Executing tool");

                    let result = tools::execute_tool(
                        &tc.function.name,
                        &tc.function.arguments,
                        &mut self.source,
                    );

                    match result {
                        ToolResult::Output(output) => {
                            println!("  [{}] {}", tc.function.name, truncate(&output, 200));
                            self.conversation.push(ChatMessage::tool(&output, &tc.id));
                        }
                        ToolResult::SubmitUpdate(path) => {
                            self.conversation.push(ChatMessage::tool(
                                "Update packaged and ready for submission.",
                                &tc.id,
                            ));
                            submit_path = Some(path);
                        }
                    }
                }

                if let Some(path) = submit_path {
                    return Ok(AgentOutput::SubmitUpdate(path));
                }
            } else {
                let text = message.content.unwrap_or_default();
                if !text.is_empty() {
                    println!("{}", text);
                }
                self.conversation.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                });
                return Ok(AgentOutput::Done);
            }
        }
    }

    pub fn reset_conversation(&mut self) {
        self.conversation = vec![ChatMessage::system(SYSTEM_PROMPT)];
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...(truncated)", &s[..max])
    }
}
