//! 基于进程内读写锁的状态仓储。

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_trait::async_trait;

use crate::domain::agent::state::{AgentState, AgentStateError, AgentStateRepository};
use crate::domain::id::AgentStateId;

/// 内存状态仓储，适合测试和单进程原型。
#[derive(Default)]
pub struct InMemoryRepository {
    states: RwLock<HashMap<AgentStateId, Arc<AgentState>>>,
}

impl InMemoryRepository {
    /// 创建空仓储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前保存的状态数量。
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// 当前是否没有状态。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read(&self) -> RwLockReadGuard<'_, HashMap<AgentStateId, Arc<AgentState>>> {
        self.states
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, HashMap<AgentStateId, Arc<AgentState>>> {
        self.states
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl AgentStateRepository for InMemoryRepository {
    async fn create(&self, state: Arc<AgentState>) -> Result<(), AgentStateError> {
        let mut states = self.write();
        if states.contains_key(&state.state_id) {
            return Err(AgentStateError::AlreadyExists(state.state_id));
        }
        states.insert(state.state_id, state);
        Ok(())
    }

    async fn get(
        &self,
        state_id: AgentStateId,
    ) -> Result<Option<Arc<AgentState>>, AgentStateError> {
        Ok(self.read().get(&state_id).cloned())
    }

    async fn update(&self, state: Arc<AgentState>) -> Result<(), AgentStateError> {
        let mut states = self.write();
        let entry = states
            .get_mut(&state.state_id)
            .ok_or(AgentStateError::NotFound(state.state_id))?;
        *entry = state;
        Ok(())
    }

    async fn delete(&self, state_id: AgentStateId) -> Result<bool, AgentStateError> {
        Ok(self.write().remove(&state_id).is_some())
    }
}
