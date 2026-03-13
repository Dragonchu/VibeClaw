use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEntry {
    pub value: serde_json::Value,
    pub schema_version: u64,
}

pub struct StateStore {
    base_dir: PathBuf,
    cache: HashMap<String, StateEntry>,
}

impl StateStore {
    pub fn new(base_dir: &Path) -> Self {
        let state_dir = base_dir.join("state");
        Self {
            base_dir: state_dir,
            cache: HashMap::new(),
        }
    }

    pub fn init(&mut self) -> Result<(), String> {
        fs::create_dir_all(&self.base_dir)
            .map_err(|e| format!("Failed to create state dir: {}", e))?;
        fs::create_dir_all(self.base_dir.join("snapshots"))
            .map_err(|e| format!("Failed to create snapshots dir: {}", e))?;
        fs::create_dir_all(self.base_dir.join("wal"))
            .map_err(|e| format!("Failed to create wal dir: {}", e))?;

        self.load_all()?;
        self.recover_from_wal()?;

        Ok(())
    }

    fn state_file_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(format!("{}.json", key))
    }

    fn load_all(&mut self) -> Result<(), String> {
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                    if let Some(stem) = path.file_stem() {
                        let key = stem.to_string_lossy().to_string();
                        match fs::read_to_string(&path) {
                            Ok(content) => match serde_json::from_str::<StateEntry>(&content) {
                                Ok(entry) => {
                                    self.cache.insert(key, entry);
                                }
                                Err(e) => {
                                    tracing::warn!(file = %path.display(), "Invalid state file: {}", e);
                                }
                            },
                            Err(e) => {
                                tracing::warn!(file = %path.display(), "Failed to read state file: {}", e);
                            }
                        }
                    }
                }
            }
        }

        tracing::info!(keys = self.cache.len(), "State loaded");
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&StateEntry> {
        self.cache.get(key)
    }

    pub fn set(
        &mut self,
        key: &str,
        value: serde_json::Value,
        schema_version: u64,
    ) -> Result<(), String> {
        let entry = StateEntry {
            value,
            schema_version,
        };

        let path = self.state_file_path(key);
        let content = serde_json::to_string_pretty(&entry)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;
        fs::write(&path, &content)
            .map_err(|e| format!("Failed to write state file {}: {}", path.display(), e))?;

        self.cache.insert(key.to_string(), entry);
        tracing::debug!(key, schema_version, "State updated");

        Ok(())
    }

    pub fn snapshot(&self, from_version: &str, to_version: &str) -> Result<PathBuf, String> {
        let snapshot_name = format!("snap_{}_{}.json", from_version, to_version);
        let snapshot_path = self.base_dir.join("snapshots").join(&snapshot_name);

        let snapshot_data = serde_json::to_string_pretty(&self.cache)
            .map_err(|e| format!("Failed to serialize snapshot: {}", e))?;

        fs::write(&snapshot_path, &snapshot_data)
            .map_err(|e| format!("Failed to write snapshot: {}", e))?;

        tracing::info!(
            path = %snapshot_path.display(),
            from = %from_version,
            to = %to_version,
            "State snapshot created"
        );

        Ok(snapshot_path)
    }

    pub fn restore_from_snapshot(&mut self, snapshot_path: &Path) -> Result<(), String> {
        let content = fs::read_to_string(snapshot_path)
            .map_err(|e| format!("Failed to read snapshot {}: {}", snapshot_path.display(), e))?;

        let restored: HashMap<String, StateEntry> = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse snapshot: {}", e))?;

        for (key, entry) in &restored {
            let path = self.state_file_path(key);
            let content = serde_json::to_string_pretty(entry)
                .map_err(|e| format!("Failed to serialize restored state: {}", e))?;
            fs::write(&path, &content)
                .map_err(|e| format!("Failed to write restored state: {}", e))?;
        }

        self.cache = restored;

        tracing::info!(
            snapshot = %snapshot_path.display(),
            "State restored from snapshot"
        );

        Ok(())
    }

    fn wal_path(&self) -> PathBuf {
        self.base_dir.join("wal").join("migration.log")
    }

    fn recover_from_wal(&mut self) -> Result<(), String> {
        let wal_path = self.wal_path();
        if !wal_path.exists() {
            return Ok(());
        }

        let content =
            fs::read_to_string(&wal_path).map_err(|e| format!("Failed to read WAL: {}", e))?;

        let entries: Vec<WalEntry> = content
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        if entries.is_empty() {
            fs::remove_file(&wal_path).ok();
            return Ok(());
        }

        let last = entries.last().unwrap();
        match last.operation.as_str() {
            "COMMIT" => {
                tracing::info!("WAL recovery: migration was committed successfully");
                fs::remove_file(&wal_path).ok();
            }
            _ => {
                tracing::warn!("WAL recovery: incomplete migration detected, rolling back");
                if let Some(begin_entry) = entries.iter().find(|e| e.operation == "BEGIN_MIGRATION")
                {
                    if let Some(snapshot_path) = &begin_entry.snapshot_path {
                        let snap = PathBuf::from(snapshot_path);
                        if snap.exists() {
                            self.restore_from_snapshot(&snap)?;
                            tracing::info!("WAL recovery: rolled back to snapshot");
                        }
                    }
                }
                fs::remove_file(&wal_path).ok();
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WalEntry {
    operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_schema: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_schema: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_path: Option<String>,
}

pub struct MigrationTransaction<'a> {
    store: &'a mut StateStore,
    from_version: String,
    to_version: String,
    snapshot_path: PathBuf,
    committed: bool,
}

impl<'a> MigrationTransaction<'a> {
    pub fn begin(
        store: &'a mut StateStore,
        from_version: &str,
        to_version: &str,
    ) -> Result<Self, String> {
        let snapshot_path = store.snapshot(from_version, to_version)?;

        let wal_path = store.wal_path();
        let entry = WalEntry {
            operation: "BEGIN_MIGRATION".to_string(),
            from_version: Some(from_version.to_string()),
            to_version: Some(to_version.to_string()),
            key: None,
            from_schema: None,
            to_schema: None,
            result: None,
            snapshot_path: Some(snapshot_path.to_string_lossy().to_string()),
        };

        append_wal(&wal_path, &entry)?;

        Ok(Self {
            store,
            from_version: from_version.to_string(),
            to_version: to_version.to_string(),
            snapshot_path,
            committed: false,
        })
    }

    pub fn transform(
        &mut self,
        key: &str,
        value: serde_json::Value,
        new_schema_version: u64,
    ) -> Result<(), String> {
        let old_schema = self.store.get(key).map(|e| e.schema_version).unwrap_or(0);

        let wal_path = self.store.wal_path();
        let wal_entry = WalEntry {
            operation: "TRANSFORM".to_string(),
            from_version: None,
            to_version: None,
            key: Some(key.to_string()),
            from_schema: Some(old_schema),
            to_schema: Some(new_schema_version),
            result: None,
            snapshot_path: None,
        };
        append_wal(&wal_path, &wal_entry)?;

        self.store.set(key, value, new_schema_version)?;

        let validate_entry = WalEntry {
            operation: "VALIDATE".to_string(),
            from_version: None,
            to_version: None,
            key: Some(key.to_string()),
            from_schema: None,
            to_schema: None,
            result: Some("OK".to_string()),
            snapshot_path: None,
        };
        append_wal(&wal_path, &validate_entry)?;

        Ok(())
    }

    pub fn commit(mut self) -> Result<(), String> {
        let wal_path = self.store.wal_path();
        let entry = WalEntry {
            operation: "COMMIT".to_string(),
            from_version: Some(self.from_version.clone()),
            to_version: Some(self.to_version.clone()),
            key: None,
            from_schema: None,
            to_schema: None,
            result: None,
            snapshot_path: None,
        };
        append_wal(&wal_path, &entry)?;

        self.committed = true;

        tracing::info!(
            from = %self.from_version,
            to = %self.to_version,
            "Migration committed"
        );

        fs::remove_file(&wal_path).ok();

        Ok(())
    }
}

impl Drop for MigrationTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            tracing::warn!(
                from = %self.from_version,
                to = %self.to_version,
                "Migration transaction dropped without commit — rolling back"
            );
            if self.snapshot_path.exists() {
                if let Err(e) = self.store.restore_from_snapshot(&self.snapshot_path) {
                    tracing::error!("Failed to rollback from snapshot: {}", e);
                }
            }
            let wal_path = self.store.wal_path();
            fs::remove_file(&wal_path).ok();
        }
    }
}

fn append_wal(wal_path: &Path, entry: &WalEntry) -> Result<(), String> {
    use std::io::Write;

    let line = serde_json::to_string(entry)
        .map_err(|e| format!("Failed to serialize WAL entry: {}", e))?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(wal_path)
        .map_err(|e| format!("Failed to open WAL: {}", e))?;

    writeln!(file, "{}", line).map_err(|e| format!("Failed to write WAL entry: {}", e))?;

    file.flush()
        .map_err(|e| format!("Failed to flush WAL: {}", e))?;

    Ok(())
}
