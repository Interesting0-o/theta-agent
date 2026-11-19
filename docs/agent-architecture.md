# Coding Agent 架构设计（草案）

状态：设计草案，尚未实现。

相关文档：[Command 系统设计](./command-system.md)、[Checkpoint 与评估设计](./checkpoint-evaluation.md)、[最小 ReAct Agent 与审核链路](./minimal-react-agent.md)。

已确认的基础设施事实：[基础设施调研结论](./infrastructure-findings.md)。

未确定的设计选择：[待决策清单](./open-decisions.md)。

## 1. 目标与总体思路

theta 的 coding agent 需要覆盖：

- 文件编辑；
- 命令执行；
- 上下文工程（上下文组装、压缩、记忆）；
- MCP 接入；
- Skill 插件载入；
- 多 agent 协作。

编排基于现有事件系统：

- 事件总线负责路由、排队、背压和死信；
- 每个能力节点既可以是事件消费者，也可以是事件生产者；
- LLM 节点消费已组装的上下文，生产文本增量、最终回复和工具调用请求；
- 工具节点消费工具调用请求，生产工具结果；
- 多 agent 通过独立状态和路由层隔离，而不是共享可变状态。

事件系统只负责“传递发生了什么”，不负责“当前状态是什么”。状态由明确的 owner 管理，这是本设计的核心原则。

关键决策：

- **状态由数据库管理**：`AgentState` 显式携带 `state_id`，事件信封也必须携带同一个 `state_id`。
- **Agent Actor 是唯一状态写者**：状态和事件在同一数据库事务中提交，通过 `version` 做乐观锁。
- **工具分为普通工具（能力工具）和编排工具**：普通工具扩展外部能力，可以走本侧沙箱或 MCP；编排工具只操作 agent 自身状态和控制流，必须留在本侧。
- **MCP 只承载能力工具**：MCP 是部署边界，不是安全边界；命令行等高风险工具仍需统一的权限、审批和沙箱策略。

## 2. 节点模型

### 2.1 能力节点

| 节点 | 消费 | 生产 | 持有的状态 |
|---|---|---|---|
| LLM 节点 | `LlmRequest` / `ContextBuilt` | `LlmDelta`、`LlmResponse`、`ToolCallRequested` | 模型客户端配置、请求级缓存；不持有会话状态 |
| 能力工具执行器 | `ToolCallRequested`（能力工具） | `ToolCallCompleted`、`ToolCallFailed` | 进行中的调用、幂等键、副作用账本 |
| 编辑器 | `FileEditRequested` | `FileEditCompleted`、`FileEditFailed` | 文件锁、修改记录、回滚信息 |
| 命令执行器 | `CommandExecRequested` | `CommandOutput`、`CommandExited` | 进程表、stdout/stderr 流、取消句柄 |
| 上下文组装器 | `AgentStateChanged`、`MemoryUpdated` | `LlmRequest` | 上下文缓存、token 预算、压缩摘要 |
| Agent Actor | `AgentEvent`（含编排工具调用） | `Effect` 对应的事件 | 该 agent 的完整会话状态 |
| Supervisor | `AgentEvent`（带 `state_id`） | 转发到 agent mailbox | agent 路由表、父子关系 |
| MCP 管理器 | 生命周期事件 | `ToolRegistered` 等 | 连接、能力发现结果 |
| Skill 加载器 | 启动/激活事件 | `ToolRegistered`、`CommandRegistered` 等 | skill manifest、激活状态 |

编辑器和命令执行器可以视为特殊的能力工具，但保留独立事件类型，便于审计、权限控制和回放策略。

编排工具不经过外部工具执行器，而是由 Agent Actor 在本侧处理，因为它们要修改 `AgentState` 或控制 agent 的决策流程。这一点见 2.3 节。

### 2.2 LLM：生产者兼消费者

```text
ContextBuilt / LlmRequest
        |
        v
     LLM 节点  ---> LlmDelta（流式增量）
        |
        +-------> LlmResponse（完整回复）
        |
        +-------> ToolCallRequested（一个或多个工具调用）
```

LLM 节点本身不保存会话历史。它只根据请求中携带的上下文工作，结果通过事件返回。这样可以：

- 多个 agent 共享同一个 LLM 客户端；
- LLM 调用可以被记录、重放和替换；
- 会话状态只存在于 Agent Actor 中。

### 2.3 工具分类：普通工具（能力工具）与编排工具

建议用“是否扩展 agent 的外部能力”作为主分类：

| 类型 | 判定 | 例子 | 执行位置 | 状态归属 |
|---|---|---|---|---|
| 能力工具（普通工具） | 改变外部世界，扩展 agent 可执行能力 | 文件/目录 CRUD、命令执行、HTTP、MCP 工具 | 本侧沙箱 / MCP | 副作用账本 |
| 编排工具 | 不扩展外部能力，只改变 agent 自身状态或控制流 | step CRUD、长期记忆写入、askuser、自启动实验模式 | Agent 本侧 | AgentState / DB |

这个分类是路由和一致性边界，但不是唯一维度。风险等级、执行后端、是否需要审批、是否流式输出，应该是独立的元数据：

```rust
pub enum ToolKind {
    Capability,
    Orchestration,
}

pub enum ToolBackend {
    InProcess,
    Sandbox,
    Mcp,
}
```

关键规则：

- **编排工具必须由 Agent Actor 在单写者路径中执行**，不能通过 MCP 或独立消费者直接改状态，否则会破坏状态机不变量和数据库事务一致性。
- **能力工具**可以是进程内执行器、沙箱执行器或 MCP 服务。MCP 只是部署形态，不等于安全边界；权限、审批、超时、输出限制和审计仍由执行器统一负责。
- **命令行工具属于能力工具，但风险最高**：必须在沙箱中执行，限制 cwd、环境变量、超时、输出大小和进程树，并记录完整审计。不能因为走 MCP 就把安全责任全部交给远端。
- **文件/目录 CRUD 属于能力工具**。如果编辑器需要原子写、回滚、路径沙箱和冲突检测，建议保留本侧实现，或者要求 MCP 服务提供等价保证。
- **长期记忆写入**改变的是 agent 内部上下文/记忆状态，归为编排工具；记忆检索可以作为能力工具或上下文工程的一部分。
- **askuser** 是控制流操作，不是外部能力，应作为编排工具：它挂起当前 turn、保存 pending 状态，收到用户回复后恢复。
- **自启动实验模式**改变运行时编排策略，也属于 agent 本侧状态。

### 2.4 能力工具节点：消费者兼生产者

能力工具节点消费 `ToolCallRequested`，执行后生产 `ToolCallCompleted` 或 `ToolCallFailed`。

建议不要为每个工具注册一个消费者并让它们自行过滤，而是：

1. 所有能力工具注册为 `ToolDescriptor`，进入 `ToolRegistry`；
2. `ToolExecutor` 作为唯一消费者接收所有能力工具调用；
3. 按 `tool_name` 查找 descriptor，执行对应 handler；
4. 结果带上原始的 `call_id` / `correlation_id` 返回。

这样工具执行、超时、取消、权限、幂等和审计都可以统一处理。

工具注册支持两种方式：

```rust
registry.add_tool(ToolDescriptor {
    name: "new_file",
    description: "Create a new file",
    kind: ToolKind::Capability,
    backend: ToolBackend::Sandbox,
    input_schema: json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "content": { "type": "string" }
        },
        "required": ["path", "content"]
    }),
    handler: Arc::new(new_file_handler),
});
```

或者使用属性宏：

```rust
#[tool("new_file", kind = capability, backend = sandbox, description = "Create a new file")]
pub async fn new_file(
    ctx: &ToolContext,
    args: NewFileArgs,
) -> Result<ToolOutput, BoxError> {
    // ...
}
```

显式注入中的 `"Args:\n path:地址\n content:文件的内容"` 可以作为给 LLM 的自然语言说明，但工具调用参数校验仍应使用结构化 JSON Schema，而不是解析这段文本。

### 2.5 编排工具：Agent 本侧状态操作

编排工具可以由 LLM 提出，也可以由 AgentActor/系统触发，但都不走外部工具执行器：

```text
LLM
  -> ToolCallRequested { kind: Orchestration, ... }
  -> AgentActor
  -> reducer + 状态事务
  -> 结果事件
```

规则：

- 不进入 `ToolExecutor`，也不允许导出为 MCP 工具；
- 状态修改与事件追加在同一个数据库事务中完成，失败整体回滚；
- AgentActor 要校验操作是否合法，不能让 LLM 随意构造状态；
- step 的创建、更新、清除，长期记忆写入，askuser，自启动自动化实验模式，都建模为受控的 `AgentEvent` / `Effect`；
- askuser 会产生 pending 状态，actor 挂起等待用户回复，恢复后继续同一个 `state_id` 的流程；
- 编排工具可以出现在同一个工具目录里供 LLM 选择，但执行时必须按 `ToolKind` 分流。

## 3. 事件与信封

当前事件系统只传递 `Arc<dyn Event>`。Agent 场景需要更多元数据，建议引入事件信封：

```rust
pub struct EventEnvelope {
    pub event_id: EventId,
    pub scope: EventScope,
    pub correlation_id: Option<EventId>,
    pub causation_id: Option<EventId>,
    pub sequence: u64,
    pub timestamp: SystemTime,
    pub payload: Arc<dyn Event>,
}

pub struct EventScope {
    pub state_id: AgentStateId,
    pub parent_state_id: Option<AgentStateId>,
    pub run_id: RunId,
}
```

关键字段：

- `event_id`：事件唯一标识，用于去重和审计；
- `scope.state_id`：**必填**，唯一标识一份 AgentState 会话，也是数据库主键和多 agent 路由键；
- `parent_state_id`：父子 agent 关系，用于追踪子 agent 的产物和副作用；
- `run_id`：一次执行尝试的标识，同一个 `state_id` 可能有多次 run；
- `correlation_id`：把请求和结果关联起来，例如 `ToolCallRequested` 与 `ToolCallCompleted`；
- `causation_id`：直接原因事件，用于构建因果链；
- `sequence`：同一 `state_id` 内的单调序号，用于检测乱序和重复；
- `payload`：具体业务事件。

建议约定：

- **不允许存在没有 `state_id` 的事件**；`state_id` 放在信封的 scope 中，而不是让每个业务 payload 自己带；
- `AgentState` 本身也必须有一个显式的 `state_id` 字段，且与事件信封中的值一致；
- 所有请求/响应型事件必须携带 `correlation_id`；
- 工具结果必须携带原始 `call_id`；
- 跨 agent 消息必须明确 `from_state_id` / `to_state_id`；
- 事件名称使用稳定字符串和版本号，不依赖 `type_name` 做持久化；
- 数据库事件表使用 `(state_id, sequence)` 唯一约束，`event_id` 全局唯一。

## 4. 多 Agent 编排

### 4.1 状态隔离原则

- 每个 agent 有且只有一个状态 owner：`AgentActor`。
- 其他节点不能直接修改 agent 状态，只能通过事件影响它。
- 多 agent 之间不共享可变内存；共享通过事件、消息或带版本号的 artifact 引用完成。
- Supervisor 只持有路由表和生命周期信息，不持有 agent 会话状态。

### 4.2 路由方式

有两种可选方案。

**方案 A：一个全局事件总线 + Supervisor 路由（推荐）**

```text
publish
  -> 全局事件队列
  -> Supervisor（消费 EventEnvelope）
  -> 按 state_id 转发到每个 AgentActor 的 mpsc
  -> AgentActor 串行处理自己的 AgentEvent
```

优点：

- 所有事件集中审计、持久化、回放；
- 多 agent 共享同一套工具、LLM、MCP 节点；
- 每个 agent 仍保持串行状态机，互不阻塞。

成本：

- 需要新增带 `state_id` 的路由层；
- 当前 `Dispatcher` 只按 `TypeId` 路由，不能直接按 agent 分区，需要 Supervisor 或扩展 Dispatcher 支持 key 路由。

**方案 B：每个 agent 一个 Dispatcher**

每个 agent 创建独立的事件队列和消费者集合，隔离更彻底，但跨 agent 事件、统一审计和共享工具会更麻烦。第一版如果只做单 agent，可以先用 B；多 agent 稳定后再转向 A。

### 4.3 父子 agent

- `parent_state_id` 写入子 agent 的所有事件；
- 子 agent 完成时发布 `AgentCompleted`，父 agent 通过 `correlation_id` 关联；
- 父 agent 可以等待、取消、汇总子 agent 结果；
- 子 agent 的文件修改和命令执行必须写入全局副作用账本，避免父 agent 不知情。

## 5. 状态管理（重点）

### 5.1 状态分层

不要把所有东西塞进一个 `AgentState`。按生命周期和一致性要求分层：

| 层 | 内容 | Owner | 生命周期 | 是否可重建 |
|---|---|---|---|---|
| 事件日志 | 所有已发生的业务事件 | StateStore / EventStore | 持久化 | 是事实来源 |
| Agent 状态 | 消息、计划、待处理调用、上下文摘要 | AgentActor 写入，StateStore 持久化 | 持久化 | 可从事件日志重建 |
| 快照 | Agent 状态在某个 sequence 的投影 | StateStore | 持久化 | 可由日志重建 |
| 运行时队列 | 事件队列、订阅者 mailbox、pending 计数 | Dispatcher | 进程内 | 不需要 |
| 待处理请求 | LLM/工具调用的 in-flight 表 | AgentActor / ToolExecutor | 调用期间 | 需要恢复逻辑 |
| 副作用账本 | 文件修改、命令执行、MCP 写入 | ToolExecutor / 编辑器 / 命令执行器 | 持久化 | 不可重建，必须记录 |
| 上下文/记忆索引 | 向量索引、摘要、检索缓存 | ContextStore | 持久化 | 可重建 |
| 能力注册表 | 工具、命令、skill、MCP 能力 | ToolRegistry / CommandRegistry | 启动时加载 | 可重建 |

核心原则：

1. **数据库是状态的唯一权威**，内存中的 `AgentState` 只是工作副本/缓存。
2. **事件日志和状态行在同一个数据库事务中更新**，避免事件已记录但状态未提交的中间态。
3. **Agent 状态只能由 AgentActor 通过状态仓储写入**，其他节点不能直接改数据库里的 agent 状态。
4. **工具副作用不能靠重放事件重建，必须单独记账。**
5. **LLM 的不确定性结果必须记录，重放时读取记录而不是重新调用模型。**

### 5.2 数据库模型与状态字段

`AgentState` 必须显式携带状态标识和版本号：

```rust
pub struct AgentState {
    pub state_id: AgentStateId,
    pub parent_state_id: Option<AgentStateId>,
    pub version: u64,
    pub last_sequence: u64,
    pub status: AgentStatus,
    // 业务字段...
}
```

`state_id` 同时出现在三处，且必须一致：

1. `AgentState::state_id`；
2. `EventEnvelope::scope.state_id`；
3. 数据库 `agent_states.state_id` / `agent_events.state_id`。

建议的最小表结构（SQLite/Postgres 均可映射）：

```sql
CREATE TABLE agent_states (
    state_id          TEXT PRIMARY KEY,
    parent_state_id   TEXT,
    version           BIGINT NOT NULL,
    last_sequence     BIGINT NOT NULL,
    status            TEXT NOT NULL,
    state_json        BLOB NOT NULL,
    updated_at        TIMESTAMP NOT NULL
);

CREATE TABLE agent_events (
    event_id          TEXT PRIMARY KEY,
    state_id          TEXT NOT NULL,
    sequence          BIGINT NOT NULL,
    correlation_id    TEXT,
    causation_id      TEXT,
    kind              TEXT NOT NULL,
    payload           BLOB NOT NULL,
    created_at        TIMESTAMP NOT NULL,
    UNIQUE (state_id, sequence)
);

CREATE TABLE event_outbox (
    event_id          TEXT PRIMARY KEY,
    state_id          TEXT NOT NULL,
    payload           BLOB NOT NULL,
    published_at      TIMESTAMP
);
```

写入流程：

```text
Actor 收到事件
  -> 开启数据库事务
  -> 校验 state_id + version（乐观锁）
  -> 追加 agent_events
  -> UPDATE agent_states SET state_json = ?, version = version + 1, last_sequence = ?
       WHERE state_id = ? AND version = ?
  -> 写入 event_outbox
  -> 提交事务
  -> outbox worker 把事件发布到 Dispatcher
```

要点：

- 使用 `WHERE state_id = ? AND version = ?` 做乐观并发控制；受影响行数为 0 表示状态已被其他写者更新，Actor 必须重新加载状态。
- 每个 `state_id` 同时只能有一个 Actor 写者，数据库层再加唯一版本校验作为兜底。
- 先提交数据库，再发布事件；`event_outbox` 保证崩溃后仍能把未发布事件补发到总线。
- 事件总线是 at-least-once，消费者需要用 `event_id` 或业务键做幂等。
- 本地 agent 优先使用 SQLite WAL；需要多进程/远程协同时再换 Postgres。
- `state_json` 是当前状态的物化结果；事件日志用于审计、回放和重建。两者在同一事务内更新，不会出现业务状态和事件日志互相矛盾。

### 5.3 Agent 状态机

推荐使用 `state + reducer + effects` 模型，而不是让多个消费者直接改共享状态：

```rust
pub struct AgentState {
    pub state_id: AgentStateId,
    pub parent_state_id: Option<AgentStateId>,
    pub version: u64,
    pub last_sequence: u64,
    pub status: AgentStatus,
    pub messages: Vec<Message>,
    pub plan: Vec<PlanStep>,
    pub pending_calls: HashMap<CallId, PendingCall>,
    pub context: ContextState,
    pub budget: BudgetState,
    pub files_touched: BTreeSet<PathBuf>,
}

pub enum AgentEvent {
    UserMessage(UserMessage),
    LlmDelta(LlmDelta),
    LlmResponse(LlmResponse),
    ToolCallRequested(ToolCallRequest),
    ToolCallCompleted(ToolCallResult),
    ToolCallFailed(ToolCallFailure),
    FileEditCompleted(FileEditResult),
    CommandExited(CommandResult),
    AgentMessageReceived(AgentMessage),
    Cancelled(CancelReason),
}

pub enum Effect {
    CallLlm(LlmRequest),
    InvokeTool(ToolCallRequest),
    Publish(AgentEvent),
    RequestApproval(ApprovalRequest),
    Checkpoint,
    Complete(AgentResult),
}
```

AgentActor 的处理循环：

```text
receive AgentEvent
  -> 校验 state_id/sequence/correlation，去重
  -> 调用 reduce(state, event) 得到新状态和 Effects（纯计算）
  -> 开启数据库事务
       -> 追加 agent_events
       -> CAS 更新 agent_states（state_json + version）
       -> 写入 event_outbox
  -> 提交事务
  -> outbox 发布事件到 Dispatcher
  -> 按顺序执行 Effects
```

关键点：

- 只有一个任务修改 `AgentState`，因此不需要给内存状态加锁；
- 数据库用 `version` 做乐观锁，Actor 写冲突时重新加载并重试或拒绝；
- reducer 是纯函数，便于单元测试；
- LLM、工具、文件、命令都通过 `Effect` 异步执行，不在 reducer 内做 I/O；
- 编排工具的状态修改也进入这个事务，不单独写数据库。

### 5.4 事件顺序与并发

- 每个 `state_id` 的 mailbox 串行处理，保证该 agent 的事件顺序；
- 不同 `state_id` 并行，不要求全局顺序；
- 同一 agent 可以同时存在多个 in-flight 工具调用，但结果通过 `call_id` 区分，不依赖返回顺序；
- 如果需要严格顺序，可以限制每个 agent 同时只有一个工具调用；
- 事件日志按 `(state_id, sequence)` 索引，跨 agent 的全局顺序只用于审计，不用于业务决策。

### 5.5 持久化、快照与恢复

`agent_states.state_json` 本身就是当前状态的物化结果，正常情况下启动时直接读取即可，不需要重放全部事件。历史快照、评估 checkpoint 和分支恢复的详细设计见 [Checkpoint 与评估设计](./checkpoint-evaluation.md)。

建议流程：

1. Actor 启动时从 `agent_states` 加载 `state_json`、`version`、`last_sequence`；
2. 每个事件在同一个事务里追加事件、更新状态行、写 outbox；
3. 如果需要历史快照，额外写 `agent_snapshots` 表，用于加速重建或时间旅行；
4. 如果 `state_json` 缺失或损坏，从最近快照开始重放其后的事件。

快照至少包含：

- `state_id`、`last_sequence`；
- `AgentState`；
- 未完成调用的状态；
- 工具副作用账本的引用。

恢复时必须处理“有请求、无结果”的中间状态：

- LLM 请求：可以安全重试或使用已记录的响应；
- 只读工具：可以重试；
- 写入型工具/命令：不能盲目重试，应标记为 `Unknown` 并等待人工确认，或通过幂等键查询实际结果。

### 5.6 LLM 的状态边界

LLM 节点不持有会话状态。它记录：

- 请求 ID、模型、参数、prompt 哈希；
- 响应内容、token 用量、结束原因；
- 工具调用请求。

重放策略：

- 默认 replay 使用已记录的 LLM 响应，不重新调用模型；
- 如需重新生成，明确使用 `replay_with_llm` 模式；
- prompt 中的敏感信息需要在日志中脱敏或加密。

### 5.7 工具副作用与幂等

能力工具执行器维护副作用账本；编排工具的状态修改属于 `AgentState` 事务，不进入这个账本。

```text
(state_id, tool_call_id) -> ToolExecutionRecord {
    status: Started | Succeeded | Failed | Unknown,
    input_hash,
    output_ref,
    side_effect_ref,
}
```

规则：

- 执行前记录 `Started`，执行后记录 `Succeeded/Failed`；
- 重试必须复用同一个 `idempotency_key`；
- 文件编辑记录目标路径、旧内容哈希、新内容哈希；
- 命令执行记录 cwd、命令行、退出码、输出摘要；
- 恢复时发现 `Started` 但没有结果，不要直接重复执行危险操作。

### 5.8 上下文工程状态

上下文是从 Agent 状态和记忆库派生出来的，不应该成为第二份事实来源。

建议：

- `ContextAssembler` 根据 `AgentState`、检索结果和 token 预算构建 `LlmRequest`；
- 压缩/摘要产生 `ContextCompacted` 事件，并更新 `ContextState`；
- prompt 中引用事件 ID 或消息 ID，便于追溯；
- 记忆索引可以重建，不作为唯一状态来源；
- 每个 agent 的上下文预算独立核算。

### 5.9 MCP 与 Skill 状态

MCP：

- `McpManager` 持有连接和握手状态；
- 发现到的工具只注册为能力工具（`kind = Capability, backend = Mcp`），经过统一的 `ToolExecutor`；
- MCP 不是安全边界，执行器仍要按服务器信任级别、工具风险和审批策略校验；
- 编排工具永远不通过 MCP 暴露；
- 连接断开产生生命周期事件，不直接修改 agent 状态。

Skill：

- `SkillLoader` 读取 manifest，注册能力工具、编排工具、命令和上下文提供者；
- 编排工具只能注册到本侧 AgentActor，不能导出给 MCP 或外部进程；
- skill 的激活状态属于能力注册表，不属于某个 agent 的会话状态；
- 如果 skill 有 per-agent 状态，通过事件和 agent 自己的 `ContextState` 管理；
- 插件加载失败进入死信或启动错误，不影响已有 agent 状态。

## 6. 工具接入 API

工具描述必须携带分类、执行后端和风险信息：

```rust
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ToolKind,
    pub backend: ToolBackend,
    pub risk: RiskLevel,
    pub requires_approval: bool,
    pub input_schema: serde_json::Value,
    pub timeout: Duration,
    pub handler: ToolHandler,
}
```

显式注册：

```rust
registry.add_tool(ToolDescriptor::new(
    "new_file",
    "Create a new file",
    ToolKind::Capability,
    ToolBackend::Sandbox,
    NewFileArgs::schema(),
    new_file_handler,
));
```

属性宏：

```rust
#[tool("new_file", kind = capability, backend = sandbox, description = "Create a new file")]
pub async fn new_file(
    ctx: &ToolContext,
    args: NewFileArgs,
) -> Result<ToolOutput, BoxError> {
    // ...
}
```

宏生成：

- `ToolDescriptor` 元数据（名称、description、kind、backend、risk）；
- 参数 JSON Schema（后续可接 `schemars`）；
- handler 包装；
- `new_file_tool()` 工厂函数。

执行时按 `ToolKind` 分流：

```text
ToolCallRequested
  -> kind == Capability
       -> ToolExecutor
       -> 本侧沙箱 / MCP
       -> ToolCallCompleted / ToolCallFailed

  -> kind == Orchestration
       -> AgentActor
       -> 状态 reducer + 数据库事务
       -> 编排结果事件
```

能力工具执行器统一负责：

- 名称查找和参数校验；
- 权限/沙箱检查；
- 超时和取消；
- 幂等键和副作用账本；
- 输出大小限制和流式输出；
- 发布 `ToolCallCompleted` / `ToolCallFailed`；
- 失败进入死信或重试策略。

命令行工具额外要求：

- 必须在沙箱或受限执行环境中运行，不能直接在宿主环境执行；
- 限制 cwd、可写路径、环境变量和网络；
- 设置超时、输出上限，并在取消时清理整个进程树；
- 高风险命令需要审批或 deny list；
- 完整记录命令行、cwd、退出码、stdout/stderr 摘要和副作用引用。

文件/目录工具额外要求：

- 路径必须限制在 workspace 根目录内；
- 写入优先使用原子替换；
- 记录旧内容哈希和新内容哈希，支持回滚和冲突检测；
- 删除操作需要审计，必要时进入回收站而不是永久删除。

编排工具执行器（AgentActor）负责：

- 只允许白名单内的编排操作；
- 状态修改和事件追加在同一个数据库事务中；
- 不允许通过 MCP 暴露编排工具；
- askuser 等控制流操作需要保存 pending 状态并支持恢复；
- 长期记忆写入要经过 memory store 的事务接口，不能直接写文件。

MCP 只承载能力工具。MCP 工具适配为同样的 `ToolDescriptor`（`backend = Mcp`），agent 和 LLM 不需要区分本地工具与 MCP 工具，但执行器仍要按服务器身份、信任级别和风险策略做校验。MCP 不是安全边界。

## 7. 建议的模块划分

```text
src/domain/
  event.rs          # Event、EventEnvelope、错误
  agent.rs          # AgentState、AgentEvent、Effect、AgentActor trait
  tool.rs           # ToolDescriptor、ToolCall、ToolResult
  context.rs        # ContextState、LlmRequest、记忆抽象
  command.rs        # Command trait（见 command-system.md）

src/core/
  dispatcher.rs     # 已有事件分发
  dead_letter.rs    # 已有死信队列
  supervisor.rs     # agent 路由与生命周期
  agent_runtime.rs  # AgentActor 状态机循环
  state_store.rs    # agent_states + agent_events + outbox 事务
  tool_registry.rs  # 工具注册与按 kind 分流
  tool_policy.rs    # 权限、审批、沙箱、命令执行限制
  mcp.rs            # MCP 连接与能力发现
  skill.rs          # skill manifest 与插件加载
```

## 8. 演进顺序

1. 给事件增加 `EventEnvelope`、`state_id`、`correlation_id`。
2. 实现数据库状态仓储：`agent_states`、`agent_events`、`event_outbox` 和乐观锁。
3. 实现 `AgentState` + reducer + `AgentActor`，先做单 agent。
4. 实现 checkpoint：turn 边界、危险工具前、workspace 快照和恢复校验。
5. 实现 `Supervisor`，支持多个 agent 按 `state_id` 路由。
6. 接入 LLM 记录/重放，接入能力工具执行器和幂等账本。
7. 实现工具分类、执行策略、沙箱、MCP 与 skill 加载。
8. 接入评估 runner、录制回放和指标采集。
9. 最后再补上命令系统与 CLI 交互（详见 command-system.md）。

## 9. 待定问题

- `EventEnvelope` 是否直接进入 Dispatcher，还是由 agent 层包装。
- Dispatcher 是否原生支持按 `state_id` 分区，还是由 Supervisor 转发。
- 本地状态库选 SQLite 还是 Postgres，以及是否使用 `sqlx`。
- `state_json` 使用 JSON、MessagePack 还是 Protobuf；迁移策略如何做。
- outbox 发布失败后的重试和告警策略。
- 工具参数 Schema 使用 `schemars` 自动生成，还是手写 JSON Schema。
- 编排工具是否在 LLM 工具目录中使用独立命名空间（如 `agent.step.create`）。
- 文件编辑和命令执行是否需要人工审批，以及审批状态如何进入状态机。
- 多 agent 的共享文件冲突如何检测和合并。
- LLM 流式增量是否全部入事件日志，还是只记录完整响应 + 摘要。
