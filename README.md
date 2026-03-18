# VibeClaw

[中文 README](./README.zh-CN.md)

> **A source-level self-evolving agent system in Rust.**
> VibeClaw is building **Loopy**: an AI agent that can read, rewrite, compile, test, judge, and hot-swap **its own source code** — so you do not have to wait for someone else to define your assistant.

**Why this project feels different:**

- **Simple and light**: a tiny Boot microkernel plus a small set of services.
- **Freedom by default**: if you need a capability, the agent can evolve toward it instead of waiting for a vendor roadmap.
- **Smaller attack surface**: fewer built-in features means less code you are forced to trust.
- **Your trade-offs, not ours**: secure or aggressive, conservative or experimental — you decide the policy, the constitution, and the upgrade gate.

## 3-second overview

VibeClaw is not just an AI app wrapper.
It is a runtime where an agent can **inspect its own Rust source, stage code changes, compile them, pass policy checks, and be replaced in place** under Boot supervision.

```text
Prompt → agent edits source → compiler builds candidate → judge/policy validates → Boot hot-swaps or rolls back
```

If you believe personal AI should be **owned, inspectable, and evolvable at the source level**, this repo is for you.

## Install

```bash
git clone https://github.com/Dragonchu/VibeClaw.git
cd VibeClaw
cargo build
```

> Full self-evolution requires a `DEEPSEEK_API_KEY`. The Boot microkernel, compiler service, and admin tooling can still be explored without it.

## Quick start

### 1) Start the microkernel

```bash
RUST_LOG=info cargo run --bin loopy-boot
```

### 2) Start the compiler service in another terminal

```bash
cargo run --bin loopy-compiler
```

### 3) Start the self-evolving peripheral agent

```bash
LOOPY_WORKSPACE=$PWD DEEPSEEK_API_KEY=your_key_here cargo run --bin loopy-peripheral
```

Then open <http://127.0.0.1:7700>.

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

![Loopy local UI](https://github.com/user-attachments/assets/47ac39aa-56b0-4187-bd7b-9d7efb5fbfdb)

Want a quick smoke test before setting an API key?

```bash
cargo run --bin loopy-admin -- status
```

## Core features

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

`loopy-boot` keeps the trusted core intentionally small:

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
| `LOOPY_WORKSPACE` | Points the peripheral to the workspace it may inspect and stage |
| `LOOPY_SOCKET` | Overrides the Unix socket path (default: `~/.loopy/loopy.sock`) |
| `LOOPY_HTTP_PORT` | Overrides the local web UI port (default: `7700`) |
| `RUST_LOG` | Controls tracing verbosity |

### Key on-disk paths

- Socket: `~/.loopy/loopy.sock`
- State: `~/.loopy/state/`
- Peripheral versions: `~/.loopy/peripheral/vNNN/`
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
cargo run --bin loopy-admin -- status
cargo run --bin loopy-admin -- peers
cargo run --bin loopy-admin -- versions
cargo run --bin loopy-admin -- runlevel
```

### Repository layout

```text
crates/ipc/                shared wire protocol and message types
crates/boot/               immutable-ish microkernel and upgrade control plane
crates/services/compiler/  compile service for candidate builds
crates/services/judge/     scoring / test service scaffold
crates/services/audit/     audit service scaffold
crates/peripheral/         self-evolving agent, web UI, source tools
crates/admin/              local administration CLI
constitution/              invariants, benchmarks, amendment log
protocol/                  protocol definitions
plan.md                    detailed design document (Chinese)
```

## Why VibeClaw instead of another agent framework?

Because sometimes the best feature is **not shipping one more feature**.

VibeClaw aims to stay small, inspectable, and editable enough that you can:

- evolve the assistant you actually want
- avoid paying for roadmap bloat you do not need
- keep the trusted core small
- choose your own balance of power and safety

That combination — **self-evolutionary, powerful, yet intentionally minimal** — is the project's real thesis.

## Contributing

Contributions are welcome, especially around:

- hardening the self-evolution pipeline
- expanding the judge and audit services
- improving constitution / benchmark design
- better local workflows, demos, and observability
- safer source transformation and migration strategies

A good way to start:

1. read [`plan.md`](./plan.md)
2. build the workspace with `cargo build`
3. inspect the Boot and peripheral crates
4. open a focused issue or PR

## License

This project is licensed under the [MIT License](./LICENSE).
