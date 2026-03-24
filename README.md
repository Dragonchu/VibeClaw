# Reloopy

[中文 README](./README.zh-CN.md)

> **A source-level self-evolving agent system in Rust.**
> Reloopy is an AI agent that can read, rewrite, compile, test, judge, and hot-swap **its own source code** — so you do not have to wait for someone else to define your assistant.

## Source-level self-evolution — what makes Reloopy different

Most agent frameworks treat "self-improvement" as prompt engineering or RAG tuning.
Reloopy operates at a fundamentally lower level: **the agent reads its own Rust source, stages code changes, compiles a candidate binary, passes policy checks, and gets hot-swapped in place** — all under Boot microkernel supervision.

```text
Prompt → agent edits source → compiler builds candidate → judge/policy validates → Boot hot-swaps or rolls back
```

This is not a diagram of a future feature — it is the implemented loop today. The mechanism lives in:

- `crates/peripheral/src/source.rs` — reads and stages source files
- `crates/peripheral/src/agent.rs` — LLM-driven tool-calling loop that invokes source tools
- `crates/boot/src/version.rs` — manages version directories and symlink switching
- `crates/boot/src/microkernel.rs` — orchestrates compile → test → swap pipeline

**Key design principles:**

- **Simple and light**: a tiny Boot microkernel plus a small set of services.
- **Freedom by default**: if you need a capability, the agent can evolve toward it instead of waiting for a vendor roadmap.
- **Smaller attack surface**: fewer built-in features means less code you are forced to trust.
- **Your trade-offs, not ours**: secure or aggressive, conservative or experimental — you decide the policy, the constitution, and the upgrade gate.

## Install & start

```bash
git clone https://github.com/Dragonchu/reloopy.git
cd reloopy
DEEPSEEK_API_KEY=your_key_here ./setup.sh
```

`setup.sh` builds all crates in release mode and installs `reloopy-*` binaries to `~/.cargo/bin/` (or a custom `--prefix`).

Then start the system:

```bash
DEEPSEEK_API_KEY=your_key_here reloopy start
```

Boot auto-spawns **Compiler**, **Admin-Web**, and **Peripheral** in the correct order. Open <http://127.0.0.1:7700> when you see the startup log.

Stop with `reloopy stop` or `Ctrl-C`.

> No API key yet? Run `reloopy start` without one — Boot and the compiler still start so you can explore the architecture.

<details>
<summary>Development workflow (without installing)</summary>

### 1) Build

```bash
cargo build
```

### 2) Start the microkernel (spawns all services automatically)

```bash
RUST_LOG=info DEEPSEEK_API_KEY=your_key_here cargo run --bin reloopy-boot
```

Then open <http://127.0.0.1:7700>.

</details>

### Minimal runnable example

Send a prompt to the local agent UI, or call the HTTP API directly:

```bash
curl -N http://127.0.0.1:7700/api/chat \
  -H 'Content-Type: text/plain' \
  --data 'Read your own source code, explain what tools you have, and propose one safe self-improvement.'
```

### What you will see

- a local chat UI streaming reasoning, tool calls, and results
- the agent reading files from `crates/peripheral/`
- when it decides to evolve, a staged update submitted back to Boot for validation

Want a quick smoke test before setting an API key?

```bash
reloopy-admin status
```

## How the self-evolution loop works

### 1. Source-level self-evolution

The peripheral agent is given explicit tools to:

- list and read its own source files
- write staged replacements for source files
- package a candidate workspace
- submit the update back to Boot

This is implemented directly in the codebase, not promised as a future abstraction. See:

- `crates/peripheral/src/agent.rs`
- `crates/peripheral/src/tools.rs`
- `crates/peripheral/src/source.rs`

### 2. Minimal immutable Boot microkernel

`reloopy-boot` keeps the trusted core intentionally small:

- IPC routing over Unix domain sockets
- process supervision via lease renewals
- version switching via `current` / `rollback` symlinks
- runlevel management and state migration coordination

### 3. Safety rails for aggressive evolution

The project combines ambitious self-modification with conservative control points:

- **capability-based permissions**
- **constitution / invariant checks**
- **multi-dimensional scoring and probation**
- **transactional state migration with rollback**
- **audit logging and version lockout after repeated failures**

In short: the agent can change itself, but not without passing through explicit gates.

### 4. Local-first, hackable architecture

All major pieces are plain Rust crates in one workspace. You can inspect the rules, change the upgrade policy, replace services, or fork the agent without waiting for a hosted platform to expose a checkbox.

## Configuration

### Runtime environment variables

| Variable | Purpose |
| --- | --- |
| `DEEPSEEK_API_KEY` | Required for the peripheral LLM client |
| `DEEPSEEK_BASE_URL` | Optional override for the DeepSeek API base URL |
| `DEEPSEEK_MODEL` | Optional model override |
| `RELOOPY_WORKSPACE` | Points the peripheral to the workspace it may inspect and stage |
| `RELOOPY_SOCKET` | Overrides the Unix socket path (default: `~/.reloopy/reloopy.sock`) |
| `RELOOPY_HTTP_PORT` | Overrides the local web UI port (default: `7700`) |
| `RUST_LOG` | Controls tracing verbosity |

### Key on-disk paths

- Socket: `~/.reloopy/reloopy.sock`
- State: `~/.reloopy/state/`
- Peripheral versions: `~/.reloopy/peripheral/vNNN/`
- Constitution: `./constitution/`
- Design doc: [`plan.md`](./plan.md) (Chinese)

## API / usage examples

### Web API

The peripheral currently exposes a tiny local HTTP interface:

- `GET /` — chat UI
- `POST /api/chat` — send a plain-text prompt and receive streamed events
- `POST /api/reset` — reset the conversation

Example:

```bash
curl -N http://127.0.0.1:7700/api/chat \
  -H 'Content-Type: text/plain' \
  --data 'Inspect src/main.rs and explain how heartbeats reach Boot.'
```

### Admin CLI

```bash
reloopy-admin status
reloopy-admin peers
reloopy-admin versions
reloopy-admin runlevel
```

### Repository layout

```text
crates/ipc/                shared wire protocol and message types
crates/boot/               immutable-ish microkernel and upgrade control plane
crates/services/compiler/  compile service for candidate builds
crates/services/judge/     scoring / test service scaffold
crates/services/audit/     audit service scaffold
crates/peripheral/         self-evolving agent, web UI, source tools
crates/cli/               unified CLI entry point (`reloopy` command)
crates/admin/              local administration CLI
constitution/              invariants, benchmarks, amendment log
protocol/                  protocol definitions
plan.md                    detailed design document (Chinese)
```

## Contributing

Contributions are welcome, especially around:

- hardening the self-evolution pipeline
- expanding the judge and audit services
- improving constitution / benchmark design
- better local workflows, demos, and observability
- safer source transformation and migration strategies

A good way to start:

1. read [`plan.md`](./plan.md)
2. run `./setup.sh` to build and install
3. inspect the Boot and peripheral crates
4. open a focused issue or PR

## License

This project is licensed under the [MIT License](./LICENSE).
