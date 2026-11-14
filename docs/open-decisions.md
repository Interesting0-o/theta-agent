# 待决策清单

状态：未决，2026-11-14。  
相关文档：[基础设施调研结论](./infrastructure-findings.md)、[Coding Agent 架构设计](./agent-architecture.md)。

本文只记录**尚未拍板**的内容。每条包含背景、选项、当前倾向（未定）、决定前需要的信息和影响面。  
“倾向”不是结论，只是基于现有事实的默认建议；拍板后请把结果写入本文末尾的决策记录。

---

## 0. 总览

| ID | 决策 | 优先级 | 当前倾向（未定） | 主要影响 |
|---|---|---|---|---|
| D1 | 事件模型：枚举 / topic / Supervisor | P0 | 单一 `AgentEvent` 枚举 | `core/dispatcher.rs`、所有消费者 |
| D2 | 持久化选型：sqlx / rusqlite | P0 | sqlx 0.9 + SQLite | StateStore、迁移、测试 |
| D3 | Checkpoint 范围 | P1 | 第一版只做 agent state | 恢复流程、schema |
| D4 | MCP 范围 | P1 | 只做 client | MCP 模块、ToolRegistry |
| D5 | 模型策略 | P0 | 先实测 qwen2.5:7b | Tool loop、并行调用、审批 |
| D6 | 状态序列化 | P2 | JSON | `state_json`、事件 payload |
| D7 | 流式 delta 是否入事件日志 | P2 | 只记录最终响应 | 事件量、UI、replay |
| D8 | `rmcp` 版本 | P1 | 3.10.0，先 spike | MCP 兼容性 |
| D9 | Workspace 快照方式 | P1 | 临时 index + `write-tree` | Checkpoint、评估 |
| D10 | 审批策略细则 | P1 | 只读允许、写/命令询问 | 安全、CLI、非交互模式 |

优先级含义：

- P0：不决定就无法开始实现。
- P1：影响模块边界，但可以先做局部 spike。
- P2：可以在实现过程中再定。

---

## 1. D1：事件模型

### 背景

当前 `Dispatcher` 按 `TypeId` 路由，并把 `Arc<dyn Event>` downcast 回具体类型。引入 `EventEnvelope` 后，如果总线只传 envelope，`TypeId` 路由会退化。

另外，`run_worker` 是单任务，任何一个订阅者通道满都会阻塞整个 worker，存在全局 head-of-line blocking。

### 选项

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| A. 单一 `AgentEvent` 枚举 | 总线只传 `Arc<AgentEvent>`；订阅者 match 变体 | 不改 Dispatcher；与 DB 的 `kind` 字段天然对应；实现最快 | 每个订阅者收到所有事件；过滤逻辑分散 |
| B. Dispatcher 增加 topic 路由 | 按稳定字符串（如 `tool.call.requested`）路由 | 类型安全、可扩展、支持多订阅者过滤 | 需要改 `HashMap<TypeId, ...>` 和 downcast 机制 |
| C. Supervisor 转发 | 全局总线 → Supervisor 按 `state_id` → 每个 Actor 的 mpsc | 多 agent 隔离最好；Actor 串行处理自己的事件 | 多一层；v1 单 agent 用不上 |

### 决定前需要的信息

- 单 agent 第一版是否真的需要多订阅者按类型过滤？
- `LlmNode` / `ToolExecutor` 是否愿意接收并忽略无关事件？
- 是否计划在第一版就支持多 agent？

### 影响面

- `src/core/dispatcher.rs` 的订阅表结构
- `src/domain/event.rs` 的 trait 设计
- 所有 `Consumer` 实现
- `event_outbox.kind` 的取值约定

### 倾向（未定）

第一版选 A；多 agent 时再引入 Supervisor（C）。如果发现过滤逻辑重复太多，再升级到 B。

---

## 2. D2：持久化选型

### 背景

需要 `agent_states`、`agent_events`、`event_outbox`、`tool_executions`、`agent_checkpoints` 的事务一致性。

### 选项

| 方案 | 优点 | 缺点 |
|---|---|---|
| sqlx 0.9 + SQLite | async、内置池、`migrate!`、`query!`、`#[sqlx::test]` | MSRV 1.94；SQLite 连接一个 OS 线程；WAL 需显式开启 |
| rusqlite 0.42 + `spawn_blocking` | 简单直接、bundled SQLite 更新 | 无内置 async/池/迁移；所有调用要包阻塞线程 |
| sea-orm / diesel | ORM/查询构建 | 依赖更重；diesel 核心同步 |

### 决定前需要的信息

- 是否接受 sqlx 的 MSRV 1.94（当前 1.98.1，满足）。
- 是否需要 `query!` 的编译期检查，还是运行时 `query()` 就够。
- 是否愿意维护 `.sqlx/` offline 缓存。

### 影响面

- `Cargo.toml`
- 迁移文件目录
- StateStore 的 API 形态
- 测试基础设施

### 倾向（未定）

sqlx 0.9 + SQLite。第一版使用运行时 `query()`，避免 `query!` 的构建期依赖；后续再决定是否启用离线缓存。

---

## 3. D3：Checkpoint 范围

### 背景

Agent 状态快照和 workspace 快照是两种东西。第一版是否要同时做？

### 选项

| 方案 | 内容 | 成本 | 恢复能力 |
|---|---|---|---|
| A. 只做 agent state | `agent_checkpoints` + `state_json` + 事件日志 | 低 | 能恢复对话/pending calls，不能恢复文件 |
| B. state + 写操作 undo 记录 | 在 A 基础上记录每次文件写入的前后 hash | 中 | 能逐文件回滚，不能整体回滚 |
| C. state + workspace git 快照 | 在 B 基础上用 git 快照整个工作区 | 高 | 能整体恢复/分支 |

### 决定前需要的信息

- 第一版的核心价值是“恢复对话继续跑”，还是“恢复代码状态”？
- 是否接受 agent 修改用户工作区后无法整体回滚？
- 评估场景是否第一版就需要可复现 workspace？

### 影响面

- `agent_checkpoints` schema
- `WorkspaceSnapshotter` trait
- 恢复流程
- artifact store

### 倾向（未定）

第一版 A + `tool_executions` 账本（相当于 B 的最小版）；workspace 快照放第二阶段。

---

## 4. D4：MCP 范围

### 背景

`rmcp` 同时支持 client 和 server。theta 第一版是否需要对外暴露自己的工具？

### 选项

| 方案 | 内容 | 成本 | 风险 |
|---|---|---|---|
| A. 只做 client | 连接外部 MCP server，把工具注册进 ToolRegistry | 低 | 需要处理 server 生命周期、命名冲突 |
| B. client + server | 额外把 theta 能力工具暴露为 MCP server | 高 | 多一层协议/认证/沙箱；编排工具绝不能暴露 |

### 决定前需要的信息

- 是否有“让其他 agent/客户端调用 theta 工具”的需求？
- 第一版是否只需要本地 CLI？

### 影响面

- `rmcp` feature 选择
- 模块划分（`mcp.rs` 是否包含 server）
- 安全策略

### 倾向（未定）

只做 client（A）。Server 等有明确消费方再做。

---

## 5. D5：模型策略

### 背景

当前 `.env` 使用 `qwen2.5:7b`，通过 Ollama 的 OpenAI-compatible `/v1` 端点。小模型的 tool-calling 稳定性、流式分片、并行调用能力需要实测。

### 选项

| 方案 | 优点 | 风险 |
|---|---|---|
| A. 继续 qwen2.5:7b 本地 | 免费、隐私、低延迟 | 工具调用质量不稳定；并行/长上下文可能受限 |
| B. 换更大的本地模型 | 保持本地 | 需要更多显存/延迟 |
| C. 云端模型（Claude/GPT 等） | 工具调用成熟 | 成本、隐私、网络 |

### 决定前需要的信息

- qwen2.5:7b 在以下场景的实际表现：
  - 单工具调用
  - 流式 tool call 分片
  - 多工具/并行调用
  - 参数 JSON 合法性
  - 长上下文下的工具选择
- 是否允许把代码发送到云端。

### 影响面

- Tool loop 复杂度
- 并行工具调用是否第一版支持
- 上下文预算
- 评估基线

### 倾向（未定）

先做 A 的能力探测；如果单工具调用都不稳定，再考虑 B/C。不要把“并行工具调用”作为第一版前提。

---

## 6. D6：状态序列化

### 背景

`state_json` 和事件 payload 需要选择序列化格式。

### 选项

| 方案 | 优点 | 缺点 |
|---|---|---|
| JSON | 可读、调试方便、SQLite 文本函数可用 | 体积大、无 schema 演进保证 |
| MessagePack | 紧凑、快 | 不可读、需要额外 crate |
| Protobuf | 强 schema、跨语言 | 工具链重、调试成本高 |

### 决定前需要的信息

- 是否需要人肉查看事件日志？
- 状态大小是否成为瓶颈？
- 是否需要跨语言/跨版本兼容？

### 影响面

- `state_json` / `payload_json` 列类型
- 事件回放
- 迁移策略

### 倾向（未定）

第一版 JSON；如果 WAL/DB 体积成为问题，再考虑 zstd 压缩而不是换格式。

---

## 7. D7：流式 delta 是否入事件日志

### 背景

LLM 流式输出会产生大量 `LlmDelta`。全部写入事件日志会放大 DB/WAL 和回放成本。

### 选项

| 方案 | 内容 | 优点 | 缺点 |
|---|---|---|---|
| A. 只记录最终响应 | 事件日志只存 `LlmResponse` + 摘要 | 事件量小、回放简单 | 无法逐字重放 |
| B. 记录所有 delta | 每个 delta 一条事件 | 完整轨迹 | 事件量巨大、DB 压力大 |
| C. delta 走 UI 通道，不落库 | 流式输出单独通道；DB 只存最终 | 兼顾体验和成本 | 需要额外 UI channel |

### 决定前需要的信息

- 评估是否需要逐字重放？
- UI 是否要实时显示 token 流？

### 影响面

- 事件类型设计
- `Dispatcher` 负载
- DB 写入量
- 回放工具

### 倾向（未定）

C（或 A）。不建议 B。

---

## 8. D8：`rmcp` 版本

### 背景

`rmcp 3.10.0` 于 2026-11-12 发布，距调研仅两天；3.x 发布节奏快，生态实际 pin 落后。

### 选项

| 方案 | 优点 | 风险 |
|---|---|---|
| A. pin `=3.10.0` | 最新协议支持（`2026-07-28`、`Auto` 生命周期） | 太新，可能有未发现的回归 |
| B. pin `=3.9.0` | Goose 等实际项目在用，相对成熟 | 可能缺少 3.10 的修复 |
| C. 范围依赖 `3` | 自动升级 | API/行为漂移风险 |

### 决定前需要的信息

- 目标 MCP server 使用的协议版本分布。
- 3.10.0 与 3.9.0 的 changelog 差异。
- 是否有阻塞性 issue（discovery timeout、OAuth、capability 字段）。

### 影响面

- `Cargo.toml`
- MCP client 代码
- 兼容性测试矩阵

### 倾向（未定）

先 pin `=3.10.0` 做 spike；如果遇到阻塞问题，回退到 `=3.9.0`。不要用范围依赖。

---

## 9. D9：Workspace 快照方式

### 背景

如果是 D3 的方案 C，需要选择 workspace 快照方式。

### 选项

| 方案 | 覆盖范围 | 优点 | 缺点 |
|---|---|---|---|
| `git stash create` | tracked 修改 | 简单、不改变工作区 | 不含 untracked；dangling commit 需要管理 |
| 临时 index + `git write-tree` | tracked + untracked（遵守 .gitignore） | 不污染真实 index/worktree；可生成 tree/commit | 需要额外实现；二进制大文件仍占 object DB |
| `git worktree` | 独立工作区 | 适合评估/分支 | 全量 checkout；不适合每 turn |
| overlayfs / 容器 | 整个文件系统 | 回滚快 | Linux-only、权限、inode 坑 |

### 决定前需要的信息

- 第一版是否需要恢复 untracked 文件？
- 是否接受 `target/`、`node_modules/` 等被排除？
- 评估是否需要独立 worktree？

### 影响面

- `WorkspaceSnapshotter`
- checkpoint artifact
- 恢复流程
- 评估 runner

### 倾向（未定）

Checkpoint 用临时 index + `write-tree`；评估用 worktree。第一版可以只实现前者。

---

## 10. D10：审批策略细则

### 背景

需要确定哪些操作自动允许、哪些询问、哪些默认拒绝，以及非交互模式的行为。

### 待定规则

| 操作 | 候选策略 |
|---|---|
| 只读工具（`list_dir` / `read_file` / `search`） | 自动允许 |
| workspace 内写文件 | 询问 |
| workspace 外写文件 | 默认拒绝 |
| 命令执行 | 询问 + 沙箱 + 超时 |
| 高风险命令 | 默认拒绝或显式白名单 |
| MCP 写操作 | 询问 + server 信任校验 |
| 编排工具（step/memory/askuser） | 由 AgentActor 校验，不走审批 UI |
| 非交互模式 | 默认拒绝需要审批的操作；`--yes` 只放宽低风险 |

还需要确定：

- `ApprovedForSession` 的作用域和过期规则。
- `args_hash` 如何计算、何时校验（防 TOCTOU）。
- 审批超时/EOF 的默认值（必须是拒绝）。
- 审批恢复时如何重新询问。

### 影响面

- `ApprovalService`
- `CliInputService`
- ToolRegistry 元数据
- checkpoint 中的 pending approval

### 倾向（未定）

采用上表的候选策略作为第一版默认值；所有“缺失 annotations”的 MCP 工具按可能破坏性处理。

---

## 11. 建议先做的验证实验

这些实验用于给上面的决策提供依据，不代表已经决定实现顺序。

| 实验 | 前置 | 通过标准 | 输出 |
|---|---|---|---|
| qwen2.5:7b tool-call 探测 | Ollama 可达 | 单工具调用成功；流式分片能拼出合法 JSON | 能力矩阵、失败样本 |
| edgequake 适配回放 | 录制或构造请求 | Anthropic/OpenAI/Gemini 的 tool_use/tool_result 配对正确 | 兼容性记录 |
| SQLite CAS + outbox spike | sqlx 0.9 | CAS 冲突回滚；outbox 重放去重；`BEGIN IMMEDIATE` 无 517 | 最小可运行 schema + 测试 |
| rmcp stdio client spike | `rmcp 3.10` | `Auto` 生命周期兼容新旧 server；`list_all_tools`/`call_tool` 正常 | 兼容性矩阵 |
| git 临时 index 快照 spike | git 仓库 | 捕获 tracked + untracked；不污染真实 index/worktree | 快照/恢复脚本或说明 |

---

## 12. 决策依赖关系

```text
D5（模型能力）
  └─> 影响 Tool loop 复杂度、并行调用、D1 的消费者设计

D2（DB 选型）
  ├─> D3（Checkpoint 范围）
  ├─> D6（序列化）
  └─> D7（delta 是否入日志）

D8（rmcp 版本）
  └─> D4（MCP 范围）

D1（事件模型）
  ├─> D7（delta 通道）
  └─> D10（审批事件的路由）

D3（Checkpoint 范围）
  └─> D9（workspace 快照方式）
```

建议决策顺序：

1. **D5**：先确认模型能不能稳定 tool calling；这会影响后面所有设计。
2. **D2**：确定持久化基础。
3. **D1**：确定事件模型，避免消费者返工。
4. **D8 + D4**：MCP spike 和范围。
5. **D3 + D9**：checkpoint 与 workspace。
6. **D6 / D7 / D10**：随实现推进逐步定稿。

---

## 13. 决策记录模板

拍板后，在本文末尾追加一条：

```markdown
### Dx: <标题>

- 日期：YYYY-MM-DD
- 状态：accepted / rejected / superseded
- 决定：<一句话>
- 理由：<关键依据>
- 备选：<被否掉的选项及原因>
- 影响：<需要改的模块/文档>
- 后续：<需要补的验证或实现>
```

### 决策记录

（暂无）
