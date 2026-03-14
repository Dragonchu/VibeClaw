use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStep {
    pub key: String,
    pub from_version: u64,
    pub to_version: u64,
    pub transform: MigrationTransform,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MigrationTransform {
    Identity,
    Custom(String),
}

pub fn migrate(
    data: &serde_json::Value,
    from_version: u64,
    to_version: u64,
) -> Result<serde_json::Value, String> {
    if from_version == to_version {
        return Ok(data.clone());
    }

    let mut current = data.clone();
    let mut version = from_version;

    while version < to_version {
        current = migrate_one_step(&current, version, version + 1)?;
        version += 1;
    }

    Ok(current)
}

pub fn rollback_migration(
    data: &serde_json::Value,
    from_version: u64,
    to_version: u64,
) -> Result<serde_json::Value, String> {
    if from_version == to_version {
        return Ok(data.clone());
    }

    if from_version < to_version {
        return Err(format!(
            "Rollback requires from_version ({}) > to_version ({})",
            from_version, to_version
        ));
    }

    let mut current = data.clone();
    let mut version = from_version;

    while version > to_version {
        current = rollback_one_step(&current, version, version - 1)?;
        version -= 1;
    }

    Ok(current)
}

fn migrate_one_step(
    data: &serde_json::Value,
    _from: u64,
    _to: u64,
) -> Result<serde_json::Value, String> {
    Ok(data.clone())
}

fn rollback_one_step(
    data: &serde_json::Value,
    _from: u64,
    _to: u64,
) -> Result<serde_json::Value, String> {
    Ok(data.clone())
}
