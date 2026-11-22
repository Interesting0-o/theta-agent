//! Agent 领域事件。

use edgequake_llm::{ChatMessage, ToolCall};

use crate::domain::event::Event;

/// 用户输入事件，由 CLI 或上层入口发布给 agent。
pub struct UserInput {
    /// 用户输入的原始文本。
    pub input: String,
}

impl Event for UserInput {
    fn event_name(&self) -> &'static str {
        "theta.agent.user_input.v1"
    }
}

/// 请求 LLM 节点执行一次补全。
///
/// 只携带本次调用所需的完整上下文；历史由 `AgentActor` 维护。
pub struct LLMRequest {
    /// 本次调用使用的消息列表。
    pub messages: Vec<ChatMessage>,
}

impl Event for LLMRequest {}

/// token 用量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// 输入 token 数。
    pub prompt_tokens: usize,
    /// 输出 token 数。
    pub completion_tokens: usize,
    /// 总 token 数。
    pub total_tokens: usize,
}

/// 一次 LLM 调用完成事件。
///
/// 只承载本次调用新产生的信息，不包含请求时的历史消息；历史由
/// `AgentActor` 维护。`tool_calls` 为空表示模型本次没有请求工具调用，
/// 不代表整个 agent 流程结束——是否结束由 `AgentActor` 判断。
pub struct LLMCompleted {
    /// 模型生成的文本；请求工具调用时可能为空。
    pub content: String,
    /// 模型请求的工具调用。
    pub tool_calls: Vec<ToolCall>,
    /// 厂商返回的结束原因，例如 `stop`、`tool_calls`、`length`。
    pub finish_reason: Option<String>,
    /// 实际使用的模型名称。
    pub model: String,
    /// 本次调用的 token 用量。
    pub usage: TokenUsage,
}

impl Event for LLMCompleted {
    fn event_name(&self) -> &'static str {
        "theta.agent.llm_completed.v1"
    }
}

/// 请求工具节点执行模型给出的工具调用。
///
/// 由 `AgentActor` 在持久化包含 `tool_calls` 的 assistant 消息之后发布，
/// 保证工具结果回到状态时对应的调用已经落库。
pub struct ToolCallRequest {
    /// 本次需要执行的工具调用。
    pub tool_calls: Vec<ToolCall>,
}

impl Event for ToolCallRequest {
    fn event_name(&self) -> &'static str {
        "theta.agent.tool_call_requested.v1"
    }
}

/// 工具调用的集中执行结果。
pub struct ToolCallResponse {
    /// 集中tool call执行结果
    pub tool_res: Vec<ChatMessage>,
}

impl Event for ToolCallResponse {}
