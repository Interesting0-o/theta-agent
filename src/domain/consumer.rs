use std::sync::Arc;

use async_trait::async_trait;

use super::event::{BoxError, Event};

/// 事件消费者。
///
/// 消费者对事件类型是泛型的，同一个类型可以为多种事件分别实现 [`Consumer`]，
/// 然后分别订阅。分发器为每个订阅维护独立队列，因此同一订阅的事件串行处理、
/// 保证顺序；不同订阅之间可以并发。
///
/// ```text
/// impl Consumer<Ping> for Handler { ... }
/// impl Consumer<Pong> for Handler { ... }
///
/// dispatcher.subscribe::<Ping, _>(handler.clone());
/// dispatcher.subscribe::<Pong, _>(handler);
/// ```
#[async_trait]
pub trait Consumer<E: Event>: Send + Sync + 'static {
    /// 消费者名称，默认使用类型完整路径，用于统计和死信记录。
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// 处理一个事件。
    ///
    /// 返回 `Err` 时事件会进入死信队列，不会影响其他消费者。
    async fn consume(&self, event: Arc<E>) -> Result<(), BoxError>;
}

/// 让 `Arc<C>` 也能直接注册为消费者，便于在多个地方共享同一个实例。
#[async_trait]
impl<E: Event, C: Consumer<E> + ?Sized> Consumer<E> for Arc<C> {
    fn name(&self) -> &'static str {
        (**self).name()
    }

    async fn consume(&self, event: Arc<E>) -> Result<(), BoxError> {
        (**self).consume(event).await
    }
}
