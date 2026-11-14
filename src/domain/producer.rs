use std::sync::Arc;

use async_trait::async_trait;

use super::event::{BoxError, Event};

/// 事件生产者。
///
/// 由 [`crate::core::dispatcher::Dispatcher::run`] 循环拉取驱动。
/// 返回 `Ok(None)` 表示生产结束；返回 `Err` 会终止 `run`，
/// 但已经发布的事件仍会继续被消费。
#[async_trait]
pub trait Producer: Send {
    /// 生产者名称，默认使用类型完整路径，用于错误上下文。
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// 拉取下一个事件；`Ok(None)` 表示没有更多事件。
    async fn next(&mut self) -> Result<Option<Arc<dyn Event>>, BoxError>;
}
