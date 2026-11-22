//! 服务层：在节点和基础设施之间提供状态写入串行化能力。
//!
//! 节点通过 [`mailbox::AgentMailbox`] 提交状态变更，不直接接触状态仓储。

pub mod mailbox;
