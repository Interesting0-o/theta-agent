//! theta 的异步事件系统。
//!
//! - [`domain`]：事件、消费者、生产者等核心抽象。
//! - [`core`]：事件队列、分发器、死信队列等运行时基础设施。
//! - [`service`]：封装基础设施，向节点提供状态写入 mailbox。
//! - [`nodes`]：消费事件并驱动流程的 Agent、LLM、工具等节点。

pub use async_trait::async_trait;

mod config;
pub mod core;
pub mod domain;
pub mod infrastructure;
pub mod nodes;
pub mod service;
