//! Scripted (mock) LLM client for deterministic integration testing.
//!
//! [`ScriptedLlmClient`] holds a pre-defined queue of [`ChatMessage`] responses.
//! Each call to `chat_stream` pops the next response off the queue, emits the
//! appropriate [`StreamEvent`]s (reasoning, content, tool-call starts / arg
//! deltas), sends [`StreamEvent::Done`], and returns the message.  Tool results
//! from the agent are ignored — the script is replayed in order regardless of
//! what the agent does with each turn.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::deepseek::{ChatMessage, LlmClient, StreamEvent, ToolDefinition};

/// A scripted LLM client that replays a fixed sequence of [`ChatMessage`]s.
///
/// # Example
/// ```no_run
/// use reloopy_peripheral::scripted_llm::ScriptedLlmClient;
/// use reloopy_peripheral::deepseek::ChatMessage;
///
/// let client = ScriptedLlmClient::new(vec![
///     ChatMessage::user("hello"),          // first response: plain text
/// ]);
/// ```
pub struct ScriptedLlmClient {
    responses: Arc<Mutex<VecDeque<ChatMessage>>>,
}

impl ScriptedLlmClient {
    /// Create a new client that will replay `responses` in order.
    pub fn new(responses: Vec<ChatMessage>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
        }
    }
}

impl LlmClient for ScriptedLlmClient {
    fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: Option<&[ToolDefinition]>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) -> impl Future<Output = Result<ChatMessage, String>> + Send {
        let responses = Arc::clone(&self.responses);

        async move {
            let msg = {
                let mut queue = responses.lock().await;
                queue
                    .pop_front()
                    .ok_or_else(|| "ScriptedLlmClient: no more scripted responses".to_string())?
            };

            // Emit stream events that mirror what a real LLM would send.
            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    event_tx
                        .send(StreamEvent::ToolCallStart {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                        })
                        .await
                        .ok();
                    if !tc.function.arguments.is_empty() {
                        event_tx
                            .send(StreamEvent::ToolCallArgDelta(tc.function.arguments.clone()))
                            .await
                            .ok();
                    }
                }
            } else if let Some(ref content) = msg.content {
                event_tx.send(StreamEvent::Content(content.clone())).await.ok();
            }

            event_tx.send(StreamEvent::Done).await.ok();

            Ok(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deepseek::{FunctionCall, ToolCall};

    #[tokio::test]
    async fn scripted_client_returns_responses_in_order() {
        let responses = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("first".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("second".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let client = ScriptedLlmClient::new(responses);
        let (tx, _rx) = mpsc::channel(16);

        let first = client.chat_stream(&[], None, tx.clone()).await.unwrap();
        assert_eq!(first.content.as_deref(), Some("first"));

        let second = client.chat_stream(&[], None, tx.clone()).await.unwrap();
        assert_eq!(second.content.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn scripted_client_errors_when_exhausted() {
        let client = ScriptedLlmClient::new(vec![]);
        let (tx, _rx) = mpsc::channel(16);
        let result = client.chat_stream(&[], None, tx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no more scripted responses"));
    }

    #[tokio::test]
    async fn scripted_client_emits_content_event() {
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: Some("hello world".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        let client = ScriptedLlmClient::new(vec![msg]);
        let (tx, mut rx) = mpsc::channel(16);

        let _ = client.chat_stream(&[], None, tx).await;

        let mut events = vec![];
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert!(
            events.iter().any(|e| matches!(e, StreamEvent::Content(s) if s == "hello world")),
            "expected Content event with 'hello world'"
        );
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Done)));
    }

    #[tokio::test]
    async fn scripted_client_emits_tool_call_events() {
        let msg = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call-1".to_string(),
                type_: "function".to_string(),
                function: FunctionCall {
                    name: "read_source_file".to_string(),
                    arguments: r#"{"path":"src/main.rs"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
        };
        let client = ScriptedLlmClient::new(vec![msg]);
        let (tx, mut rx) = mpsc::channel(16);

        let result = client.chat_stream(&[], None, tx).await.unwrap();
        assert!(result.tool_calls.is_some());

        let mut events = vec![];
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert!(
            events.iter().any(|e| matches!(e,
                StreamEvent::ToolCallStart { name, .. } if name == "read_source_file"
            )),
            "expected ToolCallStart event"
        );
        assert!(
            events.iter().any(|e| matches!(e,
                StreamEvent::ToolCallArgDelta(args) if args.contains("src/main.rs")
            )),
            "expected ToolCallArgDelta event"
        );
    }
}
