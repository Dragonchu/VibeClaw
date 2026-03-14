//! Constitution amendment management.
//!
//! Handles proposals to modify `constitution/invariants.json` and
//! `constitution/benchmarks.json`. Every amendment requires a valid
//! HMAC-SHA256 signature produced with the shared secret stored in
//! `~/.loopy/secret.key`. Approved amendments are appended to
//! `constitution/amendments.log`.
//! See plan §2.5-A.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const SECRET_KEY_FILE: &str = "secret.key";
const AMENDMENTS_LOG: &str = "amendments.log";
const SECRET_KEY_LEN: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmendmentRecord {
    pub id: String,
    pub timestamp: u64,
    pub amendment_type: String,
    pub target_file: String,
    pub description: String,
    pub changes: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AmendmentLogEntry {
    pub record: AmendmentRecord,
    pub signature_valid: bool,
}

pub struct ConstitutionManager {
    base_dir: PathBuf,
    constitution_dir: PathBuf,
    secret_key: Vec<u8>,
    amendment_counter: u64,
}

impl ConstitutionManager {
    pub fn new(base_dir: &Path) -> Result<Self, String> {
        let constitution_dir = base_dir.join("constitution");
        fs::create_dir_all(&constitution_dir)
            .map_err(|e| format!("Failed to create constitution dir: {}", e))?;

        let secret_key = Self::load_or_generate_secret(base_dir)?;
        let amendment_counter = Self::count_existing_amendments(&constitution_dir);

        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            constitution_dir,
            secret_key,
            amendment_counter,
        })
    }

    fn load_or_generate_secret(base_dir: &Path) -> Result<Vec<u8>, String> {
        let key_path = base_dir.join(SECRET_KEY_FILE);
        if key_path.exists() {
            let hex_content = fs::read_to_string(&key_path)
                .map_err(|e| format!("Failed to read secret key: {}", e))?;
            hex::decode(hex_content.trim()).map_err(|e| format!("Invalid secret key hex: {}", e))
        } else {
            let key: Vec<u8> = (0..SECRET_KEY_LEN)
                .map(|i| {
                    let t = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    ((t.wrapping_mul(6364136223846793005).wrapping_add(i as u128)) & 0xFF) as u8
                })
                .collect();
            let hex_str = hex::encode(&key);
            fs::write(&key_path, &hex_str)
                .map_err(|e| format!("Failed to write secret key: {}", e))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600));
            }

            tracing::info!(path = %key_path.display(), "Generated new HMAC secret key");
            Ok(key)
        }
    }

    fn count_existing_amendments(constitution_dir: &Path) -> u64 {
        let log_path = constitution_dir.join(AMENDMENTS_LOG);
        if !log_path.exists() {
            return 0;
        }
        fs::read_to_string(&log_path)
            .map(|content| content.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0)
    }

    pub fn verify_signature(&self, data: &str, signature: &str) -> bool {
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.secret_key) else {
            return false;
        };
        mac.update(data.as_bytes());
        let Ok(sig_bytes) = hex::decode(signature) else {
            return false;
        };
        mac.verify_slice(&sig_bytes).is_ok()
    }

    pub fn compute_signature(&self, data: &str) -> Result<String, String> {
        let mut mac = HmacSha256::new_from_slice(&self.secret_key)
            .map_err(|e| format!("HMAC init failed: {}", e))?;
        mac.update(data.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    pub fn propose_amendment(
        &mut self,
        amendment_type: &str,
        target_file: &str,
        description: &str,
        changes: &serde_json::Value,
        signature: &str,
    ) -> Result<AmendmentRecord, String> {
        let valid_targets = ["invariants.json", "benchmarks.json"];
        if !valid_targets.contains(&target_file) {
            return Err(format!(
                "Invalid target file '{}'. Must be one of: {:?}",
                target_file, valid_targets
            ));
        }

        let valid_types = [
            "add_test",
            "remove_test",
            "modify_test",
            "add_dimension",
            "remove_dimension",
            "modify_dimension",
            "update_thresholds",
        ];
        if !valid_types.contains(&amendment_type) {
            return Err(format!(
                "Invalid amendment type '{}'. Must be one of: {:?}",
                amendment_type, valid_types
            ));
        }

        let sig_payload =
            self.build_signature_payload(amendment_type, target_file, description, changes);
        if !self.verify_signature(&sig_payload, signature) {
            return Err("Invalid HMAC signature — human authorization required".to_string());
        }

        self.apply_amendment(target_file, amendment_type, changes)?;

        self.amendment_counter += 1;
        let record = AmendmentRecord {
            id: format!("amendment-{:04}", self.amendment_counter),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            amendment_type: amendment_type.to_string(),
            target_file: target_file.to_string(),
            description: description.to_string(),
            changes: changes.clone(),
        };

        self.append_amendment_log(&record)?;

        tracing::info!(
            id = %record.id,
            target = %target_file,
            amendment_type = %amendment_type,
            "Constitution amendment approved and applied"
        );

        Ok(record)
    }

    fn build_signature_payload(
        &self,
        amendment_type: &str,
        target_file: &str,
        description: &str,
        changes: &serde_json::Value,
    ) -> String {
        format!(
            "{}:{}:{}:{}",
            amendment_type,
            target_file,
            description,
            serde_json::to_string(changes).unwrap_or_default()
        )
    }

    fn apply_amendment(
        &self,
        target_file: &str,
        amendment_type: &str,
        changes: &serde_json::Value,
    ) -> Result<(), String> {
        let file_path = self.constitution_dir.join(target_file);
        let content = fs::read_to_string(&file_path)
            .map_err(|e| format!("Failed to read {}: {}", target_file, e))?;
        let mut doc: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {}", target_file, e))?;

        match target_file {
            "invariants.json" => {
                self.apply_invariant_amendment(&mut doc, amendment_type, changes)?
            }
            "benchmarks.json" => {
                self.apply_benchmark_amendment(&mut doc, amendment_type, changes)?
            }
            _ => return Err(format!("Unsupported target: {}", target_file)),
        }

        let updated = serde_json::to_string_pretty(&doc)
            .map_err(|e| format!("Failed to serialize updated {}: {}", target_file, e))?;
        fs::write(&file_path, updated)
            .map_err(|e| format!("Failed to write {}: {}", target_file, e))?;

        Ok(())
    }

    fn apply_invariant_amendment(
        &self,
        doc: &mut serde_json::Value,
        amendment_type: &str,
        changes: &serde_json::Value,
    ) -> Result<(), String> {
        let tests = doc
            .get_mut("tests")
            .and_then(|v| v.as_array_mut())
            .ok_or("invariants.json missing 'tests' array")?;

        match amendment_type {
            "add_test" => {
                let id = changes
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("add_test requires 'id' field")?;
                if tests
                    .iter()
                    .any(|t| t.get("id").and_then(|v| v.as_str()) == Some(id))
                {
                    return Err(format!("Test '{}' already exists", id));
                }
                tests.push(changes.clone());
            }
            "remove_test" => {
                let id = changes
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("remove_test requires 'id' field")?;
                let original_len = tests.len();
                tests.retain(|t| t.get("id").and_then(|v| v.as_str()) != Some(id));
                if tests.len() == original_len {
                    return Err(format!("Test '{}' not found", id));
                }
            }
            "modify_test" => {
                let id = changes
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("modify_test requires 'id' field")?;
                let test = tests
                    .iter_mut()
                    .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(id))
                    .ok_or(format!("Test '{}' not found", id))?;
                if let Some(obj) = changes.as_object() {
                    if let Some(target_obj) = test.as_object_mut() {
                        for (k, v) in obj {
                            target_obj.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            _ => {
                return Err(format!(
                    "Invalid amendment type for invariants: {}",
                    amendment_type
                ));
            }
        }

        Ok(())
    }

    fn apply_benchmark_amendment(
        &self,
        doc: &mut serde_json::Value,
        amendment_type: &str,
        changes: &serde_json::Value,
    ) -> Result<(), String> {
        match amendment_type {
            "add_dimension" => {
                let dimensions = doc
                    .get_mut("dimensions")
                    .and_then(|v| v.as_array_mut())
                    .ok_or("benchmarks.json missing 'dimensions' array")?;
                let name = changes
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("add_dimension requires 'name' field")?;
                if dimensions
                    .iter()
                    .any(|d| d.get("name").and_then(|v| v.as_str()) == Some(name))
                {
                    return Err(format!("Dimension '{}' already exists", name));
                }
                dimensions.push(changes.clone());
            }
            "remove_dimension" => {
                let dimensions = doc
                    .get_mut("dimensions")
                    .and_then(|v| v.as_array_mut())
                    .ok_or("benchmarks.json missing 'dimensions' array")?;
                let name = changes
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("remove_dimension requires 'name' field")?;
                let original_len = dimensions.len();
                dimensions.retain(|d| d.get("name").and_then(|v| v.as_str()) != Some(name));
                if dimensions.len() == original_len {
                    return Err(format!("Dimension '{}' not found", name));
                }
            }
            "modify_dimension" => {
                let dimensions = doc
                    .get_mut("dimensions")
                    .and_then(|v| v.as_array_mut())
                    .ok_or("benchmarks.json missing 'dimensions' array")?;
                let name = changes
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("modify_dimension requires 'name' field")?;
                let dim = dimensions
                    .iter_mut()
                    .find(|d| d.get("name").and_then(|v| v.as_str()) == Some(name))
                    .ok_or(format!("Dimension '{}' not found", name))?;
                if let Some(obj) = changes.as_object() {
                    if let Some(target_obj) = dim.as_object_mut() {
                        for (k, v) in obj {
                            target_obj.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            "update_thresholds" => {
                if let Some(overall_min) = changes.get("overall_min") {
                    doc["overall_min"] = overall_min.clone();
                }
                if let Some(regression_tolerance) = changes.get("regression_tolerance") {
                    doc["regression_tolerance"] = regression_tolerance.clone();
                }
            }
            _ => {
                return Err(format!(
                    "Invalid amendment type for benchmarks: {}",
                    amendment_type
                ));
            }
        }

        Ok(())
    }

    fn append_amendment_log(&self, record: &AmendmentRecord) -> Result<(), String> {
        let log_path = self.constitution_dir.join(AMENDMENTS_LOG);
        let entry = AmendmentLogEntry {
            record: record.clone(),
            signature_valid: true,
        };
        let line = serde_json::to_string(&entry)
            .map_err(|e| format!("Failed to serialize amendment log: {}", e))?;

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("Failed to open amendments.log: {}", e))?;

        writeln!(file, "{}", line).map_err(|e| format!("Failed to write amendments.log: {}", e))?;

        Ok(())
    }

    pub fn secret_key_path(&self) -> PathBuf {
        self.base_dir.join(SECRET_KEY_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().to_path_buf();

        let constitution_dir = base_dir.join("constitution");
        fs::create_dir_all(&constitution_dir).unwrap();

        let invariants = serde_json::json!({
            "version": "1.0",
            "tests": [
                {
                    "id": "test_1",
                    "name": "Test One",
                    "description": "A test",
                    "timeout_secs": 10,
                    "type": "handshake"
                }
            ]
        });
        fs::write(
            constitution_dir.join("invariants.json"),
            serde_json::to_string_pretty(&invariants).unwrap(),
        )
        .unwrap();

        let benchmarks = serde_json::json!({
            "version": "1.0",
            "dimensions": [
                {
                    "name": "protocol_compliance",
                    "weight": 0.3,
                    "min_threshold": 0.95
                }
            ],
            "overall_min": 0.8,
            "regression_tolerance": 0.05
        });
        fs::write(
            constitution_dir.join("benchmarks.json"),
            serde_json::to_string_pretty(&benchmarks).unwrap(),
        )
        .unwrap();

        (dir, base_dir)
    }

    #[test]
    fn sign_and_verify() {
        let (_dir, base_dir) = setup_test_dir();
        let mgr = ConstitutionManager::new(&base_dir).unwrap();

        let data = "test_data";
        let sig = mgr.compute_signature(data).unwrap();
        assert!(mgr.verify_signature(data, &sig));
        assert!(!mgr.verify_signature("wrong_data", &sig));
    }

    #[test]
    fn add_invariant_test() {
        let (_dir, base_dir) = setup_test_dir();
        let mut mgr = ConstitutionManager::new(&base_dir).unwrap();

        let changes = serde_json::json!({
            "id": "test_2",
            "name": "Test Two",
            "description": "Another test",
            "timeout_secs": 5,
            "type": "echo"
        });

        let sig_payload =
            mgr.build_signature_payload("add_test", "invariants.json", "Add test 2", &changes);
        let sig = mgr.compute_signature(&sig_payload).unwrap();

        let result =
            mgr.propose_amendment("add_test", "invariants.json", "Add test 2", &changes, &sig);
        assert!(result.is_ok());

        let content = fs::read_to_string(base_dir.join("constitution/invariants.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(doc["tests"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn reject_invalid_signature() {
        let (_dir, base_dir) = setup_test_dir();
        let mut mgr = ConstitutionManager::new(&base_dir).unwrap();

        let changes = serde_json::json!({
            "id": "test_2",
            "name": "Test Two",
            "description": "Another test",
            "timeout_secs": 5,
            "type": "echo"
        });

        let result = mgr.propose_amendment(
            "add_test",
            "invariants.json",
            "Add test 2",
            &changes,
            "bad_signature",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("HMAC signature"));
    }
}
