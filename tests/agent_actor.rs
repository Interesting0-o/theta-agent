use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use edgequake_llm::{ChatRole, FunctionCall, ToolCall};
use theta::core::dispatcher::{Dispatcher, EventPublisher};
use theta::domain::agent::events::{
    LLMCompleted, LLMRequest, TokenUsage, ToolCallRequest, ToolCallResponse, UserInput,
};
use theta::domain::consumer::Consumer;
use theta::domain::event::{BoxError, EventEnvelope, EventScope};
use theta::domain::id::{AgentStateId, RunId};
use theta::nodes::agent_actor::AgentActor;
use theta::nodes::tool_node::ToolNode;
use theta::service::mailbox::AgentMailbox;

fn sample_tool_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "echo".to_string(),
            arguments: "{}".to_string(),
        },
        thought_signature: None,
    }
}

struct ScriptedLlm {
    publisher: EventPublisher,
    calls: AtomicUsize,
}

#[async_trait]
impl Consumer<LLMRequest> for ScriptedLlm {
    async fn consume(
        &self,
        _event: Arc<LLMRequest>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        let completed = if turn == 0 {
            LLMCompleted {
                content: String::new(),
                tool_calls: vec![sample_tool_call("call_1")],
                finish_reason: Some("tool_calls".to_string()),
                model: "test".to_string(),
                usage: TokenUsage::default(),
            }
        } else {
            LLMCompleted {
                content: "done".to_string(),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_string()),
                model: "test".to_string(),
                usage: TokenUsage::default(),
            }
        };
        self.publisher.publish(envelope.reply(completed)).await?;
        Ok(())
    }
}

#[derive(Default)]
struct ToolRequestRecorder {
    seen: Mutex<Vec<EventEnvelope>>,
}

#[async_trait]
impl Consumer<ToolCallRequest> for ToolRequestRecorder {
    async fn consume(
        &self,
        _event: Arc<ToolCallRequest>,
        envelope: &EventEnvelope,
    ) -> Result<(), BoxError> {
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

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
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let recorder = Arc::new(RequestRecorder::default());

    dispatcher.subscribe::<LLMRequest, _>(Arc::clone(&recorder));
    dispatcher.subscribe::<UserInput, _>(AgentActor::new(mailbox.clone()));

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

    let state = mailbox
        .state(scope.state_id)
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
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let recorder = Arc::new(RequestRecorder::default());

    dispatcher.subscribe::<LLMRequest, _>(Arc::clone(&recorder));
    dispatcher.subscribe::<UserInput, _>(AgentActor::new(mailbox.clone()));

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

    let state = mailbox
        .state(scope.state_id)
        .await
        .unwrap()
        .expect("state saved");
    assert_eq!(state.messages.len(), 2);
    assert_eq!(recorder.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn llm_completed_with_tool_calls_persists_assistant_then_publishes_request() {
    let dispatcher = Dispatcher::new();
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let recorder = Arc::new(ToolRequestRecorder::default());
    let scope = EventScope::new(AgentStateId::new(), RunId::new());

    mailbox.create_state(scope.state_id).await.unwrap();

    dispatcher.subscribe::<ToolCallRequest, _>(Arc::clone(&recorder));
    dispatcher.subscribe::<LLMCompleted, _>(AgentActor::new(mailbox.clone()));

    dispatcher
        .publish(EventEnvelope::new(
            scope,
            LLMCompleted {
                content: String::new(),
                tool_calls: vec![sample_tool_call("call_1")],
                finish_reason: Some("tool_calls".to_string()),
                model: "test".to_string(),
                usage: TokenUsage::default(),
            },
        ))
        .await
        .unwrap();
    dispatcher.flush().await;

    let state = mailbox
        .state(scope.state_id)
        .await
        .unwrap()
        .expect("state saved");
    assert_eq!(state.messages.len(), 1);
    let tool_calls = state.messages[0]
        .tool_calls
        .as_ref()
        .expect("assistant tool calls persisted");
    assert_eq!(tool_calls[0].id, "call_1");

    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].scope, scope);
}

#[tokio::test]
async fn tool_call_loop_appends_results_before_next_llm_request() {
    let dispatcher = Dispatcher::new();
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let scope = EventScope::new(AgentStateId::new(), RunId::new());
    let actor = Arc::new(AgentActor::new(mailbox.clone()));
    let llm = Arc::new(ScriptedLlm {
        publisher: dispatcher.publisher(),
        calls: AtomicUsize::new(0),
    });

    dispatcher.subscribe::<UserInput, _>(Arc::clone(&actor));
    dispatcher.subscribe::<LLMCompleted, _>(Arc::clone(&actor));
    dispatcher.subscribe::<ToolCallResponse, _>(Arc::clone(&actor));
    dispatcher.subscribe::<LLMRequest, _>(Arc::clone(&llm));
    dispatcher.subscribe::<ToolCallRequest, _>(ToolNode::new(dispatcher.publisher()));

    dispatcher
        .publish(EventEnvelope::new(
            scope,
            UserInput {
                input: "hi".to_string(),
            },
        ))
        .await
        .unwrap();
    dispatcher.flush().await;

    assert_eq!(llm.calls.load(Ordering::SeqCst), 2);

    let state = mailbox
        .state(scope.state_id)
        .await
        .unwrap()
        .expect("state saved");
    let roles: Vec<ChatRole> = state.messages.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        vec![
            ChatRole::User,
            ChatRole::Assistant,
            ChatRole::Tool,
            ChatRole::Assistant,
        ]
    );
    assert_eq!(
        state.messages[1].tool_calls.as_ref().expect("tool calls")[0].id,
        "call_1"
    );
    assert_eq!(
        state.messages[2].tool_call_id.as_deref(),
        Some("call_1"),
        "tool result must follow the assistant tool call"
    );
    assert_eq!(state.messages[3].content, "done");
}

#[tokio::test]
async fn concurrent_agent_events_are_serialized_by_mailbox() {
    let dispatcher = Dispatcher::new();
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let scope = EventScope::new(AgentStateId::new(), RunId::new());
    let actor = Arc::new(AgentActor::new(mailbox.clone()));

    dispatcher.subscribe::<UserInput, _>(Arc::clone(&actor));
    dispatcher.subscribe::<LLMCompleted, _>(Arc::clone(&actor));

    mailbox.create_state(scope.state_id).await.unwrap();

    let input = EventEnvelope::new(
        scope,
        UserInput {
            input: "hello".to_string(),
        },
    );
    let completed = EventEnvelope::new(
        scope,
        LLMCompleted {
            content: "reply".to_string(),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_string()),
            model: "test".to_string(),
            usage: TokenUsage::default(),
        },
    );

    let (input, completed) = tokio::join!(dispatcher.publish(input), dispatcher.publish(completed));
    input.unwrap();
    completed.unwrap();
    dispatcher.flush().await;

    let state = mailbox
        .state(scope.state_id)
        .await
        .unwrap()
        .expect("state saved");
    assert_eq!(state.messages.len(), 2);
    assert!(
        state
            .messages
            .iter()
            .any(|message| message.role == ChatRole::User)
    );
    assert!(
        state
            .messages
            .iter()
            .any(|message| message.role == ChatRole::Assistant)
    );
}
