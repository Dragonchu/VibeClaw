# Loopy — Project Guidelines

## Overview

Self-evolving AI agent system in Rust. A minimal, immutable Boot microkernel supervises a Peripheral agent process, enabling it to rewrite its own source code, have it compiled/tested/hot-swapped — under capability-based security, scoring, audit logging, and transactional state migration. Design doc: [plan.md](../plan.md) (Chinese).

## Architecture

```
loopy-boot (microkernel, UDS listener at ~/.loopy/loopy.sock)
  ├── loopy-compiler   (service)
  ├── loopy-judge      (service, skeleton)
  ├── loopy-audit      (service, skeleton)
  └── loopy-peripheral (agent, skeleton)
```

All processes communicate exclusively over Unix Domain Sockets via `loopy-ipc`. Boot routes messages by inspecting `Envelope.to`.

| Crate              | Path                        | Role                                                                                     |
| ------------------ | --------------------------- | ---------------------------------------------------------------------------------------- |
| `loopy-ipc`        | `crates/ipc/`               | Shared IPC library: `Envelope`, message types, wire format (4-byte BE length + JSON)     |
| `loopy-boot`       | `crates/boot/`              | Microkernel: IPC routing, lease management, version switching, state store, runlevel FSM |
| `loopy-compiler`   | `crates/services/compiler/` | Receives `CompileRequest`, runs `cargo build --release`, returns `CompileResult`         |
| `loopy-judge`      | `crates/services/judge/`    | Test runner + scoring (Phase 3, not yet implemented)                                     |
| `loopy-audit`      | `crates/services/audit/`    | Audit log writer/query (Phase 3, not yet implemented)                                    |
| `loopy-peripheral` | `crates/peripheral/`        | The self-evolving agent (DeepSeek LLM, REPL, tool-calling, hot replacement)              |

### Boot Subsystems (crates/boot/src/)

- `microkernel.rs` — Main `tokio::select!` event loop, message dispatch, update pipeline orchestration
- `ipc.rs` — `IpcRouter`: UDS accept loop, peer table (`Arc<RwLock<HashMap>>`), per-peer `mpsc` queues
- `lease.rs` — `LeaseManager`: Alive/GracePeriod/Expired/Dead states, probation
- `version.rs` — `VersionManager`: `vNNN` directories, `current`/`rollback` symlinks, consecutive failure lockout
- `state.rs` — `StateStore`: JSON-file-backed KV store, `MigrationTransaction` with WAL + snapshot + RAII rollback
- `runlevel.rs` — `RunlevelManager`: Halt/Safe/Normal/Evolve FSM

## Build and Test

```sh
cargo build                                # Build all workspace crates
RUST_LOG=debug cargo run --bin loopy-boot  # Start microkernel
cargo run --bin loopy-compiler             # Start compiler service (connects to boot)
```

No test suite exists yet. Rust edition 2024, workspace resolver 3.

## Code Patterns

- **Async**: tokio (full features) everywhere. Boot's main loop uses `tokio::select!` over channels + lease tick interval.
- **Error handling**: `Result<T, Box<dyn Error>>` at async boundaries, `Result<T, String>` for internal module functions. No custom error crate.
- **IPC wire format**: `[4-byte BE length][JSON]`, max 1 MB. Handshake: `Hello` → `Welcome`. See `crates/ipc/src/wire.rs`.
- **Peer identity**: String-based (`"boot"`, `"peripheral"`, `"compiler"`, `"judge"`, `"audit"`). Message IDs: atomic counter with identity prefix (e.g., `"compiler-1"`).
- **Concurrency**: `Arc<RwLock<HashMap>>` for peer table; `mpsc::channel` for per-peer outbound + boot-inbound.
- **Tracing**: Structured fields (`peer = %identity`), consistent level usage — info for lifecycle, debug for routine, warn for recoverable errors, error for fatal.
- **RAII transactions**: `MigrationTransaction` auto-rollbacks on `Drop` if uncommitted.
- **Platform**: `#[cfg(unix)]` for symlinks and file permissions.

## Dependencies

Intentionally minimal — only: `tokio`, `serde`, `serde_json`, `tracing`, `tracing-subscriber`, `reqwest` (peripheral only, for LLM API), and internal `loopy-ipc`. Discuss before adding any new dependency.

## Project Conventions

- Crate binaries named `loopy-*`; peer identities match crate names without prefix.
- Module-level `//!` doc comments describe purpose with plan.md section references.
- State files live under `~/.loopy/state/` (JSON, with in-memory cache).
- Version directories under `~/.loopy/versions/vNNN/` with `current`/`rollback` symlinks.
- UDS socket: `~/.loopy/loopy.sock`.
- See `.claude/rules/` for additional coding rules (ownership patterns, no speculation, architecture-first design, clean code).
