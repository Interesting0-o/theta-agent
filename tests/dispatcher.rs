use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use theta::core::dead_letter::{DeadLetter, DeadLetterQueue};
use theta::core::dispatcher::Dispatcher;
use theta::domain::consumer::Consumer;
use theta::domain::event::{BoxError, Event};
use theta::domain::producer::Producer;

#[derive(Debug)]
struct Ping {
    value: u32,
}

impl Event for Ping {}

#[derive(Debug)]
struct Pong;

impl Event for Pong {}

#[derive(Default)]
struct PingCounter {
    count: AtomicUsize,
    values: Mutex<Vec<u32>>,
}

#[async_trait]
impl Consumer<Ping> for PingCounter {
    async fn consume(&self, event: Arc<Ping>) -> Result<(), BoxError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.values.lock().unwrap().push(event.value);
        Ok(())
    }
}

struct PingFailer;

#[async_trait]
impl Consumer<Ping> for PingFailer {
    async fn consume(&self, _event: Arc<Ping>) -> Result<(), BoxError> {
        Err("boom".into())
    }
}

struct VecProducer {
    events: std::vec::IntoIter<Arc<dyn Event>>,
}

impl VecProducer {
    fn new(events: Vec<Arc<dyn Event>>) -> Self {
        Self {
            events: events.into_iter(),
        }
    }
}

#[async_trait]
impl Producer for VecProducer {
    async fn next(&mut self) -> Result<Option<Arc<dyn Event>>, BoxError> {
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

    dispatcher.publish(Ping { value: 1 }).await.unwrap();
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

#[derive(Default)]
struct MultiCounter {
    pings: AtomicUsize,
    pongs: AtomicUsize,
}

#[async_trait]
impl Consumer<Ping> for MultiCounter {
    async fn consume(&self, _event: Arc<Ping>) -> Result<(), BoxError> {
        self.pings.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Consumer<Pong> for MultiCounter {
    async fn consume(&self, _event: Arc<Pong>) -> Result<(), BoxError> {
        self.pongs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn one_consumer_can_subscribe_to_multiple_event_types() {
    let dispatcher = Dispatcher::new();
    let counter = Arc::new(MultiCounter::default());
    dispatcher.subscribe::<Ping, _>(Arc::clone(&counter));
    dispatcher.subscribe::<Pong, _>(Arc::clone(&counter));

    dispatcher.publish(Ping { value: 1 }).await.unwrap();
    dispatcher.publish(Pong).await.unwrap();
    dispatcher.flush().await;

    assert_eq!(counter.pings.load(Ordering::SeqCst), 1);
    assert_eq!(counter.pongs.load(Ordering::SeqCst), 1);
    assert_eq!(dispatcher.stats().delivered, 2);
}

#[tokio::test]
async fn subscription_preserves_event_order() {
    let dispatcher = Dispatcher::new();
    let counter = Arc::new(PingCounter::default());
    dispatcher.subscribe(Arc::clone(&counter));

    for value in 1..=5 {
        dispatcher.publish(Ping { value }).await.unwrap();
    }
    dispatcher.flush().await;

    assert_eq!(*counter.values.lock().unwrap(), vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn failed_consumption_goes_to_dead_letter_queue() {
    let dispatcher = Dispatcher::new();
    dispatcher.subscribe(PingFailer);

    dispatcher.publish(Ping { value: 7 }).await.unwrap();
    dispatcher.flush().await;

    let letters = dispatcher.dead_letters().drain();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].event_name, std::any::type_name::<Ping>());
    assert_eq!(letters[0].consumer, std::any::type_name::<PingFailer>());
    assert_eq!(letters[0].error.to_string(), "boom");
    assert_eq!(dispatcher.stats().dead_lettered, 1);
    assert!(dispatcher.dead_letters().is_empty());
}

#[tokio::test]
async fn events_without_subscribers_are_dropped() {
    let dispatcher = Dispatcher::new();

    dispatcher.publish(Pong).await.unwrap();
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
            event: Arc::new(Ping { value }),
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
    dispatcher.subscribe(Arc::clone(&counter));

    let events: Vec<Arc<dyn Event>> = vec![
        Arc::new(Ping { value: 1 }),
        Arc::new(Pong),
        Arc::new(Ping { value: 2 }),
    ];
    let report = dispatcher.run(VecProducer::new(events)).await.unwrap();

    assert_eq!(report.events, 3);
    assert_eq!(report.delivered, 2);
    assert_eq!(report.dead_lettered, 0);
    assert_eq!(counter.count.load(Ordering::SeqCst), 2);
    assert_eq!(dispatcher.stats().pending, 0);
}
