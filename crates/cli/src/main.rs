//! Reloopy CLI — unified entry point for the Reloopy system.
//!
//! Subcommands:
//!   start   Launch the Reloopy system (boot + all managed services)
//!   stop    Gracefully shut down a running system

mod stop;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "reloopy",
    about = "Reloopy — self-evolving AI agent system",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the Reloopy system (foreground, Ctrl-C to stop)
    Start,
    /// Gracefully shut down a running Reloopy system
    Stop {
        #[arg(long, default_value = "CLI shutdown")]
        reason: String,
        #[arg(long, default_value = "~/.reloopy/reloopy.sock")]
        socket: String,
    },
}

/// Resolve a sibling binary by name, checking the directory of the current
/// executable first, then falling back to PATH lookup.
fn resolve_sibling(name: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(canonical) = exe.canonicalize() {
            if let Some(dir) = canonical.parent() {
                let sibling = dir.join(name);
                if sibling.exists() {
                    return sibling;
                }
            }
        }
    }
    PathBuf::from(name)
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Start => cmd_start(),
        Command::Stop { reason, socket } => cmd_stop(&reason, &socket),
    }
}

fn cmd_start() -> ExitCode {
    let boot = resolve_sibling("reloopy-boot");

    // exec replaces the current process — no return on success
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&boot).exec();

    eprintln!(
        "Failed to exec {}: {}",
        boot.display(),
        err
    );
    ExitCode::FAILURE
}

fn cmd_stop(reason: &str, socket: &str) -> ExitCode {
    let socket_path = expand_tilde(socket);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to create tokio runtime: {}", e);
            return ExitCode::FAILURE;
        }
    };

    match rt.block_on(stop::send_shutdown(&socket_path, reason)) {
        Ok(()) => {
            println!("Shutdown initiated. Boot is terminating all services.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Failed to stop reloopy: {}", e);
            ExitCode::FAILURE
        }
    }
}
