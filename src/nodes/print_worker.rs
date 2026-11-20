use std::sync::Arc;

use crate::domain::agent::events::LLMCompleted;
use crate::domain::consumer::Consumer;
use crate::domain::event::{BoxError, EventEnvelope};

/// 把 LLM 完成事件输出到标准输出的下游消费者。
#[derive(Default)]
pub struct PrintWorker;

impl PrintWorker {
    /// 创建输出节点。
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Consumer<LLMCompleted> for PrintWorker {
    async fn consume(
        &self,
        event: Arc<LLMCompleted>,
        _envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        println!("[Agent]:\n{}\n", event.content);
        Ok(())
    }
}
