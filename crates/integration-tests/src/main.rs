//! CLI integration test for the Reloopy evolution pipeline.
//!
//! Runs a full Agent loop using [`ScriptedLlmClient`] so no real LLM API is
//! needed.  A pair of [`mpsc`] channels stand in for the Boot IPC socket, so
//! no real Boot or Compiler process is required either.
//!
//! # Usage
//!
//! ```sh
//! cargo run --bin reloopy-integration-tests
//! # or with a specific scenario file:
//! cargo run --bin reloopy-integration-tests -- --scenario path/to/scenario.json
//! ```
//!
//! # Scenario format
//!
//! A JSON file containing an array of assistant [`ChatMessage`] objects (each
//! item is one LLM turn).  Tool-call messages must include a `tool_calls`
//! array; plain text turns supply `content`.  Example:
//!
//! ```json
//! [
//!   {
//!     "role": "assistant",
//!     "content": "I will read the main file first.",
//!     "tool_calls": [{
//!       "id": "tc-1",
//!       "type": "function",
//!       "function": { "name": "list_source_files", "arguments": "{\"path\":\"src\"}" }
//!     }]
//!   },
//!   {
//!     "role": "assistant",
//!     "content": "Done! The source files look good."
//!   }
//! ]
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use reloopy_ipc::messages::{Envelope, msg_types};
use reloopy_peripheral::agent::{Agent, AgentEvent, AgentOutcome};
use reloopy_peripheral::deepseek::ChatMessage;
use reloopy_peripheral::memory::MemoryManager;
use reloopy_peripheral::scripted_llm::ScriptedLlmClient;
use reloopy_peripheral::source::SourceManager;

/// Simple CLI argument parsing (no clap dependency to stay lean).
struct Args {
    scenario_path: Option<PathBuf>,
    user_input: String,
    workspace: Option<PathBuf>,
}

impl Args {
    fn parse() -> Self {
        let mut args = std::env::args().skip(1);
        let mut scenario_path = None;
        let mut user_input = "Please read the source files and give me a summary.".to_string();
        let mut workspace = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--scenario" => {
                    scenario_path = args.next().map(PathBuf::from);
                }
                "--input" => {
                    user_input = args.next().unwrap_or(user_input);
                }
                "--workspace" => {
                    workspace = args.next().map(PathBuf::from);
                }
                other => {
                    eprintln!(
                        "Unknown argument: {other}. Valid options: --scenario <path>, --input <text>, --workspace <path>"
                    );
                    std::process::exit(1);
                }
            }
        }

        Self {
            scenario_path,
            user_input,
            workspace,
        }
    }
}

/// Load a scripted scenario from a JSON file, or fall back to a default
/// single-turn response.
fn load_scenario(path: Option<&PathBuf>) -> Vec<ChatMessage> {
    if let Some(p) = path {
        let content = std::fs::read_to_string(p)
            .unwrap_or_else(|e| panic!("Cannot read scenario file {}: {e}", p.display()));
        serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("Cannot parse scenario JSON: {e}"))
    } else {
        // Default two-turn scenario: tool call then text response.
        vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![reloopy_peripheral::deepseek::ToolCall {
                    id: "tc-1".to_string(),
                    type_: "function".to_string(),
                    function: reloopy_peripheral::deepseek::FunctionCall {
                        name: "list_source_files".to_string(),
                        arguments: r#"{"path":"src"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(
                    "I reviewed the source files. Everything looks fine.".to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            },
        ]
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    // ── Workspace ────────────────────────────────────────────────────────────
    // Use a provided workspace path or the repository root detected by
    // CARGO_MANIFEST_DIR (set by cargo at compile time).
    let workspace_root = args.workspace.unwrap_or_else(|| {
        // Try RELOOPY_WORKSPACE env var, then fall back to the repo root
        // inferred from this binary's manifest directory.
        if let Ok(ws) = std::env::var("RELOOPY_WORKSPACE") {
            PathBuf::from(ws)
        } else {
            // CARGO_MANIFEST_DIR points to crates/integration-tests; go up two
            // levels to reach the workspace root.
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            manifest
                .parent()
                .and_then(|p| p.parent())
                .unwrap_or(&manifest)
                .to_path_buf()
        }
    });

    tracing::info!(workspace = %workspace_root.display(), "Integration test starting");

    // ── Temp directory for memory ────────────────────────────────────────────
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let base_dir = tmp_dir.path().to_path_buf();

    // ── IPC mock channels ────────────────────────────────────────────────────
    // The agent sends Envelopes on ipc_tx and receives results on update_result_rx.
    // We provide the other ends so the test can simulate Boot responses.
    let (ipc_tx, mut mock_boot_rx) = mpsc::channel::<Envelope>(16);
    let (mock_boot_tx, update_result_rx) = mpsc::channel::<Envelope>(16);

    // Spawn a task that acts as Boot: whenever the agent sends a SubmitUpdate,
    // reply with an UPDATE_ACCEPTED message.
    let boot_task = tokio::spawn(async move {
        while let Some(envelope) = mock_boot_rx.recv().await {
            tracing::info!(msg_type = %envelope.msg_type, "MockBoot: received message");
            if envelope.msg_type == msg_types::SUBMIT_UPDATE {
                let accepted = Envelope {
                    from: "boot".to_string(),
                    to: "peripheral".to_string(),
                    msg_type: msg_types::UPDATE_ACCEPTED.to_string(),
                    id: "mock-1".to_string(),
                    payload: serde_json::json!({ "version": "V1" }),
                    fds: vec![],
                };
                if mock_boot_tx.send(accepted).await.is_err() {
                    break;
                }
            }
        }
        tracing::debug!("MockBoot: exiting");
    });

    // ── Build Agent ──────────────────────────────────────────────────────────
    let responses = load_scenario(args.scenario_path.as_ref());
    tracing::info!(turns = responses.len(), "Loaded scripted scenario");

    let llm = ScriptedLlmClient::new(responses);
    let source = SourceManager::new(workspace_root);
    let memory = MemoryManager::new(&base_dir);

    let agent = Agent::new(llm, source, memory, ipc_tx, update_result_rx, {
        let (_tx, rx) = mpsc::channel(1);
        rx
    });
    let state = Arc::new(Mutex::new(agent));

    // ── Run scenario ─────────────────────────────────────────────────────────
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

    let agent_state = Arc::clone(&state);
    let input = args.user_input.clone();
    let agent_handle = tokio::spawn(async move {
        let mut agent = agent_state.lock().await;
        agent.handle_input_stream(&input, event_tx).await
    });

    // Print all events to stdout.
    while let Some(ev) = event_rx.recv().await {
        match &ev {
            AgentEvent::Content(s) => print!("{s}"),
            AgentEvent::Reasoning(s) => {
                tracing::debug!(reasoning = %s, "Reasoning");
            }
            AgentEvent::ToolCallStart { name, id } => {
                tracing::info!(tool = %name, id = %id, "Tool call");
            }
            AgentEvent::ToolCallArgDelta(_) => {}
            AgentEvent::ToolResult { name, output } => {
                tracing::info!(tool = %name, "Tool result ({} chars)", output.len());
            }
            AgentEvent::SubmitUpdate { source_path } => {
                tracing::info!(path = %source_path, "SubmitUpdate sent to Boot");
            }
            AgentEvent::Error(e) => {
                tracing::error!("Agent error: {e}");
            }
            AgentEvent::Done => {
                println!();
                tracing::info!("Agent finished");
            }
        }
    }

    let outcome = agent_handle
        .await
        .expect("Agent task panicked")
        .unwrap_or_else(|e| panic!("Agent returned error: {e}"));

    match outcome {
        AgentOutcome::Done => tracing::info!("Scenario completed successfully"),
    }

    boot_task.abort();

    println!("\n[reloopy-integration-tests] PASS");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scenario_has_two_turns() {
        let scenario = load_scenario(None);
        assert_eq!(scenario.len(), 2);
        assert!(
            scenario[0].tool_calls.is_some(),
            "first turn should be a tool call"
        );
        assert!(
            scenario[1].content.is_some(),
            "second turn should be plain text"
        );
    }

    #[tokio::test]
    async fn agent_with_scripted_llm_runs_to_completion() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .unwrap()
            .to_path_buf();

        let (ipc_tx, _rx) = mpsc::channel::<Envelope>(16);
        let (_tx, update_result_rx) = mpsc::channel::<Envelope>(16);

        let responses = vec![ChatMessage {
            role: "assistant".to_string(),
            content: Some("Test complete.".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];

        let llm = ScriptedLlmClient::new(responses);
        let source = SourceManager::new(workspace_root);
        let memory = MemoryManager::new(tmp.path());
        let mut agent = Agent::new(llm, source, memory, ipc_tx, update_result_rx, {
            let (_tx, rx) = mpsc::channel(1);
            rx
        });

        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
        let handle = tokio::spawn(async move {
            agent
                .handle_input_stream("hello", event_tx)
                .await
        });

        let mut saw_done = false;
        let mut content = String::new();
        while let Some(ev) = event_rx.recv().await {
            match ev {
                AgentEvent::Content(s) => content.push_str(&s),
                AgentEvent::Done => saw_done = true,
                _ => {}
            }
        }

        let outcome = handle.await.unwrap().unwrap();
        assert!(matches!(outcome, AgentOutcome::Done));
        assert!(saw_done, "expected Done event");
        assert_eq!(content, "Test complete.");
    }
}
