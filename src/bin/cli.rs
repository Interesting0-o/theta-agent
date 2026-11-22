use std::io::{self, Write};
use std::sync::Arc;

use theta::core::dispatcher::Dispatcher;
use theta::domain::agent::events::{
    LLMCompleted, LLMRequest, ToolCallRequest, ToolCallResponse, UserInput,
};
use theta::domain::agent::state::AgentStateRepository;
use theta::domain::event::{BoxError, EventEnvelope, EventScope};
use theta::domain::id::{AgentStateId, RunId};
use theta::domain::producer::Producer;
use theta::infrastructure::in_memory_repo::InMemoryRepository;
use theta::nodes::agent_actor::AgentActor;
use theta::nodes::llm_node::LLMNode;
use theta::nodes::print_worker::PrintWorker;
use theta::nodes::tool_node::ToolNode;

struct StdinProducer {
    scope: EventScope,
}

impl StdinProducer {
    fn new(scope: EventScope) -> Self {
        Self { scope }
    }
}

#[async_trait::async_trait]
impl Producer for StdinProducer {
    async fn next(&mut self) -> Result<Option<Arc<EventEnvelope>>, BoxError> {
        loop {
            let line = tokio::task::spawn_blocking(|| -> io::Result<Option<String>> {
                print!("> ");
                io::stdout().flush()?;
                let mut input = String::new();
                match io::stdin().read_line(&mut input)? {
                    0 => Ok(None),
                    _ => Ok(Some(input)),
                }
            })
            .await??;

            let Some(input) = line else {
                return Ok(None);
            };
            let input = input.trim_end().to_string();
            if input.is_empty() {
                continue;
            }

            return Ok(Some(Arc::new(EventEnvelope::new(
                self.scope,
                UserInput { input },
            ))));
        }
    }
}

#[tokio::main]
async fn main() {
    let dispatcher = Dispatcher::new();
    let repo: Arc<dyn AgentStateRepository> = Arc::new(InMemoryRepository::new());
    let actor = Arc::new(AgentActor::new(repo, dispatcher.publisher()));
    dispatcher.subscribe::<UserInput, _>(Arc::clone(&actor));
    dispatcher.subscribe::<LLMCompleted, _>(Arc::clone(&actor));
    dispatcher.subscribe::<ToolCallResponse, _>(Arc::clone(&actor));
    dispatcher.subscribe::<LLMRequest, _>(LLMNode::new(dispatcher.publisher()));
    dispatcher.subscribe::<LLMCompleted, _>(PrintWorker::new());
    dispatcher.subscribe::<ToolCallRequest, _>(ToolNode::new(dispatcher.publisher()));

    let scope = EventScope::new(AgentStateId::new(), RunId::new());
    let producer = StdinProducer::new(scope);

    match dispatcher.run(producer).await {
        Ok(Ok(report)) => eprintln!(
            "done: events={}, delivered={}, dead_lettered={}",
            report.events, report.delivered, report.dead_lettered
        ),
        Ok(Err(error)) => eprintln!("event loop failed: {error}"),
        Err(error) => eprintln!("event loop task failed: {error}"),
    }

    for letter in dispatcher.dead_letters().drain() {
        eprintln!(
            "dead letter: event={}, consumer={}, error={}",
            letter.event_name, letter.consumer, letter.error
        );
    }
}
