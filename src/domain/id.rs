use std::fmt;

use uuid::Uuid;

/// 生成一个基于 UUID v4 的标识类型。
macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            /// 随机生成一个新的标识。
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// 返回底层 UUID，便于写入数据库或日志。
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_id! {
    /// 事件的全局唯一标识，用于去重、审计和因果链。
    EventId
}

uuid_id! {
    /// 一份 `AgentState` 会话的标识，也是事件的分区键。
    AgentStateId
}

uuid_id! {
    /// 一次执行尝试的标识；同一个 `AgentState` 可以有多次 run。
    RunId
}

/// 模型返回的工具调用标识，用于把工具请求与结果配对。
///
/// 与 [`EventId`] 等本地标识不同，这个值由 provider 生成（例如
/// `call_abc123`），必须原样保留并回传给模型，不能重新生成。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// 用 provider 返回的原始标识创建。
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// 为本地发起的工具调用生成新标识。
    ///
    /// provider 返回的调用必须用 [`Self::new`] 保留原值；重放历史时不能重新生成。
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// 返回原始标识。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 取出内部字符串。
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ToolCallId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ToolCallId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<ToolCallId> for String {
    fn from(value: ToolCallId) -> Self {
        value.0
    }
}

impl AsRef<str> for ToolCallId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
