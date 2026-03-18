# VibeClaw

[English README](./README.md)

> **一个 Rust 编写的源码级自进化 Agent 系统。**
> VibeClaw 正在构建 **Loopy**：一个能够读取、改写、编译、测试、裁决并热切换**自身源码**的 AI Agent——你不需要继续等待别人来定义你的 AI 助手。

**为什么它会让人眼前一亮：**

- **简单、轻**：极小 Boot 微内核 + 少量系统服务。
- **自由度高**：有需求就让 Agent 朝那个方向进化，而不是等平台排期。
- **攻击面更小**：内建功能越少，被迫信任的代码就越少。
- **选择权在你手里**：要安全、要激进、要保守、要强大，都由你自己决定升级规则与边界。

## 3 秒看懂

VibeClaw 不是套了聊天界面的 AI 壳子。
它是一个运行时：Agent 可以**检查自己的 Rust 源码、暂存代码修改、编译候选版本、通过策略校验，并在 Boot 监管下原地热替换**。

```text
Prompt → Agent 改源码 → Compiler 编译候选版本 → Judge / Policy 裁决 → Boot 热切换或回滚
```

如果你认同个人 AI 应该是**可拥有、可审查、可在源码层进化**的，这个项目就是为你准备的。

## 安装

```bash
git clone https://github.com/Dragonchu/VibeClaw.git
cd VibeClaw
cargo build
```

> 完整的自进化能力需要配置 `DEEPSEEK_API_KEY`。即便没有 API Key，你仍然可以先体验 Boot 微内核、Compiler 服务和 Admin 工具。

## 快速开始

### 1）启动微内核

```bash
RUST_LOG=info cargo run --bin loopy-boot
```

### 2）在另一个终端启动编译服务

```bash
cargo run --bin loopy-compiler
```

### 3）启动自进化 Peripheral Agent

```bash
LOOPY_WORKSPACE=$PWD DEEPSEEK_API_KEY=your_key_here cargo run --bin loopy-peripheral
```

然后打开 <http://127.0.0.1:7700>。

### 最简可运行示例

你可以直接在本地 UI 中发送 prompt，也可以通过 HTTP API 调用：

```bash
curl -N http://127.0.0.1:7700/api/chat \
  -H 'Content-Type: text/plain' \
  --data '读取你自己的源码，说明你有哪些工具，并提出一个安全的小改进方案。'
```

### 运行后你会看到什么

- 一个本地聊天界面，实时流式展示 reasoning、tool call 与结果
- Agent 直接读取 `crates/peripheral/` 下的源码
- 当它决定进化时，会把候选更新提交给 Boot 做验证和裁决

![Loopy 本地界面](https://github.com/user-attachments/assets/47ac39aa-56b0-4187-bd7b-9d7efb5fbfdb)

如果你想在配置 API Key 之前先做一次 smoke test：

```bash
cargo run --bin loopy-admin -- status
```

## 核心功能

### 1. 源码级自进化

Peripheral Agent 被明确授予以下能力：

- 枚举并读取自己的源码文件
- 写入暂存区版本的源码文件
- 打包候选工作区
- 将更新重新提交给 Boot

这不是 README 里的概念图，而是已经写进代码里的机制。关键位置见：

- `crates/peripheral/src/agent.rs`
- `crates/peripheral/src/tools.rs`
- `crates/peripheral/src/source.rs`

### 2. 极简且尽量不可变的 Boot 微内核

`loopy-boot` 刻意把可信核心缩到最小，只负责：

- 基于 Unix Domain Socket 的 IPC 路由
- 基于 Lease 的进程监管
- 基于 `current` / `rollback` symlink 的版本切换
- runlevel 管理与状态迁移协调

### 3. 为激进进化加上保守护栏

项目把强自修改能力和审慎控制点放在一起：

- **Capability-Based 权限模型**
- **constitution / invariant 校验**
- **多维评分 + probation 缓刑机制**
- **带回滚保证的事务化状态迁移**
- **审计日志与多次失败后的版本锁定**

换句话说：它允许 Agent 改自己，但不会让它毫无约束地改自己。

### 4. 本地优先、可 hack、可分叉

所有关键组件都是一个 Rust workspace 里的普通 crate。你可以自己检查规则、调整升级策略、替换服务、分叉 Agent，而不是等某个托管平台给你开放一个设置项。

## 配置

### 运行时环境变量

| 变量 | 作用 |
| --- | --- |
| `DEEPSEEK_API_KEY` | Peripheral 访问 LLM 所必需 |
| `DEEPSEEK_BASE_URL` | 可选，覆盖 DeepSeek API 地址 |
| `DEEPSEEK_MODEL` | 可选，覆盖模型名 |
| `LOOPY_WORKSPACE` | 指定 Peripheral 可检查、可暂存的工作区 |
| `LOOPY_SOCKET` | 覆盖 Unix Socket 路径（默认：`~/.loopy/loopy.sock`） |
| `LOOPY_HTTP_PORT` | 覆盖本地 Web UI 端口（默认：`7700`） |
| `RUST_LOG` | 控制 tracing 日志级别 |

### 关键磁盘路径

- Socket：`~/.loopy/loopy.sock`
- State：`~/.loopy/state/`
- Peripheral 版本目录：`~/.loopy/peripheral/vNNN/`
- Constitution：`./constitution/`
- 设计文档：[`plan.md`](./plan.md)

## API / 使用示例

### Web API

当前 Peripheral 暴露了一个很小的本地 HTTP 接口：

- `GET /` —— 聊天 UI
- `POST /api/chat` —— 发送纯文本 prompt，接收流式事件
- `POST /api/reset` —— 重置对话

示例：

```bash
curl -N http://127.0.0.1:7700/api/chat \
  -H 'Content-Type: text/plain' \
  --data '检查 src/main.rs，并解释心跳是如何发回 Boot 的。'
```

### Admin CLI

```bash
cargo run --bin loopy-admin -- status
cargo run --bin loopy-admin -- peers
cargo run --bin loopy-admin -- versions
cargo run --bin loopy-admin -- runlevel
```

### 仓库结构

```text
crates/ipc/                共享协议、消息类型与线协议
crates/boot/               微内核与升级控制平面
crates/services/compiler/  候选版本编译服务
crates/services/judge/     评分 / 测试服务骨架
crates/services/audit/     审计服务骨架
crates/peripheral/         自进化 Agent、Web UI、源码工具
crates/admin/              本地管理 CLI
constitution/              不变量、基准、修正案日志
protocol/                  协议定义
plan.md                    详细设计文档
```

## 为什么是 VibeClaw，而不是另一个 Agent 框架？

因为很多时候，真正厉害的能力是：**不预装一大堆你并不需要的功能**。

VibeClaw 想保持足够小、足够透明、足够可改，让你可以：

- 进化出你真正想要的助手
- 不为别人的 roadmap 膨胀买单
- 把可信核心保持在尽可能小的范围内
- 自己决定能力和安全之间的平衡点

这也是项目最核心的主张：**它既自进化、很强，也刻意保持克制与极简。**

## 贡献指南

欢迎贡献，尤其是这些方向：

- 强化自进化流水线的安全性
- 完善 judge / audit 服务
- 改进 constitution / benchmark 设计
- 打磨本地开发体验、演示与可观测性
- 提升源码变换与状态迁移的安全性

建议的开始方式：

1. 先读 [`plan.md`](./plan.md)
2. 用 `cargo build` 构建 workspace
3. 阅读 Boot 与 Peripheral 相关 crate
4. 提交一个聚焦的小 issue 或 PR

## 许可证

本项目基于 [MIT License](./LICENSE) 开源。
