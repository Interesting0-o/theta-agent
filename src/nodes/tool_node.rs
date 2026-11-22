use std::sync::Arc;

use edgequake_llm::ChatMessage;

use crate::core::dispatcher::EventPublisher;
use crate::domain::agent::events::{ToolCallRequest, ToolCallResponse};
use crate::domain::consumer::Consumer;
use crate::domain::event::{BoxError, EventEnvelope};
use crate::domain::id::ToolCallId;

/// 消费 LLM 的工具调用，执行后发布汇总结果。
pub struct ToolNode {
    event_publisher: EventPublisher,
}

impl ToolNode {
    /// 注入用于回发结果的事件总线句柄。
    pub fn new(event_publisher: EventPublisher) -> Self {
        Self { event_publisher }
    }
}

#[async_trait::async_trait]
impl Consumer<ToolCallRequest> for ToolNode {
    async fn consume(
        &self,
        event: Arc<ToolCallRequest>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        let tool_res: Vec<ChatMessage> = event
            .tool_calls
            .iter()
            .map(|tool_call| {
                ChatMessage::tool_result(
                    ToolCallId::new(tool_call.id.clone()),
                    "tool execution not implemented",
                )
            })
            .collect();

        let response = envelope.reply(ToolCallResponse { tool_res });
        self.event_publisher.publish(response).await?;
        Ok(())
    }
}
