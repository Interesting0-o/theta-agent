# Command 系统设计（草案）

状态：设计草案，尚未实现。

相关文档：[Coding Agent 架构设计](./agent-architecture.md)、[基础设施调研结论](./infrastructure-findings.md)、[待决策清单](./open-decisions.md)。

## 1. 背景与目标

theta 已经有一套基于事件队列和 channel 的事件系统。接下来需要增加命令系统，用于 CLI 或 agent 执行 `init`、`build`、`run` 等请求/响应型操作。

命令与事件是两种不同的语义：

- 事件是通知，发布方不关心谁消费，也不等待结果。
- 命令是请求/响应，调用方关心参数解析、执行结果、错误和取消。

因此命令系统独立于事件系统，但命令执行过程中可以通过 `Dispatcher` 发布事件。

第一版目标：

- 提供 `Command` trait、`CommandContext`、`CommandRegistry`。
- 提供 `#[command("name")]` 属性宏。
- 命令通过显式 `registry.register(...)` 注册，与现有 `dispatcher.subscribe(...)` 的风格保持一致。
- 不引入全局静态注册表。

## 2. 设计约束

属性宏在编译期展开，无法获取运行期的 `CommandRegistry` 实例，也无法在 item 位置插入注册语句。因此 `#[command("init")]` 不能自动完成注册，只能生成：

- 命令包装类型；
- 构造该类型的工厂函数；
- `Command` trait 实现。

真正的注册仍由启动代码显式完成：

```rust
let mut registry = CommandRegistry::new();
registry.register(init_command())?;
```

这与当前显式构造消费者并调用 `dispatcher.subscribe(...)` 的模式一致。

如果未来需要自动发现，可以再引入 `inventory` 收集命令工厂，并在启动时调用 `register_all(&mut registry)`。第一版不做。

## 3. 核心抽象

### 3.1 Command trait

命令需要强类型参数和输出。为了支持不同的命令实现，使用关联类型：

```rust
#[async_trait]
pub trait Command: Send + Sync + 'static {
    type Args: DeserializeOwned + Send;
    type Output: Serialize + Send;

    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str { "" }
    fn aliases(&self) -> &'static [&'static str] { &[] }

    async fn execute(
        &self,
        ctx: &CommandContext,
        args: Self::Args,
    ) -> Result<Self::Output, BoxError>;
}
```

没有参数的命令使用 `type Args = ()`，没有输出的命令使用 `type Output = ()`。

### 3.2 类型擦除

`Command` 带关联类型，不能直接作为 trait object 存入 `HashMap`。注册表内部使用 `ErasedCommand` 做适配：

```rust
pub struct CommandMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub aliases: &'static [&'static str],
}

trait ErasedCommand: Send + Sync {
    fn meta(&self) -> CommandMeta;

    fn execute<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, BoxError>> + Send + 'a>>;
}

struct CommandAdapter<C: Command> {
    inner: C,
}

impl<C: Command> ErasedCommand for CommandAdapter<C> {
    fn meta(&self) -> CommandMeta {
        CommandMeta {
            name: self.inner.name(),
            description: self.inner.description(),
            aliases: self.inner.aliases(),
        }
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a CommandContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, BoxError>> + Send + 'a>> {
        Box::pin(async move {
            let args = serde_json::from_value::<C::Args>(args)?;
            let output = self.inner.execute(ctx, args).await?;
            Ok(serde_json::to_value(output)?)
        })
    }
}
```

选择 `serde_json::Value` 作为命令边界的中间表示：

- CLI 层可以从 clap 参数生成 JSON；
- agent 可以直接构造 JSON 参数；
- 命令内部仍然是强类型。

如果后续需要流式输出或富文本输出，再扩展 `CommandOutput`，第一版使用 `serde_json::Value`。

### 3.3 CommandContext

命令执行时需要访问运行时能力。第一版建议包含：

```rust
pub struct CommandContext {
    pub dispatcher: Dispatcher,
    pub working_dir: PathBuf,
    pub session_id: Option<String>,
}
```

后续可以增加：

- `CancellationToken`：支持用户中断和超时。
- 权限/沙箱策略。
- 预算（token、时间、费用）。
- 输出 sink（流式打印）。

`Dispatcher` 已经是可克隆句柄，命令可以通过 `ctx.dispatcher.publish(...)` 发布事件。

### 3.4 CommandRegistry

```rust
pub struct CommandRegistry {
    commands: HashMap<&'static str, Arc<dyn ErasedCommand>>,
    aliases: HashMap<&'static str, &'static str>,
}

impl CommandRegistry {
    pub fn new() -> Self;

    pub fn register<C: Command>(&mut self, command: C) -> Result<(), RegistryError>;

    pub fn get(&self, name: &str) -> Option<Arc<dyn ErasedCommand>>;

    pub fn names(&self) -> impl Iterator<Item = &'static str>;

    pub async fn execute(
        &self,
        name: &str,
        ctx: &CommandContext,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, CommandError>;
}
```

注册时需要检查：

- 命令名不能重复；
- 别名不能与已有命令名或其他别名冲突；
- 空名称、空别名直接拒绝。

第一版不支持子命令，命令名使用扁平命名（如 `init`、`build`、`agent.run`）。

## 4. `#[command]` 属性宏

### 4.1 语法

无参数命令：

```rust
#[command("init", description = "初始化项目", aliases = ["i"])]
pub async fn init(ctx: &CommandContext) -> Result<InitReport, BoxError> {
    // ...
}
```

带参数命令：

```rust
#[command("build")]
pub async fn build(
    ctx: &CommandContext,
    args: BuildArgs,
) -> Result<BuildReport, BoxError> {
    // ...
}
```

### 4.2 展开结果

以 `init` 为例，宏生成：

```rust
pub struct InitCommand;

pub const fn init_command() -> InitCommand {
    InitCommand
}

#[theta::async_trait]
impl theta::domain::command::Command for InitCommand {
    type Args = ();
    type Output = InitReport;

    fn name(&self) -> &'static str { "init" }
    fn description(&self) -> &'static str { "初始化项目" }
    fn aliases(&self) -> &'static [&'static str] { &["i"] }

    async fn execute(
        &self,
        ctx: &CommandContext,
        _args: (),
    ) -> Result<InitReport, BoxError> {
        init(ctx).await
    }
}
```

有参数时将 `Args` 替换为第二个参数的类型，并调用 `init(ctx, args).await`。

### 4.3 宏约束

第一版要求：

- 函数必须是 `async fn`；
- 函数不能有泛型参数；
- 第一个参数必须是 `&CommandContext`；
- 第二个参数可选，作为强类型 `Args`；
- 不能有第三个参数；
- 返回类型必须能写成 `Result<T, E>`，其中 `T: Serialize`、`E: Into<BoxError>`；
- 属性第一个参数必须是字符串字面量命令名；
- 支持 `description = "..."` 和 `aliases = ["..."]` 元数据。

宏无法在编译期检查 trait bound，最终由编译器报错。宏本身只做语法层面的校验并给出更清晰的错误信息。

### 4.4 命名规则

沿用消费者包装类型的命名方式：

- 函数 `init` 生成类型 `InitCommand`；
- 生成工厂函数 `init_command()`；
- 函数名转为 PascalCase 时去掉下划线，例如 `agent_run` -> `AgentRunCommand`；
- 如果生成名称冲突，由编译器报重复定义错误。

### 4.5 结构体/impl 形式

带状态的命令后续可以支持：

```rust
#[command("init")]
impl InitCommandHandler {
    async fn handle(
        &self,
        ctx: &CommandContext,
        args: InitArgs,
    ) -> Result<InitReport, BoxError> {
        // ...
    }
}
```

宏为 `InitCommandHandler` 生成 `Command` 实现，注册时由用户构造实例：

```rust
registry.register(InitCommandHandler { ... })?;
```

第一版先只实现函数形式，等函数形式的契约稳定后再加 impl 形式。

## 5. 使用方式

```rust
let dispatcher = Dispatcher::new();
let ctx = CommandContext {
    dispatcher: dispatcher.clone(),
    working_dir: std::env::current_dir()?,
    session_id: None,
};

let mut registry = CommandRegistry::new();
registry.register(init_command())?;
registry.register(build_command())?;

let output = registry
    .execute("init", &ctx, serde_json::json!({}))
    .await?;
```

CLI 层负责：

1. 解析用户输入，确定命令名和参数；
2. 从注册表查找命令；
3. 将参数转换为 JSON；
4. 调用 `execute`；
5. 输出结果或错误。

## 6. 与事件系统的关系

命令和事件保持解耦：

- `CommandRegistry` 不依赖 `Dispatcher`；
- `CommandContext` 持有 `Dispatcher`，命令可以选择发布事件；
- 建议定义命令生命周期事件：
  - `CommandStarted`
  - `CommandCompleted`
  - `CommandFailed`
- 命令失败直接返回给调用方，不自动进入死信队列；只有命令发布的事件被消费者处理失败时才进入 DLQ。

注意不要在事件的消费者内部执行命令并调用 `dispatcher.flush()`，否则可能等待自身投递而死锁。

## 7. 错误处理与取消

第一版：

- 命令错误使用 `BoxError` 返回调用方；
- 参数反序列化失败、输出序列化失败都包装成 `CommandError`；
- 注册冲突返回 `RegistryError`。

后续扩展：

- `CancellationToken` 传入 `CommandContext`；
- 命令执行超时；
- 权限拒绝错误；
- 流式输出和中间进度事件。

## 8. 实现顺序

1. `src/domain/command.rs`：`Command`、`CommandContext`、`CommandMeta`。
2. `src/core/command_registry.rs`：`ErasedCommand`、`CommandAdapter`、`CommandRegistry`。
3. 宏 crate：需要时重新引入，新增 `#[command]` 展开。
4. `tests/commands.rs`：无参数命令、带参数命令、注册冲突、执行结果。
5. CLI 接入和帮助信息。

## 9. 待定问题

- 参数中间表示：继续使用 `serde_json::Value`，还是直接使用 clap 的 `ArgMatches`。
- 输出：`serde_json::Value` 是否足够，是否需要流式输出。
- 命令是否需要权限/沙箱声明。
- 是否支持子命令和命令组。
- 是否引入 `inventory` 做自动发现。
- 帮助信息是由宏元数据生成，还是单独维护 clap 定义。
