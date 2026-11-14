use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::domain::event::{BoxError, EventEnvelope};

/// 一条死信记录，保存消费失败的事件及原因。
pub struct DeadLetter {
    /// 原始事件信封，保留 `Arc` 以便后续重放。
    pub envelope: Arc<EventEnvelope>,
    /// 事件类型名，便于在没有具体类型信息时展示。
    pub event_name: &'static str,
    /// 处理失败的消费者名称。
    pub consumer: &'static str,
    /// 消费者返回的错误或 panic 信息。
    pub error: BoxError,
}

impl std::fmt::Debug for DeadLetter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeadLetter")
            .field("event_name", &self.event_name)
            .field("consumer", &self.consumer)
            .field("error", &self.error)
            .finish()
    }
}

struct Inner {
    letters: VecDeque<DeadLetter>,
    /// 因容量满而丢弃的最旧死信累计数量。
    dropped: u64,
}

/// 有界死信队列。
///
/// 写入时如果队列已满，会丢弃最旧的一条死信并累加 [`Self::dropped`]，
/// 保证死信本身不会无限占用内存。
pub struct DeadLetterQueue {
    capacity: usize,
    inner: Mutex<Inner>,
}

impl DeadLetterQueue {
    /// 创建指定容量的死信队列，容量至少为 1。
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(Inner {
                letters: VecDeque::new(),
                dropped: 0,
            }),
        }
    }

    /// 队列最多能保存的死信数量。
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 当前保存的死信数量。
    pub fn len(&self) -> usize {
        self.lock().letters.len()
    }

    /// 当前是否没有死信。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 因容量满而被丢弃的死信累计数量。
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    /// 写入一条死信；队列满时先移除最旧的一条。
    pub fn push(&self, letter: DeadLetter) {
        let mut inner = self.lock();
        if inner.letters.len() == self.capacity {
            inner.letters.pop_front();
            inner.dropped += 1;
        }
        inner.letters.push_back(letter);
    }

    /// 取出并清空所有死信，可用于重放或落盘。
    pub fn drain(&self) -> Vec<DeadLetter> {
        self.lock().letters.drain(..).collect()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 锁中毒时恢复内部数据，避免一次 panic 让队列永久不可用。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
