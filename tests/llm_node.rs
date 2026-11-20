use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use edgequake_llm::{ChatMessage, MockProvider};
use theta::core::dispatcher::Dispatcher;
use theta::domain::agent::events::{LLMCompleted, LLMRequest};
use theta::domain::consumer::Consumer;
use theta::domain::event::{BoxError, EventEnvelope, EventScope};
use theta::domain::id::{AgentStateId, RunId};
use theta::nodes::llm_node::LLMNode;

#[derive(Default)]
struct CompletionRecorder {
    seen: Mutex<Vec<(Arc<LLMCompleted>, EventEnvelope)>>,
}

#[async_trait]
impl Consumer<LLMCompleted> for CompletionRecorder {
    async fn consume(
        &self,
        event: Arc<LLMCompleted>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        self.seen.lock().unwrap().push((event, envelope.clone()));
        Ok(())
    }
}

#[tokio::test]
async fn llm_node_publishes_completed_event_without_history() {
    let dispatcher = Dispatcher::new();
    let recorder = Arc::new(CompletionRecorder::default());
    dispatcher.subscribe::<LLMCompleted, _>(Arc::clone(&recorder));

    let provider = Arc::new(MockProvider::new());
    provider.add_response("hello from mock").await;
    dispatcher.subscribe::<LLMRequest, _>(LLMNode::with_provider(dispatcher.publisher(), provider));

    let scope = EventScope::new(AgentStateId::new(), RunId::new());
    let request = EventEnvelope::new(
        scope,
        LLMRequest {
            messages: vec![ChatMessage::user("hi")],
        },
    );
    let request_id = request.event_id;

    dispatcher.publish(request).await.unwrap();
    dispatcher.flush().await;

    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);

    let (completed, envelope) = &seen[0];
    assert_eq!(completed.content, "hello from mock");
    assert!(completed.tool_calls.is_empty());
    assert_eq!(completed.model, "mock-model");
    assert_eq!(completed.usage, Default::default());

    assert_eq!(envelope.scope, scope);
    assert_eq!(envelope.correlation_id, Some(request_id));
    assert_eq!(envelope.causation_id, Some(request_id));
}
