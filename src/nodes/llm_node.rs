use crate::config::config;
use crate::core::dispatcher::EventPublisher;
use crate::domain::agent::events::{LLMCompleted, LLMRequest, TokenUsage};
use crate::domain::consumer::Consumer;
use crate::domain::event::{BoxError, EventEnvelope};
use edgequake_llm::{LLMProvider, OpenAIProvider};
use secrecy::ExposeSecret;
use std::sync::Arc;

/// 无状态的 LLM 节点：消费请求，调用模型，发布完成事件。
pub struct LLMNode {
    llm_provider: Arc<dyn LLMProvider>,
    event_publisher: EventPublisher,
}

impl LLMNode {
    /// 按全局配置创建厂商客户端，并注入用于回发事件的总线句柄。
    pub fn new(event_publisher: EventPublisher) -> Self {
        let provider = OpenAIProvider::new(config().chat_model_api_key.expose_secret())
            .with_base_url(&config().chat_model_base_url)
            .with_model(&config().chat_model_name);
        Self::with_provider(event_publisher, Arc::new(provider))
    }

    /// 使用外部提供的 provider 创建节点，便于测试或切换实现。
    pub fn with_provider(
        event_publisher: EventPublisher,
        llm_provider: Arc<dyn LLMProvider>,
    ) -> Self {
        Self {
            llm_provider,
            event_publisher,
        }
    }
}

#[async_trait::async_trait]
impl Consumer<LLMRequest> for LLMNode {
    async fn consume(
        &self,
        event: Arc<LLMRequest>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        let response = self.llm_provider.chat(&event.messages, None).await?;

        let completed = envelope.reply(LLMCompleted {
            content: response.content,
            tool_calls: response.tool_calls,
            finish_reason: response.finish_reason,
            model: response.model,
            usage: TokenUsage {
                prompt_tokens: response.prompt_tokens,
                completion_tokens: response.completion_tokens,
                total_tokens: response.total_tokens,
            },
        });
        self.event_publisher.publish(completed).await?;
        Ok(())
    }
}
