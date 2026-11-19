use crate::config::config;
use crate::domain::agent::events::UserInput;
use crate::domain::consumer::Consumer;
use crate::domain::event::{BoxError, EventEnvelope};
use edgequake_llm::{ChatMessage, LLMProvider, OpenAIProvider};
use secrecy::ExposeSecret;
use std::sync::Arc;

pub struct LLMNode {
    llm_provider: Arc<dyn LLMProvider>,
}

impl LLMNode {
    pub fn new() -> Self {
        let provider = OpenAIProvider::new(config().chat_model_api_key.expose_secret())
            .with_base_url(&config().chat_model_base_url)
            .with_model(&config().chat_model_name);
        Self {
            llm_provider: Arc::new(provider),
        }
    }
}

#[async_trait::async_trait]
impl Consumer<UserInput> for LLMNode {
    async fn consume(
        &self,
        event: Arc<UserInput>,
        _envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        let messages = vec![ChatMessage::user(event.input.clone())];
        let response = self.llm_provider.chat(&messages, None).await?;
        println!("{}", response.content);
        Ok(())
    }
}
