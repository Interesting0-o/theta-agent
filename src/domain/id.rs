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
