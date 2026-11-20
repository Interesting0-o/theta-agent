# 基础设施调研结论（已确认事实）

状态：调研结论，2026-11-14。  
范围：tool call 处理、MCP 接入、checkpoint、数据库持久化与事务。  
原则：本文只记录**已在本地源码/依赖中核实**，或**已对照一手资料核实**的内容；未确定的决策见 [待决策清单](./open-decisions.md)，不与事实混写。

标注约定：

- 【本地核实】直接读取本仓库、Cargo 元数据或已下载的 crate 源码得到。
- 【外部核实】对照官方仓库、crates.io、规范或 pinned commit 得到，注明截至日期。
- 【设计约束】由已验证事实直接推导出的、必须遵守的约束。
- 【待决策】尚未确定，本文不当作结论。

---

## 1. 项目与依赖现状

### 1.1 本地环境【本地核实】

| 项 | 值 |
|---|---|
| rustc / cargo | 1.98.1 |
| edition | 2024 |
| 事件系统 | `src/core/dispatcher.rs` + `src/core/dead_letter.rs`，已有 7 个测试 |
| 领域抽象 | `Event` / `Consumer` / `Producer` |
| LLM 依赖 | `edgequake-llm 0.10.10`（MSRV 1.95，edition 2021） |
| 其他依赖 | `async-trait 0.1.92`、`dotenv 0.15.0`、`tokio 1.54.1` |
| 当前 tokio 实际 feature | 传递依赖启用了 `full`；但 `Cargo.toml` 只声明了 `macros`、`rt-multi-thread` |

【设计约束】不要依赖传递启用的 tokio feature。需要 MCP 子进程、超时、文件操作时，应在 `Cargo.toml` 中显式声明 `process`、`time`、`fs`、`signal` 等 feature。

### 1.2 当前模型配置【本地核实】

`.env` 中的配置形态：

- `CHAT_MODEL_NAME=qwen2.5:7b`
- `CHAT_MODEL_BASE_URL` 指向本地 Ollama 的 OpenAI-compatible `/v1` 端点
- `CHAT_MODEL_PROVIDER` 为空

【设计约束】

1. 这些变量不是 `edgequake-llm` 的约定。它读取的是 `OPENAI_API_KEY`、`OPENAI_BASE_URL`、`OPENAI_MODEL`、`EDGEQUAKE_LLM_PROVIDER` 等。
2. 对 Ollama 的 `/v1` 端点，正确入口是：
   ```rust
   OpenAIProvider::compatible(api_key, base_url)
   ```
   `OpenAIProvider::new()` 走官方 OpenAI 端点；`OpenAIProvider::from_env()` 读的是 `OPENAI_*`，不是 `CHAT_MODEL_*`。
3. `qwen2.5:7b` 的 tool-calling / 流式分片 / 并行调用能力必须实测，不能假设与 GPT、Claude 一致。
4. `edgequake-llm` 中 OpenAI provider 的 `supports_function_calling()`、`supports_streaming()`、`supports_tool_streaming()` 是无条件 `true`，**不能用来探测具体后端能力**。

---

## 2. Tool call 处理

### 2.1 `edgequake-llm` 已提供的能力【本地核实】

`src/traits.rs` 中的核心契约：

```rust
async fn chat_with_tools(
    &self,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    tool_choice: Option<ToolChoice>,
    options: Option<&CompletionOptions>,
) -> Result<LLMResponse>;

async fn chat_with_tools_stream(...) -> Result<BoxStream<'static, Result<StreamChunk>>>;
```

已确认的类型与行为：

- `ToolDefinition::function(name, description, parameters)`，其中 `parameters` 是 JSON Schema。
- `ToolCall { id, function: { name, arguments: String }, thought_signature }`。
- `LLMResponse { content, tool_calls, finish_reason, usage, ... }`。
- `ChatMessage::assistant_with_tools(content, tool_calls)`。
- `ChatMessage::tool_result(tool_call_id, content)`。
- `StreamChunk::ToolCallDelta { index, id, function_name, function_arguments, thought_signature }`。
- `StreamToolCallAccumulator`：按 `index` 累积分片。
- `finalize_streamed_tool_calls_with_repair`：处理空参数补 `{}`、缺少函数名、非法 JSON。
- `ToolDefinition` 默认带 `strict = Some(true)`；OpenAI provider 在转换为请求时不传 `strict`，因此不会影响 OpenAI-compatible 后端。

Provider 适配已实现：

- Anthropic：`ChatRole::Tool` 会转换为 user message 中的 `tool_result` 块（`src/providers/anthropic.rs`）。
- Gemini：`ChatRole::Tool` 会转换为 user Content 中的 `functionResponse`（`src/providers/gemini.rs`）。
- 因此 theta 只需要维护一套会话历史，不需要为每个 provider 写分支。

Schema 限制：

- `src/providers/schema_utils.rs` 定义了 Anthropic strict-mode 限制：
  - strict tools ≤ 20
  - 可选参数 ≤ 24
  - union / `anyOf` 参数 ≤ 16
- 【设计约束】工具 schema 不能无限膨胀；MCP 工具 schema 必须裁剪/规范化后再交给模型。

### 2.2 已知限制【本地核实】

- `ChatMessage::tool_result` 的 `content` 只能是 `String`，没有 `is_error` 字段。
- 多模态工具结果（图片、资源）需要 theta 侧拍平为文本，或定义自己的 `ToolResultContent` 再转换。
- `finalize_streamed_tool_calls_with_repair` 在缺少函数名或非法 JSON 时会返回错误；agent 层应把它转换成 tool error observation，而不是终止 turn。

### 2.3 生产实现的共同规则【外部核实，pinned source】

参考实现：

- `openai/codex` @ `b1573f102413`（rust-v0.169.0）
- `aaif-goose/goose` @ `2365bc5e898a`（v1.59.1）
- `zed-industries/zed` @ `f0fa51bb510f`（v1.28.1）

所有三个项目都遵守：

1. **一个 tool call 必须有一个 result**，包括取消、拒绝审批、未知工具、参数解析失败。
2. **工具错误是给模型的数据**；只有总线、数据库、运行时错误才终止 turn。
3. **工具可以并发执行，但历史必须按固定顺序写回**。
4. **取消时给未完成的 call 合成一个错误 result**，不能让 `tool_use` 悬空。
5. **参数在 wire 上是字符串**，只在流结束后解析一次；解析失败附带原始参数作为 tool error。
6. **`call_id` 是幂等键**，重放历史时不能重新生成。
7. **大输出必须截断或落盘**，事件里只放摘要和引用。

【设计约束】theta 的 ToolExecutor 必须实现上述语义；不能把 tool error 和 fatal error 混成同一个 `anyhow::Error`。

### 2.4 建议的 theta 事件流

```text
LlmRequest
  -> LlmNode
  -> chat_with_tools_stream
  -> StreamToolCallAccumulator
  -> LlmResponse / ToolCallRequested
  -> ToolExecutor / AgentActor
  -> ToolCallCompleted / ToolCallFailed
  -> AgentActor
```

- `ToolCallRequested { state_id, call_id, tool_name, arguments, kind, idempotency_key }`
- `ToolCallCompleted { state_id, call_id, content, structured, truncated, artifact_ref }`
- `ToolCallFailed { state_id, call_id, error, raw_arguments }`
- 第一版每个 turn 只处理一个工具调用。
- `Capability` 工具走 ToolExecutor；`Orchestration` 工具只由 AgentActor 在本侧执行。

---

## 3. MCP 接入

### 3.1 `rmcp` 现状【本地核实 + 外部核实】

| 项 | 值 |
|---|---|
| crate | `rmcp` / `rmcp-macros` |
| 最新版本 | 3.10.0（2026-11-12） |
| 官方仓库 | `modelcontextprotocol/rust-sdk` |
| MSRV | Rust 1.88 |
| License | Apache-2.0 |
| 默认 feature | `base64`、`macros`、`server`；**client 不是默认** |

客户端 feature：

```toml
rmcp = { version = "3.10", default-features = false, features = [
    "client",
    "transport-child-process",
    "transport-streamable-http-client-reqwest",
    "schemars",
] }
```

服务端 feature：

```toml
features = ["server", "transport-io", "transport-streamable-http-server", "schemars"]
```

【设计约束】README 中 `cargo add rmcp --features server` 的快速开始没有启用 `transport-io`，直接复制会导致 `stdio()` 不可用。

### 3.2 协议版本【外部核实】

- 最新规范：`2026-07-28`。
- 该版本是 **stateless**：
  - 移除 `initialize` / `notifications/initialized`
  - 每个请求在 `_meta` 中携带协议版本和客户端能力
  - 新增 `server/discover`
  - 结果需要 `resultType`
  - 移除 `ping`、HTTP GET stream、`logging/setLevel`
- 旧版本仍受支持：`2025-11-25`、`2025-06-18`、`2025-03-26`、`2024-11-05`。
- `rmcp` 中：
  - `serve()` 使用旧的 initialize 生命周期。
  - `ClientLifecycleMode::Auto` 会先探测 `server/discover`，失败后回退到 initialize（本地源码已核实该枚举存在）。

【设计约束】MCP client 应使用 `ClientLifecycleMode::Auto`，否则无法兼容大量仍在使用 `2025-06-18` / `2025-11-25` 的服务器。

### 3.3 Client API【本地核实 + 官方示例】

```rust
use rmcp::{ServiceExt, transport::{TokioChildProcess, ConfigureCommandExt}};
use tokio::process::Command;

let client = ().serve(TokioChildProcess::new(
    Command::new("npx").configure(|cmd| {
        cmd.arg("-y").arg("@modelcontextprotocol/server-everything");
    })
)?).await?;

let tools = client.list_all_tools().await?;

let result = client.call_tool(
    CallToolRequestParams::new("echo")
        .with_arguments(object!({ "message": "hi" })),
).await?;
```

服务端通知：

- 实现 `ClientHandler`
- 可用回调包括 `on_progress`、`on_cancelled`、`on_tool_list_changed`、`on_logging_message` 等
- `tools/list_changed` 在 `2026-07-28` 中通过 `subscriptions/listen` 传递

超时/取消：

- `call_tool()` 没有 timeout 参数；使用 `tokio::time::timeout(...)` 包裹
- 丢弃 future 时 `rmcp` 会尽力发送 `notifications/cancelled`
- 也可使用 `peer.send_cancellable_request(...)` 传入 `PeerRequestOptions`

### 3.4 映射到 theta【设计约束】

```text
McpManager
  ├─ 每个 server 一个 rmcp RunningService / Peer
  ├─ 启动时 list_all_tools()
  ├─ rmcp::model::Tool -> theta ToolDescriptor
  │    kind = Capability
  │    backend = Mcp { server_id, remote_name }
  │    input_schema = tool.input_schema
  │    risk / requires_approval = annotations + 本地策略
  ├─ tools/list_changed -> 重新发现 -> ToolRegistryChanged
  └─ ToolExecutor 调用 -> mcp.call_tool(...)
```

- 工具命名建议：`mcp__<server>__<tool>`。
- MCP 工具名允许 `.` 等字符，但 OpenAI function name 限制更严；必须维护双向映射和名称清洗。
- 不同 server 的同名工具必须隔离。
- `CallToolResult` 映射：
  - `content: Vec<ContentBlock>` → theta 的工具结果内容
  - `structured_content: Option<Value>` → 结构化结果
  - `is_error: Option<bool>` → 错误标记
- 大文本/图片放 artifact store，事件里只放摘要。
- 编排工具永远不通过 MCP 暴露。

### 3.5 安全结论【外部核实】

- stdio MCP server 与客户端共享同一信任域、同一 OS 权限，**不是沙箱**。
- 工具描述是不可信输入，存在 tool poisoning、tool shadowing、rug pull。
- `annotations` 只是 hint，`readOnlyHint` 不可信；缺失时按可能破坏性处理。
- MCP 不是安全边界；命令执行、文件写入仍要经过 theta 的权限、审批、沙箱。
- HTTP 传输要求 OAuth 2.1 + audience binding；禁止 token passthrough。
- 本地 server 应只绑定 `127.0.0.1`，校验 `Origin`。

### 3.6 版本风险【外部核实】

- 3.10.0 于 2026-11-12 发布，距调研仅两天。
- 3.x 发布节奏很快：3.5.0 到 3.10.0 约 6 周。
- 3.0.0 是大型 breaking release（对齐 `2026-07-28`）。
- 仓库仍有约 70 个 open issue。
- 生态实际使用落后：Goose pin `3.9.0`，hyper-mcp 仍在 `2.1`。

【设计约束】MCP 依赖应 pin 精确版本，并先做兼容性 spike。

---

## 4. Checkpoint

### 4.1 必须区分两类 checkpoint【设计约束】

| 类型 | 保存什么 | 不应做什么 |
|---|---|---|
| Agent/conversation checkpoint | 消息、pending calls、审批、预算、上下文摘要 | 不要把整个 workspace 塞进去 |
| Workspace checkpoint | 文件内容或 diff | 不要每 turn 全量复制 |

### 4.2 真实项目做法【外部核实，pinned source】

| 项目 | 会话/状态持久化 | Workspace 持久化 |
|---|---|---|
| Codex | 每 session 一个 JSONL rollout；SQLite 做元数据索引 | 默认不存整 workspace |
| Goose | SQLite `sessions.db`（WAL） | 默认不存 |
| Zed | SQLite `threads.db`，zstd 压缩 JSON BLOB | 默认不存 |
| OpenHands | `<conversation>/base_state.json` + 每事件一个 JSON 文件 | 工作目录本身就是 workspace |
| Aider | 无 DB，git 历史即记录 | 每次编辑自动 git commit |

结论：

- 会话状态进数据库是主流做法。
- Workspace 快照不是默认能力；Aider 的 git commit 是最接近“可恢复 workspace”的参考。
- 不要把 workspace 全量塞进 SQLite。

### 4.3 第一版建议【设计约束】

- 只做 agent 状态 checkpoint。
- 表：`agent_checkpoints`，包含：
  - `checkpoint_id`
  - `state_id`
  - `sequence`
  - `kind`
  - `state_json`（可压缩）
  - `last_event_hash`
  - `workspace_ref`（先留空或只记 `git_head`）
- 触发点：
  - turn 结束
  - 危险工具执行前（safety）
  - 最终回答 / 失败 / 退出前
- 每个写文件/命令操作写入 `tool_executions` 账本：
  - `started` / `succeeded` / `failed` / `unknown`
  - 输入 hash
  - 旧/新内容 hash
  - 命令、cwd、退出码
  - 幂等键

恢复规则：

- pending approval：重新询问，绝不自动批准。
- 有 `started` 无结果的写操作：标记 `Unknown`，等待人工确认或幂等查询。
- 只读工具：可以重试。
- LLM 请求：优先使用录制响应；没有录制再重试。
- workspace hash 不匹配：checkpoint 标记无效。

### 4.4 Workspace 快照（第二阶段）【本地/外部核实】

可选方案：

1. `git stash create`：生成 dangling commit，不改变工作区；只覆盖 tracked 文件。
2. 临时 index + `git write-tree`：
   - `GIT_INDEX_FILE=/tmp/...`
   - `git read-tree HEAD`
   - `git add -A`
   - `git write-tree`
   - 不污染真实 index/worktree，能覆盖 tracked + untracked（遵守 `.gitignore`）。
3. `git worktree`：适合评估/分支，每个 run 独立工作区，共享 object DB。
4. overlayfs / 容器：只在评估隔离场景使用；Linux-only，需要 mount 权限，inode 行为有坑。

【设计约束】不要把 `target/`、`.git/` 等目录纳入 workspace 快照；必须先定义 include/exclude 规则。

### 4.5 可复用的 checkpoint 实现调研（2026-11-19）【外部核实】

先区分同名包：

- PyPI 上的 `langgraph-checkpoint`、`langgraph-checkpoint-sqlite`、`langgraph-checkpoint-postgres` 是 LangChain 官方的 **Python** 包，Rust 不能直接使用。
- crates.io 上也有同名 `langgraph-checkpoint` / `langgraph-checkpoint-sqlite` / `langgraph-checkpoint-postgres` 0.1.0，但它们是社区 Rust 移植（仓库 `CYXCAT/langgraph-rust`），和官方 Python 包没有关系。
- crates.io 还有 `langgraph` 0.2.5（`Onelevenvy/langgraph-rust`）和 `rust-langgraph` 0.1.1（`kareem2002-k/rust-langgraph`），同样是独立社区项目，不是 LangChain 官方实现。

Rust 生态现状：

| 方案 | 版本 | 能提供 | 局限 |
|---|---|---|---|
| `langgraph-checkpoint` + sqlite/postgres | 0.1.0 | 同步 `CheckpointSaver`；`Checkpoint{thread_id, checkpoint_id, state, pending_writes}`；InMemory / SQLite / Postgres | 极新；绑定 `langgraph_core::StatePatch`；无 parent/fork、workspace、artifact、事件 hash；SQLite schema 只有 `thread_id/checkpoint_id/payload/created_at` |
| `rust-langgraph` | 0.1.1 | 异步 `BaseCheckpointSaver`、`CheckpointTuple`、`MemorySaver` | SQLite/Postgres 后端仍是 stub；绑定其 graph/Pregel 模型 |
| `agentplane` | 0.61.0 | durable journal、strict replay、redb/Postgres、hash chain | pre-alpha；完整 runtime；其 Checkpoint 是 Merkle 透明日志检查点，不是 AgentState 快照；采用它等于替换现有 Dispatcher/AgentActor |
| `yoagent-state` | 0.5.3 | `EventStore` + `SnapshotStore` + `ForkStore` + `ArtifactStore`、JSONL/git 持久化、lineage/fork | 领域模型是 goal/task/patch/eval 图，不是通用 AgentState；trait 拆分可借鉴 |
| `durare` / `dbos` | 0.5.0 / 0.6.0 | DBOS 风格 step checkpoint、崩溃恢复、durable timer/queue；SQLite/Postgres | 解决工作流 step 重放，不是 AgentState/workspace 快照；与事件驱动 Actor 模型耦合成本高 |
| `eventastic` / `esrs` / `eventually` | 0.5.0 / 0.18.0 / 0.4.0 | 事件溯源、aggregate、事务、幂等、部分 snapshot | 面向 CQRS/aggregate，不具备 agent checkpoint、workspace、审批语义 |
| `aion-store` | 0.31.0 | durable workflow 的事件存储契约 | AGPL-3.0-only，许可不适合当前项目 |
| Restate / Temporal | `restate-sdk` 0.13.0 / `temporal-sdk-core` alpha | 通用 durable execution | 需要服务端或确定性工作流模型，第一版过重 |

结论：

- **没有可直接采用的完整实现**；第一版仍按 [`checkpoint-evaluation.md`](./checkpoint-evaluation.md) 的 `CheckpointStore` / `WorkspaceSnapshotter` 自行实现，先做内存实现，再按 5.1 的选型接 SQLite。
- 可借鉴 `langgraph-checkpoint` 的 `thread_id/checkpoint_id/parent_id/pending_writes` 字段设计，以及 `yoagent-state` 的 `EventStore/SnapshotStore/ForkStore/ArtifactStore` 拆分。
- 不直接依赖 graph 框架或完整 agent runtime；它们会替换现有事件模型，且版本过新。
- `AgentState` 需要补 `serde` derive；`ChatMessage` 已实现 `Serialize/Deserialize`。事件链与 artifact hash 可用 `blake3`。

---

## 5. 数据库持久化与事务

### 5.1 选型【本地核实 + 外部核实】

| 维度 | sqlx 0.9.0 | rusqlite 0.42.0 |
|---|---|---|
| 模型 | async SQL toolkit | 同步 SQLite 封装 |
| MSRV | 1.94.0 | 1.89.0 |
| 连接池 | 内置 | 需要 r2d2/deadpool |
| 迁移 | `sqlx::migrate!` + CLI + 校验和 | 需要 rusqlite_migration/refinery |
| 编译期检查 | `query!` + offline `.sqlx` | 无 |
| bundled SQLite | 3.51.3（`libsqlite3-sys 0.37.0`） | 3.54.1 |
| 适合 | tokio 项目 | 能接受阻塞封装 |

结论：**theta 第一版选 sqlx 0.9 + SQLite**。tokio 项目、需要迁移和连接池，sqlx 更合适；工具链 1.98.1 满足 MSRV。

### 5.2 SQLite 配置【本地核实】

sqlx 的 `SqliteConnectOptions::new()`：

- `journal_mode` **不设置** → SQLite 默认 `DELETE`
- `foreign_keys = ON`（sqlx 特意与 SQLite 默认相反）
- `synchronous` 不设置 → SQLite 默认 `FULL`
- `busy_timeout` 默认 5s
- `PoolOptions` 默认 `max_connections = 10`

推荐配置：

```rust
let opts = SqliteConnectOptions::new()
    .filename("theta.db")
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .synchronous(SqliteSynchronous::Normal)
    .busy_timeout(Duration::from_secs(5))
    .foreign_keys(true);

let pool = SqlitePoolOptions::new()
    .max_connections(1)
    .connect_with(opts)
    .await?;
```

规则：

- 所有写事务使用 `BEGIN IMMEDIATE`。
- 默认 deferred 的读→写升级可能返回 `SQLITE_BUSY_SNAPSHOT (517)`。
- `BEGIN IMMEDIATE` 自身可能 `SQLITE_BUSY`，但拿到写锁后直到 COMMIT 不会中途失败。
- 写事务中不要跨 await 持有事务；不要在其中发布事件、调用 LLM、执行命令。
- WAL + `synchronous=NORMAL`：应用崩溃安全；断电可能丢最后几个已提交事务。
- DB 放在 ext4 / WSL 原生路径；不要放 `/mnt/c`（9P/DrvFS 锁不可靠）。
- sqlx 0.9 已提供 `Pool::begin_with("BEGIN IMMEDIATE")`（本地源码已核实）。

### 5.3 Schema 草案【已确认可映射到 SQLite】

```sql
PRAGMA foreign_keys = ON;

CREATE TABLE agent_states (
    state_id        TEXT PRIMARY KEY,
    parent_state_id TEXT,
    version         INTEGER NOT NULL,
    last_sequence   INTEGER NOT NULL,
    status          TEXT NOT NULL,
    state_json      TEXT NOT NULL,
    updated_at_ms   INTEGER NOT NULL
) STRICT;

CREATE TABLE agent_events (
    event_id       TEXT PRIMARY KEY,
    state_id       TEXT NOT NULL,
    sequence       INTEGER NOT NULL,
    correlation_id TEXT,
    causation_id   TEXT,
    kind           TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    UNIQUE (state_id, sequence)
) STRICT;

CREATE TABLE event_outbox (
    event_id        TEXT PRIMARY KEY,
    state_id        TEXT NOT NULL,
    kind            TEXT NOT NULL,
    payload_json    TEXT NOT NULL,
    created_at_ms   INTEGER NOT NULL,
    published_at_ms INTEGER
) STRICT;

CREATE TABLE processed_messages (
    subscriber_id   TEXT NOT NULL,
    message_id      TEXT NOT NULL,
    processed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (subscriber_id, message_id)
) STRICT;

CREATE TABLE tool_executions (
    state_id        TEXT NOT NULL,
    call_id         TEXT NOT NULL,
    tool_name       TEXT NOT NULL,
    status          TEXT NOT NULL,
    input_hash      TEXT NOT NULL,
    output_ref      TEXT,
    side_effect_ref TEXT,
    updated_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (state_id, call_id)
) STRICT;

CREATE TABLE agent_checkpoints (
    checkpoint_id        TEXT PRIMARY KEY,
    state_id             TEXT NOT NULL,
    parent_checkpoint_id TEXT,
    sequence             INTEGER NOT NULL,
    kind                 TEXT NOT NULL,
    state_json           TEXT NOT NULL,
    workspace_ref        TEXT,
    last_event_hash      TEXT NOT NULL,
    created_at_ms        INTEGER NOT NULL,
    UNIQUE (state_id, sequence, kind)
) STRICT;
```

`STRICT` 需要 SQLite 3.37+；当前 bundled 版本满足。

### 5.4 事务序列【设计约束】

```text
Actor 收到事件
  -> reduce(state, event) 得到新状态 + Effects（纯计算）
  -> BEGIN IMMEDIATE
       -> UPDATE agent_states
             SET version = version + 1, state_json = ?, last_sequence = ?
           WHERE state_id = ? AND version = ?
          rows_affected != 1 -> rollback + reload/retry
       -> INSERT agent_events
       -> INSERT event_outbox
       -> 可选：INSERT agent_checkpoints
  -> COMMIT
  -> outbox worker 发布事件
  -> 执行 Effects
```

- `rows_affected() == 0` 是 CAS 失败，不是 `SQLITE_BUSY`。
- CAS 失败不能拿同一个 expected version 盲目重试。
- `SQLITE_BUSY`（5 / 261 / 517 / 773）才做退避重试。
- 事务提交时，状态、事件、outbox 必须同时可见。

### 5.5 Outbox 与幂等【设计约束】

顺序必须是 **先 publish，再 mark published**：

```text
SELECT ... FROM event_outbox WHERE published_at_ms IS NULL ORDER BY rowid LIMIT N
  -> publish
  -> UPDATE event_outbox SET published_at_ms = ? WHERE event_id = ? AND published_at_ms IS NULL
```

- 发布后崩溃 → 重复发布 → 消费者去重。
- 先 mark 后发布 → 崩溃会丢事件。
- 事件总线是 at-least-once，不提供 exactly-once。
- 消费者使用 `processed_messages` 去重。
- `tool_executions` 的 `(state_id, call_id)` 是工具副作用的幂等键。

### 5.6 当前 `Dispatcher` 的结构性约束【本地核实】

1. **按 `TypeId` 路由**
   - `run_worker` 使用 `event.event_type_id()` 查找订阅者，并 downcast 回具体类型。
   - 引入统一 `EventEnvelope` 后，如果总线只传 envelope，`TypeId` 路由会退化为“所有订阅者收到同一种类型”。

2. **单个 worker，串行路由**
   - `run_worker` 对每个订阅者执行 `sender.send(event).await`。
   - 任何一个订阅者通道满，都会阻塞整个 worker，导致其他订阅者也停止收到事件。
   - 这是**全局 head-of-line blocking**。

3. **订阅者内部串行**
   - `run_subscriber` 对每个事件 `tokio::spawn(consume)` 后 `await`。
   - 单个订阅者同一时刻只处理一个事件。

4. **总线不持久化**
   - 无订阅者的事件直接丢弃。
   - 消费失败进入有界死信队列，满了丢弃最旧。
   - 没有自动重试。
   - 进程崩溃时，队列中的事件全部丢失。

【设计约束】

- `LlmNode` / `ToolExecutor` 的 `consume()` 必须快速返回；长任务放到内部任务池或专用队列，完成后把结果作为新事件发布。
- 不能把 `Dispatcher` 当持久化队列；durability 必须由 DB + outbox 提供。
- 不能把 `flush()` 当作“已落库”的信号；它只代表内存队列排空。
- 事件路由有三个可选方案：
  - A. 总线只传 `AgentEvent` 枚举，订阅者 match 变体（改动最小）。
  - B. Dispatcher 增加 topic 路由（按稳定字符串）。
  - C. Supervisor 按 `state_id` 转发到每个 Actor 的 mpsc。
- 第一版建议 A；多 agent 时再加 C。

---

## 6. 未确认事项

本文只记录已确认事实。尚未决定的设计选择、验证实验和决策记录模板，见 [待决策清单](./open-decisions.md)。

---

## 7. 来源

### 本地源码 / 依赖

- `src/core/dispatcher.rs`
- `src/core/dead_letter.rs`
- `src/domain/event.rs`
- `~/.cargo/registry/.../edgequake-llm-0.10.10/src/traits.rs`
- `~/.cargo/registry/.../edgequake-llm-0.10.10/src/stream_tool_calls.rs`
- `~/.cargo/registry/.../edgequake-llm-0.10.10/src/stream_tools.rs`
- `~/.cargo/registry/.../edgequake-llm-0.10.10/src/providers/{anthropic,gemini,schema_utils}.rs`
- `~/.cargo/registry/.../rmcp-3.10.0/src/{service/client.rs,model/tool.rs,model/content.rs}`
- `~/.cargo/registry/.../sqlx-0.9.0`, `sqlx-core-0.9.0`, `sqlx-sqlite-0.9.0`

### 外部资料

**MCP / rmcp**

- https://github.com/modelcontextprotocol/rust-sdk
- https://github.com/modelcontextprotocol/rust-sdk/blob/rmcp-v3.10.0/README.md
- https://modelcontextprotocol.io/specification/2026-07-28/changelog
- https://modelcontextprotocol.io/specification/2026-07-28/basic
- https://modelcontextprotocol.io/specification/2026-07-28/server/tools
- https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/security_best_practices
- https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/local-server-security
- https://crates.io/crates/rmcp

**Tool loop 参考实现**

- https://github.com/openai/codex/blob/b1573f102413/codex-rs/core/src/session/turn.rs
- https://github.com/openai/codex/blob/b1573f102413/codex-rs/core/src/tools/registry.rs
- https://github.com/openai/codex/blob/b1573f102413/codex-rs/core/src/tools/parallel.rs
- https://github.com/openai/codex/blob/b1573f102413/codex-rs/core/src/exec_policy.rs
- https://github.com/aaif-goose/goose/blob/2365bc5e898a/crates/goose/src/agents/state_machine/ops_toolcalling.rs
- https://github.com/aaif-goose/goose/blob/2365bc5e898a/crates/goose-provider-types/src/formats/anthropic.rs
- https://github.com/aaif-goose/goose/blob/2365bc5e898a/crates/goose/src/agents/tool_schema_normalize.rs
- https://github.com/zed-industries/zed/blob/f0fa51bb510f/crates/agent/src/thread.rs
- https://github.com/zed-industries/zed/blob/f0fa51bb510f/crates/agent/src/tool_permissions.rs

**Checkpoint 实现**

- https://pypi.org/project/langgraph-checkpoint/
- https://pypi.org/project/langgraph-checkpoint-sqlite/
- https://pypi.org/project/langgraph-checkpoint-postgres/
- https://github.com/CYXCAT/langgraph-rust
- https://github.com/Onelevenvy/langgraph-rust
- https://github.com/kareem2002-k/rust-langgraph
- https://github.com/hupe1980/agentplane
- https://github.com/yologdev/yoagent-state
- https://github.com/SamuelXing/durare
- https://github.com/dbos-inc/dbos-transact-rust
- https://github.com/primait/event_sourcing.rs
- https://github.com/jdon/eventastic
- https://github.com/ablative-io/aion
- https://github.com/restatedev/sdk-rust

**SQLite / sqlx / rusqlite**

- https://sqlite.org/wal.html
- https://sqlite.org/lang_transaction.html
- https://sqlite.org/rescode.html
- https://sqlite.org/pragma.html
- https://sqlite.org/inmemorydb.html
- https://sqlite.org/useovernet.html
- https://crates.io/crates/sqlx
- https://github.com/transact-rs/sqlx
- https://docs.rs/sqlx/0.9.0/sqlx/
- https://crates.io/crates/rusqlite
- https://github.com/rusqlite/rusqlite

**Outbox / 幂等**

- https://microservices.io/patterns/data/transactional-outbox.html
- https://microservices.io/patterns/communication-style/idempotent-consumer.html

**Checkpoint / 持久化参考**

- https://github.com/openai/codex/blob/main/codex-rs/rollout/src/lib.rs
- https://github.com/openai/codex/blob/main/codex-rs/state/src/sqlite.rs
- https://github.com/aaif-goose/goose/blob/main/crates/goose/src/session/session_manager.rs
- https://github.com/zed-industries/zed/blob/main/crates/agent/src/db.rs
- https://docs.openhands.dev/sdk/guides/convo-persistence
- https://aider.chat/docs/git.html
- https://git-scm.com/docs/git-worktree
- https://git-scm.com/docs/git-stash
- https://docs.kernel.org/filesystems/overlayfs.html
