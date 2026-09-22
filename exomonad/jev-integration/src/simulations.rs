//! Finite synthetic command worlds. Actual Jev selections drive subsequent calls;
//! command observations are authored fixtures, never real shell execution.
use clap::ValueEnum;
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, io::Write, path::PathBuf, process::ExitCode, time::Duration};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Scenario {
    Failure,
    Source,
    Context,
    Swarm,
    TraceDelivery,
    TraceNoPath,
    VerifyBehavior,
    VerifyNoConsumer,
    ReuseFit,
    ReuseNoFit,
    MigrationCurrent,
    MigrationStale,
    ReproducerReachable,
    ReproducerNoPath,
}

#[derive(Clone)]
enum Node {
    Decision {
        observation: Value,
        question: String,
        alternatives: BTreeMap<String, (Value, Node)>,
        presence: Option<String>,
    },
    Done(&'static str),
}

fn decision(observation: Value, question: &str, alternatives: Vec<(&str, Value, Node)>) -> Node {
    Node::Decision {
        observation,
        question: question.into(),
        alternatives: alternatives
            .into_iter()
            .map(|(id, description, node)| (id.into(), (description, node)))
            .collect(),
        presence: None,
    }
}

fn search_decision(
    observation: Value,
    question: &str,
    presence: &str,
    alternatives: Vec<(&str, Value, Node)>,
) -> Node {
    let Node::Decision {
        observation,
        question,
        alternatives,
        ..
    } = decision(observation, question, alternatives)
    else {
        unreachable!()
    };
    Node::Decision {
        observation,
        question,
        alternatives,
        presence: Some(presence.into()),
    }
}

fn without_alternative(node: Node, id: &str) -> Node {
    let Node::Decision {
        observation,
        question,
        mut alternatives,
        presence,
    } = node
    else {
        return node;
    };
    alternatives.remove(id);
    Node::Decision {
        observation,
        question,
        alternatives,
        presence,
    }
}

fn assessment(observation: Value, question: &str, outcomes: &[(&str, &str)]) -> Node {
    decision(
        observation,
        question,
        outcomes
            .iter()
            .map(|(id, description)| {
                (
                    *id,
                    json!({"meaning":description}),
                    Node::Done(match *id {
                        "supported" => "supported",
                        "refuted" => "refuted",
                        "deliver" => "deliver",
                        "consult" => "consult",
                        _ => "need_evidence",
                    }),
                )
            })
            .collect(),
    )
}

fn fixture(scenario: Scenario) -> Node {
    match scenario {
        Scenario::Failure => {
            let finish = |observation| {
                assessment(observation,
                "Does the complete observed path support the hypothesis that bookmarks retain stale byte offsets after insertion?",
                &[("supported", "Evidence supports stale retained offsets"), ("refuted", "Evidence contradicts stale retained offsets"), ("unknown", "Need a discriminating observation")])
            };
            let tests = decision(json!({"simulated_command":"read bookmark source", "stdout":"bookmark stores byte_offset at creation; lookup uses that unchanged offset. Edit normalization code not shown."}),
                "Which available test most directly distinguishes stale retained offsets from incorrect edit normalization?",
                vec![
                    ("offset_probe", json!({"test":"bookmark_prefix_insert", "measures":"Compare normalized edit coordinates, saved bookmark offset, and actual logical character before/after a prefix insert"}), finish(json!({"simulated_command":"run bookmark_prefix_insert", "stdout":{"normalized_edit_correct":true,"saved_offset_before":10,"saved_offset_after":10,"logical_character_new_offset":13,"lookup_returns_wrong_character":true}}))),
                    ("speed", json!({"test":"lookup_benchmark", "measures":"Lookup time only"}), finish(json!({"simulated_command":"run lookup_benchmark", "stdout":"Lookup median 4 microseconds; no identity assertions"}))),
                    ("unknown", json!("No relevant test"), Node::Done("need_evidence"))]);
            decision(json!({"simulated_command":"run bookmark regression", "exit":1,"diagnostics":[{"id":"d1","text":"warning: unused import StableId"},{"id":"d2","text":"bookmark identity assertion failed after prefix insertion; observed old numerical offset"},{"id":"d3","text":"test suite aborted because bookmark regression failed"}]}),
                "Which diagnostic identifies the behavior to investigate, rather than a warning or downstream consequence?",
                vec![("d1",json!("Unused import warning"),Node::Done("need_evidence")),("d2",json!("Bookmark identity assertion with offset observation"),tests),("d3",json!("Suite aborted"),Node::Done("need_evidence"))])
        }
        Scenario::Source => {
            let inspect = decision(json!({"simulated_command":"read worker", "source":"let value = compute(); record_cancel_metric(); publish(value);", "index":[{"id":"e1","destination":"record_cancel_metric","kind":"telemetry","preview":"counter increment"},{"id":"e2","destination":"publish","kind":"result transfer","preview":"checks token then enqueues value"}]}),
                "Which relationship should be inspected to locate the gate where cancellation prevents an already computed result reaching its requester?",
                vec![("e1",json!("Cancellation telemetry"),Node::Done("need_evidence")),("e2",json!("Result publication"),assessment(json!({"simulated_command":"read publish", "source":"if token.cancelled() { return Dropped; } requester.enqueue(value);", "revision":"r17"}),
                    "Does inspected source establish a cancellation gate between computation and enqueue?", &[("supported","Exact source witnesses the gate"),("unknown","Source is insufficient")]))]);
            decision(
                json!({"simulated_command":"rg --json cancel|publish|reply src", "hits":[{"id":"h1","path":"metrics.rs","text":"cancel_count.increment()"},{"id":"h2","path":"worker.rs","text":"compute then publish result"},{"id":"h3","path":"docs.rs","text":"string literal: cancellation is supported"}]}),
                "Where should we begin finding the actual result-delivery gate?",
                vec![
                    (
                        "h1",
                        json!("Metrics implementation"),
                        Node::Done("need_evidence"),
                    ),
                    ("h2", json!("Worker compute/publication path"), inspect),
                    (
                        "h3",
                        json!("Documentation string"),
                        Node::Done("need_evidence"),
                    ),
                ],
            )
        }
        Scenario::Context => {
            let apply = assessment(json!({"simulated_commands":["read accepted C7","git diff r17..r18 -- receiver.rs"],
                "contract":{"revision":"r17","text":"A replay carrying the same stable message ID must not invoke the consumer twice. Receiver owns deduplication."},
                "diff":"r18 renames a local variable; stable message ID and dedup contract are unchanged",
                "worker_question":"Must receiver skip duplicate callbacks after an acknowledgment timeout, or should sender avoid retries?"}),
                "Can the accepted decision answer the worker locally, or does this require a new shared semantic decision?",
                &[("deliver","Deliver existing receiver-dedup contract with exact revision/diff evidence"),("consult","Shared semantics remain undecided; consult semantic owner"),("unknown","More source evidence required")]);
            decision(
                json!({"simulated_command":"rg retry|ack|dedup project-plan", "worker_question":"Who removes receiver-visible duplicate callbacks after an ack timeout?", "hits":[{"id":"c1","status":"accepted","summary":"Receiver deduplicates stable message IDs"},{"id":"c2","status":"draft","summary":"Maybe disable retries entirely"},{"id":"c3","status":"accepted","summary":"UI deduplicates repeated progress snapshots"}]}),
                "Which decision governs this exact operation and failure condition?",
                vec![
                    ("c1", json!("Accepted receiver semantics"), apply),
                    (
                        "c2",
                        json!("Draft retry suggestion"),
                        Node::Done("need_evidence"),
                    ),
                    (
                        "c3",
                        json!("UI progress rendering"),
                        Node::Done("need_evidence"),
                    ),
                ],
            )
        }
        Scenario::Swarm => {
            let final_node = |packet| {
                assessment(json!({"assembled_packet":packet}),
                "Using only assembled_packet, is there sufficient evidence of an unresolved shared semantic decision for the semantic owner? Do not fill gaps from earlier observations.",
                &[("consult","Packet establishes the missing contract decision and interacting behaviors"),("deliver","Contract already determines a local repair"),("unknown","Packet lacks essential evidence")])
            };
            let contract = json!({"id":"C7","text":"Retries may repeat delivery; receiver-visible duplication semantics were left undecided"});
            let left = json!({"id":"d19","observed":"ack timeout retries same ID; receiver invokes callback again"});
            let right = json!({"id":"s11","observed":"history appends a visible row for each callback; consumers require one visible row per message"});
            let packet = json!({"contract":contract,"delivery":left,"consumer":right});
            let assemble = decision(json!({"simulated_commands":["git show d19","git show s11"],"contract":contract,"delivery":left,"consumer":right,
                "unrelated":"UI u9 reviewed and ready","old_review":"d18 approved for single-send test"}),
                "Which smallest self-contained packet lets the semantic owner decide the retry/history interaction?",
                vec![("joint",packet.clone(),final_node(packet)),("delivery_only",json!({"delivery":left}),final_node(json!({"delivery":left}))),
                    ("old_approval",json!({"review":"d18 passed"}),final_node(json!({"review":"d18 passed"})))]);
            decision(json!({"agents":[{"id":"a","role":"delivery","publication":"timeout repeats callbacks"},{"id":"b","role":"history","publication":"each callback appends a row; must show each message once"},{"id":"c","role":"UI","publication":"progress layout done"}],"supervision":{"a":"root","b":"ui-lead"},"contract_summary":"Duplicate visibility semantics unresolved"}),
                "Which pair of publications exposes a shared semantic interaction needing source inspection?",
                vec![("ab",json!("Delivery callbacks and visible history"),assemble),("ac",json!("Delivery callbacks and UI layout"),Node::Done("need_evidence")),("bc",json!("History and UI layout"),Node::Done("need_evidence"))])
        }
        Scenario::TraceDelivery => trace_fixture(true),
        Scenario::TraceNoPath => trace_fixture(false),
        Scenario::VerifyBehavior => verify_fixture(true),
        Scenario::VerifyNoConsumer => verify_fixture(false),
        Scenario::ReuseFit => reuse_fixture(true),
        Scenario::ReuseNoFit => reuse_fixture(false),
        Scenario::MigrationCurrent => migration_fixture(true),
        Scenario::MigrationStale => migration_fixture(false),
        Scenario::ReproducerReachable => reproducer_fixture(true),
        Scenario::ReproducerNoPath => reproducer_fixture(false),
    }
}

fn trace_fixture(has_path: bool) -> Node {
    let test = search_decision(
        json!({"tool":"lsp.references + enclosingTests", "inquiry":"Request finishes without notifying caller",
            "terminal_symbol":"complete_request", "tests": if has_path {json!([
                {"id":"t1","name":"request_finishes_after_enqueue","asserts":"request result and enqueue success; notification is mocked"},
                {"id":"t2","name":"completion_notifies_waiting_owner","asserts":"owner notification is observed after request completion"},
                {"id":"t3","name":"queue_capacity","asserts":"queue capacity only"}])} else {json!([
                {"id":"t1","name":"request_finishes_after_enqueue","asserts":"request result only; notification is mocked"},
                {"id":"t3","name":"queue_capacity","asserts":"queue capacity only"}])}}),
        "Which existing test directly observes whether completion notifies the waiting owner?",
        "Does any supplied test directly observe the notification, rather than mocking it or testing a neighboring property?",
        vec![
            ("t1",json!({"what":"Completion test with notification mocked","not_for":"Observing notification delivery"}),Node::Done("wrong_test")),
            ("t2",json!({"what":"Observes owner notification after completion","evidence":"Asserts notification receipt"}),Node::Done("test_selected")),
            ("t3",json!({"what":"Queue capacity test","not_for":"Request completion notification"}),Node::Done("wrong_test")),
            ("no_test",json!({"meaning":"No supplied test directly observes the behavior"}),Node::Done("need_test")),
        ]);
    let test = if has_path {
        test
    } else {
        without_alternative(test, "t2")
    };
    let call = search_decision(
        json!({"tool":"lsp.outgoingCalls + source.read", "inquiry":"Find the decision that can skip notifying the requester",
            "symbol":"finish_request", "source":"persist(result); settle(cell); if watchers.is_empty() { return; } notify_owner(cell);",
            "calls":[{"id":"c1","symbol":"persist","preview":"write result bytes"},{"id":"c2","symbol":"settle","preview":"mark response terminal"},{"id":"c3","symbol":"notify_owner","preview":"deliver native owner wake"}]}),
        "Which call or control edge determines whether a finished request notifies its owner?",
        "Does the supplied source expose a call or control edge that governs owner notification?",
        vec![
            (
                "c1",
                json!({"what":"Persists result","not_for":"Owner notification"}),
                Node::Done("wrong_edge"),
            ),
            (
                "c2",
                json!({"what":"Settles response cell","not_for":"Conditional wake delivery"}),
                Node::Done("wrong_edge"),
            ),
            (
                "c3",
                json!({"what":"Notifies owner","precondition":"Skipped when watchers is empty"}),
                test.clone(),
            ),
            (
                "guard",
                json!({"what":"Early-return guard","effect":"Suppresses notify_owner when no watchers exist"}),
                test,
            ),
            (
                "no_edge",
                json!("No supplied edge governs notification"),
                Node::Done("need_source"),
            ),
        ],
    );
    search_decision(
        json!({"tool":"bash reproduction + parsed events", "symptom":"Request cell settles, owner receives no wake",
            "events":[{"id":"e1","kind":"storage","source":"persist.rs:41","detail":"result bytes written"},
                {"id":"e2","kind":"request_transition","source":"request.rs:219","detail":"Running -> Settled"},
                {"id":"e3","kind":"unrelated","source":"metrics.rs:9","detail":"wake counter sampled"}]}),
        "Which event is the best precise entry point for tracing why a settled request produced no owner wake?",
        "Does any supplied event locate the request completion path relevant to the missing wake?",
        vec![("e1",json!({"what":"Storage write","not_for":"Completion control flow"}),Node::Done("wrong_start")),
             ("e2",json!({"what":"Exact terminal transition","relation":"Precedes conditional wake delivery"}),call),
             ("e3",json!({"what":"Metrics sample","not_for":"Wake decision implementation"}),Node::Done("wrong_start")),
             ("no_event",json!("No event locates the relevant path"),Node::Done("need_event"))])
}

fn verify_fixture(has_consumer: bool) -> Node {
    let tests = search_decision(
        json!({"tool":"test discovery", "changed_contract":"Decode returns explicit Incomplete when bytes end mid-frame",
            "affected_consumers": if has_consumer {json!(["stream_reader::resume"])} else {json!([])},
            "tests":[{"id":"t1","asserts":"roundtrip complete frame"},{"id":"t2","asserts":"partial frame resumes after more bytes"},{"id":"t3","asserts":"CLI help text"}]}),
        "Which test exercises the affected incomplete-frame behavior with a useful assertion?",
        "Does any supplied test exercise incomplete-frame handling for an affected consumer?",
        vec![
            (
                "t1",
                json!({"covers":"Complete frame only","not_for":"Incomplete resume"}),
                Node::Done("wrong_test"),
            ),
            (
                "t2",
                json!({"covers":"Partial frame then resume","assertion":"No error before remaining bytes arrive"}),
                Node::Done("focused_test"),
            ),
            ("t3", json!("CLI help rendering"), Node::Done("wrong_test")),
            (
                "no_test",
                json!("No applicable behavioral test"),
                Node::Done("need_test"),
            ),
        ],
    );
    let consumers = search_decision(
        json!({"tool":"lsp.references + source summaries", "changed_contract":"Decoder now distinguishes incomplete input from malformed input",
            "references": if has_consumer {json!([
                {"id":"r1","symbol":"stream_reader::resume","behavior":"buffers Incomplete and reads more bytes"},
                {"id":"r2","symbol":"debug_dump","behavior":"prints decoder type name only"},
                {"id":"r3","symbol":"docs_example","behavior":"mentions decode in prose"}])} else {json!([
                {"id":"r2","symbol":"debug_dump","behavior":"prints decoder type name only"},
                {"id":"r3","symbol":"docs_example","behavior":"mentions decode in prose"}])}}),
        "Which caller behavior depends on the changed incomplete-versus-malformed distinction?",
        "Does any supplied production caller branch on or otherwise rely on that semantic distinction?",
        vec![("r1",json!({"what":"Buffers and retries only Incomplete","dependency":"Direct semantic dependency"}),tests),
             ("r2",json!({"what":"Debug name reference","not_for":"Decoder behavior"}),Node::Done("not_consumer")),
             ("r3",json!({"what":"Prose reference","not_for":"Production caller"}),Node::Done("not_consumer")),
             ("no_consumer",json!("No supplied production caller depends on the change"),Node::Done("no_affected_consumer"))]);
    let consumers = if has_consumer {
        consumers
    } else {
        without_alternative(consumers, "r1")
    };
    search_decision(
        json!({"tool":"git diff + lsp.symbolsIntersecting", "task":"Verify consumers of decoder contract change",
            "changed_symbols":[{"id":"s1","name":"DecodeError","change":"added Incomplete constructor"},
                {"id":"s2","name":"render_error","change":"wording only"},{"id":"s3","name":"README","change":"example updated"}]}),
        "Which changed symbol owns the behavioral contract requiring consumer verification?",
        "Does any changed symbol carry the behavioral contract described by the task?",
        vec![
            (
                "s1",
                json!({"what":"Closed error type","behavioral_change":"New control-flow case"}),
                consumers,
            ),
            (
                "s2",
                json!({"what":"Rendered wording","not_for":"Control flow"}),
                Node::Done("wrong_symbol"),
            ),
            ("s3", json!("Documentation"), Node::Done("wrong_symbol")),
            (
                "no_symbol",
                json!("No changed symbol carries the contract"),
                Node::Done("need_diff"),
            ),
        ],
    )
}

fn reuse_fixture(has_fit: bool) -> Node {
    let test = search_decision(
        json!({"tool":"test discovery", "required_edge":"stale source must reject mutation",
            "tests": if has_fit {json!([{"id":"t1","asserts":"edit with old digest returns StaleSource without write"},{"id":"t2","asserts":"fresh edit writes"},{"id":"t3","asserts":"path normalization"}])}
                     else {json!([{"id":"t2","asserts":"fresh edit writes"},{"id":"t3","asserts":"path normalization"}])}}),
        "Which test demonstrates the required stale-source rejection edge case?",
        "Does any supplied test assert stale-source rejection without mutation?",
        vec![
            (
                "t1",
                json!({"asserts":"Old digest rejected and file unchanged"}),
                Node::Done("reuse_evidence"),
            ),
            (
                "t2",
                json!({"asserts":"Fresh edit writes","not_for":"Stale rejection"}),
                Node::Done("wrong_test"),
            ),
            ("t3", json!("Path normalization"), Node::Done("wrong_test")),
            (
                "no_test",
                json!("No supplied test demonstrates the edge"),
                Node::Done("missing_edge_test"),
            ),
        ],
    );
    let usage = search_decision(
        json!({"tool":"lsp.incomingCalls", "candidate":"apply_checked_edit",
            "callers":[{"id":"u1","name":"workspace_patch","behavior":"passes observed digest and surfaces StaleSource"},
                {"id":"u2","name":"fixture_setup","behavior":"uses unchecked write in tests"},{"id":"u3","name":"render_preview","behavior":"formats edits without applying"}]}),
        "Which production use demonstrates the candidate's checked mutation contract?",
        "Does any supplied production caller exercise the checked mutation contract?",
        vec![
            (
                "u1",
                json!({"what":"Production checked edit","evidence":"Passes digest and handles stale result"}),
                test,
            ),
            (
                "u2",
                json!({"what":"Test setup","not_for":"Production checked mutation"}),
                Node::Done("wrong_usage"),
            ),
            (
                "u3",
                json!({"what":"Preview only","not_for":"Mutation"}),
                Node::Done("wrong_usage"),
            ),
            (
                "no_usage",
                json!("No representative production use"),
                Node::Done("need_usage"),
            ),
        ],
    );
    let root = search_decision(
        json!({"tool":"rg concept + LSP symbol resolution", "need":"Apply an edit only if source still matches the observed digest",
            "symbols": if has_fit {json!([
                {"id":"x1","name":"apply_checked_edit","contract":"compare expected digest, reject stale, then write atomically"},
                {"id":"x2","name":"write_file","contract":"unconditional overwrite"},{"id":"x3","name":"check_digest","contract":"comparison only; no mutation"}])} else {json!([
                {"id":"x2","name":"write_file","contract":"unconditional overwrite"},{"id":"x3","name":"check_digest","contract":"comparison only; caller must separately write"}])}}),
        "Which existing implementation satisfies the complete checked-mutation need?",
        "Does any single supplied implementation own comparison, stale rejection, and mutation atomically?",
        vec![("x1",json!({"what":"Atomic checked edit","covers":["compare","reject stale","write"]}),usage),
             ("x2",json!({"what":"Unconditional writer","missing":"stale check"}),Node::Done("not_fit")),
             ("x3",json!({"what":"Digest check","missing":"owned mutation; check-then-write race"}),Node::Done("not_fit")),
             ("no_fit",json!("No supplied implementation owns the full invariant"),Node::Done("no_reuse"))]);
    if has_fit {
        root
    } else {
        without_alternative(root, "x1")
    }
}

fn migration_fixture(current: bool) -> Node {
    let example = search_decision(
        json!({"tool":"lsp.references + source summaries", "migration":"Response Text became Response Report at commit m42",
            "current_revision":"m57", "callers": if current {json!([
                {"id":"u1","revision":"m57","source":"request @Report actor assignment >>= receiveReport"},
                {"id":"u2","revision":"m31","source":"request @Text actor assignment"},
                {"id":"u3","revision":"m57","source":"unrelated Response Text logging"}])} else {json!([
                {"id":"u2","revision":"m31","source":"request @Text actor assignment"},
                {"id":"u3","revision":"m57","source":"unrelated Response Text logging"}])}}),
        "Which current caller demonstrates the intended migration for this exact request API?",
        "Does any supplied caller at the current revision demonstrate the migration?",
        vec![
            (
                "u1",
                json!({"what":"Current migrated caller","matches":"Response Report request API"}),
                Node::Done("migration_evidence"),
            ),
            (
                "u2",
                json!({"what":"Pre-migration caller","not_current":true}),
                Node::Done("stale_example"),
            ),
            (
                "u3",
                json!({"what":"Current but different Response Text API","not_for":"Requested migration"}),
                Node::Done("wrong_example"),
            ),
            (
                "no_example",
                json!("No current migrated caller supplied"),
                Node::Done("need_current_example"),
            ),
        ],
    );
    let example = if current {
        example
    } else {
        without_alternative(example, "u1")
    };
    let history = search_decision(
        json!({"tool":"git log -S + git show", "type_mismatch":"expected Response Report, actual Response Text",
            "changes":[{"id":"m42","subject":"type actor replies by assignment result","diff":"request result changes from Text to typed Report; callers must select @Report"},
                {"id":"m44","subject":"rename response display","diff":"rendering names only"},{"id":"m12","subject":"add Text reply","diff":"old API introduction"}]}),
        "Which change explains the current type divergence and states the applicable migration?",
        "Does any supplied change directly explain both types in this mismatch?",
        vec![
            (
                "m42",
                json!({"what":"Introduces typed Report response","migration":"select request result type explicitly"}),
                example,
            ),
            (
                "m44",
                json!({"what":"Display rename","not_for":"Type migration"}),
                Node::Done("wrong_change"),
            ),
            (
                "m12",
                json!({"what":"Historical old API","not_for":"Current divergence"}),
                Node::Done("wrong_change"),
            ),
            (
                "no_change",
                json!("No supplied history explains mismatch"),
                Node::Done("need_history"),
            ),
        ],
    );
    search_decision(
        json!({"tool":"focused build + parsed diagnostics", "task":"Resolve request reply type migration",
            "diagnostics":[{"id":"d1","severity":"error","message":"expected Response Report, found Response Text","site":"Coordinator.hs:81"},
                {"id":"d2","severity":"warning","message":"unused import Report"},{"id":"d3","severity":"error","message":"build stopped after previous error"}]}),
        "Which diagnostic directly expresses the type mismatch needing migration archaeology?",
        "Does any supplied diagnostic directly state both actual and expected types?",
        vec![
            (
                "d1",
                json!({"what":"Primary mismatch","contains":["expected type","actual type","source site"]}),
                history,
            ),
            (
                "d2",
                json!("Unused import warning"),
                Node::Done("wrong_diagnostic"),
            ),
            (
                "d3",
                json!("Downstream stop"),
                Node::Done("wrong_diagnostic"),
            ),
            (
                "no_diagnostic",
                json!("No direct mismatch supplied"),
                Node::Done("need_diagnostic"),
            ),
        ],
    )
}

fn reproducer_fixture(reachable: bool) -> Node {
    let probe = search_decision(
        json!({"tool":"baseline test output + available diagnostic modes", "suspected_paths":["cancel-before-publish","publish-then-cancel"],
            "baseline":"intermittent missing reply, no ordering trace",
            "modes":[{"id":"p1","effect":"trace cancellation and publish sequence with request IDs"},
                {"id":"p2","effect":"enable allocator statistics"},{"id":"p3","effect":"repeat test without additional observations"}]}),
        "Which available diagnostic mode distinguishes the two suspected orderings?",
        "Does any supplied mode observe the relative order of cancellation and publication?",
        vec![
            (
                "p1",
                json!({"what":"Ordering trace","observes":["request ID","cancel timestamp","publish timestamp"]}),
                Node::Done("diagnostic_reproducer"),
            ),
            (
                "p2",
                json!({"what":"Allocator stats","not_for":"Ordering"}),
                Node::Done("wrong_probe"),
            ),
            (
                "p3",
                json!({"what":"Repeat only","not_for":"Distinguishing paths"}),
                Node::Done("wrong_probe"),
            ),
            (
                "no_probe",
                json!("No supplied diagnostic distinguishes order"),
                Node::Done("need_instrumentation"),
            ),
        ],
    );
    let fixture_node = search_decision(
        json!({"tool":"source.testFixtures", "path_preconditions":["request computed","cancellation races publish"],
            "fixtures": if reachable {json!([
                {"id":"f1","setup":"single request with controllable gate between compute and publish","cost_ms":40},
                {"id":"f2","setup":"full integration suite with random cancellation","cost_ms":8000},
                {"id":"f3","setup":"request without cancellation","cost_ms":20}])} else {json!([
                {"id":"f2","setup":"full integration suite; cancellation timing not controllable","cost_ms":8000},
                {"id":"f3","setup":"request without cancellation","cost_ms":20}])}}),
        "Which smallest supplied fixture preserves every path precondition?",
        "Does any supplied fixture preserve both computation and a controllable cancellation/publish race?",
        vec![("f1",json!({"what":"Controllable race fixture","preserves":["compute","cancel/publish race"],"small":true}),probe),
             ("f2",json!({"what":"Broad suite","missing":"controlled ordering; not minimal"}),Node::Done("not_reproducer")),
             ("f3",json!({"what":"No cancellation","missing":"race precondition"}),Node::Done("not_reproducer")),
             ("no_fixture",json!("No supplied fixture preserves all preconditions"),Node::Done("need_fixture"))]);
    let root = search_decision(
        json!({"tool":"lsp.incomingCalls backward frontier", "target":"publish: cancellation check before enqueue",
            "callers": if reachable {json!([
                {"id":"c1","symbol":"worker::finish","path":"test -> worker::finish -> publish","preconditions":"test can inject token and gate publish"},
                {"id":"c2","symbol":"metrics::record","path":"metrics only"},
                {"id":"c3","symbol":"production_loop","path":"daemon entry; no test fixture or controllable token"}])} else {json!([
                {"id":"c2","symbol":"metrics::record","path":"metrics only"},
                {"id":"c3","symbol":"production_loop","path":"daemon entry; no test fixture or controllable token"}])}}),
        "Which supplied caller path can plausibly reach the target from a controllable test?",
        "Does any supplied path connect a test-controlled entry to the target with required cancellation inputs?",
        vec![("c1",json!({"what":"Test-controllable path","carries":["token","publish gate"]}),fixture_node),
             ("c2",json!({"what":"Metrics path","not_for":"Result publication"}),Node::Done("wrong_path")),
             ("c3",json!({"what":"Production-only path","missing":"test control"}),Node::Done("not_controllable")),
             ("no_path",json!("No supplied test-controllable path reaches target"),Node::Done("need_test_seam"))]);
    if reachable {
        root
    } else {
        without_alternative(root, "c1")
    }
}

pub async fn run(
    scenario: Scenario,
    output_dir: PathBuf,
    model: &str,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let key =
        std::env::var("TYPESAFE_API_KEY").map_err(|_| "Set TYPESAFE_API_KEY before simulations")?;
    crate::bearer(&key)?;
    // Exclusive directory reservation prevents accidental reuse of a partial run.
    std::fs::create_dir(&output_dir)?;
    let mut summary_file = crate::new_evidence_file(&output_dir.join("summary.json"))?;
    let mut node = fixture(scenario);
    let mut steps = Vec::new();
    let mut outcome = "call_budget_exhausted";
    for index in 0..3 {
        let Node::Decision {
            observation,
            question,
            alternatives,
            presence,
        } = node
        else {
            break;
        };
        let criteria: Map<String, Value> = alternatives
            .iter()
            .map(|(id, (description, _))| (id.clone(), description.clone()))
            .collect();
        // Final packet sufficiency sees only the actual selected packet. Other
        // decisions get prior observations, but never unselected fixture branches.
        let prior = if observation.get("assembled_packet").is_some() {
            json!([])
        } else {
            json!(steps)
        };
        let mut questions = Map::new();
        questions.insert(
            "next".into(),
            json!({"type":"choice","instructions":question,"criteria":criteria}),
        );
        if let Some(presence) = presence {
            questions.insert("exists".into(), json!({"type":"noul","instructions":presence,
                "criteria":{"true":{"meaning":"At least one supplied candidate satisfies the requested relationship"},
                            "false":{"meaning":"No supplied candidate satisfies it"}}}));
        }
        let request = json!({"model":model,"state":{"observation":observation,"prior_steps":prior},
            "questions":questions});
        let (evidence, valid) = crate::capture_exchange(
            "https://api.typesafe.ai/v1/systemone",
            Some(format!("simulation-{scenario:?}-{index}")),
            Some(request),
            Some(key.clone()),
            output_dir.join(format!("step-{index}.json")),
            Duration::from_secs(15),
        )
        .await?;
        if !valid {
            outcome = "provider_or_contract_failure";
            break;
        }
        let Some(crate::interpret::Answer::Choice {
            choice,
            probabilities,
            ..
        }) = evidence
            .interpretation
            .as_ref()
            .and_then(|i| i.evaluation.as_ref())
            .and_then(|e| e.answers.get("next"))
        else {
            outcome = "invalid_answer";
            break;
        };
        let (_, next) = alternatives
            .get(choice)
            .ok_or("validated choice lacks local continuation")?;
        let exists = evidence
            .interpretation
            .as_ref()
            .and_then(|i| i.evaluation.as_ref())
            .and_then(|e| e.answers.get("exists"))
            .and_then(|answer| match answer {
                crate::interpret::Answer::Noul { noul } => Some(*noul),
                _ => None,
            });
        steps.push(json!({"observation":observation,"selected":choice,
            "probabilities":probabilities,"exists":exists}));
        node = next.clone();
        if let Node::Done(result) = node {
            outcome = result;
            break;
        }
    }
    let summary = json!({"scenario":format!("{scenario:?}"),"outcome":outcome,"steps":steps,"synthetic_commands":true});
    serde_json::to_writer_pretty(&mut summary_file, &summary)?;
    summary_file.write_all(b"\n")?;
    summary_file.sync_all()?;
    println!("{summary}");
    Ok(
        if matches!(
            outcome,
            "provider_or_contract_failure" | "invalid_answer" | "call_budget_exhausted"
        ) {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn verify(node: &Node, depth: usize) {
        match node {
            Node::Done(_) => assert!(depth <= 3),
            Node::Decision { alternatives, .. } => {
                assert!(!alternatives.is_empty() && alternatives.len() <= 255);
                for (_, next) in alternatives.values() {
                    verify(next, depth + 1);
                }
            }
        }
    }
    #[test]
    fn every_simulated_path_is_bounded_including_wrong_choices() {
        for scenario in Scenario::value_variants() {
            verify(&fixture(*scenario), 0);
        }
    }
}
