use std::sync::Arc;

use edgequake_llm::ChatMessage;

use crate::core::dispatcher::EventPublisher;
use crate::domain::agent::events::{
    LLMCompleted, LLMRequest, ToolCallRequest, ToolCallResponse, UserInput,
};
use crate::domain::agent::state::{AgentState, AgentStateRepository};
use crate::domain::consumer::Consumer;
use crate::domain::event::{BoxError, EventEnvelope};

/// 单写者的 agent 状态机，负责读写状态并驱动下游节点。
pub struct AgentActor {
    repo: Arc<dyn AgentStateRepository>,
    publisher: EventPublisher,
}

impl AgentActor {
    /// 注入状态仓储和只含发布能力的事件总线句柄。
    pub fn new(repo: Arc<dyn AgentStateRepository>, publisher: EventPublisher) -> Self {
        Self { repo, publisher }
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
        let existing = self.repo.get(state_id).await?;
        let mut state = match &existing {
            Some(state) => (**state).clone(),
            None => AgentState::new(state_id),
        };
        state.messages.push(ChatMessage::user(event.input.clone()));

        let state = Arc::new(state);
        if existing.is_some() {
            self.repo.update(Arc::clone(&state)).await?;
        } else {
            self.repo.create(Arc::clone(&state)).await?;
        }

        let request = envelope.reply(LLMRequest {
            messages: state.messages.clone(),
        });
        self.publisher.publish(request).await?;
        Ok(())
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
        if let Some(agent_state) = self.repo.get(state_id).await? {
            // 拿到Agent实体
            let mut agent_state = (*agent_state).clone();
            let tool_calls = event.tool_calls.clone();

            if tool_calls.is_empty() {
                agent_state
                    .messages
                    .push(ChatMessage::assistant(event.content.clone()));
            } else {
                agent_state.messages.push(ChatMessage::assistant_with_tools(
                    event.content.clone(),
                    tool_calls.clone(),
                ));
            }
            let agent_state = Arc::new(agent_state);
            self.repo.update(Arc::clone(&agent_state)).await?;

            // 先落库再请求工具，保证结果返回时 assistant 消息已经存在。
            if !tool_calls.is_empty() {
                let request = envelope.reply(ToolCallRequest { tool_calls });
                self.publisher.publish(request).await?;
            }
        }
        Ok(())
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
        if let Some(state) = self.repo.get(state_id).await? {
            // 拿到agent状态实体
            let mut agent_state = (*state).clone();

            // 将tool call返回拼接会状态
            agent_state.messages.extend(event.tool_res.iter().cloned());

            // 更新状态
            let agent_state = Arc::new(agent_state);
            self.repo.update(Arc::clone(&agent_state)).await?;

            // 构建请求并发布
            let request = envelope.reply(LLMRequest {
                messages: agent_state.messages.clone(),
            });
            self.publisher.publish(request).await?;
        }
        Ok(())
    }
}
