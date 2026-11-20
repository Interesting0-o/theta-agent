//! theta 的异步事件系统。
//!
//! - [`domain`]：事件、消费者、生产者等核心抽象。
//! - [`core`]：事件队列、分发器、死信队列等运行时基础设施。

pub use async_trait::async_trait;

mod config;
pub mod core;
pub mod domain;
pub mod infrastructure;
pub mod nodes;
