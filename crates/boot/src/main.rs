mod capability;
mod constitution;
mod ipc;
mod lease;
mod microkernel;
mod protocol;
mod resource;
mod runlevel;
mod state;
mod version;

use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("reloopy-boot microkernel starting");

    let config = microkernel::BootConfig::default();

    if let Err(e) = microkernel::RuntimeSupervisor::run(config).await {
        tracing::error!("Boot microkernel fatal error: {}", e);
        std::process::exit(1);
    }
}
