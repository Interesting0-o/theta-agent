use std::sync::Arc;

use edgequake_llm::ChatMessage;
use theta::domain::agent::state::{AgentState, AgentStateError, AgentStateRepository};
use theta::domain::id::AgentStateId;
use theta::infrastructure::in_memory_repo::InMemoryRepository;

fn state(state_id: AgentStateId) -> Arc<AgentState> {
    Arc::new(AgentState::new(state_id))
}

#[tokio::test]
async fn create_and_get_roundtrip() {
    let repo = InMemoryRepository::new();
    let state_id = AgentStateId::new();

    repo.create(state(state_id)).await.unwrap();

    let loaded = repo.get(state_id).await.unwrap().expect("state exists");
    assert_eq!(loaded.state_id, state_id);
    assert!(loaded.messages.is_empty());
    assert_eq!(repo.len(), 1);
}

#[tokio::test]
async fn create_rejects_duplicate_ids() {
    let repo = InMemoryRepository::new();
    let state_id = AgentStateId::new();

    repo.create(state(state_id)).await.unwrap();
    let error = repo.create(state(state_id)).await.unwrap_err();

    assert!(matches!(
        error,
        AgentStateError::AlreadyExists(existing) if existing == state_id
    ));
    assert_eq!(repo.len(), 1);
}

#[tokio::test]
async fn update_replaces_existing_state() {
    let repo = InMemoryRepository::new();
    let state_id = AgentStateId::new();

    repo.create(state(state_id)).await.unwrap();

    let mut updated = AgentState::new(state_id);
    updated.messages.push(ChatMessage::user("hello"));
    repo.update(Arc::new(updated)).await.unwrap();

    let loaded = repo.get(state_id).await.unwrap().unwrap();
    assert_eq!(loaded.messages.len(), 1);
}

#[tokio::test]
async fn update_rejects_missing_state() {
    let repo = InMemoryRepository::new();
    let state_id = AgentStateId::new();

    let error = repo.update(state(state_id)).await.unwrap_err();

    assert!(matches!(
        error,
        AgentStateError::NotFound(missing) if missing == state_id
    ));
}

#[tokio::test]
async fn get_missing_state_returns_none() {
    let repo = InMemoryRepository::new();

    assert!(repo.get(AgentStateId::new()).await.unwrap().is_none());
    assert!(repo.is_empty());
}

#[tokio::test]
async fn delete_reports_whether_state_existed() {
    let repo = InMemoryRepository::new();
    let state_id = AgentStateId::new();

    repo.create(state(state_id)).await.unwrap();

    assert!(repo.delete(state_id).await.unwrap());
    assert!(!repo.delete(state_id).await.unwrap());
    assert!(repo.get(state_id).await.unwrap().is_none());
    assert!(repo.is_empty());
}
