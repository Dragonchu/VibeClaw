//! Capability-Based Security for Peripheral versions.
//!
//! Each Peripheral version declares its required permissions in `capabilities.json`.
//! Boot enforces:
//! - Default deny: undeclared permissions are forbidden.
//! - Shrink-only: new versions cannot request more permissions than the previous version
//!   (escalation requires human approval).
//! See plan §3.3.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub filesystem: FilesystemCaps,
    pub network: NetworkCaps,
    pub exec: ExecCaps,
    #[serde(default)]
    pub ipc: Vec<String>,
    #[serde(default)]
    pub max_child_processes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FilesystemCaps {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkCaps {
    #[serde(default)]
    pub allowed: bool,
    #[serde(default)]
    pub whitelist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecCaps {
    #[serde(default)]
    pub allowed: bool,
    #[serde(default)]
    pub whitelist: Vec<String>,
}

#[derive(Debug)]
pub enum EscalationViolation {
    NewFilesystemRead(Vec<String>),
    NewFilesystemWrite(Vec<String>),
    NetworkEscalation,
    NewNetworkWhitelist(Vec<String>),
    ExecEscalation,
    NewExecWhitelist(Vec<String>),
    NewIpcEndpoints(Vec<String>),
    ChildProcessEscalation { old: u32, new: u32 },
}

impl std::fmt::Display for EscalationViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NewFilesystemRead(paths) => {
                write!(f, "New filesystem read paths: {:?}", paths)
            }
            Self::NewFilesystemWrite(paths) => {
                write!(f, "New filesystem write paths: {:?}", paths)
            }
            Self::NetworkEscalation => write!(f, "Network access escalation (was denied)"),
            Self::NewNetworkWhitelist(hosts) => {
                write!(f, "New network whitelist entries: {:?}", hosts)
            }
            Self::ExecEscalation => write!(f, "Exec access escalation (was denied)"),
            Self::NewExecWhitelist(cmds) => {
                write!(f, "New exec whitelist entries: {:?}", cmds)
            }
            Self::NewIpcEndpoints(eps) => {
                write!(f, "New IPC endpoints: {:?}", eps)
            }
            Self::ChildProcessEscalation { old, new } => {
                write!(
                    f,
                    "Child process limit escalation: {} -> {}",
                    old, new
                )
            }
        }
    }
}

pub struct CapabilityManager {
    base_dir: PathBuf,
}

impl CapabilityManager {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.join("peripheral"),
        }
    }

    pub fn load_capabilities(&self, version: &str) -> Result<Capabilities, String> {
        let caps_path = self.base_dir.join(version).join("capabilities.json");
        if !caps_path.exists() {
            return Ok(Capabilities::default());
        }

        let content = std::fs::read_to_string(&caps_path)
            .map_err(|e| format!("Failed to read capabilities.json for {}: {}", version, e))?;

        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse capabilities.json for {}: {}", version, e))
    }

    pub fn write_default_capabilities(&self, version: &str, base_dir: &Path) -> Result<(), String> {
        let caps = default_peripheral_capabilities(base_dir);
        let caps_path = self.base_dir.join(version).join("capabilities.json");

        let content = serde_json::to_string_pretty(&caps)
            .map_err(|e| format!("Failed to serialize capabilities: {}", e))?;

        std::fs::write(&caps_path, content)
            .map_err(|e| format!("Failed to write capabilities.json: {}", e))?;

        Ok(())
    }

    pub fn check_escalation(
        &self,
        old_version: &str,
        new_version: &str,
    ) -> Result<Vec<EscalationViolation>, String> {
        let old_caps = self.load_capabilities(old_version)?;
        let new_caps = self.load_capabilities(new_version)?;

        Ok(detect_escalations(&old_caps, &new_caps))
    }
}

fn default_peripheral_capabilities(base_dir: &Path) -> Capabilities {
    let state_dir = base_dir.join("state").to_string_lossy().to_string();
    let constitution_dir = base_dir.join("constitution").to_string_lossy().to_string();
    let memory_file = base_dir
        .join("state")
        .join("memory.json")
        .to_string_lossy()
        .to_string();
    let context_file = base_dir
        .join("state")
        .join("context.json")
        .to_string_lossy()
        .to_string();

    Capabilities {
        filesystem: FilesystemCaps {
            read: vec![state_dir, constitution_dir],
            write: vec![memory_file, context_file],
        },
        network: NetworkCaps {
            allowed: false,
            whitelist: vec![],
        },
        exec: ExecCaps {
            allowed: false,
            whitelist: vec![],
        },
        ipc: vec!["reloopy.sock".to_string()],
        max_child_processes: 0,
    }
}

fn detect_escalations(old: &Capabilities, new: &Capabilities) -> Vec<EscalationViolation> {
    let mut violations = Vec::new();

    let old_reads: HashSet<&str> = old.filesystem.read.iter().map(|s| s.as_str()).collect();
    let new_reads: Vec<String> = new
        .filesystem
        .read
        .iter()
        .filter(|p| !old_reads.contains(p.as_str()))
        .cloned()
        .collect();
    if !new_reads.is_empty() {
        violations.push(EscalationViolation::NewFilesystemRead(new_reads));
    }

    let old_writes: HashSet<&str> = old.filesystem.write.iter().map(|s| s.as_str()).collect();
    let new_writes: Vec<String> = new
        .filesystem
        .write
        .iter()
        .filter(|p| !old_writes.contains(p.as_str()))
        .cloned()
        .collect();
    if !new_writes.is_empty() {
        violations.push(EscalationViolation::NewFilesystemWrite(new_writes));
    }

    if new.network.allowed && !old.network.allowed {
        violations.push(EscalationViolation::NetworkEscalation);
    }

    if new.network.allowed && old.network.allowed {
        let old_hosts: HashSet<&str> =
            old.network.whitelist.iter().map(|s| s.as_str()).collect();
        let new_hosts: Vec<String> = new
            .network
            .whitelist
            .iter()
            .filter(|h| !old_hosts.contains(h.as_str()))
            .cloned()
            .collect();
        if !new_hosts.is_empty() {
            violations.push(EscalationViolation::NewNetworkWhitelist(new_hosts));
        }
    }

    if new.exec.allowed && !old.exec.allowed {
        violations.push(EscalationViolation::ExecEscalation);
    }

    if new.exec.allowed && old.exec.allowed {
        let old_cmds: HashSet<&str> = old.exec.whitelist.iter().map(|s| s.as_str()).collect();
        let new_cmds: Vec<String> = new
            .exec
            .whitelist
            .iter()
            .filter(|c| !old_cmds.contains(c.as_str()))
            .cloned()
            .collect();
        if !new_cmds.is_empty() {
            violations.push(EscalationViolation::NewExecWhitelist(new_cmds));
        }
    }

    let old_ipc: HashSet<&str> = old.ipc.iter().map(|s| s.as_str()).collect();
    let new_ipc: Vec<String> = new
        .ipc
        .iter()
        .filter(|e| !old_ipc.contains(e.as_str()))
        .cloned()
        .collect();
    if !new_ipc.is_empty() {
        violations.push(EscalationViolation::NewIpcEndpoints(new_ipc));
    }

    if new.max_child_processes > old.max_child_processes {
        violations.push(EscalationViolation::ChildProcessEscalation {
            old: old.max_child_processes,
            new: new.max_child_processes,
        });
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_escalation_on_identical_caps() {
        let caps = Capabilities {
            filesystem: FilesystemCaps {
                read: vec!["/a".to_string()],
                write: vec!["/b".to_string()],
            },
            network: NetworkCaps {
                allowed: false,
                whitelist: vec![],
            },
            exec: ExecCaps {
                allowed: false,
                whitelist: vec![],
            },
            ipc: vec!["reloopy.sock".to_string()],
            max_child_processes: 0,
        };
        let violations = detect_escalations(&caps, &caps);
        assert!(violations.is_empty());
    }

    #[test]
    fn detects_filesystem_read_escalation() {
        let old = Capabilities {
            filesystem: FilesystemCaps {
                read: vec!["/a".to_string()],
                write: vec![],
            },
            ..Default::default()
        };
        let new = Capabilities {
            filesystem: FilesystemCaps {
                read: vec!["/a".to_string(), "/secret".to_string()],
                write: vec![],
            },
            ..Default::default()
        };
        let violations = detect_escalations(&old, &new);
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            EscalationViolation::NewFilesystemRead(paths) if paths == &vec!["/secret".to_string()]
        ));
    }

    #[test]
    fn detects_network_escalation() {
        let old = Capabilities::default();
        let mut new = Capabilities::default();
        new.network.allowed = true;
        let violations = detect_escalations(&old, &new);
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            EscalationViolation::NetworkEscalation
        ));
    }

    #[test]
    fn shrink_is_allowed() {
        let old = Capabilities {
            filesystem: FilesystemCaps {
                read: vec!["/a".to_string(), "/b".to_string()],
                write: vec!["/c".to_string()],
            },
            network: NetworkCaps {
                allowed: true,
                whitelist: vec!["host1".to_string(), "host2".to_string()],
            },
            exec: ExecCaps {
                allowed: true,
                whitelist: vec!["cmd1".to_string()],
            },
            ipc: vec!["reloopy.sock".to_string()],
            max_child_processes: 4,
        };
        let new = Capabilities {
            filesystem: FilesystemCaps {
                read: vec!["/a".to_string()],
                write: vec![],
            },
            network: NetworkCaps {
                allowed: true,
                whitelist: vec!["host1".to_string()],
            },
            exec: ExecCaps {
                allowed: false,
                whitelist: vec![],
            },
            ipc: vec!["reloopy.sock".to_string()],
            max_child_processes: 2,
        };
        let violations = detect_escalations(&old, &new);
        assert!(violations.is_empty());
    }

    #[test]
    fn detects_child_process_escalation() {
        let old = Capabilities {
            max_child_processes: 2,
            ..Default::default()
        };
        let new = Capabilities {
            max_child_processes: 5,
            ..Default::default()
        };
        let violations = detect_escalations(&old, &new);
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            EscalationViolation::ChildProcessEscalation { old: 2, new: 5 }
        ));
    }
}
