use std::any::{Any, TypeId};
use std::sync::Arc;
use std::time::SystemTime;

use super::id::{AgentStateId, EventId, RunId};

/// 事件处理链路的通用错误类型。
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// 事件所属的会话与执行范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventScope {
    /// 事件所属的 `AgentState` 会话；每个事件都必须归属于某个会话。
    pub state_id: AgentStateId,
    /// 父 agent 的会话标识，用于追踪子 agent 产生的行为。
    pub parent_state_id: Option<AgentStateId>,
    /// 本次执行尝试的标识；同一会话可以有多次 run。
    pub run_id: RunId,
}

impl EventScope {
    /// 为指定会话和 run 创建作用域。
    pub fn new(state_id: AgentStateId, run_id: RunId) -> Self {
        Self {
            state_id,
            parent_state_id: None,
            run_id,
        }
    }

    /// 标记该事件来自某个父 agent。
    pub fn with_parent(mut self, parent_state_id: AgentStateId) -> Self {
        self.parent_state_id = Some(parent_state_id);
        self
    }
}

/// 事件信封：在业务事件外携带统一的路由与因果元数据。
///
/// 总线只传递信封，消费者通过 [`Self::payload`] 拿到具体的业务事件。
/// `state_id` 是会话路由键，`correlation_id` 用于配对请求与结果，
/// `causation_id` 用于还原直接因果链。
#[derive(Clone)]
pub struct EventEnvelope {
    /// 事件全局唯一标识，用于去重和审计。
    pub event_id: EventId,
    /// 事件所属的会话与执行范围。
    pub scope: EventScope,
    /// 请求/响应事件的配对标识。
    pub correlation_id: Option<EventId>,
    /// 直接触发本事件的事件标识。
    pub causation_id: Option<EventId>,
    /// 同一 `state_id` 内单调递增的序号。
    pub sequence: u64,
    /// 事件产生时间。
    pub timestamp: SystemTime,
    /// 具体业务事件。
    pub payload: Arc<dyn Event>,
}

impl EventEnvelope {
    /// 用具体事件创建信封。
    pub fn new<E: Event>(scope: EventScope, event: E) -> Self {
        Self::from_arc(scope, Arc::new(event))
    }

    /// 用已经共享的事件创建信封。
    pub fn from_arc(scope: EventScope, payload: Arc<dyn Event>) -> Self {
        Self {
            event_id: EventId::new(),
            scope,
            correlation_id: None,
            causation_id: None,
            sequence: 0,
            timestamp: SystemTime::now(),
            payload,
        }
    }

    /// 设置请求/响应配对标识。
    pub fn with_correlation(mut self, correlation_id: EventId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    /// 设置直接原因事件。
    pub fn with_causation(mut self, causation_id: EventId) -> Self {
        self.causation_id = Some(causation_id);
        self
    }

    /// 设置会话内序号；序号通常由状态存储统一分配。
    pub fn with_sequence(mut self, sequence: u64) -> Self {
        self.sequence = sequence;
        self
    }

    /// 针对当前事件构造一个响应信封。
    ///
    /// 响应会继承当前作用域，并把 `correlation_id` 指向请求所属的关联链，
    /// 同时把 `causation_id` 设为当前事件。
    pub fn reply<E: Event>(&self, event: E) -> Self {
        Self::new(self.scope, event)
            .with_correlation(self.correlation_id.unwrap_or(self.event_id))
            .with_causation(self.event_id)
    }

    /// 业务事件名称。
    pub fn event_name(&self) -> &'static str {
        self.payload.event_name()
    }

    /// 业务事件类型 ID，分发器用它查找订阅者。
    pub fn event_type_id(&self) -> TypeId {
        self.payload.event_type_id()
    }
}

impl std::fmt::Debug for EventEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventEnvelope")
            .field("event_id", &self.event_id)
            .field("event_name", &self.event_name())
            .field("scope", &self.scope)
            .field("correlation_id", &self.correlation_id)
            .field("causation_id", &self.causation_id)
            .field("sequence", &self.sequence)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}

/// 所有事件的基类。
///
/// 下游只需为空实现即可声明一个新事件类型：
///
/// ```text
/// struct OrderCreated { id: u64 }
/// impl Event for OrderCreated {}
/// ```
///
/// `Any` 让分发器可以把 `Arc<dyn Event>` downcast 回具体类型；
/// `event_name` 与 `event_type_id` 用于日志、死信记录和 O(1) 路由。
pub trait Event: Any + Send + Sync + 'static {
    /// 事件名称，默认使用具体类型的完整路径。
    fn event_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// 事件类型 ID，分发器用它查找订阅者。
    fn event_type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }
}

/// 事件系统运行期的错误。
#[derive(Debug)]
pub enum EventError {
    /// 生产者拉取事件失败，`Dispatcher::run` 会因此终止。
    Producer {
        /// 失败的生产者名称。
        producer: &'static str,
        /// 生产者返回的原始错误。
        source: BoxError,
    },
    /// 事件 downcast 失败；正常情况下按 `TypeId` 路由不会出现。
    EventTypeMismatch {
        /// 订阅者期望的事件类型。
        expected: &'static str,
        /// 实际收到的事件类型。
        actual: &'static str,
    },
    /// 订阅者的接收通道已经关闭，事件无法投递。
    SubscriberClosed {
        /// 已关闭的消费者名称。
        consumer: &'static str,
    },
    /// 事件队列已关闭，无法继续发布事件。
    BusClosed,
}

impl std::fmt::Display for EventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Producer { producer, source } => {
                write!(f, "producer `{producer}` failed: {source}")
            }
            Self::EventTypeMismatch { expected, actual } => {
                write!(
                    f,
                    "event type mismatch: expected `{expected}`, got `{actual}`"
                )
            }
            Self::SubscriberClosed { consumer } => {
                write!(f, "subscriber `{consumer}` is closed")
            }
            Self::BusClosed => write!(f, "event bus is closed"),
        }
    }
}

impl std::error::Error for EventError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Producer { source, .. } => Some(source.as_ref()),
            Self::EventTypeMismatch { .. } | Self::SubscriberClosed { .. } | Self::BusClosed => {
                None
            }
        }
    }
}
