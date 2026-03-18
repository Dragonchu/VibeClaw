mod client;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use reloopy_ipc::messages::{self, msg_types};

use client::AdminClient;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser)]
#[command(name = "reloopy-admin", about = "Reloopy system administration tool")]
struct Cli {
    #[arg(long, default_value = "~/.reloopy/reloopy.sock")]
    socket: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Status,
    Versions,
    VersionDetail {
        version: String,
    },
    Rollback {
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "Admin-initiated rollback")]
        reason: String,
    },
    Cleanup {
        #[arg(long, default_value_t = 5)]
        keep: usize,
    },
    Runlevel {
        #[command(subcommand)]
        action: Option<RunlevelAction>,
    },
    Peers,
    Audit {
        #[arg(long)]
        event: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Unlock,
    Shutdown {
        #[arg(long, default_value = "Admin-initiated shutdown")]
        reason: String,
    },
}

#[derive(Subcommand)]
enum RunlevelAction {
    Set {
        level: u8,
        #[arg(long, default_value = "Admin request")]
        reason: String,
    },
}

fn expand_socket_path(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();
    let socket_path = expand_socket_path(&cli.socket);

    let mut client = match AdminClient::connect(&socket_path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to boot at {}: {}", socket_path.display(), e);
            std::process::exit(1);
        }
    };

    let result = match cli.command {
        Command::Status => cmd_status(&mut client).await,
        Command::Versions => cmd_versions(&mut client).await,
        Command::VersionDetail { version } => cmd_version_detail(&mut client, &version).await,
        Command::Rollback { to, reason } => cmd_rollback(&mut client, to, &reason).await,
        Command::Cleanup { keep } => cmd_cleanup(&mut client, keep).await,
        Command::Runlevel { action } => cmd_runlevel(&mut client, action).await,
        Command::Peers => cmd_peers(&mut client).await,
        Command::Audit { event, limit } => cmd_audit(&mut client, event, limit).await,
        Command::Unlock => cmd_unlock(&mut client).await,
        Command::Shutdown { reason } => cmd_shutdown(&mut client, &reason).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn cmd_status(client: &mut AdminClient) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_STATUS_REQUEST,
            serde_json::to_value(&messages::AdminStatusRequest {})?,
        )
        .await?;

    let status: messages::AdminStatusResponse = serde_json::from_value(resp.payload)?;

    let runlevel_name = match status.runlevel {
        0 => "Halt",
        1 => "Safe",
        2 => "Normal",
        3 => "Evolve",
        _ => "Unknown",
    };

    println!("=== Reloopy System Status ===");
    println!("Runlevel:         {} ({})", status.runlevel, runlevel_name);
    println!(
        "Current version:  {}",
        status.current_version.as_deref().unwrap_or("(none)")
    );
    println!(
        "Rollback version: {}",
        status.rollback_version.as_deref().unwrap_or("(none)")
    );
    println!("Version locked:   {}", status.version_locked);
    println!("Probation active: {}", status.probation_active);
    println!(
        "Connected peers:  [{}]",
        status.connected_peers.join(", ")
    );
    Ok(())
}

async fn cmd_versions(client: &mut AdminClient) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_LIST_VERSIONS_REQUEST,
            serde_json::to_value(&messages::AdminListVersionsRequest {})?,
        )
        .await?;

    let data: messages::AdminListVersionsResponse = serde_json::from_value(resp.payload)?;

    if data.versions.is_empty() {
        println!("No versions found.");
        return Ok(());
    }

    println!("{:<12} {:<10} {:<10}", "VERSION", "CURRENT", "ROLLBACK");
    println!("{}", "-".repeat(32));
    for v in &data.versions {
        println!(
            "{:<12} {:<10} {:<10}",
            v.version,
            if v.is_current { "*" } else { "" },
            if v.is_rollback { "*" } else { "" },
        );
    }
    Ok(())
}

async fn cmd_version_detail(
    client: &mut AdminClient,
    version: &str,
) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_VERSION_DETAIL_REQUEST,
            serde_json::to_value(&messages::AdminVersionDetailRequest {
                version: version.to_string(),
            })?,
        )
        .await?;

    let data: messages::AdminVersionDetailResponse = serde_json::from_value(resp.payload)?;

    println!("=== Version: {} ===", data.version);
    println!("Current:    {}", data.is_current);
    println!("Rollback:   {}", data.is_rollback);
    println!("Has binary: {}", data.has_binary);
    println!("Has source: {}", data.has_source);
    if let Some(manifest) = &data.manifest {
        println!("Manifest:\n{}", serde_json::to_string_pretty(manifest)?);
    } else {
        println!("Manifest:   (none)");
    }
    Ok(())
}

async fn cmd_rollback(
    client: &mut AdminClient,
    to: Option<String>,
    reason: &str,
) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_FORCE_ROLLBACK_REQUEST,
            serde_json::to_value(&messages::AdminForceRollbackRequest {
                reason: reason.to_string(),
                to_version: to,
            })?,
        )
        .await?;

    let data: messages::AdminForceRollbackResponse = serde_json::from_value(resp.payload)?;

    if data.success {
        println!(
            "Rollback successful. Now at: {}",
            data.rolled_back_to.as_deref().unwrap_or("unknown")
        );
    } else {
        eprintln!(
            "Rollback failed: {}",
            data.error.as_deref().unwrap_or("unknown error")
        );
        std::process::exit(1);
    }
    Ok(())
}

async fn cmd_cleanup(
    client: &mut AdminClient,
    keep: usize,
) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_CLEANUP_VERSIONS_REQUEST,
            serde_json::to_value(&messages::AdminCleanupVersionsRequest { keep })?,
        )
        .await?;

    let data: messages::AdminCleanupVersionsResponse = serde_json::from_value(resp.payload)?;

    if let Some(err) = &data.error {
        eprintln!("Cleanup error: {}", err);
        std::process::exit(1);
    }

    if data.removed.is_empty() {
        println!("Nothing to clean up.");
    } else {
        println!("Removed {} version(s):", data.removed.len());
        for v in &data.removed {
            println!("  - {}", v);
        }
    }
    Ok(())
}

async fn cmd_runlevel(
    client: &mut AdminClient,
    action: Option<RunlevelAction>,
) -> Result<(), BoxError> {
    match action {
        None => {
            let resp = client
                .request(
                    msg_types::ADMIN_STATUS_REQUEST,
                    serde_json::to_value(&messages::AdminStatusRequest {})?,
                )
                .await?;
            let status: messages::AdminStatusResponse = serde_json::from_value(resp.payload)?;
            let name = match status.runlevel {
                0 => "Halt",
                1 => "Safe",
                2 => "Normal",
                3 => "Evolve",
                _ => "Unknown",
            };
            println!("Current runlevel: {} ({})", status.runlevel, name);
        }
        Some(RunlevelAction::Set { level, reason }) => {
            let resp = client
                .request(
                    msg_types::RUNLEVEL_REQUEST,
                    serde_json::to_value(&messages::RunlevelRequest {
                        to: level,
                        reason: reason.clone(),
                    })?,
                )
                .await?;
            let result: messages::RunlevelRequestResult = serde_json::from_value(resp.payload)?;
            if result.accepted {
                println!(
                    "Runlevel changed: {} -> {}",
                    result.from, result.to
                );
            } else {
                eprintln!("Runlevel change rejected: {}", result.reason);
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

async fn cmd_peers(client: &mut AdminClient) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_LEASE_STATUS_REQUEST,
            serde_json::to_value(&messages::AdminLeaseStatusRequest {})?,
        )
        .await?;

    let data: messages::AdminLeaseStatusResponse = serde_json::from_value(resp.payload)?;

    if data.leases.is_empty() {
        println!("No peers registered.");
        return Ok(());
    }

    println!(
        "{:<15} {:<12} {:<10} {:<10} {:<10}",
        "PEER", "STATUS", "PROBATION", "CPU%", "MEM(MB)"
    );
    println!("{}", "-".repeat(57));
    for p in &data.leases {
        let (cpu, mem) = match &p.last_health {
            Some(h) => (
                format!("{:.1}", h.cpu_percent),
                format!("{:.1}", h.memory_bytes as f64 / 1_048_576.0),
            ),
            None => ("-".to_string(), "-".to_string()),
        };
        println!(
            "{:<15} {:<12} {:<10} {:<10} {:<10}",
            p.identity, p.status, p.probation, cpu, mem
        );
    }
    Ok(())
}

async fn cmd_audit(
    client: &mut AdminClient,
    event: Option<String>,
    limit: usize,
) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_AUDIT_QUERY_REQUEST,
            serde_json::to_value(&messages::AdminAuditQueryRequest {
                event_filter: event,
                limit: Some(limit),
            })?,
        )
        .await?;

    let data: messages::AdminAuditQueryResponse = serde_json::from_value(resp.payload)?;

    if let Some(err) = &data.error {
        eprintln!("Audit query: {}", err);
        return Ok(());
    }

    if data.entries.is_empty() {
        println!("No audit entries found.");
        return Ok(());
    }

    for entry in &data.entries {
        println!(
            "[{}] {} version={} {}",
            entry.timestamp,
            entry.event,
            entry.version.as_deref().unwrap_or("-"),
            entry.details
        );
    }
    Ok(())
}

async fn cmd_unlock(client: &mut AdminClient) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_UNLOCK_VERSION_REQUEST,
            serde_json::to_value(&messages::AdminUnlockVersionRequest {})?,
        )
        .await?;

    let data: messages::AdminUnlockVersionResponse = serde_json::from_value(resp.payload)?;

    if data.was_locked {
        println!("Version manager unlocked successfully.");
    } else {
        println!("Version manager was not locked.");
    }
    Ok(())
}

async fn cmd_shutdown(client: &mut AdminClient, reason: &str) -> Result<(), BoxError> {
    let resp = client
        .request(
            msg_types::ADMIN_SHUTDOWN_REQUEST,
            serde_json::to_value(&messages::AdminShutdownRequest {
                reason: reason.to_string(),
            })?,
        )
        .await?;

    let data: messages::AdminShutdownResponse = serde_json::from_value(resp.payload)?;

    if data.success {
        println!("Shutdown initiated. Boot is terminating all services.");
    } else {
        eprintln!(
            "Shutdown failed: {}",
            data.error.as_deref().unwrap_or("unknown error")
        );
        std::process::exit(1);
    }
    Ok(())
}
