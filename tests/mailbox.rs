use edgequake_llm::ChatMessage;
use theta::core::dispatcher::Dispatcher;
use theta::domain::agent::state::AgentState;
use theta::domain::id::AgentStateId;
use theta::service::mailbox::{AgentMailbox, StateTransition};

#[tokio::test]
async fn concurrent_operations_on_same_state_are_serialized() {
    let dispatcher = Dispatcher::new();
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let state_id = AgentStateId::new();

    let first = mailbox.execute(state_id, move |current| {
        let mut state = current
            .cloned()
            .unwrap_or_else(|| AgentState::new(state_id));
        state.messages.push(ChatMessage::user("first"));
        Ok(StateTransition::new(state))
    });
    let second = mailbox.execute(state_id, move |current| {
        let mut state = current
            .cloned()
            .unwrap_or_else(|| AgentState::new(state_id));
        state.messages.push(ChatMessage::user("second"));
        Ok(StateTransition::new(state))
    });

    let (first, second) = tokio::join!(first, second);
    first.unwrap();
    second.unwrap();

    let state = mailbox.state(state_id).await.unwrap().expect("state saved");
    assert_eq!(state.messages.len(), 2);
    let contents: Vec<&str> = state
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert!(contents.contains(&"first"));
    assert!(contents.contains(&"second"));
}

#[tokio::test]
async fn different_states_use_independent_mailboxes() {
    let dispatcher = Dispatcher::new();
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let first_state = AgentStateId::new();
    let second_state = AgentStateId::new();

    let first = mailbox.execute(first_state, move |current| {
        let mut state = current
            .cloned()
            .unwrap_or_else(|| AgentState::new(first_state));
        state.messages.push(ChatMessage::user("first"));
        Ok(StateTransition::new(state))
    });
    let second = mailbox.execute(second_state, move |current| {
        let mut state = current
            .cloned()
            .unwrap_or_else(|| AgentState::new(second_state));
        state.messages.push(ChatMessage::user("second"));
        Ok(StateTransition::new(state))
    });

    let (first, second) = tokio::join!(first, second);
    first.unwrap();
    second.unwrap();

    let first = mailbox
        .state(first_state)
        .await
        .unwrap()
        .expect("first state saved");
    let second = mailbox
        .state(second_state)
        .await
        .unwrap()
        .expect("second state saved");
    assert_eq!(first.messages.len(), 1);
    assert_eq!(first.messages[0].content, "first");
    assert_eq!(second.messages.len(), 1);
    assert_eq!(second.messages[0].content, "second");
}

#[tokio::test]
async fn failed_operation_does_not_commit_state() {
    let dispatcher = Dispatcher::new();
    let mailbox = AgentMailbox::in_memory(dispatcher.publisher());
    let state_id = AgentStateId::new();

    mailbox.create_state(state_id).await.unwrap();

    let result = mailbox
        .execute(state_id, |_| Err("reject this transition".into()))
        .await;
    assert!(result.is_err());

    let state = mailbox.state(state_id).await.unwrap().expect("state saved");
    assert!(state.messages.is_empty());
}
