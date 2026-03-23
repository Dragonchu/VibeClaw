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

    // Acquire an exclusive lock so start.sh (and other tooling) can reliably
    // detect whether Boot is still alive.  The lock is automatically released
    // when the process exits — even on panic or SIGKILL.
    let lock_path = config.base_dir.join("boot.lock");
    std::fs::create_dir_all(&config.base_dir).ok();
    let lock_file = match std::fs::File::create(&lock_path) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to create lock file {}: {}", lock_path.display(), e);
            std::process::exit(1);
        }
    };
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        tracing::error!(
            "Another reloopy-boot instance is already running (cannot acquire {})",
            lock_path.display()
        );
        std::process::exit(1);
    }
    // Write our PID so stop_existing() can force-kill if graceful shutdown fails.
    use std::io::Write;
    let _ = writeln!(&lock_file, "{}", std::process::id());
    // Keep lock_file alive for the entire process lifetime.

    if let Err(e) = microkernel::RuntimeSupervisor::run(config).await {
        tracing::error!("Boot microkernel fatal error: {}", e);
        std::process::exit(1);
    }

    drop(lock_file);
}
