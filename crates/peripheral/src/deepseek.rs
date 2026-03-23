use std::future::Future;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool(content: &str, tool_call_id: &str) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub type_: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChunk {
    pub choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub delta: StreamDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamToolCall {
    pub index: Option<usize>,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub function: Option<StreamFunctionCall>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamFunctionCall {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum StreamEvent {
    Reasoning(String),
    Content(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgDelta(String),
    Done,
    Error(String),
}

/// Abstraction over any LLM backend.  Implement this trait to substitute a
/// different backend (e.g. [`ScriptedLlmClient`] for testing).
pub trait LlmClient: Send + Sync {
    fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) -> impl Future<Output = Result<ChatMessage, String>> + Send;
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

pub struct DeepSeekClient {
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl DeepSeekClient {
    pub fn new(api_key: String, base_url: Option<String>, model: Option<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.deepseek.com".to_string()),
            model: model.unwrap_or_else(|| "deepseek-chat".to_string()),
            client: reqwest::Client::new(),
        }
    }

    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
    ) -> Result<ChatResponse, String> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            tools: tools.map(|t| t.to_vec()),
            stream: false,
        };

        let url = format!("{}/v1/chat/completions", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("API error ({}): {}", status, body));
        }

        response
            .json::<ChatResponse>()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))
    }
}

impl LlmClient for DeepSeekClient {
    fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) -> impl Future<Output = Result<ChatMessage, String>> + Send {
        let model = self.model.clone();
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let messages = messages.to_vec();
        let tools_owned = tools.map(|t| t.to_vec());

        async move {
            let request = ChatRequest {
                model,
                messages,
                tools: tools_owned,
                stream: true,
            };

            let url = format!("{}/v1/chat/completions", base_url);

            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(&request)
                .send()
                .await
                .map_err(|e| format!("HTTP request failed: {}", e))?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(format!("API error ({}): {}", status, body));
            }

            let mut full_content = String::new();
            let mut pending_tool_calls: std::collections::BTreeMap<usize, ToolCall> =
                std::collections::BTreeMap::new();

            use tokio_stream::StreamExt;
            let mut byte_stream = std::pin::pin!(response.bytes_stream());
            let mut buf = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::error!("Stream read error: {}", e);
                        event_tx
                            .send(StreamEvent::Error(format!("Stream error: {}", e)))
                            .await
                            .ok();
                        break;
                    }
                };
                buf.push_str(&String::from_utf8_lossy(&bytes));

                while let Some(line_end) = buf.find('\n') {
                    let line = buf[..line_end].trim().to_string();
                    buf = buf[line_end + 1..].to_string();

                    if line.is_empty() || line == "data: [DONE]" {
                        continue;
                    }

                    let json_str = line.strip_prefix("data: ").unwrap_or(&line);
                    let chunk: StreamChunk = match serde_json::from_str(json_str) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    for choice in &chunk.choices {
                        if let Some(ref reasoning) = choice.delta.reasoning_content {
                            event_tx
                                .send(StreamEvent::Reasoning(reasoning.clone()))
                                .await
                                .ok();
                        }

                        if let Some(ref content) = choice.delta.content {
                            full_content.push_str(content);
                            event_tx.send(StreamEvent::Content(content.clone())).await.ok();
                        }

                        if let Some(ref tcs) = choice.delta.tool_calls {
                            for tc in tcs {
                                let update =
                                    apply_tool_call_delta(&mut pending_tool_calls, tc.clone());

                                if let Some((id, name)) = update.start {
                                    event_tx
                                        .send(StreamEvent::ToolCallStart { id, name })
                                        .await
                                        .ok();
                                }

                                if let Some(args) = update.arg_delta {
                                    event_tx
                                        .send(StreamEvent::ToolCallArgDelta(args))
                                        .await
                                        .ok();
                                }
                            }
                        }
                    }
                }
            }

            let tool_calls: Vec<ToolCall> = pending_tool_calls.into_values().collect();

            event_tx.send(StreamEvent::Done).await.ok();

            Ok(ChatMessage {
                role: "assistant".to_string(),
                content: if full_content.is_empty() {
                    None
                } else {
                    Some(full_content)
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
            })
        }
    }
}

const DEFAULT_TOOL_CALL_TYPE: &str = "function";

#[derive(Default)]
struct ToolCallUpdate {
    start: Option<(String, String)>,
    arg_delta: Option<String>,
}

fn apply_tool_call_delta(
    pending_tool_calls: &mut std::collections::BTreeMap<usize, ToolCall>,
    tc: StreamToolCall,
) -> ToolCallUpdate {
    use std::collections::btree_map::Entry;

    let idx = tc.index.unwrap_or(0);
    let fn_call = tc.function.as_ref();
    let fn_name = fn_call.and_then(|f| f.name.clone()).unwrap_or_default();
    let fn_args = fn_call.and_then(|f| f.arguments.clone());

    let mut update = ToolCallUpdate::default();

    if let Some(id) = tc.id {
        let type_ = tc.type_.as_deref().unwrap_or(DEFAULT_TOOL_CALL_TYPE);

        match pending_tool_calls.entry(idx) {
            Entry::Vacant(v) => {
                v.insert(ToolCall {
                    id: id.clone(),
                    type_: type_.to_string(),
                    function: FunctionCall {
                        name: fn_name.clone(),
                        arguments: String::new(),
                    },
                });
                update.start = Some((id, fn_name.clone()));
            }
            Entry::Occupied(mut o) => {
                let tc_ref = o.get_mut();
                if tc_ref.id != id {
                    tracing::warn!(
                        %idx,
                        previous = %tc_ref.id,
                        new = %id,
                        "Tool call id changed within stream"
                    );
                    debug_assert_eq!(
                        tc_ref.id, id,
                        "Tool call id changed within stream (idx={})",
                        idx
                    );
                }
                if tc_ref.type_.as_str() != type_ {
                    tracing::warn!(
                        %idx,
                        previous = %tc_ref.type_,
                        new = %type_,
                        "Tool call type changed within stream"
                    );
                    debug_assert_eq!(
                        tc_ref.type_.as_str(),
                        type_,
                        "Tool call type changed within stream (idx={})",
                        idx
                    );
                }
                if !fn_name.is_empty() && tc_ref.function.name.is_empty() {
                    tc_ref.function.name = fn_name.clone();
                } else if !fn_name.is_empty() {
                    if tc_ref.function.name != fn_name {
                        tracing::warn!(
                            %idx,
                            previous = %tc_ref.function.name,
                            new = %fn_name,
                            "Tool call name changed within stream"
                        );
                        debug_assert_eq!(
                            tc_ref.function.name, fn_name,
                            "Tool call name changed within stream (idx={})",
                            idx
                        );
                    }
                }
            }
        }
    }

    if let Some(args) = fn_args {
        if let Some(tc_ref) = pending_tool_calls.get_mut(&idx) {
            tc_ref.function.arguments.push_str(&args);
        }
        update.arg_delta = Some(args);
    }

    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn tool_call_delta_preserves_arguments_across_duplicate_ids() {
        let mut pending: BTreeMap<usize, ToolCall> = BTreeMap::new();

        let first = StreamToolCall {
            index: Some(0),
            id: Some("call-1".into()),
            type_: Some("function".into()),
            function: Some(StreamFunctionCall {
                name: Some("write_source_file".into()),
                arguments: None,
            }),
        };

        let second = StreamToolCall {
            index: Some(0),
            id: Some("call-1".into()),
            type_: Some("function".into()),
            function: Some(StreamFunctionCall {
                name: Some("write_source_file".into()),
                arguments: Some("part-1".into()),
            }),
        };

        let third = StreamToolCall {
            index: Some(0),
            id: Some("call-1".into()),
            type_: Some("function".into()),
            function: Some(StreamFunctionCall {
                name: Some("write_source_file".into()),
                arguments: Some("part-2".into()),
            }),
        };

        let update = apply_tool_call_delta(&mut pending, first);
        assert_eq!(
            update.start,
            Some(("call-1".to_string(), "write_source_file".to_string()))
        );
        assert!(
            pending
                .get(&0)
                .map(|tc| tc.function.arguments.is_empty())
                .unwrap_or(false)
        );

        let update2 = apply_tool_call_delta(&mut pending, second);
        assert!(update2.start.is_none(), "start should not fire twice");
        assert_eq!(
            pending.get(&0).unwrap().function.arguments,
            "part-1".to_string()
        );

        apply_tool_call_delta(&mut pending, third);
        assert_eq!(
            pending.get(&0).unwrap().function.arguments,
            "part-1part-2".to_string(),
            "arguments should accumulate across deltas without being reset"
        );
    }
}
