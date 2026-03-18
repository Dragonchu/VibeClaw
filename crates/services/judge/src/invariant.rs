use std::path::{Path, PathBuf};
use std::time::Duration;

use reloopy_ipc::messages::{
    Envelope, Hello, LeaseRenew, Welcome, msg_types, InvariantResult,
};
use reloopy_ipc::wire;
use serde::Deserialize;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};

#[derive(Debug, Deserialize)]
pub struct InvariantSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    pub timeout_secs: u64,
    #[serde(rename = "type")]
    pub test_type: String,
}

#[derive(Debug, Deserialize)]
pub struct InvariantsConfig {
    pub version: String,
    pub tests: Vec<InvariantSpec>,
}

impl InvariantsConfig {
    pub fn load(constitution_dir: &Path) -> Result<Self, String> {
        let path = constitution_dir.join("invariants.json");
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
    }
}

pub struct InvariantRunner {
    config: InvariantsConfig,
}

impl InvariantRunner {
    pub fn new(config: InvariantsConfig) -> Self {
        Self { config }
    }

    pub async fn run_all(&self, binary_path: &str) -> Vec<InvariantResult> {
        let mut results = Vec::new();

        for spec in &self.config.tests {
            let result = self.run_single(spec, binary_path).await;
            results.push(result);
        }

        results
    }

    async fn run_single(&self, spec: &InvariantSpec, binary_path: &str) -> InvariantResult {
        let timeout = Duration::from_secs(spec.timeout_secs);

        match tokio::time::timeout(timeout, self.execute_test(spec, binary_path)).await {
            Ok(Ok(())) => InvariantResult {
                test_id: spec.id.clone(),
                passed: true,
                detail: None,
            },
            Ok(Err(e)) => InvariantResult {
                test_id: spec.id.clone(),
                passed: false,
                detail: Some(e),
            },
            Err(_) => InvariantResult {
                test_id: spec.id.clone(),
                passed: false,
                detail: Some(format!("Test timed out after {}s", spec.timeout_secs)),
            },
        }
    }

    async fn execute_test(&self, spec: &InvariantSpec, binary_path: &str) -> Result<(), String> {
        match spec.test_type.as_str() {
            "handshake" => self.test_handshake(binary_path).await,
            "echo" => self.test_echo(binary_path).await,
            "heartbeat" => self.test_heartbeat(binary_path).await,
            other => Err(format!("Unknown test type: {}", other)),
        }
    }

    async fn spawn_candidate(&self, binary_path: &str) -> Result<(Child, UnixStream, PathBuf), String> {
        let temp_dir = std::env::temp_dir().join(format!("reloopy-judge-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to create temp dir: {}", e))?;

        let sock_path = temp_dir.join("test.sock");
        if sock_path.exists() {
            let _ = std::fs::remove_file(&sock_path);
        }

        let listener = UnixListener::bind(&sock_path)
            .map_err(|e| format!("Failed to bind test socket: {}", e))?;

        let child = Command::new(binary_path)
            .env("LOOPY_SOCK", &sock_path)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to spawn candidate binary: {}", e))?;

        let accept_timeout = Duration::from_secs(5);
        let (stream, _) = tokio::time::timeout(accept_timeout, listener.accept())
            .await
            .map_err(|_| "Candidate did not connect within 5s".to_string())?
            .map_err(|e| format!("Failed to accept connection: {}", e))?;

        Ok((child, stream, temp_dir))
    }

    fn cleanup_temp(temp_dir: &Path) {
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    async fn test_handshake(&self, binary_path: &str) -> Result<(), String> {
        let (mut child, stream, temp_dir) = self.spawn_candidate(binary_path).await?;
        let (mut reader, mut writer) = stream.into_split();

        let result = async {
            let hello_env = wire::read_envelope(&mut reader)
                .await
                .map_err(|e| format!("Failed to read Hello: {}", e))?;

            if hello_env.msg_type != msg_types::HELLO {
                return Err(format!("Expected Hello, got: {}", hello_env.msg_type));
            }

            let _hello: Hello = serde_json::from_value(hello_env.payload)
                .map_err(|e| format!("Invalid Hello payload: {}", e))?;

            let welcome = Welcome {
                accepted_capabilities: serde_json::json!([]),
                runlevel: 2,
            };

            let response = Envelope {
                from: "boot".to_string(),
                to: hello_env.from,
                msg_type: msg_types::WELCOME.to_string(),
                id: hello_env.id,
                payload: serde_json::to_value(&welcome).unwrap_or_default(),
            };

            wire::write_envelope(&mut writer, &response)
                .await
                .map_err(|e| format!("Failed to send Welcome: {}", e))?;

            Ok(())
        }
        .await;

        child.kill().await.ok();
        Self::cleanup_temp(&temp_dir);
        result
    }

    async fn test_echo(&self, binary_path: &str) -> Result<(), String> {
        let (mut child, stream, temp_dir) = self.spawn_candidate(binary_path).await?;
        let (mut reader, mut writer) = stream.into_split();

        let result = async {
            let hello_env = wire::read_envelope(&mut reader)
                .await
                .map_err(|e| format!("Failed to read Hello: {}", e))?;

            let welcome = Welcome {
                accepted_capabilities: serde_json::json!([]),
                runlevel: 2,
            };

            let welcome_response = Envelope {
                from: "boot".to_string(),
                to: hello_env.from.clone(),
                msg_type: msg_types::WELCOME.to_string(),
                id: hello_env.id,
                payload: serde_json::to_value(&welcome).unwrap_or_default(),
            };

            wire::write_envelope(&mut writer, &welcome_response)
                .await
                .map_err(|e| format!("Failed to send Welcome: {}", e))?;

            let echo_payload = serde_json::json!({"echo": "ping"});
            let echo_request = Envelope {
                from: "boot".to_string(),
                to: hello_env.from.clone(),
                msg_type: "Echo".to_string(),
                id: "echo-test-1".to_string(),
                payload: echo_payload.clone(),
            };

            wire::write_envelope(&mut writer, &echo_request)
                .await
                .map_err(|e| format!("Failed to send Echo: {}", e))?;

            let response = tokio::time::timeout(
                Duration::from_secs(3),
                wire::read_envelope(&mut reader),
            )
            .await
            .map_err(|_| "Echo response timed out".to_string())?
            .map_err(|e| format!("Failed to read Echo response: {}", e))?;

            if response.msg_type == "EchoResponse" && response.payload == echo_payload {
                Ok(())
            } else if response.msg_type == msg_types::LEASE_RENEW {
                // Heartbeat came first — read next message
                let response = tokio::time::timeout(
                    Duration::from_secs(3),
                    wire::read_envelope(&mut reader),
                )
                .await
                .map_err(|_| "Echo response timed out (after heartbeat)".to_string())?
                .map_err(|e| format!("Failed to read Echo response: {}", e))?;

                if response.msg_type == "EchoResponse" && response.payload == echo_payload {
                    Ok(())
                } else {
                    Err(format!(
                        "Unexpected echo response: type={}, payload={}",
                        response.msg_type, response.payload
                    ))
                }
            } else {
                Err(format!(
                    "Unexpected response: type={}, payload={}",
                    response.msg_type, response.payload
                ))
            }
        }
        .await;

        child.kill().await.ok();
        Self::cleanup_temp(&temp_dir);
        result
    }

    async fn test_heartbeat(&self, binary_path: &str) -> Result<(), String> {
        let (mut child, stream, temp_dir) = self.spawn_candidate(binary_path).await?;
        let (mut reader, mut writer) = stream.into_split();

        let result = async {
            let hello_env = wire::read_envelope(&mut reader)
                .await
                .map_err(|e| format!("Failed to read Hello: {}", e))?;

            let welcome = Welcome {
                accepted_capabilities: serde_json::json!([]),
                runlevel: 2,
            };

            let welcome_response = Envelope {
                from: "boot".to_string(),
                to: hello_env.from.clone(),
                msg_type: msg_types::WELCOME.to_string(),
                id: hello_env.id,
                payload: serde_json::to_value(&welcome).unwrap_or_default(),
            };

            wire::write_envelope(&mut writer, &welcome_response)
                .await
                .map_err(|e| format!("Failed to send Welcome: {}", e))?;

            let heartbeat_timeout = Duration::from_secs(12);
            let envelope = tokio::time::timeout(heartbeat_timeout, wire::read_envelope(&mut reader))
                .await
                .map_err(|_| "No heartbeat received within 12s".to_string())?
                .map_err(|e| format!("Failed to read heartbeat: {}", e))?;

            if envelope.msg_type != msg_types::LEASE_RENEW {
                return Err(format!("Expected LeaseRenew, got: {}", envelope.msg_type));
            }

            let _health: LeaseRenew = serde_json::from_value(envelope.payload)
                .map_err(|e| format!("Invalid LeaseRenew payload: {}", e))?;

            Ok(())
        }
        .await;

        child.kill().await.ok();
        Self::cleanup_temp(&temp_dir);
        result
    }
}
