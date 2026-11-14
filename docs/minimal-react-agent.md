# 最小 ReAct Agent 与审核链路（草案）

状态：设计草案，尚未实现。

相关文档：

- [Coding Agent 架构设计](./agent-architecture.md)
- [Checkpoint 与评估设计](./checkpoint-evaluation.md)
- [Command 系统设计](./command-system.md)
- [基础设施调研结论](./infrastructure-findings.md)
- [待决策清单](./open-decisions.md)

## 1. 范围

第一版目标是一个**单 agent、CLI 交互、最小闭环**的 ReAct Agent：

```text
UserMessage
  -> AgentActor
  -> LlmRequest
  -> LlmResponse / ToolCallRequested
  -> ApprovalRequested（可选）
  -> ApprovalResolved
  -> ToolCallCompleted / ToolCallFailed
  -> AgentActor
  -> LlmRequest
  -> ... 直到最终回答
```

第一版不做：

- 多 agent 通信；
- MCP；
- skill 插件；
- 长期记忆和复杂上下文压缩；
- 完整编排工具集。

但状态、事件、checkpoint 和审核接口要按后续可扩展的方式设计。

## 2. 参与组件

| 组件 | 职责 | 是否持有状态 |
|---|---|---|
| CLI Input Service | 独占 stdin，读取用户输入和审核决定 | 输入队列 |
| CLI Output Service | 独占 stdout/stderr，串行输出 | 输出队列 |
| AgentActor | ReAct 状态机，单写者，决定下一步 | AgentState / DB |
| LlmNode | 消费 `LlmRequest`，生产回复、delta、工具调用请求 | 无会话状态 |
| ToolRegistry | 工具元数据、schema、审批策略 | 只读能力表 |
| ToolExecutor | 执行能力工具，管理超时、取消、副作用 | 副作用账本 |
| ApprovalService | 消费 `ApprovalRequested`，生产 `ApprovalResolved` | 无长期状态 |
| Dispatcher | 事件队列、路由、DLQ | 运行时队列 |
| StateStore | `agent_states`、事件日志、checkpoint | 数据库 |

## 3. ReAct 状态机

AgentActor 的状态：

```text
Idle
  -> Thinking         收到 LlmRequest 已发出
  -> WaitingApproval  工具需要人工审核
  -> Acting           工具正在执行
  -> Observing        工具结果已返回，准备继续思考
  -> Done             最终回答
  -> Failed
  -> Cancelled
```

事件与状态转换：

```text
UserMessage
  -> Thinking
  -> 发布 LlmRequest

LlmResponse(final)
  -> Done

LlmResponse(tool_calls)
  -> 对每个工具调用：
       -> 需要审核：WaitingApproval + 发布 ApprovalRequested
       -> 不需要审核：Acting + 发布 ToolCallRequested

ApprovalResolved(approved)
  -> Acting + 发布 ToolCallRequested

ApprovalResolved(denied)
  -> 生成 ToolCallDenied observation
  -> Thinking + 发布 LlmRequest

ToolCallCompleted / ToolCallFailed
  -> 写入 observation
  -> Thinking + 发布 LlmRequest
```

第一版建议：

- 每个 turn 只处理一个工具调用，降低并发和审核复杂度；
- 后续再支持同一 turn 多个工具调用，并保持结果与 `call_id` 的关联；
- 每轮检查最大步数、token、时间和重复工具调用，防止死循环；
- 工具失败作为 observation 交给 LLM，而不是直接终止 agent；
- 基础设施错误（总线关闭、数据库失败）才终止 run。

ReAct 循环伪代码：

```text
loop:
    request = build_llm_request(state)
    response = call_llm(request)          // 异步 Effect，不阻塞 actor
    apply(response)                        // 产生 final 或 tool_call

    if final:
        return answer

    tool_call = response.tool_call
    if requires_approval(tool_call):
        pending_approvals.insert(tool_call)
        publish(ApprovalRequested)
        wait_for(ApprovalResolved)         // 不阻塞线程，actor 继续收件
        if denied:
            append(observation = "user denied")
            continue

    result = tool_executor.call(tool_call) // 异步 Effect
    append(observation = result)
    checkpoint_if_needed()
```

## 4. 审核设计

### 4.1 审核是控制面，不是能力工具

审核不扩展 agent 能力，不应注册为普通 MCP 工具。建议：

- `ApprovalService` 消费 `ApprovalRequested`；
- 产生 `ApprovalResolved`；
- AgentActor 根据决定继续或终止工具调用。

`ApprovalService` 是“消费者兼生产者”，但它只处理控制面事件，不执行工具。

### 4.2 审核事件

```rust
pub struct ApprovalRequested {
    pub approval_id: ApprovalId,
    pub state_id: AgentStateId,
    pub tool_call_id: CallId,
    pub tool_name: String,
    pub args_hash: String,
    pub risk: RiskLevel,
    pub reason: String,
    pub timeout: Duration,
}

pub struct ApprovalResolved {
    pub approval_id: ApprovalId,
    pub state_id: AgentStateId,
    pub decision: ApprovalDecision,
}

pub enum ApprovalDecision {
    ApprovedOnce,
    ApprovedForSession,
    Denied,
    Cancelled,
    Expired,
}
```

关键点：

- `args_hash` 绑定审批时的参数，ToolExecutor 执行前必须重新计算并校验，防止 TOCTOU；
- `approval_id` 和 `tool_call_id` 必须写入 checkpoint，恢复时可以重新询问；
- `ApprovedForSession` 只对当前 `state_id` 的本次会话有效，不能跨 session 复用；
- 重复的 `ApprovalResolved` 必须幂等忽略；
- 超时或 EOF 默认 `Denied/Cancelled`，不能默认放行。

### 4.3 审核策略

```text
read-only 工具        -> 自动允许
workspace 内写文件    -> 询问用户
workspace 外写文件    -> 默认拒绝
命令执行              -> 询问 + 沙箱 + 超时
高风险命令            -> 默认拒绝或需要显式白名单
MCP 写操作            -> 询问 + 服务器信任校验
```

策略来源：

- 内置默认策略；
- 项目配置；
- CLI `--yes` / `--non-interactive` / `--deny`；
- 会话内“本次允许”/“本会话允许”；
- 后续可加权限 profile（如只读、沙箱、完全信任）。

策略判断应该产生可审计的 `ApprovalPolicyDecision`，记录命中规则和原因。

### 4.4 AgentActor 与审核的关系

- Actor 不直接读 stdin；
- Actor 只发 `ApprovalRequested`，把 `pending_approvals` 写入 AgentState；
- Actor 进入 `WaitingApproval`，但事件循环继续运行，可以处理取消、超时、其他事件；
- 收到 `ApprovalResolved` 后 reducer 更新状态并产生 `InvokeTool` 或拒绝 observation；
- 审核前强制 checkpoint，记录工具参数、hash 和 workspace 基线。

## 5. CLI stdin/stdout 所有权

`std::io::stdin()` 是进程级共享资源，`stdin().lock()` 会阻塞其他读取者。多个消费者各自加锁会出现：

- 提示语和输入错位；
- 一个消费者长期持锁导致其他消费者饿死；
- 在 async runtime 线程里阻塞；
- 取消时丢失已经输入的数据。

建议做成**单所有者**模型。

### 5.1 CliInputService

由一个专门的任务/线程独占 stdin：

```rust
enum InputRequest {
    UserMessage {
        prompt: String,
        reply: oneshot::Sender<Result<String, InputError>>,
    },
    Approval {
        approval_id: ApprovalId,
        prompt: String,
        reply: oneshot::Sender<ApprovalDecision>,
    },
    Shutdown,
}
```

- 可以是一个专用 OS 线程，循环 `stdin().read_line()`；
- 也可以是 `tokio::task::spawn_blocking` 中的阻塞循环；
- 所有输入请求通过 `mpsc` 进入同一个队列；
- 其他组件只持有 `InputHandle`，发送请求并等待 oneshot；
- 不需要在其他地方调用 `stdin().lock()`。

### 5.2 CliOutputService

同理，stdout/stderr 也建议由单一任务串行输出：

- 避免 LLM delta、工具日志、审核提示互相穿插；
- 支持普通文本和 JSON 两种模式；
- 输出通道要有背压或丢弃策略，避免日志阻塞事件处理。

### 5.3 非交互模式

CI 或脚本模式下不能等待 stdin：

- `--yes`：自动批准低风险操作；
- `--non-interactive`：所有需要审核的操作直接拒绝；
- `--approve-policy <file>`：使用配置文件；
- EOF：视为拒绝对话中的审核请求，并优雅结束 agent。

### 5.4 为什么不用全局 Mutex<Stdin>

`Arc<Mutex<Stdin>>` 只能保证互斥，不能保证：

- 提示和响应的配对顺序；
- 输入队列的优先级；
- 取消语义；
- 多个审核请求的超时管理。

单输入任务 + channel 更简单，也更容易测试。

## 6. 最小工具集

第一版建议只实现以下能力工具：

| 工具 | 风险 | 审核 | 说明 |
|---|---|---|---|
| `list_dir` | 低 | 自动允许 | 限制在 workspace |
| `read_file` | 低 | 自动允许 | 只读、限制大小 |
| `search` / `glob` | 低 | 自动允许 | 只读 |
| `write_file` | 中 | 询问 | 路径沙箱、原子写 |
| `apply_patch` | 中 | 询问 | 统一 diff，冲突检测 |
| `run_command` | 高 | 询问 + 沙箱 | cwd、超时、输出上限、进程树清理 |

第一版不做：

- `communicate_with_other_agent`；
- step 编排工具；
- 长期记忆写入；
- MCP 工具。

工具结果必须截断到 token/字节预算内，完整输出存 artifact store，事件里只放摘要和引用。

## 7. 状态与 checkpoint 接入

AgentState 至少包含：

```rust
pub struct AgentState {
    pub state_id: AgentStateId,
    pub version: u64,
    pub last_sequence: u64,
    pub status: AgentStatus,
    pub messages: Vec<Message>,
    pub pending_tool_calls: HashMap<CallId, PendingToolCall>,
    pub pending_approvals: HashMap<ApprovalId, PendingApproval>,
    pub current_turn: u32,
    pub budget: BudgetState,
}
```

Checkpoint 触发点：

- user turn 开始；
- LLM 请求发出前/后；
- 每个工具调用前（safety checkpoint）；
- 审核请求发出后、审核结果写入后；
- 工具结果写入后；
- 最终回答或失败时；
- 进程退出前。

恢复时：

- `pending_approvals` 不能自动批准；
- 默认重新询问用户，或按非交互策略拒绝；
- 已开始但未完成的写操作标记 `Unknown`；
- `pending_tool_calls` 根据工具类型决定重试或等待。

## 8. LLM 节点与 session 锁

单 agent 情况下：

- AgentActor 天然保证同一个 `state_id` 同时只有一个 LLM 请求；
- LlmNode 可以是无状态、可并发的消费者/生产者；
- 不需要额外 session 锁。

多 agent 情况下的建议：

- **锁的粒度是 `state_id`，不是全局锁**；
- 如果 provider 有会话亲和性、streaming 顺序或 SDK 非线程安全问题，使用 `HashMap<StateId, Arc<tokio::sync::Mutex<()>>>` 或 keyed semaphore；
- 同一 `state_id` 的请求串行，不同 `state_id` 并发；
- 锁必须支持超时和取消，session 结束后清理；
- 更推荐由 AgentActor 保证单 in-flight 请求，LlmNode 只做 provider 级限流；
- 不要把 DB 事务或 workspace 锁持有跨越 LLM 调用。

## 9. 多 agent 扩展预留

### 9.1 communicate_with_other_agent

建议作为**本侧编排工具**，不走 MCP：

```text
send_message(to_state_id, message)
  -> AgentMessageSent { message_id, from, to, correlation_id }
  -> Supervisor 路由
  -> 目标 agent 收到 AgentMessageReceived
```

可选能力：

- `wait_for_reply(message_id, timeout)`：挂起当前 agent，收到回复或超时后恢复；
- 消息写入目标 agent 的 mailbox，不直接修改目标 AgentState；
- 需要防循环、防广播风暴、设置消息大小和 hop 上限；
- 消息事件和 pending 状态进入 checkpoint。

### 9.2 多 agent 审核

- CLI 审核提示必须显示 `state_id`/agent 名称、来源任务和工具参数；
- 多个审核请求进入同一个审核队列，按优先级和超时处理；
- 父子 agent 的审核决定不能隐式继承，除非策略明确允许；
- 审核结果只作用于对应的 `approval_id` 和 `state_id`。

## 10. 最小闭环实现顺序

1. `EventEnvelope` 增加 `state_id`、`correlation_id`；
2. `AgentState` + `agent_states`/`agent_events` + 乐观锁；
3. AgentActor ReAct 状态机和事件 flow；
4. LlmNode（先支持录制回放，再接真实 provider）；
5. ToolRegistry + `read_file` / `write_file` / `run_command`；
6. ApprovalService + `CliInputService` / `CliOutputService`；
7. turn 边界、危险工具前 checkpoint；
8. 恢复流程和 pending approval/tool call 处理；
9. 集成测试和最小评估场景。

## 11. 测试矩阵

- LLM 直接回答，不调用工具；
- 工具调用 → 审核通过 → 结果 → 最终回答；
- 工具调用 → 审核拒绝 → observation → LLM 继续；
- 审核超时、EOF、取消；
- 多个审核请求排队，提示与响应不串线；
- 非交互模式自动拒绝/自动允许；
- 命令超时和进程树清理；
- 工具结果截断和完整输出引用；
- checkpoint 恢复时 pending approval 重新询问；
- 恢复时 pending write 标记 Unknown；
- 最大步数、重复调用、无进展检测；
- stdin 由单一服务持有，其他组件无直接读取。

## 12. 待定问题

- CLI 使用逐行输入，还是引入 `rustyline`/`reedline`；
- 审核提示是否支持“本次允许/本会话允许/始终允许”；
- 审核策略配置放在项目文件、全局配置还是 CLI 参数；
- AgentActor 是否允许同一 turn 并发执行多个工具；
- 流式 LLM 输出在 CLI 中如何与工具日志交错；
- 命令执行使用本地沙箱、容器还是远程执行器；
- 是否第一版就支持取消正在执行的命令。
