use std::any::{Any, TypeId};

/// 事件处理链路的通用错误类型。
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

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
