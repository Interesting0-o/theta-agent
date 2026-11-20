use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use theta::core::dead_letter::{DeadLetter, DeadLetterQueue};
use theta::core::dispatcher::Dispatcher;
use theta::domain::consumer::Consumer;
use theta::domain::event::{BoxError, Event, EventEnvelope, EventScope};
use theta::domain::id::{AgentStateId, EventId, RunId};
use theta::domain::producer::Producer;
use tokio::sync::Notify;

#[derive(Debug)]
struct Ping {
    value: u32,
}

impl Event for Ping {}

#[derive(Debug)]
struct Pong;

impl Event for Pong {}

fn scope() -> EventScope {
    EventScope::new(AgentStateId::new(), RunId::new())
}

#[derive(Default)]
struct PingCounter {
    count: AtomicUsize,
    values: Mutex<Vec<u32>>,
}

#[async_trait]
impl Consumer<Ping> for PingCounter {
    async fn consume(&self, event: Arc<Ping>, _envelope: &EventEnvelope) -> Result<(), BoxError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.values.lock().unwrap().push(event.value);
        Ok(())
    }
}

struct PingFailer;

#[async_trait]
impl Consumer<Ping> for PingFailer {
    async fn consume(&self, _event: Arc<Ping>, _envelope: &EventEnvelope) -> Result<(), BoxError> {
        Err("boom".into())
    }
}

struct SlowConsumer {
    blocked_once: AtomicBool,
    release: Arc<Notify>,
    seen: Mutex<Vec<u32>>,
}

impl SlowConsumer {
    fn new(release: Arc<Notify>) -> Self {
        Self {
            blocked_once: AtomicBool::new(false),
            release,
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Consumer<Ping> for SlowConsumer {
    async fn consume(&self, event: Arc<Ping>, _envelope: &EventEnvelope) -> Result<(), BoxError> {
        self.seen.lock().unwrap().push(event.value);
        if !self.blocked_once.swap(true, Ordering::SeqCst) {
            self.release.notified().await;
        }
        Ok(())
    }
}

struct VecProducer {
    events: std::vec::IntoIter<Arc<EventEnvelope>>,
}

impl VecProducer {
    fn new(events: Vec<Arc<EventEnvelope>>) -> Self {
        Self {
            events: events.into_iter(),
        }
    }
}

#[async_trait]
impl Producer for VecProducer {
    async fn next(&mut self) -> Result<Option<Arc<EventEnvelope>>, BoxError> {
        Ok(self.events.next())
    }
}

#[tokio::test]
async fn pub_sub_fans_out_to_all_subscribers() {
    let dispatcher = Dispatcher::new();
    let first = Arc::new(PingCounter::default());
    let second = Arc::new(PingCounter::default());
    dispatcher.subscribe(Arc::clone(&first));
    dispatcher.subscribe(Arc::clone(&second));

    dispatcher
        .publish(EventEnvelope::new(scope(), Ping { value: 1 }))
        .await
        .unwrap();
    dispatcher.flush().await;

    assert_eq!(first.count.load(Ordering::SeqCst), 1);
    assert_eq!(second.count.load(Ordering::SeqCst), 1);

    let stats = dispatcher.stats();
    assert_eq!(stats.published, 1);
    assert_eq!(stats.delivered, 2);
    assert_eq!(stats.dead_lettered, 0);
    assert_eq!(stats.pending, 0);
    assert!(dispatcher.dead_letters().is_empty());
}

#[tokio::test]
async fn publisher_handle_publishes_to_the_same_bus() {
    let dispatcher = Dispatcher::new();
    let counter = Arc::new(PingCounter::default());
    dispatcher.subscribe(Arc::clone(&counter));

    let publisher = dispatcher.publisher();
    publisher
        .publish(EventEnvelope::new(scope(), Ping { value: 9 }))
        .await
        .unwrap();
    dispatcher.flush().await;

    assert_eq!(counter.count.load(Ordering::SeqCst), 1);
    assert_eq!(*counter.values.lock().unwrap(), vec![9]);
    assert_eq!(dispatcher.stats().published, 1);
    assert_eq!(dispatcher.stats().delivered, 1);
    assert_eq!(dispatcher.stats().pending, 0);
}

#[derive(Default)]
struct MultiCounter {
    pings: AtomicUsize,
    pongs: AtomicUsize,
}

#[async_trait]
impl Consumer<Ping> for MultiCounter {
    async fn consume(&self, _event: Arc<Ping>, _envelope: &EventEnvelope) -> Result<(), BoxError> {
        self.pings.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Consumer<Pong> for MultiCounter {
    async fn consume(&self, _event: Arc<Pong>, _envelope: &EventEnvelope) -> Result<(), BoxError> {
        self.pongs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn one_consumer_can_subscribe_to_multiple_event_types() {
    let dispatcher = Dispatcher::new();
    let counter = Arc::new(MultiCounter::default());
    let scope = scope();
    dispatcher.subscribe::<Ping, _>(Arc::clone(&counter));
    dispatcher.subscribe::<Pong, _>(Arc::clone(&counter));

    dispatcher
        .publish(EventEnvelope::new(scope, Ping { value: 1 }))
        .await
        .unwrap();
    dispatcher
        .publish(EventEnvelope::new(scope, Pong))
        .await
        .unwrap();
    dispatcher.flush().await;

    assert_eq!(counter.pings.load(Ordering::SeqCst), 1);
    assert_eq!(counter.pongs.load(Ordering::SeqCst), 1);
    assert_eq!(dispatcher.stats().delivered, 2);
}

#[tokio::test]
async fn subscription_preserves_event_order() {
    let dispatcher = Dispatcher::new();
    let counter = Arc::new(PingCounter::default());
    let scope = scope();
    dispatcher.subscribe(Arc::clone(&counter));

    for value in 1..=5 {
        dispatcher
            .publish(EventEnvelope::new(scope, Ping { value }))
            .await
            .unwrap();
    }
    dispatcher.flush().await;

    assert_eq!(*counter.values.lock().unwrap(), vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn slow_subscriber_does_not_block_other_subscribers() {
    let dispatcher = Dispatcher::with_capacity(64, 1);
    let release = Arc::new(Notify::new());
    let slow = Arc::new(SlowConsumer::new(Arc::clone(&release)));
    let fast = Arc::new(PingCounter::default());
    let scope = scope();

    dispatcher.subscribe(Arc::clone(&slow));
    dispatcher.subscribe(Arc::clone(&fast));

    for value in 0..5 {
        dispatcher
            .publish(EventEnvelope::new(scope, Ping { value }))
            .await
            .unwrap();
    }

    tokio::time::timeout(Duration::from_secs(2), async {
        while fast.count.load(Ordering::SeqCst) < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("slow subscriber blocked the fast subscriber");

    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), dispatcher.flush())
        .await
        .expect("flush timed out");

    assert_eq!(*slow.seen.lock().unwrap(), vec![0, 1, 2, 3, 4]);
}

#[tokio::test]
async fn failed_consumption_goes_to_dead_letter_queue() {
    let dispatcher = Dispatcher::new();
    dispatcher.subscribe(PingFailer);

    dispatcher
        .publish(EventEnvelope::new(scope(), Ping { value: 7 }))
        .await
        .unwrap();
    dispatcher.flush().await;

    let letters = dispatcher.dead_letters().drain();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].event_name, std::any::type_name::<Ping>());
    assert_eq!(letters[0].consumer, std::any::type_name::<PingFailer>());
    assert_eq!(letters[0].error.to_string(), "boom");
    assert_eq!(letters[0].envelope.event_name(), letters[0].event_name);
    assert_eq!(dispatcher.stats().dead_lettered, 1);
    assert!(dispatcher.dead_letters().is_empty());
}

#[derive(Default)]
struct EnvelopeRecorder {
    seen: Mutex<Vec<EventEnvelope>>,
}

#[async_trait]
impl Consumer<Pong> for EnvelopeRecorder {
    async fn consume(&self, _event: Arc<Pong>, envelope: &EventEnvelope) -> Result<(), BoxError> {
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

#[tokio::test]
async fn envelope_metadata_reaches_consumer() {
    let dispatcher = Dispatcher::new();
    let recorder = Arc::new(EnvelopeRecorder::default());
    dispatcher.subscribe(Arc::clone(&recorder));

    let parent = AgentStateId::new();
    let scope = scope().with_parent(parent);
    let request = EventEnvelope::new(scope, Ping { value: 1 });
    let request_id = request.event_id;

    let response = request.reply(Pong).with_sequence(7);
    let response_id = response.event_id;
    dispatcher.publish(response).await.unwrap();
    dispatcher.flush().await;

    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let envelope = &seen[0];
    assert_eq!(envelope.scope, scope);
    assert_eq!(envelope.scope.state_id, scope.state_id);
    assert_eq!(envelope.scope.run_id, scope.run_id);
    assert_eq!(envelope.scope.parent_state_id, Some(parent));
    assert_eq!(envelope.correlation_id, Some(request_id));
    assert_eq!(envelope.causation_id, Some(request_id));
    assert_eq!(envelope.sequence, 7);
    assert_ne!(request_id, response_id);
}

#[tokio::test]
async fn events_without_subscribers_are_dropped() {
    let dispatcher = Dispatcher::new();

    dispatcher
        .publish(EventEnvelope::new(scope(), Pong))
        .await
        .unwrap();
    dispatcher.flush().await;

    let stats = dispatcher.stats();
    assert_eq!(stats.published, 1);
    assert_eq!(stats.delivered, 0);
    assert_eq!(stats.dead_lettered, 0);
    assert_eq!(stats.pending, 0);
    assert!(dispatcher.dead_letters().is_empty());
}

#[test]
fn dead_letter_queue_is_bounded() {
    let queue = DeadLetterQueue::new(2);

    for value in 0..3 {
        queue.push(DeadLetter {
            envelope: Arc::new(EventEnvelope::new(scope(), Ping { value })),
            event_name: std::any::type_name::<Ping>(),
            consumer: "test",
            error: "boom".into(),
        });
    }

    assert_eq!(queue.len(), 2);
    assert_eq!(queue.dropped(), 1);
    assert_eq!(queue.drain().len(), 2);
    assert!(queue.is_empty());
}

#[tokio::test]
async fn run_publishes_producer_events_and_waits_for_completion() {
    let dispatcher = Dispatcher::new();
    let counter = Arc::new(PingCounter::default());
    let scope = scope();
    dispatcher.subscribe(Arc::clone(&counter));

    let events: Vec<Arc<EventEnvelope>> = vec![
        Arc::new(EventEnvelope::new(scope, Ping { value: 1 })),
        Arc::new(EventEnvelope::new(scope, Pong)),
        Arc::new(EventEnvelope::new(scope, Ping { value: 2 })),
    ];
    let report = dispatcher
        .run(VecProducer::new(events))
        .await
        .expect("producer task panicked")
        .unwrap();

    assert_eq!(report.events, 3);
    assert_eq!(report.delivered, 2);
    assert_eq!(report.dead_lettered, 0);
    assert_eq!(counter.count.load(Ordering::SeqCst), 2);
    assert_eq!(dispatcher.stats().pending, 0);
}

#[test]
fn event_ids_are_unique() {
    assert_ne!(EventId::new(), EventId::new());
}
