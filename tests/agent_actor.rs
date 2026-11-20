use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use theta::core::dispatcher::Dispatcher;
use theta::domain::agent::events::{LLMRequest, UserInput};
use theta::domain::agent::state::AgentStateRepository;
use theta::domain::consumer::Consumer;
use theta::domain::event::{BoxError, EventEnvelope, EventScope};
use theta::domain::id::{AgentStateId, RunId};
use theta::infrastructure::in_memory_repo::InMemoryRepository;
use theta::nodes::agent_actor::AgentActor;

#[derive(Default)]
struct RequestRecorder {
    seen: Mutex<Vec<EventEnvelope>>,
}

#[async_trait]
impl Consumer<LLMRequest> for RequestRecorder {
    async fn consume(
        &self,
        _event: Arc<LLMRequest>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

#[tokio::test]
async fn user_input_updates_state_and_publishes_llm_request() {
    let dispatcher = Dispatcher::new();
    let repo: Arc<dyn AgentStateRepository> = Arc::new(InMemoryRepository::new());
    let recorder = Arc::new(RequestRecorder::default());

    dispatcher.subscribe::<LLMRequest, _>(Arc::clone(&recorder));
    dispatcher
        .subscribe::<UserInput, _>(AgentActor::new(Arc::clone(&repo), dispatcher.publisher()));

    let scope = EventScope::new(AgentStateId::new(), RunId::new());
    let input = EventEnvelope::new(
        scope,
        UserInput {
            input: "hello".to_string(),
        },
    );
    let input_id = input.event_id;

    dispatcher.publish(input).await.unwrap();
    dispatcher.flush().await;

    let state = repo
        .get(scope.state_id)
        .await
        .unwrap()
        .expect("state saved");
    assert_eq!(state.messages.len(), 1);

    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].scope, scope);
    assert_eq!(seen[0].correlation_id, Some(input_id));
    assert_eq!(seen[0].causation_id, Some(input_id));
}

#[tokio::test]
async fn second_input_appends_to_existing_state() {
    let dispatcher = Dispatcher::new();
    let repo: Arc<dyn AgentStateRepository> = Arc::new(InMemoryRepository::new());
    let recorder = Arc::new(RequestRecorder::default());

    dispatcher.subscribe::<LLMRequest, _>(Arc::clone(&recorder));
    dispatcher
        .subscribe::<UserInput, _>(AgentActor::new(Arc::clone(&repo), dispatcher.publisher()));

    let scope = EventScope::new(AgentStateId::new(), RunId::new());
    for input in ["first", "second"] {
        dispatcher
            .publish(EventEnvelope::new(
                scope,
                UserInput {
                    input: input.to_string(),
                },
            ))
            .await
            .unwrap();
    }
    dispatcher.flush().await;

    let state = repo
        .get(scope.state_id)
        .await
        .unwrap()
        .expect("state saved");
    assert_eq!(state.messages.len(), 2);
    assert_eq!(recorder.seen.lock().unwrap().len(), 2);
}
