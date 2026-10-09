use async_trait::async_trait;
use harness::{
    engine::{Engine, EngineConfig, ResponsesTransport},
    item::Item,
    model::{AgentPath, Effort, RequestId},
    provider::{CallContext, Provider, ProviderError, ToolScheduling},
    store::{actor_output::ActorOutputOrigin, chat::ChatEntry, Store, Usage},
    transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError},
    turn::JobScheduler,
};
use proptest::prelude::*;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct Offline;
impl Auth for Offline {
    fn access(&self) -> Result<(String, String), TransportError> {
        panic!("scripted projection property requires no authentication")
    }
}

struct Echo;
#[async_trait]
impl Provider for Echo {
    async fn call(&self, _: &str, args: Value) -> Result<Value, ProviderError> {
        Ok(args)
    }
    async fn call_custom_with_context(
        &self,
        _: &str,
        input: String,
        _: CallContext,
    ) -> Result<Value, ProviderError> {
        Ok(json!({"text":input}))
    }
    fn tools(&self) -> Vec<Value> {
        vec![]
    }
    fn tool_scheduling(&self, _: &str) -> ToolScheduling {
        ToolScheduling::BeforeNextInference
    }
}

struct Script(Mutex<VecDeque<Item>>);
#[async_trait]
impl ResponsesTransport for Script {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        Ok(ResponsesTurn {
            response_id: "projection".into(),
            items: vec![self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected model round")],
            usage: Default::default(),
        })
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn unified_chat_preserves_exact_successor_outputs_and_request_history(
        texts in prop::collection::vec("[a-z0-9λ🐈]{0,32}", 1..6),
        page_size in 1usize..8,
    ) {
        let store = Arc::new(Store::memory().unwrap());
        let origin = ActorOutputOrigin { run: "run".into(), native_actor: 1, incarnation: 1 };
        let mut turns = VecDeque::new();
        let mut expected_outputs = Vec::new();
        for (round, text) in texts.into_iter().enumerate() {
            let text = format!("  {text}\n\"quoted\" \\  ");
            for custom in [true, false] {
                let call = format!("call-{round}-{custom}");
                turns.push_back(Item(if custom {
                    json!({"type":"custom_tool_call", "name":"echo", "call_id":call, "input":text})
                } else {
                    json!({"type":"function_call", "name":"echo", "call_id":call, "arguments":json!({"text":text}).to_string()})
                }));
                expected_outputs.push((if custom {"custom_tool_call_output"} else {"function_call_output"}.to_string(), call, json!({"text":text})));
            }
        }
        let final_item = Item(json!({"type":"message", "role":"assistant", "phase":"final_answer", "content":"outputs retained"}));
        turns.push_back(final_item.clone());
        let engine = Engine::<Offline, Echo, _>::with_transport(
            Script(Mutex::new(turns)), store.clone(), Arc::new(JobScheduler::new(1).unwrap()), Arc::new(Echo),
            EngineConfig { instructions:"projection property".into(), tools:vec![], model:"offline".into(), effort:Effort::Low, session_id:"projection".into(), agent:AgentPath("/root".into()) },
        );
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let completion = runtime.block_on(async {
            let (_cancel, cancellation) = tokio::sync::watch::channel(false);
            let (_mailbox, incoming) = tokio::sync::mpsc::unbounded_channel();
            tokio::time::timeout(std::time::Duration::from_secs(5), engine.run(None, vec![], cancellation, incoming)).await.unwrap().unwrap()
        });

        let mut expected_pages = Vec::new();
        let mut request = Some(completion.head_request.clone());
        while let Some(id) = request {
            let page = store.history_page(&id, 0, 100).unwrap();
            expected_pages.push(page.items.into_iter().map(|entry| (id.0.clone(), entry.position as i64, entry.hash, entry.item.0)).collect::<Vec<_>>());
            request = store.request(&id).unwrap().unwrap().parent;
        }
        let expected = expected_pages.into_iter().rev().flatten().collect::<Vec<_>>();
        // Similar unrelated messages cannot enter this exact request lineage.
        store.write_request(&RequestId("sibling".into()), None, "/sibling", &[final_item.clone()], Usage::default()).unwrap();
        let mut after = 0;
        let mut projected = Vec::new();
        let mut finished = false;
        for _ in 0..=expected.len() {
            let page = store.chat_page(&origin, Some(&completion.head_request), after, page_size).unwrap();
            prop_assert!(!page.legacy_history);
            prop_assert_eq!(page.cutover_sequence, 0);
            for entry in page.entries {
                match entry {
                    ChatEntry::Message { sequence, request_id, position, hash, item } => {
                        prop_assert!(sequence > after);
                        after = sequence;
                        projected.push((request_id, position, hash, item));
                    }
                    _ => prop_assert!(false, "model publications changed entry kind"),
                }
            }
            match page.next_after {
                Some(cursor) => prop_assert_eq!(cursor, after),
                None => { finished = true; break; }
            }
        }
        prop_assert!(finished, "conversation pagination did not terminate");
        prop_assert_eq!(&projected, &expected);
        let outputs = projected.iter().filter_map(|(_, _, _, item)| match item["type"].as_str() {
            Some(kind @ ("custom_tool_call_output" | "function_call_output")) => Some((kind.to_string(), item["call_id"].as_str().unwrap().to_string(), serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap())),
            _ => None,
        }).collect::<Vec<_>>();
        prop_assert_eq!(outputs, expected_outputs);
        prop_assert_eq!(&projected.last().unwrap().3, &final_item.0);
    }
}
