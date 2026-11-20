//! Agent 状态与状态仓储。

use std::sync::Arc;

use async_trait::async_trait;
use edgequake_llm::ChatMessage;

use crate::domain::event::BoxError;
use crate::domain::id::AgentStateId;

/// 一份 agent 会话状态。
#[derive(Debug, Clone)]
pub struct AgentState {
    /// 会话标识，必须与事件信封中的 `state_id` 保持一致。
    pub state_id: AgentStateId,
    /// 对话历史。
    pub messages: Vec<ChatMessage>,
}

impl AgentState {
    /// 创建指定会话的空状态。
    pub fn new(state_id: AgentStateId) -> Self {
        Self {
            state_id,
            messages: Vec::new(),
        }
    }
}

/// 状态仓储错误。
#[derive(Debug)]
pub enum AgentStateError {
    /// 创建时状态已经存在。
    AlreadyExists(AgentStateId),
    /// 更新时目标状态不存在。
    NotFound(AgentStateId),
    /// 底层存储错误。
    Backend(BoxError),
}

impl std::fmt::Display for AgentStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(state_id) => {
                write!(f, "agent state `{state_id}` already exists")
            }
            Self::NotFound(state_id) => write!(f, "agent state `{state_id}` not found"),
            Self::Backend(source) => write!(f, "agent state backend error: {source}"),
        }
    }
}

impl std::error::Error for AgentStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source.as_ref()),
            Self::AlreadyExists(_) | Self::NotFound(_) => None,
        }
    }
}

/// `AgentState` 的持久化仓储。
///
/// 只应由 `AgentActor` 调用写方法；其他节点通过事件影响状态，
/// 不能直接修改仓储中的数据。
#[async_trait]
pub trait AgentStateRepository: Send + Sync + 'static {
    /// 创建新状态；标识已存在时返回 [`AgentStateError::AlreadyExists`]。
    async fn create(&self, state: Arc<AgentState>) -> Result<(), AgentStateError>;

    /// 读取状态快照；不存在时返回 `Ok(None)`。
    async fn get(&self, state_id: AgentStateId)
    -> Result<Option<Arc<AgentState>>, AgentStateError>;

    /// 覆盖已有状态；目标不存在时返回 [`AgentStateError::NotFound`]。
    async fn update(&self, state: Arc<AgentState>) -> Result<(), AgentStateError>;

    /// 删除状态，返回是否真的删除了一条记录。
    async fn delete(&self, state_id: AgentStateId) -> Result<bool, AgentStateError>;
}
