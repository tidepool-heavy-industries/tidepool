//! Structured synthetic swarm observations, not live actor state or authority.
use serde_json::{json, Map, Value};

fn world(local: bool, renamed: bool) -> Value {
    let ids = if renamed {
        ["n73", "n08", "n51", "n24", "n96", "n42"]
    } else {
        [
            "planner",
            "integration",
            "delivery",
            "search",
            "ui",
            "reviewer",
        ]
    };
    let actors: Vec<Value> = ids.iter().enumerate().map(|(i, id)| json!({
        "id": id,
        "responsibility": (["Own shared delivery semantics; consult only when accepted contract cannot settle cross-component disagreement",
            "Integrate checked artifacts; cannot change shared semantics",
            "Implement durable handoff and retries",
            "Implement searchable history of acknowledged handoffs",
            "Render progress; no dependency on retry semantics",
            "Review delivery candidates against accepted contract"][i]),
        "supervised_by": if i == 0 {Value::Null} else if i == 1 {json!(ids[0])} else {json!(ids[1])},
        "context_parent": if i == 3 {json!(ids[4])} else {json!(ids[0])},
        "native_model": if i == 0 {"large-reasoner"} else {"small-worker"}
    })).collect();
    json!({
        "actors": actors,
        "contract": {"id": "C7", "status": "accepted", "text": if local {
            "Acceptance requires a durable acknowledgment before success. Retries after missing acknowledgment may repeat transport delivery; receiver must deduplicate by stable message ID. One user-visible effect per message ID."
        } else {
            "Acceptance requires a durable acknowledgment before success. Retries after missing acknowledgment may repeat delivery. No decision was made about whether receiver-visible duplicates are permitted or which component removes them."
        }},
        "references": {"semantic_owner": ids[0], "integrator": ids[1], "delivery_owner": ids[2], "search_owner": ids[3], "ui_owner": ids[4], "review_owner": ids[5]},
        "observations": [
            {"id": "o1", "author": ids[2], "contract": "C7", "revision": "d19", "text": "On an acknowledgment timeout we send the same message ID again. The receiver callback is invoked for every delivery; no deduplication there. Our retry test passes."},
            {"id": "o2", "author": ids[3], "contract": "C7", "revision": "s11", "text": "Each receiver callback appends a visible history row. Consumers expect a message to appear once. Our tests use a single delivery."},
            {"id": "o3", "author": ids[5], "revision": "d18", "text": "Approved delivery implementation", "tested": "single send and ack", "applies_to": "d18 only"},
            {"id": "o4", "author": ids[4], "revision": "u9", "text": "Progress rendering complete; tests pass; review at u9 accepted", "depends_on": []},
            {"id": "o5", "author": ids[2], "revision": "d19", "text": "It is just internal retry plumbing, unrelated to history display. Probably no need to involve anyone else."}
        ],
        "policies": {"hold": "Only affected work is held; independent reviewed work may proceed",
            "consult": "Large reasoner handles missing shared semantic decisions. Known contract violations go to the responsible small worker.",
            "evidence": "Approval applies only to its exact candidate. Worker opinions are claims, not authoritative dependency facts."}
    })
}

fn choice(instructions: Value, criteria: Value) -> Value {
    json!({"type":"choice", "instructions":instructions, "criteria":criteria})
}

pub fn swarm(model: &str, local: bool, renamed: bool) -> Value {
    let state = world(local, renamed);
    let mut questions = Map::new();
    let mut recipients = Map::new();
    for actor in state["actors"].as_array().unwrap() {
        recipients.insert(actor["id"].as_str().unwrap().to_owned(), actor.clone());
    }
    recipients.insert(
        "nobody".into(),
        json!({"meaning":"No actor needs to handle this"}),
    );
    questions.insert("decision.owner".into(), choice(
        json!({"question":"Who should own the next substantive decision or repair concerning the interaction of o1 and o2?", "rule":"Use accepted contract and responsibilities, not supervisor/context ancestry or o5's opinion."}),
        Value::Object(recipients)));
    questions.insert("decision.kind".into(), choice(json!("What does the interaction of o1, o2 and accepted contract require next?"), json!({
        "shared_decision": {"meaning":"Shared semantics are undecided; semantic owner must resolve the contract before repair is specified"},
        "local_repair": {"meaning":"Accepted contract already decides the behavior; delivery owner must implement receiver deduplication"},
        "ship": {"meaning":"Both implementations already satisfy the accepted contract together"},
        "unknown": {"meaning":"Insufficient evidence even to identify the kind of next step"}
    })));
    questions.insert(
        "decision.witness".into(),
        choice(
            json!(
                "Which supplied observation pair most directly exposes the cross-component issue?"
            ),
            json!({
                "o1_o2": [state["observations"][0].clone(), state["observations"][1].clone()],
                "o3_o4": [state["observations"][2].clone(), state["observations"][3].clone()],
                "o4_o5": [state["observations"][3].clone(), state["observations"][4].clone()]
            }),
        ),
    );
    for (branch, description) in [
        ("delivery", "delivery candidate d19"),
        ("search", "search integration with repeated delivery"),
        ("ui", "independent progress-rendering candidate u9"),
    ] {
        questions.insert(format!("branches.{branch}.action"), choice(
            json!(format!("What should happen next to {description}, under the supplied contract and policies?")), json!({
                "continue": {"meaning":"May proceed through ordinary integration without waiting for retry/history semantics"},
                "hold_for_contract": {"meaning":"Hold affected integration until semantic owner resolves the missing shared decision"},
                "repair_or_revalidate": {"meaning":"Contract already suffices; repair or revalidate affected behavior against it"}
            })));
        questions.insert(format!("branches.{branch}.affected"), json!({"type":"noul", "instructions":format!("Is {description} semantically affected by the interaction of o1 and o2?")}));
        questions.insert(format!("branches.{branch}.readiness"), json!({"type":"score", "instructions":format!("How strong is current acceptance evidence for {description}?"), "criteria":[
            {"meaning":"Evidence exposes an unresolved correctness or contract issue affecting this candidate"},
            {"meaning":"Only partial or older-candidate evidence is available; current behavior remains unverified"},
            {"meaning":"Current exact candidate has applicable review and tests without a supplied unresolved issue"}
        ]}));
    }
    questions.insert(
        "evidence.old_review_applies".into(),
        json!({"type":"noul", "instructions":"Does o3 establish review approval of d19 itself?"}),
    );
    questions.insert("evidence.opinion_overrides".into(), json!({"type":"noul", "instructions":"Does o5's opinion establish that search/history cannot be affected by repeated receiver callbacks?"}));
    json!({"model":model, "state":state, "questions":questions})
}

pub fn packet(model: &str) -> Value {
    let state = world(false, false);
    let pieces = [
        json!({"id":"contract", "content":state["contract"]}),
        json!({"id":"delivery_trace", "content":state["observations"][0]}),
        json!({"id":"consumer_behavior", "content":state["observations"][1]}),
        json!({"id":"old_approval", "content":state["observations"][2]}),
        json!({"id":"ui_progress", "content":state["observations"][3]}),
        json!({"id":"worker_opinion", "content":state["observations"][4]}),
    ];
    let criteria: Map<String, Value> = (0u32..64)
        .map(|mask| {
            let includes: Vec<Value> = pieces
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, p)| p.clone())
                .collect();
            // Scramble labels so the option name does not encode subset membership.
            (
                format!("packet_{:02}", (mask * 17 + 11) % 64),
                json!({"pieces": includes}),
            )
        })
        .collect();
    json!({"model":model, "state": {"world":state,
        "task":"Select the smallest self-contained evidence packet allowing the semantic owner to understand the unresolved retry/history contract issue, its mechanism, and both affected behaviors. Do not include unrelated progress, nonapplicable approval, or unsupported conclusions. All subsets are supplied; select by content."},
        "questions":{"packet":choice(json!("Which candidate is the smallest sufficient packet for `task`?"), Value::Object(criteria))}})
}

pub fn packet_membership(model: &str) -> Value {
    let state = world(false, false);
    let pieces = [
        ("contract", "contract"),
        ("delivery_trace", "observations[0]"),
        ("consumer_behavior", "observations[1]"),
        ("old_approval", "observations[2]"),
        ("ui_progress", "observations[3]"),
        ("worker_opinion", "observations[4]"),
    ];
    let questions: Map<String, Value> = pieces.iter().map(|(id, path)| (
        (*id).into(), json!({"type":"noul", "instructions":{
            "question":format!("Must the smallest sufficient evidence packet include `{path}` so the semantic owner can understand the unresolved retry/history contract issue, its mechanism, and both affected behaviors?"),
            "rule":"Judge necessity given all the supplied state. Exclude unrelated progress, nonapplicable approval, and unsupported conclusions."
        }})
    )).collect();
    json!({"model":model, "state":state, "questions":questions})
}
