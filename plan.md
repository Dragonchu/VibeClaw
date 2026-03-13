# Loopy — 自进化 Agent 系统设计方案 (v3)

## 设计决策总结

| 决策项 | 选择 |
|--------|------|
| Boot 架构 | **微内核**：极简不可变核心 + 可升级系统服务 |
| Boot ↔ Peripheral 关系 | 独立子进程 + IPC |
| 版本管理 | 文件系统版本目录 + symlink 切换 |
| IPC 协议 | Unix Domain Socket + JSON 自定义协议 |
| 接口契约 | 纯协议文件（JSON 定义），运行时握手验证，**支持协商式升级** |
| 安全机制 | **Capability-Based 权限模型** + 编译验证 → 基准测试套件 → 握手验证 → **租约心跳** → 自动回滚 + **资源配额** |
| 仲裁机制 | **分层仲裁**：Invariant Test Suite（可通过高门槛修正）+ 多维评分 + 修正重试 + 缓刑机制 + 人类审计日志 |
| 运行模式 | **多运行级别**：停机 / 安全模式 / 正常模式 / 进化模式 |
| 状态管理 | **事务化迁移**：snapshot + WAL + 回滚保证 |

---

## 一、整体架构

```
~/.loopy/
├── boot                          ← Boot 微内核二进制（固定，release 编译）
├── loopy.sock                    ← Unix Domain Socket
├── services/                     ← 系统服务（可通过高权限更新）
│   ├── compiler/                 ← 编译引擎服务
│   │   ├── current -> v001
│   │   └── v001/
│   ├── judge/                    ← 测试运行器服务
│   │   ├── current -> v001
│   │   └── v001/
│   └── audit/                    ← 审计日志服务
│       ├── current -> v001
│       └── v001/
├── peripheral/
│   ├── v001/
│   │   ├── source/               ← 外围源代码
│   │   ├── binary                ← 编译产物
│   │   ├── manifest.json         ← 版本元数据
│   │   └── capabilities.json     ← 权限清单
│   ├── v002/
│   │   ├── source/
│   │   ├── binary
│   │   ├── manifest.json
│   │   └── capabilities.json
│   ├── current -> v002           ← symlink，指向当前活跃版本
│   └── rollback -> v001          ← symlink，指向回滚目标
├── state/
│   ├── memory.json               ← Agent 持久化记忆（版本无关）
│   ├── context.json              ← 运行上下文
│   ├── snapshots/                ← 状态快照（迁移前备份）
│   │   └── snap_v001_v002.json
│   └── wal/                      ← Write-Ahead Log
│       └── migration.log
├── protocol/
│   └── messages.json             ← 协议定义文件（Boot 和 Peripheral 共同遵守）
├── constitution/                 ← "宪法"：测试套件定义（高门槛可修正）
│   ├── invariants.json           ← 不可变测试用例
│   ├── benchmarks.json           ← 基准测试定义（多维评分）
│   └── amendments.log            ← 宪法修正记录（需人类签名）
└── audit/                        ← 审计日志
    ├── judgments.log             ← 记录所有更新请求、测试结果和仲裁决定
    └── resource_violations.log   ← 资源配额违规记录
```

---

## 二、Boot (微内核) 职责

Boot 采用**微内核架构**：自身只保留最小化的、真正不可变的核心功能，将编译、测试、审计等拆分为可升级的系统服务。这最小化了**可信计算基 (TCB)**，降低了"Boot 自身有 bug 导致整个系统永久瘫痪"的风险。

### 2.1 微内核核心（不可变）

Boot 二进制本身只负责以下极简功能：

- **进程启动/停止**：启动 Peripheral 和系统服务进程
- **Symlink 切换**：管理 `current` / `rollback` 指针
- **基础 IPC 路由**：在各进程间转发消息
- **租约管理**：维护各进程的心跳租约

其他功能全部委托给**系统服务**。

### 2.2 系统服务（可通过高权限更新）

系统服务是独立进程，自身也可以版本化更新，但更新门槛高于 Peripheral：

| 服务 | 职责 | 更新门槛 |
|------|------|----------|
| `compiler-service` | 编译引擎（调用 cargo） | 需人类授权 |
| `judge-service` | 测试运行器 + 评分系统 | 需人类授权 |
| `audit-service` | 审计日志写入与查询 | 需人类授权 |

> **设计哲学**：类比 Linux 微内核方案——最小化不可变内核，将驱动和服务都放到用户空间。Boot 核心出 bug 的概率因此大幅降低。

### 2.3 进程生命周期管理 + 租约心跳

采用**租约 (Lease) 机制**替代简单心跳：

- Peripheral 每 `T` 秒向 Boot 发送 `LeaseRenew` 消息
- Boot 维护租约过期时间 `deadline = now + lease_duration`
- 如果租约过期（Peripheral 未续租），Boot 可以**安全地**认为 Peripheral 已死
- 在启动新实例前，Boot 等待一个 `grace_period`，避免脑裂（旧实例其实还活着）

```
租约参数（可配置）:
  lease_duration: 10s      ← Peripheral 必须每 10s 续租一次
  grace_period: 5s         ← 租约过期后额外等待 5s 再启动新实例
  max_renewals_missed: 3   ← 连续 3 次未续租才判定死亡
```

租约续约消息还可携带**健康状态报告**，包括：

- 当前运行级别
- 内存/CPU 使用量
- 最近处理的任务数

### 2.4 版本管理器

- 管理 `peripheral/` 下的版本目录
- 控制 `current` 和 `rollback` symlink
- **连续失败熔断**：连续 3 次升级失败 → 锁定到最后已知可用版本 → 通知人类介入

### 2.5 验证与仲裁系统

这是防止 Agent "退化" 或 "发疯" 的关键防线。相较 v2 版本，引入了**正当程序、比例原则和宪法修正案机制**。

#### A. 宪法与修正案机制

测试套件不再是硬编码在 Boot 中的，而是存储在 `constitution/` 目录下作为**"宪法"**：

- `invariants.json`：必须通过的基础测试（协议一致性、安全边界等）
- `benchmarks.json`：多维性能评分定义

**宪法修正流程**（类比人类宪法修正案需要超级多数票）：

1. 修正提案由 Peripheral 或人类提出
2. **必须有人类签名授权**（即人类确认修改合理）
3. 修正记录写入 `constitution/amendments.log`，包含修改原因、修改内容、签名
4. Boot 重新加载测试套件

> **设计哲学**：绝对不可变的法律会导致系统僵化。宪法应当可修正，但修正门槛必须极高，防止 Agent 通过降低测试标准来"钻空子"。

#### B. 多维评分与比例原则

废弃原来的单一阈值 (`Score < 0.8`)，改为**多维加权评分**：

```json
{
  "dimensions": [
    { "name": "protocol_compliance", "weight": 0.3, "min_threshold": 0.95 },
    { "name": "task_correctness",    "weight": 0.3, "min_threshold": 0.85 },
    { "name": "response_latency",    "weight": 0.2, "min_threshold": 0.70 },
    { "name": "resource_efficiency", "weight": 0.2, "min_threshold": 0.60 }
  ],
  "overall_min": 0.80,
  "regression_tolerance": 0.05
}
```

**判定规则**（比例原则）：

- 任何单维度低于其 `min_threshold` → **强制拒绝**
- 加权总分低于 `overall_min` → **强制拒绝**
- 总分在 `[overall_min, old_score - regression_tolerance]` 之间 → **进入缓刑** (Probation)
- 总分高于 `old_score - regression_tolerance` → **正常通过**
- 某维度下降但另一维度大幅提升 → 综合评估后可能通过

> **设计哲学**：法律惩罚应与过错程度成比例。微小的性能波动不应阻止有益的进化；严重的退化必须被拦截。

#### C. 正当程序 (Due Process)

更新被拒绝时，Peripheral 享有以下权利：

1. **告知权**：Boot 返回结构化的拒绝理由（不仅写日志，还通过 IPC 发回）

   ```json
   {
     "type": "UpdateRejected",
     "failed_tests": ["protocol_echo_test"],
     "scores": { "protocol_compliance": 0.92, "task_correctness": 0.88 },
     "suggestion": "protocol_compliance below min_threshold 0.95"
   }
   ```

2. **修正重试权**：Peripheral 可以针对具体失败原因提交 **patch**，无需重新走完整编译流程（仅增量编译 + 重跑失败的测试）

3. **缓刑机制 (Probation)**：
   - 评分处于灰色地带的新版本，可以在**受限模式**下试运行
   - 缓刑期间 Boot 加强监控（心跳间隔减半，资源配额收紧）
   - 缓刑期满 → 重新评分，通过则正式上线，否则回滚
   - 缓刑期限：可配置，默认 1 小时

   ```
   缓刑参数:
     probation_duration: 3600s
     probation_heartbeat_interval: 5s   ← 正常 10s 的一半
     probation_resource_quota: 0.5x     ← 正常配额的一半
   ```

### 2.6 审计日志 (Audit Log)

- Boot 必须记录每一次 "判决" 的详细原因。

- `judgments.log`:

    ```json
    {
      "timestamp": "...",
      "event": "update_rejected",
      "candidate_version": "v005",
      "reason": "Invariant Test Failed: 'protocol_echo_test'",
      "details": "Expected 'pong', got 'crash'",
      "scores": { "protocol_compliance": 0.92 },
      "action": "notified_peripheral_with_structured_feedback"
    }
    ```

- 方便人类事后 "上诉" 或分析。

### 2.7 运行级别 (Runlevel / Graceful Degradation)

借鉴 Linux systemd targets，定义多个运行级别，实现**优雅降级**而非简单的"正常/崩溃"二元状态：

| Level | 名称 | 含义 | 触发条件 |
|-------|------|------|----------|
| 0 | `halt` | 停机 | 严重不可恢复故障、人类手动停机 |
| 1 | `safe` | 安全模式（仅核心 Agent，禁用所有插件） | 插件导致连续崩溃 |
| 2 | `normal` | 正常模式 | 默认启动级别 |
| 3 | `evolve` | 进化模式（允许自我修改） | 资源充足 + 无活跃任务时 |

**级别转换规则**：

- `normal → safe`：连续 2 次崩溃且回滚后仍崩溃
- `normal → evolve`：系统空闲 + 资源使用率 < 50%
- `evolve → normal`：自我修改完成或资源不足
- `任意 → halt`：人类手动命令或不可恢复错误
- `safe → normal`：人类手动确认修复或自动恢复检测通过

### 2.8 资源配额与 OOM 保护

Boot 启动 Peripheral 时设置**资源限制**，防止 Agent 因 bug 或"疯狂进化"消耗过多资源：

**资源限制清单**：

```json
{
  "resource_limits": {
    "max_memory_mb": 512,
    "max_cpu_percent": 80,
    "max_open_files": 256,
    "max_disk_write_mb": 100,
    "max_process_count": 4
  }
}
```

**实现方式**（按平台）：

- macOS: `sandbox-exec` profiles + `ulimit`
- Linux: `cgroups v2` + `seccomp` + `landlock`

**超限处理**：

- 软限制（80%）→ 警告 + 记录日志
- 硬限制（100%）→ 降级到安全模式或终止进程

### 2.9 热替换流程（灵魂出窍）v2

结合新增的正当程序、缓刑和事务化迁移：

1. **Submit**: Peripheral(v_old) 提交源代码
2. **Build**: `compiler-service` 编译代码
3. **Test**: `judge-service` 在沙箱中启动 v_new，运行 **Invariant Test Suite**
    - *Hard Fail*（不可变测试未通过）→ 记录审计日志 → **结构化反馈**返回 v_old → 允许**修正重试**
    - *Soft Fail*（评分处于灰色地带）→ 进入步骤 3b
    - *Pass* → 继续到步骤 4
3b. **Probation**: 以缓刑模式启动 v_new（资源受限、加强监控）
    - 缓刑期满重新评分 → Pass 则继续，Fail 则回滚
4. **Snapshot**: 对当前状态做快照（写入 `state/snapshots/`）
5. **Handshake**: 完整启动 v_new，进行协议握手
6. **State Migration**: 事务化迁移状态
    - 写 WAL → 执行迁移 → 验证 → 提交
    - 任何步骤失败 → 从快照恢复
7. **Switch**: 切换 symlink (current -> v_new)，杀 v_old

---

## 三、Peripheral (外围程序) 职责

### 3.1 核心与插件

- **Agent 核心**：负责决策、反思。

- **插件系统**：建议采用双层架构。
  - *Rhai/Lua 脚本*：快速热更新，用于调整策略。
  - *Rust 编译模块*：高性能，需走完整更新流程。

### 3.2 自我修改与进化

- Agent 负责编写新版本的代码。

- **关键约束**：Agent 必须确保新代码能通过 Boot 的测试套件（Agent 可以读取 `constitution/` 下的测试题目，但不能修改判定逻辑——修改需走宪法修正案流程）。

### 3.3 Capability-Based Security (权限模型)

每个 Peripheral 版本必须附带 `capabilities.json`，声明其所需权限。Boot 根据此声明进行**最小权限的强制隔离**：

```json
{
  "capabilities": {
    "filesystem": {
      "read": ["~/.loopy/state/", "~/.loopy/constitution/"],
      "write": ["~/.loopy/state/memory.json", "~/.loopy/state/context.json"]
    },
    "network": {
      "allowed": false,
      "whitelist": []
    },
    "exec": {
      "allowed": false,
      "whitelist": []
    },
    "ipc": ["loopy.sock"],
    "max_child_processes": 0
  }
}
```

**核心原则**：

- **默认拒绝**：未声明的权限一律禁止
- **权限只能缩小不能扩大**：新版本请求的权限不能超过上一版本（扩展权限需人类审批）
- **运行时强制执行**：通过 OS 级沙箱机制实现（macOS: `sandbox-exec`; Linux: `seccomp` + `landlock`）

> **设计哲学**：操作系统的 Capability-Based Security 原则——进程只能做被明确授予的事。这防止 Agent 在自我进化中逐渐"越权"。

---

## 四、协议设计 (扩展)

### 4.1 协议协商式升级

协议不再是一成不变的。支持 Peripheral 提议协议扩展：

**升级流程**（类比社会契约的立法提案）：

1. Peripheral 发送 `ProtocolExtensionProposal` 消息，包含新消息类型定义
2. Boot 评估扩展是否兼容现有协议（不破坏已有消息）
3. 如果是**向后兼容的扩展**（新增消息类型）→ Boot 可以自动采纳
4. 如果是**破坏性变更**（修改已有消息格式）→ 需要人类审批
5. 采纳后更新 `messages.json` 的 `protocol_version`

### 4.2 消息定义

在 `messages.json` 中增加完整消息类型：

```json
{
  "protocol_version": "1.1",
  "messages": {
    "test": {
      "request": { "type": "RunTest", "test_id": "string", "input": "any" },
      "response": { "type": "TestResult", "output": "any" }
    },
    "update": {
      "submit": { "type": "SubmitUpdate", "source_path": "string" },
      "rejected": {
        "type": "UpdateRejected",
        "failed_tests": ["string"],
        "scores": "object",
        "suggestion": "string",
        "allows_patch_retry": "boolean"
      },
      "probation": { "type": "ProbationStarted", "duration_secs": "number", "constraints": "object" },
      "accepted": { "type": "UpdateAccepted", "version": "string" }
    },
    "handshake": {
      "request": { "type": "Hello", "protocol_version": "string", "capabilities": "object" },
      "response": { "type": "Welcome", "accepted_capabilities": "object", "runlevel": "number" }
    },
    "heartbeat": {
      "renew": { "type": "LeaseRenew", "health": "object" },
      "ack": { "type": "LeaseAck", "next_deadline_ms": "number" }
    },
    "protocol_extension": {
      "propose": { "type": "ProtocolExtensionProposal", "new_messages": "object" },
      "result": { "type": "ProtocolExtensionResult", "accepted": "boolean", "reason": "string" }
    },
    "runlevel": {
      "change": { "type": "RunlevelChange", "from": "number", "to": "number", "reason": "string" }
    }
  }
}
```

---

## 五、状态迁移 (Transactional Schema Versioning)

采用**事务化迁移**机制，借鉴数据库和文件系统的崩溃一致性设计：

### 5.1 基本规则

- `SetState` 必须包含版本号：`SetState(key="memory", value={...}, schema_version=2)`
- `GetState` 返回数据和版本号。
- Peripheral 必须实现 `migrate(data, from_ver, to_ver)` 逻辑，处理旧版本数据的升级。

### 5.2 事务化保证

**迁移前**：自动创建状态快照

```
state/snapshots/snap_v001_v002.json  ← 完整的状态副本
```

**Write-Ahead Log (WAL)**：迁移过程中的每一步操作先写入日志

```
state/wal/migration.log:
  [1] BEGIN_MIGRATION from=v001 to=v002
  [2] TRANSFORM key="memory" schema_version=1->2
  [3] VALIDATE key="memory" result=OK
  [4] COMMIT
```

**迁移失败恢复**：

- 任何步骤失败 → 从快照恢复到迁移前状态
- Boot 重启时检查 WAL：如果发现未完成的迁移，自动回滚
- 类似 ZFS/Btrfs 的 **copy-on-write** 思路：旧数据在新数据完全可用之前不会被删除

### 5.3 迁移回滚脚本

每个 Peripheral 版本除了实现 `migrate()` 外，还必须实现 `rollback_migration()`：

- 向前迁移：`migrate(data, v1, v2)` — 将 v1 格式转为 v2
- 向后回滚：`rollback_migration(data, v2, v1)` — 将 v2 格式退回 v1

双向迁移保证了回滚的安全性。

---

## 六、Rust 项目结构

```
loopy/                          ← workspace root
├── Cargo.toml                  ← workspace 定义
├── crates/
│   ├── boot/                   ← Boot 微内核二进制
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs         ← 入口
│   │       ├── microkernel.rs  ← 微内核核心（进程启动/停止/IPC路由）
│   │       ├── lease.rs        ← 租约管理
│   │       ├── version.rs      ← 版本管理 + symlink 切换
│   │       ├── runlevel.rs     ← 运行级别管理
│   │       ├── resource.rs     ← 资源配额与限制
│   │       ├── sandbox.rs      ← 沙箱 / Capability 执行
│   │       └── ipc.rs          ← IPC 路由
│   ├── services/               ← 可升级系统服务
│   │   ├── compiler/           ← 编译引擎服务
│   │   │   ├── Cargo.toml
│   │   │   └── src/
│   │   │       └── main.rs
│   │   ├── judge/              ← 裁判系统服务
│   │   │   ├── Cargo.toml
│   │   │   └── src/
│   │   │       ├── main.rs
│   │   │       ├── invariant.rs    ← 不变量测试运行器
│   │   │       ├── benchmark.rs    ← 多维评分
│   │   │       └── due_process.rs  ← 正当程序（反馈/重试/缓刑）
│   │   └── audit/              ← 审计日志服务
│   │       ├── Cargo.toml
│   │       └── src/
│   │           └── main.rs
│   └── peripheral/             ← 外围程序
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── agent.rs        ← Agent 核心
│           ├── migration.rs    ← 状态迁移 + 回滚逻辑
│           ├── library/        ← 核心能力库
│           └── plugins/        ← 插件目录
├── protocol/
│   └── messages.json           ← 协议定义
├── constitution/
│   ├── invariants.json         ← 不可变测试用例定义
│   └── benchmarks.json         ← 多维评分基准定义
└── tests/
    ├── invariants/             ← 测试用例实现
    └── integration/            ← 集成测试
```

---

## 七、实现计划

- [ ] **Phase 1: 微内核骨架**
  - [ ] 搭建 workspace (Boot + Services + Peripheral)
  - [ ] 实现基础 IPC (Unix Domain Socket) 路由
  - [ ] 实现 Boot 微内核：进程启动/停止
  - [ ] 实现租约心跳机制
- [ ] **Phase 2: 灵魂出窍 (Update Loop)**
  - [ ] 实现 compiler-service（Cargo invoke）
  - [ ] 实现版本目录管理和 Symlink 切换
  - [ ] 实现基础状态持久化 (JSON)
  - [ ] 实现状态快照 + WAL 事务化迁移
- [ ] **Phase 3: 裁判系统 (The Judge)**
  - [ ] 实现 judge-service 基础框架
  - [ ] 定义 `constitution/invariants.json` 第一批测试（协议测试）
  - [ ] 实现多维评分系统 (`constitution/benchmarks.json`)
  - [ ] 实现正当程序：结构化反馈 + 修正重试
  - [ ] 实现缓刑 (Probation) 机制
  - [ ] 实现 audit-service + `judgments.log`
- [ ] **Phase 4: 安全与降级**
  - [ ] 实现 Capability-Based Security（`capabilities.json` + OS 沙箱）
  - [ ] 实现资源配额与 OOM 保护
  - [ ] 实现运行级别 (Runlevel) 状态机
  - [ ] 实现优雅降级转换逻辑
- [ ] **Phase 5: 宪法与协议进化**
  - [ ] 实现宪法修正案流程（人类签名授权）
  - [ ] 实现协议协商式升级
- [ ] **Phase 6: Agent 逻辑**
  - [ ] 实现一个简单的 "自我复制" Agent（读取源码 -> 稍微改动 -> 提交）
  - [ ] 集成 Rhai 脚本引擎作为轻量插件层
  - [ ] 实现双向状态迁移逻辑 (`migrate` + `rollback_migration`)
