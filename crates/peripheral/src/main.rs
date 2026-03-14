mod agent;
mod deepseek;
mod ipc_client;
mod migration;
mod source;
mod tools;

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use loopy_ipc::messages::{msg_types, Envelope};

use crate::agent::{Agent, AgentOutput};
use crate::deepseek::DeepSeekClient;
use crate::ipc_client::IpcHandle;
use crate::source::SourceManager;

struct Config {
    sock_path: PathBuf,
    workspace_root: PathBuf,
    heartbeat_interval: Duration,
    api_key: String,
    api_base_url: Option<String>,
    model: Option<String>,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));

        let base_dir = home.join(".loopy");

        let sock_path = std::env::var("LOOPY_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base_dir.join("loopy.sock"));

        let workspace_root = resolve_workspace_root(&base_dir)?;

        let api_key = std::env::var("DEEPSEEK_API_KEY").or_else(|_| read_config_api_key(&base_dir))?;

        let api_base_url = std::env::var("DEEPSEEK_BASE_URL").ok();
        let model = std::env::var("DEEPSEEK_MODEL").ok();

        Ok(Self {
            sock_path,
            workspace_root,
            heartbeat_interval: Duration::from_secs(8),
            api_key,
            api_base_url,
            model,
        })
    }
}

fn resolve_workspace_root(base_dir: &PathBuf) -> Result<PathBuf, String> {
    if let Ok(ws) = std::env::var("LOOPY_WORKSPACE") {
        let path = PathBuf::from(ws);
        if path.join("crates").join("peripheral").exists() {
            return Ok(path);
        }
        return Err(format!(
            "LOOPY_WORKSPACE={} does not contain crates/peripheral/",
            path.display()
        ));
    }

    let evolved_source = base_dir
        .join("peripheral")
        .join("current")
        .join("source");
    if evolved_source.join("crates").join("peripheral").exists() {
        return Ok(evolved_source);
    }

    Err(
        "Cannot determine workspace root. Set LOOPY_WORKSPACE env var or ensure ~/.loopy/peripheral/current/source/ exists."
            .to_string(),
    )
}

fn read_config_api_key(base_dir: &PathBuf) -> Result<String, String> {
    let config_path = base_dir.join("config.json");
    if config_path.exists() {
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| format!("Read config: {}", e))?;
        let config: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Parse config: {}", e))?;
        if let Some(key) = config["deepseek_api_key"].as_str() {
            return Ok(key.to_string());
        }
    }
    Err("DEEPSEEK_API_KEY not set and not found in ~/.loopy/config.json".to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Configuration error: {}", e);
            std::process::exit(1);
        }
    };

    tracing::info!(
        workspace = %config.workspace_root.display(),
        sock = %config.sock_path.display(),
        "loopy-peripheral starting"
    );

    let ipc = match ipc_client::connect_and_handshake(&config.sock_path).await {
        Ok(handle) => handle,
        Err(e) => {
            eprintln!("Failed to connect to Boot: {}", e);
            eprintln!("Make sure loopy-boot is running.");
            std::process::exit(1);
        }
    };

    let deepseek = DeepSeekClient::new(config.api_key, config.api_base_url, config.model);
    let source = SourceManager::new(config.workspace_root);
    let agent = Agent::new(deepseek, source);

    run_main_loop(agent, ipc, config.heartbeat_interval).await;
}

async fn run_main_loop(mut agent: Agent, ipc: IpcHandle, heartbeat_interval: Duration) {
    let ipc_tx = ipc.tx;
    let ipc_rx = ipc.rx;
    let runlevel = ipc.runlevel;

    let heartbeat_tx: mpsc::Sender<Envelope> = ipc_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(heartbeat_interval);
        loop {
            interval.tick().await;
            let hb = ipc_client::make_heartbeat(runlevel);
            if heartbeat_tx.send(hb).await.is_err() {
                break;
            }
        }
    });

    let (update_result_tx, mut update_result_rx) = mpsc::channel::<Envelope>(4);

    let message_tx: mpsc::Sender<Envelope> = update_result_tx.clone();
    let mut ipc_rx = ipc_rx;
    tokio::spawn(async move {
        while let Some(envelope) = ipc_rx.recv().await {
            match envelope.msg_type.as_str() {
                msg_types::LEASE_ACK => {
                    tracing::trace!("LeaseAck received");
                }
                msg_types::SHUTDOWN => {
                    let reason = envelope.payload.get("reason")
                        .and_then(|v: &serde_json::Value| v.as_str())
                        .unwrap_or("unknown");
                    tracing::info!(
                        %reason,
                        "Shutdown received"
                    );
                    println!("\n[system] Shutdown received. Exiting...");
                    let _ = message_tx.send(envelope).await;
                    break;
                }
                msg_types::RUNLEVEL_CHANGE => {
                    tracing::info!("Runlevel change: {}", envelope.payload);
                }
                msg_types::UPDATE_ACCEPTED | msg_types::UPDATE_REJECTED => {
                    let _ = message_tx.send(envelope).await;
                }
                other => {
                    tracing::debug!(msg_type = %other, "Unhandled message");
                }
            }
        }
    });

    println!("Loopy Agent ready. Type your instructions or 'quit' to exit.");
    println!("---");

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    loop {
        eprint!("loopy> ");

        tokio::select! {
            biased;

            Some(result_msg) = update_result_rx.recv() => {
                if result_msg.msg_type == msg_types::SHUTDOWN {
                    break;
                }
                handle_update_result(&result_msg);
            }

            result = lines.next_line() => {
                match result {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if trimmed == "quit" || trimmed == "exit" {
                            println!("Goodbye.");
                            break;
                        }
                        if trimmed == "reset" {
                            agent.reset_conversation();
                            println!("[system] Conversation reset.");
                            continue;
                        }

                        match agent.handle_input(trimmed).await {
                            Ok(AgentOutput::Done) => {}
                            Ok(AgentOutput::SubmitUpdate(source_path)) => {
                                println!("[system] Submitting update...");
                                let submit = ipc_client::make_submit_update(&source_path);
                                if ipc_tx.send(submit).await.is_err() {
                                    println!("[error] Lost connection to Boot");
                                    break;
                                }

                                println!("[system] Waiting for build result...");
                                match tokio::time::timeout(
                                    Duration::from_secs(300),
                                    update_result_rx.recv(),
                                )
                                .await
                                {
                                    Ok(Some(msg)) => {
                                        handle_update_result(&msg);
                                        if msg.msg_type == msg_types::SHUTDOWN {
                                            println!("[system] Hot replacement in progress. Shutting down...");
                                            break;
                                        }
                                    }
                                    Ok(None) => {
                                        println!("[error] IPC channel closed");
                                        break;
                                    }
                                    Err(_) => {
                                        println!("[error] Timed out waiting for build result");
                                    }
                                }

                                agent.source_mut().reset_staging();
                            }
                            Err(e) => {
                                println!("[error] {}", e);
                            }
                        }
                    }
                    Ok(None) => {
                        println!("\n[system] EOF on stdin. Exiting...");
                        break;
                    }
                    Err(e) => {
                        println!("[error] stdin read error: {}", e);
                        break;
                    }
                }
            }
        }
    }
}

fn handle_update_result(envelope: &Envelope) {
    match envelope.msg_type.as_str() {
        msg_types::UPDATE_ACCEPTED => {
            let version = envelope
                .payload
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!("[system] Update ACCEPTED — version {} deployed", version);
        }
        msg_types::UPDATE_REJECTED => {
            let reason = envelope
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let errors = envelope
                .payload
                .get("errors")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            println!("[system] Update REJECTED — {}", reason);
            if !errors.is_empty() {
                println!("[errors] {}", errors);
            }
        }
        _ => {}
    }
}
