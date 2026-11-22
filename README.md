# theta

用 Rust 编写的事件驱动 coding agent 原型。

当前仓库处于早期开发阶段：事件总线、Agent 状态仓储和最小的 LLM 对话闭环已经打通，工具调用、持久化和多 agent 仍在规划中。

## 核心特性

- **事件信封**：业务事件外统一携带 `state_id`、`run_id`、`event_id`、`correlation_id`、`causation_id`、`sequence` 和时间戳。
- **异步事件总线**：按事件 `TypeId` 路由；每个订阅者独立队列，慢消费者只背压自己；支持 `flush`、运行统计和有界死信队列。
- **状态服务 mailbox**：`AgentMailbox` 封装 `AgentStateRepository`，按 `state_id` 串行执行状态变更，节点不直接操作仓储。
- **单写者 AgentActor**：负责状态机与上下文组装，通过 mailbox 提交状态变更并驱动下游节点。
- **无状态 LLMNode**：只消费 `LLMRequest`、调用模型并发布 `LLMCompleted`，不保存会话历史。
- **可拆分的输出节点**：`PrintWorker` 作为 `LLMCompleted` 的独立消费者负责终端输出。

## 当前事件流

```text
Stdin
  -> UserInput
  -> AgentActor          # 通过 mailbox 串行更新 AgentState，组装上下文
  -> LLMRequest
  -> LLMNode             # 调用 OpenAI-compatible provider
  -> LLMCompleted
  -> PrintWorker         # 输出到终端
```

事件总线只传递“发生了什么”；`AgentState` 只由 AgentActor 通过 mailbox 写入。

## 快速开始

要求：Rust stable（edition 2024）。

```bash
cp .env.example .env
# 填入下面这些配置
cargo run --bin cli
```

配置项：

| 变量 | 说明 |
|---|---|
| `CHAT_MODEL_NAME` | 模型名称，例如 `qwen2.5:7b` |
| `CHAT_MODEL_API_KEY` | API key；本地 Ollama 等无需鉴权的服务可留空 |
| `CHAT_MODEL_BASE_URL` | OpenAI-compatible API 地址，例如 `http://localhost:11434/v1` |
| `CHAT_MODEL_PROVIDER` | 提供方，留空表示使用默认值 |

## 测试与检查

```bash
cargo test --all-targets
cargo clippy --all-targets
```

## 目录结构

```text
src/
  domain/            # 事件、信封、ID、消费者/生产者、Agent 状态与事件
  core/              # Dispatcher、EventPublisher、死信队列
  service/           # AgentMailbox：封装仓储，按 state_id 串行化状态写入
  nodes/             # AgentActor、LLMNode、PrintWorker
  infrastructure/    # 内部状态仓储实现，仅 service 使用
  bin/cli.rs         # CLI 入口
docs/                # 架构、ReAct、checkpoint、基础设施调研
tests/               # 总线路由、mailbox、Actor、LLM 节点集成测试
```

## 设计文档

- [Coding Agent 架构设计](./docs/agent-architecture.md)
- [最小 ReAct Agent 与审核链路](./docs/minimal-react-agent.md)
- [Checkpoint 与评估设计](./docs/checkpoint-evaluation.md)
- [Command 系统设计](./docs/command-system.md)
- [基础设施调研结论](./docs/infrastructure-findings.md)
- [待决策清单](./docs/open-decisions.md)

## Roadmap

- [x] 事件信封、唯一 ID、因果链元数据
- [x] 订阅者隔离、背压、死信队列
- [x] Agent 状态仓储 CRUD 与内存实现
- [x] AgentActor 单写者闭环
- [x] 无状态 LLMNode 与独立输出节点
- [ ] 工具调用循环（`ToolCallRequested` / `ToolCallCompleted`）
- [ ] SQLite 持久化、事件日志与 outbox
- [ ] 流式输出
- [ ] Checkpoint 与恢复
- [ ] MCP 能力工具
- [ ] 多 agent 路由与 Supervisor

早期原型，API 与模块边界可能继续调整。
