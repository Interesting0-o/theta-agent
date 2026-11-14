//! 基于 Tokio channel 的事件分发器。
//!
//! 处理链路：
//!
//! ```text
//! publish
//!   -> 有界事件队列
//!   -> worker 按 TypeId 路由
//!   -> 每个订阅者各自的有界 mpsc
//!   -> 订阅者任务串行消费
//! ```
//!
//! 同一订阅者的事件保持顺序；不同订阅者之间并发处理。
//! 队列满时 `publish` 会等待，订阅者通道满时 worker 会等待，形成背压。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, Weak};

use tokio::sync::{Notify, mpsc};

use crate::core::dead_letter::{DeadLetter, DeadLetterQueue};
use crate::domain::consumer::Consumer;
use crate::domain::event::{Event, EventError};
use crate::domain::producer::Producer;

/// 事件队列的默认容量。
const DEFAULT_QUEUE_CAPACITY: usize = 1024;
/// 每个订阅者接收通道的默认容量。
const DEFAULT_SUBSCRIBER_CAPACITY: usize = 256;
/// 死信队列的默认容量。
const DEFAULT_DEAD_LETTER_CAPACITY: usize = 1024;

/// 发布事件时的错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// 事件队列已关闭，通常表示后台 worker 已退出。
    Closed,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "event bus is closed"),
        }
    }
}

impl std::error::Error for PublishError {}

/// 分发器的累计统计与当前积压量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchStats {
    /// 成功进入事件队列的事件数。
    pub published: u64,
    /// 消费者成功处理的次数；一个事件被多个消费者处理会累计多次。
    pub delivered: u64,
    /// 进入死信队列的次数。
    pub dead_lettered: u64,
    /// 尚未完成的事件或投递数量；`0` 表示队列已排空。
    pub pending: usize,
}

/// 一次 `run` 调用的汇总结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunReport {
    /// 生产者产出并成功入队的事件数。
    pub events: usize,
    /// 本次 `run` 期间消费者成功处理的次数。
    pub delivered: u64,
    /// 本次 `run` 期间产生的死信数。
    pub dead_lettered: u64,
}

/// 无锁累加的分发统计。
#[derive(Default)]
struct Metrics {
    published: AtomicU64,
    delivered: AtomicU64,
    dead_lettered: AtomicU64,
    pending: AtomicUsize,
}

/// 所有 `Dispatcher` clone 与后台任务共享的状态。
struct Shared {
    /// 按事件 `TypeId` 分组的订阅者列表。
    subscribers: RwLock<HashMap<TypeId, Vec<Subscriber>>>,
    /// 保存消费失败的事件。
    dead_letters: DeadLetterQueue,
    /// 运行统计。
    metrics: Metrics,
    /// `pending` 归零时唤醒 `flush` 的等待者。
    idle: Notify,
    /// 每个订阅者接收通道的创建容量。
    subscriber_capacity: usize,
}

impl Shared {
    fn new(subscriber_capacity: usize, dead_letter_capacity: usize) -> Self {
        Self {
            subscribers: RwLock::new(HashMap::new()),
            dead_letters: DeadLetterQueue::new(dead_letter_capacity),
            metrics: Metrics::default(),
            idle: Notify::new(),
            subscriber_capacity,
        }
    }

    /// 记录一个待完成单元：一个排队事件或一次投递。
    fn begin_pending(&self) {
        self.metrics.pending.fetch_add(1, Ordering::SeqCst);
    }

    /// 完成一个待完成单元；计数归零时唤醒 `flush`。
    fn finish_pending(&self) {
        if self.metrics.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_one();
        }
    }

    /// 记录一条死信并更新统计。
    fn dead_letter(&self, letter: DeadLetter) {
        self.metrics.dead_lettered.fetch_add(1, Ordering::Relaxed);
        self.dead_letters.push(letter);
    }
}

/// 注册表中的订阅者句柄，worker 通过它把事件投递给对应的消费任务。
#[derive(Clone)]
struct Subscriber {
    name: &'static str,
    sender: mpsc::Sender<Arc<dyn Event>>,
}

/// 事件分发器，同时也是可克隆的发布句柄。
///
/// 所有 clone 共享同一个事件队列、订阅表、统计和死信队列。
#[derive(Clone)]
pub struct Dispatcher {
    /// 发布端：事件通过它进入有界队列。
    tx: mpsc::Sender<Arc<dyn Event>>,
    /// 订阅表、统计、死信等共享状态。
    shared: Arc<Shared>,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher {
    /// 使用默认容量创建分发器，并在当前 Tokio runtime 上启动后台 worker。
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_QUEUE_CAPACITY, DEFAULT_SUBSCRIBER_CAPACITY)
    }

    /// 自定义事件队列和每个订阅者通道的容量。
    ///
    /// 两个容量都会被限制为至少 1。
    pub fn with_capacity(queue_capacity: usize, subscriber_capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(queue_capacity.max(1));
        let shared = Arc::new(Shared::new(
            subscriber_capacity.max(1),
            DEFAULT_DEAD_LETTER_CAPACITY,
        ));
        tokio::spawn(run_worker(rx, Arc::clone(&shared)));
        Self { tx, shared }
    }

    /// 注册一个消费者并启动其专属消费任务。
    ///
    /// 同一事件类型的多个消费者都会收到该事件，彼此独立处理。
    /// 同一消费者类型可以为多种事件实现 [`Consumer`] 并分别订阅：
    ///
    /// ```text
    /// dispatcher.subscribe::<Ping, _>(handler.clone());
    /// dispatcher.subscribe::<Pong, _>(handler);
    /// ```
    pub fn subscribe<E, C>(&self, consumer: C) -> &Self
    where
        E: Event,
        C: Consumer<E>,
    {
        let name = consumer.name();
        let (tx, rx) = mpsc::channel(self.shared.subscriber_capacity);
        tokio::spawn(run_subscriber::<E, C>(
            Arc::new(consumer),
            rx,
            Arc::downgrade(&self.shared),
        ));

        let mut subscribers = self
            .shared
            .subscribers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        subscribers
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Subscriber { name, sender: tx });
        drop(subscribers);
        self
    }

    /// 发布一个具体类型的事件。
    ///
    /// 只保证事件成功入队，不等待消费者处理完成；需要等待时调用 [`Self::flush`]。
    pub async fn publish<E: Event>(&self, event: E) -> Result<(), PublishError> {
        self.publish_shared(Arc::new(event)).await
    }

    /// 发布一个已经包在 `Arc` 中的无类型事件。
    ///
    /// 队列满时会等待，形成向生产者的背压。
    pub async fn publish_shared(&self, event: Arc<dyn Event>) -> Result<(), PublishError> {
        // 先增加 pending，避免 flush 在事件真正入队前提前返回。
        self.shared.begin_pending();
        if self.tx.send(event).await.is_err() {
            self.shared.finish_pending();
            return Err(PublishError::Closed);
        }
        self.shared
            .metrics
            .published
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// 等待所有已入队事件及其派生投递完成。
    ///
    /// 不要在一个消费者内部调用，否则会等待自身而永远无法返回。
    pub async fn flush(&self) {
        loop {
            // 先创建 notified future 再检查计数，避免检查与等待之间丢失通知。
            let notified = self.shared.idle.notified();
            if self.shared.metrics.pending.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    /// 获取当前统计快照。
    pub fn stats(&self) -> DispatchStats {
        DispatchStats {
            published: self.shared.metrics.published.load(Ordering::Relaxed),
            delivered: self.shared.metrics.delivered.load(Ordering::Relaxed),
            dead_lettered: self.shared.metrics.dead_lettered.load(Ordering::Relaxed),
            pending: self.shared.metrics.pending.load(Ordering::SeqCst),
        }
    }

    /// 获取死信队列，可用于查看、取走或重放失败事件。
    pub fn dead_letters(&self) -> &DeadLetterQueue {
        &self.shared.dead_letters
    }

    /// 拉取生产者事件并发布，生产结束后等待队列排空。
    ///
    /// 返回的统计是本次调用期间的增量，不包含调用前已经处理的事件。
    pub async fn run<P: Producer>(&self, mut producer: P) -> Result<RunReport, EventError> {
        // 记录调用前的累计值，最后用差值表示本次 run 的成果。
        let before = self.stats();
        let mut events = 0;

        loop {
            let next = match producer.next().await {
                Ok(next) => next,
                Err(source) => {
                    return Err(EventError::Producer {
                        producer: producer.name(),
                        source,
                    });
                }
            };

            let Some(event) = next else {
                break;
            };

            self.publish_shared(event)
                .await
                .map_err(|_| EventError::BusClosed)?;
            events += 1;
        }

        self.flush().await;
        let after = self.stats();

        Ok(RunReport {
            events,
            delivered: after.delivered.saturating_sub(before.delivered),
            dead_lettered: after.dead_lettered.saturating_sub(before.dead_lettered),
        })
    }
}

/// 后台路由任务：从事件队列取出事件，按 `TypeId` 投递给所有匹配的订阅者。
async fn run_worker(mut rx: mpsc::Receiver<Arc<dyn Event>>, shared: Arc<Shared>) {
    while let Some(event) = rx.recv().await {
        let event_name = event.event_name();
        let event_type = event.event_type_id();

        // 先复制订阅者快照，读锁在后续 await 之前释放。
        let subscribers = {
            let subscribers = shared
                .subscribers
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            subscribers.get(&event_type).cloned().unwrap_or_default()
        };

        if subscribers.is_empty() {
            // 没有订阅者的事件直接丢弃，但仍要结束该事件的 pending。
            shared.finish_pending();
            continue;
        }

        // 逐个投递；某个订阅者通道满时，这里会等待，形成全局背压。
        for subscriber in subscribers {
            // 每次成功投递都算一个待完成单元，由订阅者处理完后归还。
            shared.begin_pending();
            if subscriber.sender.send(Arc::clone(&event)).await.is_err() {
                shared.dead_letter(DeadLetter {
                    event: Arc::clone(&event),
                    event_name,
                    consumer: subscriber.name,
                    error: Box::new(EventError::SubscriberClosed {
                        consumer: subscriber.name,
                    }),
                });
                remove_subscriber(&shared, event_type, &subscriber);
                shared.finish_pending();
            }
        }

        // 事件已完成路由，结束发布时占用的 pending。
        shared.finish_pending();
    }
}

/// 订阅任务已经退出时，从注册表移除对应句柄，避免后续事件继续产生死信。
fn remove_subscriber(shared: &Shared, event_type: TypeId, subscriber: &Subscriber) {
    let mut subscribers = shared
        .subscribers
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(subscribers) = subscribers.get_mut(&event_type) {
        subscribers.retain(|candidate| !candidate.sender.same_channel(&subscriber.sender));
    }
}

/// 单个订阅者的消费循环，串行处理自己通道中的事件。
///
/// 这里持有 `Weak<Shared>` 而不是 `Arc`，避免
/// `Shared -> 注册表 -> Sender -> 订阅任务 -> Shared` 形成引用环。
async fn run_subscriber<E, C>(
    consumer: Arc<C>,
    mut rx: mpsc::Receiver<Arc<dyn Event>>,
    shared: Weak<Shared>,
) where
    E: Event,
    C: Consumer<E>,
{
    let consumer_name = consumer.name();
    let expected = std::any::type_name::<E>();

    while let Some(event) = rx.recv().await {
        // 所有 Dispatcher 句柄都释放后，worker 会退出并释放 Shared，
        // 此时订阅任务也可以结束。
        let Some(shared) = shared.upgrade() else {
            break;
        };

        let event_name = event.event_name();
        // 保留原始句柄，消费失败时放进死信。
        let original = Arc::clone(&event);
        let any: Arc<dyn Any + Send + Sync> = event;

        let Ok(event) = any.downcast::<E>() else {
            shared.dead_letter(DeadLetter {
                event: original,
                event_name,
                consumer: consumer_name,
                error: Box::new(EventError::EventTypeMismatch {
                    expected,
                    actual: event_name,
                }),
            });
            shared.finish_pending();
            continue;
        };

        // 用独立 task 执行消费，把 panic 转成 JoinError，
        // 避免一个事件让整个订阅任务退出。
        let consumer = Arc::clone(&consumer);
        let result = tokio::spawn(async move { consumer.consume(event).await }).await;

        match result {
            Ok(Ok(())) => {
                shared.metrics.delivered.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Err(error)) => {
                shared.dead_letter(DeadLetter {
                    event: original,
                    event_name,
                    consumer: consumer_name,
                    error,
                });
            }
            Err(error) => {
                shared.dead_letter(DeadLetter {
                    event: original,
                    event_name,
                    consumer: consumer_name,
                    error: Box::new(error),
                });
            }
        }

        shared.finish_pending();
    }
}
