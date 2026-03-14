//! Protocol evolution via negotiated extensions.
//!
//! Maintains a JSON protocol registry at `~/.loopy/protocol/messages.json`
//! that tracks known message types and the current protocol version.
//! Backward-compatible extensions (new message types) can be auto-adopted;
//! breaking changes (modifying existing message schemas) require human
//! HMAC approval.
//! See plan §4.1.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const PROTOCOL_DIR: &str = "protocol";
const MESSAGES_FILE: &str = "messages.json";
const EXTENSION_LOG: &str = "extensions.log";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolRegistry {
    pub protocol_version: String,
    pub messages: HashMap<String, MessageTypeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTypeEntry {
    pub category: String,
    pub direction: String,
    pub schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_in: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExtensionLogEntry {
    pub timestamp: u64,
    pub old_version: String,
    pub new_version: String,
    pub breaking: bool,
    pub added_types: Vec<String>,
    pub modified_types: Vec<String>,
    pub description: String,
}

pub struct ProtocolManager {
    protocol_dir: PathBuf,
    registry: ProtocolRegistry,
}

impl ProtocolManager {
    pub fn new(base_dir: &Path) -> Result<Self, String> {
        let protocol_dir = base_dir.join(PROTOCOL_DIR);
        fs::create_dir_all(&protocol_dir)
            .map_err(|e| format!("Failed to create protocol dir: {}", e))?;

        let registry = Self::load_or_create_registry(&protocol_dir)?;

        Ok(Self {
            protocol_dir,
            registry,
        })
    }

    fn load_or_create_registry(protocol_dir: &Path) -> Result<ProtocolRegistry, String> {
        let path = protocol_dir.join(MESSAGES_FILE);
        if path.exists() {
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
        } else {
            let registry = Self::default_registry();
            let content = serde_json::to_string_pretty(&registry)
                .map_err(|e| format!("Failed to serialize registry: {}", e))?;
            fs::write(&path, content)
                .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
            tracing::info!(path = %path.display(), "Created default protocol registry");
            Ok(registry)
        }
    }

    fn default_registry() -> ProtocolRegistry {
        let mut messages = HashMap::new();

        let core_types = [
            ("Hello", "handshake", "peer->boot"),
            ("Welcome", "handshake", "boot->peer"),
            ("LeaseRenew", "heartbeat", "peer->boot"),
            ("LeaseAck", "heartbeat", "boot->peer"),
            ("RunlevelChange", "runlevel", "boot->all"),
            ("Shutdown", "lifecycle", "boot->peer"),
            ("SubmitUpdate", "update", "peripheral->boot"),
            ("CompileRequest", "update", "boot->compiler"),
            ("CompileResult", "update", "compiler->boot"),
            ("UpdateRejected", "update", "boot->peripheral"),
            ("UpdateAccepted", "update", "boot->peripheral"),
            ("GetState", "state", "peer->boot"),
            ("GetStateResponse", "state", "boot->peer"),
            ("SetState", "state", "peer->boot"),
            ("SetStateAck", "state", "boot->peer"),
            ("TestRequest", "judge", "boot->judge"),
            ("TestResult", "judge", "judge->boot"),
            ("ProbationStarted", "judge", "boot->peripheral"),
            ("ProbationEnded", "judge", "boot->peripheral"),
            ("AuditLog", "audit", "boot->audit"),
            ("ResourceViolation", "security", "boot->audit"),
            ("RunlevelRequest", "runlevel", "peer->boot"),
            ("RunlevelRequestResult", "runlevel", "boot->peer"),
            ("CapabilityEscalation", "security", "boot->audit"),
            ("ConstitutionAmendmentProposal", "constitution", "peer->boot"),
            ("ConstitutionAmendmentResult", "constitution", "boot->peer"),
            ("ProtocolExtensionProposal", "protocol", "peer->boot"),
            ("ProtocolExtensionResult", "protocol", "boot->peer"),
        ];

        for (name, category, direction) in core_types {
            messages.insert(
                name.to_string(),
                MessageTypeEntry {
                    category: category.to_string(),
                    direction: direction.to_string(),
                    schema: serde_json::Value::Object(serde_json::Map::new()),
                    added_in: Some("1.0".to_string()),
                },
            );
        }

        ProtocolRegistry {
            protocol_version: "1.0".to_string(),
            messages,
        }
    }

    pub fn current_version(&self) -> &str {
        &self.registry.protocol_version
    }

    pub fn is_known_message(&self, msg_type: &str) -> bool {
        self.registry.messages.contains_key(msg_type)
    }

    pub fn propose_extension(
        &mut self,
        new_messages: &serde_json::Value,
        breaking: bool,
        description: &str,
        signature_verifier: Option<&dyn Fn(&str, &str) -> bool>,
        signature: Option<&str>,
    ) -> Result<String, String> {
        let proposed = new_messages.as_object()
            .ok_or("new_messages must be a JSON object")?;

        if proposed.is_empty() {
            return Err("No message types proposed".to_string());
        }

        let mut added_types = Vec::new();
        let mut modified_types = Vec::new();

        for (name, _schema) in proposed {
            if self.registry.messages.contains_key(name) {
                modified_types.push(name.clone());
            } else {
                added_types.push(name.clone());
            }
        }

        let is_breaking = breaking || !modified_types.is_empty();

        if is_breaking {
            match (signature_verifier, signature) {
                (Some(verify), Some(sig)) => {
                    let payload = format!(
                        "protocol_extension:{}:{}",
                        description,
                        serde_json::to_string(new_messages).unwrap_or_default()
                    );
                    if !verify(&payload, sig) {
                        return Err(
                            "Breaking protocol change requires valid human HMAC signature"
                                .to_string(),
                        );
                    }
                }
                _ => {
                    return Err(
                        "Breaking protocol changes require human HMAC signature".to_string()
                    );
                }
            }
        }

        let new_version = self.bump_version(is_breaking);

        for (name, schema) in proposed {
            let entry = MessageTypeEntry {
                category: schema
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("extension")
                    .to_string(),
                direction: schema
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("peer->boot")
                    .to_string(),
                schema: schema.clone(),
                added_in: Some(new_version.clone()),
            };
            self.registry.messages.insert(name.clone(), entry);
        }

        self.registry.protocol_version = new_version.clone();
        self.persist_registry()?;

        self.append_extension_log(&ExtensionLogEntry {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            old_version: self.registry.protocol_version.clone(),
            new_version: new_version.clone(),
            breaking: is_breaking,
            added_types,
            modified_types,
            description: description.to_string(),
        })?;

        tracing::info!(
            new_version = %new_version,
            breaking = is_breaking,
            "Protocol extension accepted"
        );

        Ok(new_version)
    }

    fn bump_version(&self, breaking: bool) -> String {
        let parts: Vec<&str> = self.registry.protocol_version.split('.').collect();
        match parts.as_slice() {
            [major, minor] => {
                let major_n: u32 = major.parse().unwrap_or(1);
                let minor_n: u32 = minor.parse().unwrap_or(0);
                if breaking {
                    format!("{}.0", major_n + 1)
                } else {
                    format!("{}.{}", major_n, minor_n + 1)
                }
            }
            _ => {
                if breaking {
                    "2.0".to_string()
                } else {
                    "1.1".to_string()
                }
            }
        }
    }

    fn persist_registry(&self) -> Result<(), String> {
        let path = self.protocol_dir.join(MESSAGES_FILE);
        let content = serde_json::to_string_pretty(&self.registry)
            .map_err(|e| format!("Failed to serialize registry: {}", e))?;
        fs::write(&path, content)
            .map_err(|e| format!("Failed to write registry: {}", e))?;
        Ok(())
    }

    fn append_extension_log(&self, entry: &ExtensionLogEntry) -> Result<(), String> {
        let log_path = self.protocol_dir.join(EXTENSION_LOG);
        let line = serde_json::to_string(entry)
            .map_err(|e| format!("Failed to serialize extension log: {}", e))?;

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("Failed to open extensions.log: {}", e))?;

        writeln!(file, "{}", line)
            .map_err(|e| format!("Failed to write extensions.log: {}", e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().to_path_buf();
        (dir, base_dir)
    }

    #[test]
    fn creates_default_registry() {
        let (_dir, base_dir) = setup_test_dir();
        let mgr = ProtocolManager::new(&base_dir).unwrap();
        assert_eq!(mgr.current_version(), "1.0");
        assert!(mgr.is_known_message("Hello"));
        assert!(mgr.is_known_message("ProtocolExtensionProposal"));
        assert!(!mgr.is_known_message("NonexistentMessage"));
    }

    #[test]
    fn add_non_breaking_extension() {
        let (_dir, base_dir) = setup_test_dir();
        let mut mgr = ProtocolManager::new(&base_dir).unwrap();

        let new_msgs = serde_json::json!({
            "CustomPing": {
                "category": "custom",
                "direction": "peer->boot",
                "schema": {}
            }
        });

        let result = mgr.propose_extension(&new_msgs, false, "Add ping", None, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "1.1");
        assert!(mgr.is_known_message("CustomPing"));
    }

    #[test]
    fn reject_breaking_without_signature() {
        let (_dir, base_dir) = setup_test_dir();
        let mut mgr = ProtocolManager::new(&base_dir).unwrap();

        let new_msgs = serde_json::json!({
            "Hello": {
                "category": "handshake",
                "direction": "peer->boot",
                "schema": { "new_field": "string" }
            }
        });

        let result = mgr.propose_extension(&new_msgs, true, "Modify Hello", None, None);
        assert!(result.is_err());
    }

    #[test]
    fn accept_breaking_with_valid_signature() {
        let (_dir, base_dir) = setup_test_dir();
        let mut mgr = ProtocolManager::new(&base_dir).unwrap();

        let new_msgs = serde_json::json!({
            "Hello": {
                "category": "handshake",
                "direction": "peer->boot",
                "schema": { "new_field": "string" }
            }
        });

        let always_valid = |_data: &str, _sig: &str| true;

        let result = mgr.propose_extension(
            &new_msgs,
            true,
            "Modify Hello",
            Some(&always_valid),
            Some("valid_sig"),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "2.0");
    }
}
