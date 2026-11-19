//! Agent 领域事件。

use crate::domain::event::Event;

/// 用户输入事件，由 CLI 或上层入口发布给 agent。
pub struct UserInput {
    /// 用户输入的原始文本。
    pub input: String,
}

impl Event for UserInput {
    fn event_name(&self) -> &'static str {
        "theta.agent.user_input.v1"
    }
}
