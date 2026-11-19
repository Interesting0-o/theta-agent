use std::sync::LazyLock;

use secrecy::SecretString;

/// 应用全局配置。
///
/// 首次访问 [`config`] 时加载 `.env` 并完成校验，之后复用同一份配置。
pub struct Config {
    /// 应用版本。
    pub version: String,
    /// 主事件队列容量。
    pub main_queue_capacity: usize,
    /// 每个订阅者接收通道的容量。
    pub subscriber_queue_capacity: usize,
    /// 死信队列容量。
    pub dead_letter_capacity: usize,
    /// 对话模型名称，对应 `CHAT_MODEL_NAME`。
    pub chat_model_name: String,
    /// 对话模型 API key，对应 `CHAT_MODEL_API_KEY`；本地服务可为空。
    ///
    /// 使用 [`SecretString`] 包裹，调试输出会被遮盖，取值需显式调用 `expose_secret()`。
    pub chat_model_api_key: SecretString,
    /// 对话模型 API 地址，对应 `CHAT_MODEL_BASE_URL`。
    pub chat_model_base_url: String,
    /// 对话模型提供方，对应 `CHAT_MODEL_PROVIDER`；为空表示不指定。
    pub chat_model_provider: String,
}

/// 将容量限制为至少 1，避免通道容量为 0 时 panic。
fn capacity(value: usize) -> usize {
    value.max(1)
}

/// 读取环境变量；未设置时返回空字符串。
fn env_string(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn load_config() -> Config {
    dotenv::dotenv().ok();

    Config {
        version: "0.0.1".to_string(),
        main_queue_capacity: capacity(1024),
        subscriber_queue_capacity: capacity(1024),
        dead_letter_capacity: capacity(1024),
        chat_model_name: env_string("CHAT_MODEL_NAME"),
        chat_model_api_key: SecretString::from(env_string("CHAT_MODEL_API_KEY")),
        chat_model_base_url: env_string("CHAT_MODEL_BASE_URL"),
        chat_model_provider: env_string("CHAT_MODEL_PROVIDER"),
    }
}

static CONFIG: LazyLock<Config> = LazyLock::new(load_config);

/// 获取进程内唯一的全局配置。
pub fn config() -> &'static Config {
    &CONFIG
}
