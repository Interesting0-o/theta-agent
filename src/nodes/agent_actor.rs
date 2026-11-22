use std::sync::Arc;

use edgequake_llm::ChatMessage;

use crate::domain::agent::events::{
    LLMCompleted, LLMRequest, ToolCallRequest, ToolCallResponse, UserInput,
};
use crate::domain::agent::state::AgentState;
use crate::domain::consumer::Consumer;
use crate::domain::event::{BoxError, EventEnvelope};
use crate::service::mailbox::{AgentMailbox, AgentMailboxError, StateTransition};

/// Agent 状态机与上下文组装点。
///
/// 状态读写通过 [`AgentMailbox`] 串行提交，这里只负责决定状态如何变化，
/// 以及如何把状态组装成发给模型的上下文。
pub struct AgentActor {
    mailbox: AgentMailbox,
}

impl AgentActor {
    /// 注入用于提交状态变更的 mailbox 句柄。
    pub fn new(mailbox: AgentMailbox) -> Self {
        Self { mailbox }
    }

    /// 组装本次发给模型的上下文。
    ///
    /// 当前直接使用完整消息历史；后续的 token 裁剪、摘要和检索注入都在这里接入。
    fn build_llm_request(state: &AgentState) -> LLMRequest {
        LLMRequest {
            messages: state.messages.clone(),
        }
    }
}

#[async_trait::async_trait]
impl Consumer<UserInput> for AgentActor {
    async fn consume(
        &self,
        event: Arc<UserInput>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        let state_id = envelope.scope.state_id;
        let envelope = Arc::new(envelope.clone());
        self.mailbox
            .execute(state_id, move |current| {
                let mut state = current
                    .cloned()
                    .unwrap_or_else(|| AgentState::new(state_id));
                state.messages.push(ChatMessage::user(event.input.clone()));

                let request = envelope.reply(Self::build_llm_request(&state));
                Ok(StateTransition::new(state).with_effect(request))
            })
            .await
    }
}

#[async_trait::async_trait]
impl Consumer<LLMCompleted> for AgentActor {
    async fn consume(
        &self,
        event: Arc<LLMCompleted>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        let state_id = envelope.scope.state_id;
        let envelope = Arc::new(envelope.clone());
        self.mailbox
            .execute(state_id, move |current| {
                let Some(current) = current else {
                    return Err(AgentMailboxError::MissingState(state_id).into());
                };

                let mut state = current.clone();
                let tool_calls = event.tool_calls.clone();
                if tool_calls.is_empty() {
                    state
                        .messages
                        .push(ChatMessage::assistant(event.content.clone()));
                } else {
                    state.messages.push(ChatMessage::assistant_with_tools(
                        event.content.clone(),
                        tool_calls.clone(),
                    ));
                }

                let mut transition = StateTransition::new(state);
                if !tool_calls.is_empty() {
                    transition =
                        transition.with_effect(envelope.reply(ToolCallRequest { tool_calls }));
                }
                Ok(transition)
            })
            .await
    }
}

#[async_trait::async_trait]
impl Consumer<ToolCallResponse> for AgentActor {
    async fn consume(
        &self,
        event: Arc<ToolCallResponse>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        let state_id = envelope.scope.state_id;
        let envelope = Arc::new(envelope.clone());
        self.mailbox
            .execute(state_id, move |current| {
                let Some(current) = current else {
                    return Err(AgentMailboxError::MissingState(state_id).into());
                };

                let mut state = current.clone();
                state.messages.extend(event.tool_res.iter().cloned());

                let request = envelope.reply(Self::build_llm_request(&state));
                Ok(StateTransition::new(state).with_effect(request))
            })
            .await
    }
}
