//! 按 `AgentStateId` 分区的状态写入 mailbox。
//!
//! 每个 `state_id` 对应一条有界队列和一个消费任务。所有状态变更都通过
//! [`AgentMailbox::execute`] 入队，由唯一的消费任务按顺序执行：
//!
//! ```text
//! 读取状态 -> 应用操作 -> 提交仓储 -> 发布副作用事件
//! ```
//!
//! 同一状态不会出现两个并发的读改写，状态机逻辑仍由上层节点提供。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::config::config;
use crate::core::dispatcher::EventPublisher;
use crate::domain::agent::state::{AgentState, AgentStateError, AgentStateRepository};
use crate::domain::event::{BoxError, EventEnvelope};
use crate::domain::id::AgentStateId;
use crate::infrastructure::in_memory_repo::InMemoryRepository;

/// mailbox 关闭或提交校验失败。
#[derive(Debug)]
pub enum AgentMailboxError {
    /// 消费任务已经退出，无法继续提交。
    Closed,
    /// 操作需要一个已经存在的状态。
    MissingState(AgentStateId),
    /// 操作返回的状态与 mailbox 分区不一致。
    StateIdMismatch {
        /// mailbox 对应的状态标识。
        expected: AgentStateId,
        /// 操作返回的状态标识。
        actual: AgentStateId,
    },
    /// 创建状态时目标已经存在。
    AlreadyExists(AgentStateId),
    /// 状态仓储读写失败。
    State(AgentStateError),
}

impl fmt::Display for AgentMailboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "agent mailbox is closed"),
            Self::MissingState(state_id) => write!(f, "agent state `{state_id}` not found"),
            Self::StateIdMismatch { expected, actual } => {
                write!(
                    f,
                    "operation produced state `{actual}` for mailbox `{expected}`"
                )
            }
            Self::AlreadyExists(state_id) => {
                write!(f, "agent state `{state_id}` already exists")
            }
            Self::State(source) => write!(f, "agent state backend error: {source}"),
        }
    }
}

impl std::error::Error for AgentMailboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::State(source) => Some(source),
            Self::Closed
            | Self::MissingState(_)
            | Self::StateIdMismatch { .. }
            | Self::AlreadyExists(_) => None,
        }
    }
}

impl From<AgentStateError> for AgentMailboxError {
    fn from(value: AgentStateError) -> Self {
        Self::State(value)
    }
}

/// 一次状态变更的提交内容。
pub struct StateTransition {
    /// 变更后的完整状态。
    pub state: AgentState,
    /// 提交成功后需要发布的事件。
    pub effects: Vec<EventEnvelope>,
}

impl StateTransition {
    /// 创建只更新状态、不发布事件的变更。
    pub fn new(state: AgentState) -> Self {
        Self {
            state,
            effects: Vec::new(),
        }
    }

    /// 追加一个提交成功后发布的事件。
    pub fn with_effect(mut self, effect: EventEnvelope) -> Self {
        self.effects.push(effect);
        self
    }
}

type StateOperation =
    Box<dyn FnOnce(Option<&AgentState>) -> Result<StateTransition, BoxError> + Send + 'static>;

struct MailboxCommand {
    operation: StateOperation,
    reply: oneshot::Sender<Result<(), BoxError>>,
}

struct MailboxShared {
    repo: Arc<dyn AgentStateRepository>,
    publisher: EventPublisher,
    mailboxes: Mutex<HashMap<AgentStateId, mpsc::Sender<MailboxCommand>>>,
    capacity: usize,
}

/// 按状态分区的写入 mailbox 句柄。
///
/// clone 后共享同一组 mailbox；每个 `state_id` 只会创建一个消费任务。
#[derive(Clone)]
pub struct AgentMailbox {
    shared: Arc<MailboxShared>,
}

impl AgentMailbox {
    /// 使用内置内存仓储创建 mailbox，适合原型和测试。
    pub fn in_memory(publisher: EventPublisher) -> Self {
        Self::new(Arc::new(InMemoryRepository::new()), publisher)
    }

    /// 使用指定仓储创建 mailbox。
    pub fn new(repo: Arc<dyn AgentStateRepository>, publisher: EventPublisher) -> Self {
        Self::with_capacity(repo, publisher, config().mailbox_queue_capacity)
    }

    /// 使用指定仓储和每个状态的队列容量创建 mailbox。
    pub fn with_capacity(
        repo: Arc<dyn AgentStateRepository>,
        publisher: EventPublisher,
        capacity: usize,
    ) -> Self {
        Self {
            shared: Arc::new(MailboxShared {
                repo,
                publisher,
                mailboxes: Mutex::new(HashMap::new()),
                capacity: capacity.max(1),
            }),
        }
    }

    /// 读取状态快照；不存在时返回 `None`。
    pub async fn state(
        &self,
        state_id: AgentStateId,
    ) -> Result<Option<Arc<AgentState>>, AgentMailboxError> {
        self.shared
            .repo
            .get(state_id)
            .await
            .map_err(AgentMailboxError::State)
    }

    /// 创建空状态；标识已存在时返回 [`AgentMailboxError::AlreadyExists`]。
    pub async fn create_state(&self, state_id: AgentStateId) -> Result<(), BoxError> {
        self.execute(state_id, move |current| {
            if current.is_some() {
                return Err(AgentMailboxError::AlreadyExists(state_id).into());
            }
            Ok(StateTransition::new(AgentState::new(state_id)))
        })
        .await
    }

    /// 在指定状态的 mailbox 中串行执行一次状态变更。
    ///
    /// 操作返回后，mailbox 会提交状态并发布 `effects`，全部完成后才向调用方返回。
    pub async fn execute<F>(&self, state_id: AgentStateId, operation: F) -> Result<(), BoxError>
    where
        F: FnOnce(Option<&AgentState>) -> Result<StateTransition, BoxError> + Send + 'static,
    {
        // 拿到此次会话的通道发送端
        let sender = self.sender(state_id);
        let (reply, reply_rx) = oneshot::channel();
        sender
            .send(MailboxCommand {
                operation: Box::new(operation),
                reply,
            })
            .await
            .map_err(|_| AgentMailboxError::Closed)?;
        reply_rx.await.map_err(|_| AgentMailboxError::Closed)?
    }

    fn sender(&self, state_id: AgentStateId) -> mpsc::Sender<MailboxCommand> {
        let mut mailboxes = self
            .shared
            .mailboxes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sender) = mailboxes.get(&state_id) {

            // 如果找到会话的通道直接使用
            return sender.clone();
        }

        // 如果没有找到会话的通道就重建一个
        let (sender, receiver) = mpsc::channel(self.shared.capacity);
        tokio::spawn(run_mailbox(
            state_id,
            receiver,
            Arc::clone(&self.shared.repo),
            self.shared.publisher.clone(),
        ));
        // 登记到 共享MailboxShared中
        mailboxes.insert(state_id, sender.clone());
        sender
    }
}

async fn run_mailbox(
    state_id: AgentStateId,
    mut receiver: mpsc::Receiver<MailboxCommand>,
    repo: Arc<dyn AgentStateRepository>,
    publisher: EventPublisher,
) {
    // Worker 守护队列
    while let Some(command) = receiver.recv().await {
        let MailboxCommand { operation, reply } = command;
        let result = apply_command(state_id, operation, &repo, &publisher).await;
        let _ = reply.send(result);
    }
}

async fn apply_command(
    state_id: AgentStateId,
    operation: StateOperation,
    repo: &Arc<dyn AgentStateRepository>,
    publisher: &EventPublisher,
) -> Result<(), BoxError> {
    let current = repo.get(state_id).await?;
    
    let transition = operation(current.as_deref())?;

    if transition.state.state_id != state_id {
        return Err(AgentMailboxError::StateIdMismatch {
            expected: state_id,
            actual: transition.state.state_id,
        }
        .into());
    }

    let next = Arc::new(transition.state);
    if current.is_some() {
        repo.update(Arc::clone(&next)).await?;
    } else {
        repo.create(Arc::clone(&next)).await?;
    }

    for effect in transition.effects {
        publisher.publish(effect).await?;
    }
    Ok(())
}
