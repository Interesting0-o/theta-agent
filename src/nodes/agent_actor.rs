use std::sync::Arc;

use edgequake_llm::ChatMessage;

use crate::core::dispatcher::EventPublisher;
use crate::domain::agent::events::{LLMRequest, UserInput};
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
