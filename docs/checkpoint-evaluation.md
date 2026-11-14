# Checkpoint 与评估设计（草案）

状态：设计草案，尚未实现。

相关文档：

- [Coding Agent 架构设计](./agent-architecture.md)
- [Command 系统设计](./command-system.md)
- [基础设施调研结论](./infrastructure-findings.md)
- [待决策清单](./open-decisions.md)

## 1. 目标

Checkpoint 不只是“崩溃恢复快照”，它同时服务于：

- 运行恢复：进程退出、崩溃、超时后从最近边界继续；
- 评估：从同一初始状态重复运行、比较轨迹、计算指标；
- 分支：从某个决策点 fork 出多个尝试；
- 调试：回到某个 turn、查看当时的上下文和工具状态；
- 审计：确认某个文件修改或命令执行发生时的完整上下文；
- 上下文压缩：在压缩前后保存可恢复点。

Checkpoint 不是事件日志的替代品：

- 事件日志记录“发生过什么”；
- `agent_states` 记录“当前是什么”；
- checkpoint 记录“某个确定时间点的完整可恢复状态”。

## 2. Checkpoint 类型

| 类型 | 触发时机 | 主要内容 | 主要用途 |
|---|---|---|---|
| Runtime | turn 结束、每 N 个事件、进程退出前 | AgentState、pending calls、sequence | 崩溃恢复、续跑 |
| Safety | 执行写文件/命令/MCP 写操作前 | AgentState、工具意图、workspace 基线 | 危险操作恢复和审计 |
| Evaluation | 评估场景开始、每个 step、结束时 | AgentState、workspace、配置、录制数据、指标 | 可复现评估、轨迹比较 |
| Branch | 手动 fork、并行尝试 | AgentState、workspace 引用、parent checkpoint | 多方案对比、分支回放 |
| Context | 上下文压缩前后 | ContextState、摘要、prompt 引用 | 调试上下文、恢复压缩前状态 |
| Global Epoch | 多 agent 的同步边界 | 每个 agent 的 checkpoint ID + epoch | 多 agent 一致性评估 |

## 3. Checkpoint 包含什么

一个完整的 checkpoint 至少包含：

```rust
pub struct Checkpoint {
    pub checkpoint_id: CheckpointId,
    pub state_id: AgentStateId,
    pub parent_checkpoint_id: Option<CheckpointId>,
    pub sequence: u64,
    pub kind: CheckpointKind,
    pub created_at: SystemTime,

    pub agent_state: AgentState,
    pub workspace_ref: WorkspaceRef,
    pub config_ref: ConfigRef,
    pub recordings_ref: RecordingsRef,
    pub side_effects_ref: SideEffectRef,

    pub last_event_hash: EventHash,
}
```

### 3.1 Agent 状态

- `AgentState` 全量快照；
- `version`、`last_sequence`；
- `pending_calls`：LLM 请求、工具调用、askuser 等未完成状态；
- `context`：摘要、token 预算、prompt 版本；
- `budget`：时间、token、费用、步数；
- `status`：运行中、等待审批、等待用户、完成、失败、取消。

### 3.2 事件位置与完整性

- `sequence` 必须对应一个已经提交的事件边界；
- `last_event_hash` 是事件链哈希，恢复时校验事件日志没有被篡改；
- checkpoint 不允许落在事件事务中间。

### 3.3 Workspace 快照

AgentState 只描述 agent 内部状态，不包含文件系统。评估和恢复必须额外记录 workspace：

```rust
pub struct WorkspaceRef {
    pub root: PathBuf,
    pub git_head: Option<String>,
    pub dirty_patch_ref: Option<ArtifactRef>,
    pub untracked_manifest_ref: Option<ArtifactRef>,
    pub file_manifest_ref: Option<ArtifactRef>,
    pub container_image: Option<String>,
    pub base_commit: Option<String>,
}
```

优先方案：

1. git 仓库：记录 HEAD + `git diff` 补丁 + untracked 文件 manifest；
2. content-addressed store：文件内容按 hash 存储，manifest 记录路径和 hash；
3. 评估环境：使用独立 worktree、overlayfs 或容器快照，确保冻结。

仅记录 git commit 不够，因为未提交修改和未跟踪文件也会影响 agent 行为。

### 3.4 配置与版本

为了评估可复现，需要记录：

- 模型 ID、provider、温度、top_p、max_tokens、seed；
- system prompt、prompt 模板版本/hash；
- 工具注册表 hash（工具名、schema、backend、policy）；
- skill manifest 版本；
- MCP server 列表和版本；
- OS、Rust/Node/Python 等运行时版本；
- 仓库 commit、基础镜像、环境变量（脱敏后）；
- 随机种子。

### 3.5 非确定性录制

Coding agent 的评估必须区分“可重放”和“真实执行”：

- LLM 响应：记录请求 hash、完整响应、工具调用；
- 工具输出：记录输入、输出、退出码、副作用；
- 命令输出：stdout/stderr、退出码、耗时；
- 时间、随机数：如果影响决策，需要记录或注入固定值。

评估 replay 默认读取录制结果，不重新调用 LLM、不重新执行危险工具。

### 3.6 副作用引用

Checkpoint 不复制整个外部世界，只引用副作用账本：

- 文件修改记录；
- 命令执行记录；
- MCP 写操作记录；
- 每个记录的幂等键和前后 hash。

## 4. 数据库设计

建议新增不可变 checkpoint 表：

```sql
CREATE TABLE agent_checkpoints (
    checkpoint_id        TEXT PRIMARY KEY,
    state_id             TEXT NOT NULL,
    parent_checkpoint_id TEXT,
    sequence             BIGINT NOT NULL,
    kind                 TEXT NOT NULL,
    state_json           BLOB NOT NULL,
    workspace_ref        TEXT,
    config_ref           TEXT,
    recordings_ref       TEXT,
    side_effects_ref     TEXT,
    last_event_hash      TEXT NOT NULL,
    created_at           TIMESTAMP NOT NULL,
    UNIQUE (state_id, sequence, kind)
);

CREATE TABLE checkpoint_artifacts (
    artifact_id          TEXT PRIMARY KEY,
    checkpoint_id        TEXT NOT NULL,
    kind                 TEXT NOT NULL,
    uri                  TEXT NOT NULL,
    content_hash         TEXT NOT NULL,
    size_bytes           BIGINT NOT NULL
);
```

设计要点：

- checkpoint 表只追加，不修改；
- 大对象（补丁、文件 manifest、模型响应）放 artifact store，数据库只存引用和 hash；
- `state_json` 可以压缩；
- `(state_id, sequence, kind)` 唯一，避免同一位置重复 checkpoint；
- `last_event_hash` 用于恢复校验；
- `parent_checkpoint_id` 支持分支和评估谱系。

## 5. 写入路径

Checkpoint 只能在 AgentActor 的静止点产生：没有正在进行的 reducer，状态与事件已经对齐。

```text
Actor 收到事件
  -> reduce(state, event)
  -> 开启事务
       -> 追加 agent_events
       -> CAS 更新 agent_states
       -> 如果到达 checkpoint 边界：
            -> 写入 agent_checkpoints
            -> 写入 checkpoint_artifacts 引用
       -> 写入 event_outbox
  -> 提交事务
  -> 发布 outbox
  -> 执行 Effects
```

Workspace 快照的写入顺序：

1. 在 Actor 静止点捕获 workspace 基线；
2. 计算文件 manifest / patch / hash；
3. 写入 artifact store；
4. 在同一个数据库事务里写 checkpoint 引用。

评估环境必须冻结 workspace，避免捕获过程中被外部进程修改。普通开发环境至少要在 checkpoint 前后重新校验文件 hash，发现不一致则标记 checkpoint 无效。

安全 checkpoint：

- 在执行写文件、命令、MCP 写操作前创建；
- 记录工具意图、参数 hash、workspace 基线；
- 如果 checkpoint 写入失败，默认阻止危险操作；
- 执行后创建结果 checkpoint，写入真实副作用引用。

失败策略：

- Safety / Evaluation checkpoint 失败：中止当前操作或评估；
- Runtime / Context checkpoint 失败：记录告警并继续，但降低恢复保证；
- checkpoint 写入后必须回读校验。

## 6. 恢复路径

恢复流程：

```text
load checkpoint
  -> 校验 checkpoint 完整性（state_id / sequence / last_event_hash）
  -> 恢复 AgentState
  -> 恢复 workspace（git / CAS / container）
  -> 校验文件 manifest
  -> 重建派生状态（context index、memory index）
  -> 根据 pending_calls 决定重试、等待或人工确认
  -> 从 sequence + 1 继续接收事件
```

恢复策略：

- LLM 请求：优先使用录制响应；没有录制时可以安全重试；
- 只读工具：可以重试；
- 写文件/命令/MCP 写操作：不能盲目重试；状态标记为 `Unknown`，通过幂等键查询或等待人工确认；
- workspace hash 不匹配：checkpoint 标记为无效，或进入只读调查模式；
- 事件 sequence 落后于 checkpoint：拒绝恢复，说明数据损坏。

## 7. 分支与多 Agent

### 7.1 单 agent 分支

从 checkpoint fork：

- 新建 `state_id`；
- `parent_checkpoint_id` 指向源 checkpoint；
- 工作区使用独立 worktree 或 overlay，避免污染原 workspace；
- 事件日志从 fork 点开始独立追加；
- 原 agent 状态不变。

评估时可以从同一个 checkpoint 启动多个 run，比较不同模型或策略。

### 7.2 多 agent

多 agent 很难做全局一致的分布式快照。建议：

- 每个 agent 独立 checkpoint；
- Supervisor 在 turn/epoch 边界暂停接收新事件；
- 记录 epoch 内所有 agent 的 checkpoint ID；
- 评估时以 epoch 为单位恢复；
- 父子 agent 通过 `parent_checkpoint_id` 和 `state_id` 关联；
- 跨 agent 的共享 artifact 用不可变引用和 hash 固定。

## 8. 评估设计

### 8.1 评估场景

一个评估场景至少包含：

```rust
pub struct EvalScenario {
    pub scenario_id: String,
    pub initial_checkpoint: CheckpointId,
    pub instruction: String,
    pub workspace_fixture: WorkspaceRef,
    pub tool_mocks: Vec<ToolMock>,
    pub llm_mode: LlmMode,
    pub grader: Grader,
}
```

- `initial_checkpoint`：统一起点；
- `workspace_fixture`：仓库初始状态；
- `tool_mocks`：可选的确定性工具结果；
- `llm_mode`：录制重放 / 实时模型 / 混合；
- `grader`：测试、文件 diff、命令检查或人工评分。

### 8.2 两种运行模式

**严格重放模式**

- LLM 和工具都读取录制结果；
- 不执行真实写操作；
- 用于回归测试、状态机验证、确定性检查；
- 结果应与录制轨迹一致。

**实时沙箱模式**

- 使用真实 LLM 和工具；
- 在独立 workspace/容器中执行；
- 记录所有非确定性结果和副作用；
- 用于能力评估和真实任务 benchmark。

### 8.3 指标

至少采集：

- 任务成功率、测试通过率；
- 最终 workspace diff 是否正确；
- turn 数、工具调用数、无效重试数；
- token、费用、执行时间；
- 死信数量、恢复次数；
- 安全策略违规（越界路径、危险命令、未审批操作）；
- 从 checkpoint 恢复后的续跑成功率；
- 相同 checkpoint 多次运行的一致性。

### 8.4 轨迹评估

除了最终结果，还应该评估过程：

- 每个 turn 或 step 的 checkpoint；
- 关键决策点的事件序列；
- 工具调用参数和结果；
- 是否出现重复步骤、死循环、无进展；
- 失败后是否能从最近的 safety checkpoint 恢复。

这样可以对长任务做部分评分，而不是只看最终文件。

### 8.5 可复现性等级

| 等级 | LLM | 工具 | 用途 |
|---|---|---|---|
| Strict replay | 录制响应 | 录制结果，不执行 | 回归测试、确定性验证 |
| Controlled live | 固定模型/参数/seed | 沙箱真实执行 | 稳定性评估 |
| Live | 真实模型 | 真实环境 | 最终能力评估 |

评估结果必须记录 checkpoint ID、配置 hash、workspace hash 和事件链 hash，否则结果不可比较。

### 8.6 评估隔离

- 每次评估使用新的 `state_id` 和独立 workspace；
- 不在用户工作区直接跑评估；
- 评估产生的文件和命令副作用只能落在沙箱；
- 共享的 MCP 服务要使用测试账号或 mock；
- 评估 checkpoint 只读，不随后续运行修改。

## 9. API 草图

```rust
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    async fn save(&self, checkpoint: Checkpoint) -> Result<CheckpointId, CheckpointError>;
    async fn load(&self, id: &CheckpointId) -> Result<Checkpoint, CheckpointError>;
    async fn list(&self, state_id: &AgentStateId) -> Result<Vec<CheckpointMeta>, CheckpointError>;
    async fn fork(
        &self,
        id: &CheckpointId,
        new_state_id: AgentStateId,
    ) -> Result<CheckpointId, CheckpointError>;
}

#[async_trait]
pub trait WorkspaceSnapshotter {
    async fn capture(&self, root: &Path) -> Result<WorkspaceRef, CheckpointError>;
    async fn restore(&self, reference: &WorkspaceRef) -> Result<(), CheckpointError>;
    async fn verify(&self, reference: &WorkspaceRef) -> Result<(), CheckpointError>;
}
```

AgentActor 暴露显式 checkpoint 接口：

```rust
actor.checkpoint(CheckpointKind::Evaluation).await?;
```

以及自动触发点：

- `TurnEnd`；
- `BeforeDangerousTool`；
- `AfterToolResult`；
- `ContextCompacted`；
- `AgentCompleted`；
- `Shutdown`。

## 10. 保留与清理

- Evaluation checkpoint 和其 artifacts 默认不可变，长期保留；
- Runtime checkpoint 可以按最近 N 个或最近 T 天滚动清理；
- 大文件内容和工具输出使用 CAS 去重；
- checkpoint 中可能包含源码、prompt、密钥，必须做访问控制和脱敏；
- 清理前要保证仍有一个可恢复 checkpoint 和完整事件日志。

## 11. 实现顺序

1. `agent_checkpoints` 表和 `Checkpoint` 数据结构；
2. AgentState 序列化、事件链 hash、恢复时校验；
3. TurnEnd / BeforeDangerousTool 两类 checkpoint；
4. WorkspaceSnapshotter（先 git patch + manifest，再考虑 overlay/container）；
5. 恢复流程和 pending calls 处理；
6. fork / branch；
7. 评估场景 runner、录制回放和指标采集；
8. 多 agent epoch checkpoint。

## 12. 待定问题

- Workspace 快照优先用 git patch，还是直接 overlayfs/容器；
- `state_json` 使用 JSON、MessagePack 还是 Protobuf；
- checkpoint 写入是否必须与状态更新同事务，还是允许两阶段提交；
- 多 agent 的全局一致性边界如何定义；
- 大工具输出和命令日志是否全部进 artifact store；
- 评估 grader 使用测试命令、diff 规则还是模型评审；
- 是否允许从 checkpoint 重新执行危险工具，还是永远只读重放。
