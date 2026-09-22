//! Adversarial Jev-shaped cases. They test selection and judgment boundaries,
//! not domain truth, tool execution, or the future public DSL.
use crate::interpret::Answer;
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, io::Write, path::PathBuf, process::ExitCode, time::Duration};

enum Expected {
    Choice {
        question: &'static str,
        selected: String,
    },
    ChoiceSet {
        question: &'static str,
        allowed: Vec<String>,
        min_allowed_mass: f64,
        max_pair_gap: f64,
    },
    Nouls(BTreeMap<String, bool>),
    Mixed {
        choices: BTreeMap<String, String>,
        nouls: BTreeMap<String, bool>,
    },
}

struct Case {
    name: String,
    request: Value,
    expected: Expected,
}

fn opaque(index: usize, salt: usize) -> String {
    format!("k{:03}", (index * 73 + salt * 41 + 19) % 997)
}

fn constraint_case(model: &str, count: usize, valid: bool, salt: usize) -> Case {
    let winner = (salt * 11 + 7) % (count - 1);
    let mut criteria = Map::new();
    let mut selected = "none".to_owned();
    for i in 0..count - 1 {
        let key = opaque(i, salt);
        let is_winner = valid && i == winner;
        if is_winner {
            selected = key.clone();
        }
        let violated = i % 6;
        criteria.insert(key, json!({
            "candidate": {"opaque_index": i, "irrelevant_build_ms": 20 + i},
            "ownership": if is_winner || violated != 0 {"parser"} else {"renderer"},
            "source_revision": if is_winner || violated != 1 {"r19"} else {"r18"},
            "write_authority": is_winner || violated != 2,
            "blocked_dependency": if is_winner || violated != 3 {Value::Null} else {json!("compiler-cutover")},
            "result_type": if is_winner || violated != 4 {"Patch"} else {"Report"},
            "availability": if is_winner || violated != 5 {"before-review"} else {"after-review"}
        }));
    }
    criteria.insert(
        "none".into(),
        json!({"meaning":"No candidate satisfies every required constraint"}),
    );
    Case {
        name: format!(
            "constraints-{count}-{}-{salt}",
            if valid { "valid" } else { "none" }
        ),
        request: json!({"model":model,"state":{
            "task":"Select an executor only if every requirement holds simultaneously.",
            "requirements":{"ownership":"parser","source_revision":"r19","write_authority":true,
                "blocked_dependency":null,"result_type":"Patch","availability":"before-review"},
            "rule":"Candidate names and irrelevant build times carry no meaning. Choose none unless one candidate satisfies all six requirements."
        },"questions":{"executor":{"type":"choice","instructions":{
            "question":"Which candidate satisfies every requirement?","method":"Compare every named field; one failed field disqualifies a candidate."},
            "criteria":criteria}}}),
        expected: Expected::Choice {
            question: "executor",
            selected,
        },
    }
}

fn graph_case(model: &str, depth: usize, valid: bool) -> Case {
    let mut edges = Vec::new();
    let mut correct = Vec::new();
    for i in 0..depth {
        let id = format!("e{i}_good");
        correct.push(id.clone());
        edges.push(
            json!({"id":id,"from":format!("n{i}"),"to":format!("n{}",i+1),
            "kind":if i % 2 == 0 {"calls"} else {"returns-to"},"current":true,"forbidden":false}),
        );
        edges.push(
            json!({"id":format!("e{i}_stale"),"from":format!("n{i}"),"to":format!("n{}",i+1),
            "kind":if i % 2 == 0 {"calls"} else {"returns-to"},"current":false,"forbidden":false}),
        );
        edges.push(
            json!({"id":format!("e{i}_wrong"),"from":format!("n{i}"),"to":format!("x{i}"),
            "kind":"mentions","current":true,"forbidden":false}),
        );
    }
    let mut criteria = Map::new();
    let correct_key = "path_17".to_owned();
    if valid {
        criteria.insert(correct_key.clone(), json!({"edge_ids":correct}));
    }
    for j in 0..24 {
        let mut path: Vec<String> = (0..depth).map(|i| format!("e{i}_good")).collect();
        let at = j % depth;
        path[at] = if j % 2 == 0 {
            format!("e{at}_stale")
        } else {
            format!("e{at}_wrong")
        };
        criteria.insert(format!("path_{:02}", j + 30), json!({"edge_ids":path}));
    }
    criteria.insert(
        "no_path".into(),
        json!({"meaning":"No supplied path is valid"}),
    );
    Case {
        name: format!(
            "graph-depth-{depth}-{}",
            if valid { "valid" } else { "none" }
        ),
        request: json!({"model":model,"state":{
            "start":"n0","target":format!("n{depth}"),"edges":edges,
            "rules":{"required_kinds_by_step":(0..depth).map(|i|if i%2==0{"calls"}else{"returns-to"}).collect::<Vec<_>>(),
                "require_current":true,"forbid_flagged_edges":true,"require_connected_path":true},
            "instruction":"Resolve edge IDs against edges. A path is valid only if every step has the required kind, is current, is not forbidden, and connects continuously from start to target."
        },"questions":{"path":{"type":"choice","instructions":"Which supplied path satisfies every graph rule? Choose no_path if none does.","criteria":criteria}}}),
        expected: Expected::Choice {
            question: "path",
            selected: if valid { correct_key } else { "no_path".into() },
        },
    }
}

fn pointer_case(model: &str, depth: usize, salt: usize) -> Case {
    let node_count = depth * 2 + 1;
    let node_id = |index: usize| opaque((index * 31 + salt * 17) % 997, salt + 29);
    let chain: Vec<String> = (0..=depth).map(node_id).collect();
    let mut links = Vec::new();
    for i in 0..depth {
        links.push(json!({"node":chain[i],"next":chain[i + 1]}));
    }
    for i in depth + 1..node_count {
        links.push(json!({"node":node_id(i),"next":node_id(depth + 1 + (i + 1) % depth)}));
    }
    // Present the relation out of traversal order. The opaque identifiers and
    // record order must not disclose the endpoint.
    links.sort_by_key(|link| link["node"].as_str().unwrap().to_owned());
    let mut criteria = Map::new();
    for i in 0..node_count {
        let id = node_id(i);
        criteria.insert(format!("endpoint_{i:03}"), json!({"node":id}));
    }
    let selected = format!("endpoint_{depth:03}");
    Case {
        name: format!("pointer-depth-{depth}"),
        request: json!({"model":model,"state":{
            "start":chain[0],"hops":depth,"links":links,
            "rule":"Beginning at start, apply the supplied node-to-next relation exactly once per hop. Select the candidate containing the node reached after exactly hops transitions. Identifier spelling, candidate index, and record order carry no meaning."
        },"questions":{"endpoint":{"type":"choice","instructions":{
            "question":"Which candidate contains the exact endpoint?",
            "method":"Resolve each next identifier back to a node and repeat for exactly the stated hop count. Do not stop early and do not infer from names."},
            "criteria":criteria}}}),
        expected: Expected::Choice {
            question: "endpoint",
            selected,
        },
    }
}

fn temporal_case(model: &str, conflict: bool) -> Case {
    let decisions = json!([
        {"id":"d1","scope":"receiver duplicate callbacks","revision":"r10","status":"accepted","rule":"sender suppresses retries","superseded_by":"d4"},
        {"id":"d2","scope":"UI progress snapshots","revision":"r21","status":"accepted","rule":"UI deduplicates progress","superseded_by":null},
        {"id":"d3","scope":"receiver duplicate callbacks","revision":"r22","status":"draft","rule":"receiver deduplicates stable message IDs","superseded_by":null},
        {"id":"d4","scope":"receiver duplicate callbacks","revision":"r19","status":"accepted","rule":if conflict {"receiver deduplicates, except retries after timeout remain undecided"} else {"receiver deduplicates stable message IDs"},"superseded_by":null},
        {"id":"d5","scope":"receiver duplicate callbacks","revision":"other-branch","status":"accepted","rule":"duplicates allowed","superseded_by":null}
    ]);
    let selected = if conflict { "needs_decision" } else { "d4" };
    Case {
        name: format!("temporal-{}", if conflict { "gap" } else { "resolved" }),
        request: json!({"model":model,"state":{"question":"What governs receiver callbacks when the same message ID is retried after an acknowledgment timeout?",
            "current_revision":"r23","revision_ancestry":["r23","r22","r21","r19","r10"],"decisions":decisions,
            "rules":["Only accepted decisions on current ancestry can govern","A superseded decision cannot govern","Scope and exception must cover the exact question","Drafts and other branches are evidence, not governing decisions"]},
            "questions":{"governing":{"type":"choice","instructions":"Which accepted decision governs the exact question now? Choose needs_decision if the current accepted contract leaves this case undecided.",
                "criteria":{"d1":{"meaning":"Decision d1"},"d2":{"meaning":"Decision d2"},"d3":{"meaning":"Decision d3"},"d4":{"meaning":"Decision d4"},"d5":{"meaning":"Decision d5"},"needs_decision":{"meaning":"No current accepted decision resolves the exact case"}}}}}),
        expected: Expected::Choice {
            question: "governing",
            selected: selected.into(),
        },
    }
}

fn ambiguity_case(model: &str, salt: usize) -> Case {
    let (left, right) = [
        ("route_a", "route_b"),
        ("first", "second"),
        ("k019", "k873"),
        ("option_z", "option_a"),
    ][salt % 4];
    let mut criteria = Map::new();
    criteria.insert(left.into(), json!({"action":"Ask the parser owner to inspect the production caller","timing":"next normal wake","authority":"read-only investigation"}));
    criteria.insert(right.into(), json!({"action":"Ask the parser owner to inspect the production caller","timing":"next normal wake","authority":"read-only investigation"}));
    criteria.insert("route_c".into(), json!({"action":"Wake the release owner immediately","reason":"No release impact is present"}));
    criteria.insert(
        "route_d".into(),
        json!({"action":"Ask the renderer owner","reason":"Different subsystem"}),
    );
    Case {
        name: format!("equivalent-routes-{salt}"),
        request: json!({"model":model,"state":{
            "finding":"A parser helper has a type mismatch in a non-urgent branch.",
            "policy":"Use the owning subsystem; queue non-urgent investigations for normal wake. Equivalent continuations are equally acceptable."
        },"questions":{"route":{"type":"choice","instructions":"Which continuation best matches the finding and policy? Reflect genuine equivalence in the probability distribution.","criteria":criteria}}}),
        expected: Expected::ChoiceSet {
            question: "route",
            allowed: vec![left.into(), right.into()],
            min_allowed_mass: 0.80,
            max_pair_gap: 0.35,
        },
    }
}

fn fanout_case(model: &str, count: usize) -> Case {
    let facts: Vec<Value> = (0..count)
        .filter(|i| i % 2 == 0)
        .map(|i| json!({"agent":format!("agent_{i:03}"),"status":"blocked","cause":"awaiting review"}))
        .collect();
    let mut questions = Map::new();
    let mut expected = BTreeMap::new();
    for i in 0..count {
        let name = format!("agent_{i:03}_blocked");
        questions.insert(name.clone(), json!({"type":"noul","instructions":{
            "question":format!("Does state.agents explicitly report agent_{i:03} as blocked awaiting review?"),
            "evidence_rule":"Answer from an exact matching agent record only; absence is false."
        }}));
        expected.insert(name, i % 2 == 0);
    }
    Case {
        name: format!("fanout-{count}"),
        request: json!({"model":model,"state":{"agents":facts},"questions":questions}),
        expected: Expected::Nouls(expected),
    }
}

fn noise_case(model: &str, noise_count: usize) -> Case {
    let noise: Vec<Value> = (0..noise_count)
        .map(|i| json!({"seq":i,"component":format!("unrelated_{}",i % 17),"event":"heartbeat completed normally","revision":format!("r{}",i % 23)}))
        .collect();
    Case {
        name: format!("irrelevant-state-{noise_count}"),
        request: json!({"model":model,"state":{
            "current_finding":{"symptom":"E0308 mismatched types","owner":"parser","urgency":"normal","next_evidence":"production callers"},
            "historical_unrelated_events":noise
        },"questions":{"continuation":{"type":"choice","instructions":{
            "question":"Which next action follows current_finding?",
            "focus":"Use current_finding. Historical unrelated events are deliberately irrelevant."
        },"criteria":{
            "inspect_callers":"Inspect production callers of the parser symbol",
            "wake_release":"Wake the release owner for an emergency",
            "inspect_renderer":"Inspect renderer styling",
            "stop":"Stop without further evidence"
        }}}}),
        expected: Expected::Choice {
            question: "continuation",
            selected: "inspect_callers".into(),
        },
    }
}

fn microprogram_case(model: &str, sample: usize) -> Case {
    Case {
        name: format!("shoal-microprogram-fanout-{sample}"),
        request: json!({"model":model,"state":{
            "goal":"Explain and safely advance a duplicate visible history row after message retry.",
            "observations":{
                "inbox":"message m42 admitted once",
                "actor":"handler callback observed twice for stable message id m42, second callback followed acknowledgment timeout retry",
                "projection":"one visible history row is inserted per handler callback; no stable-message-id deduplication",
                "current_test":"retry fixture reproduces two callbacks and two visible rows"
            },
            "accepted_contract":{
                "delivery":"Retries may repeat delivery",
                "duplicate_callback":"Receiver deduplication is required for stable message IDs",
                "duplicate_visibility":"Undecided"
            },
            "ownership":{"inbox":"node","callback_delivery":"actor","visible_projection":"shoal"},
            "wake_policy":"Wake immediately only for active outage, data loss, deadlock, or a decision blocking all useful work; otherwise queue for normal wake."
        },"questions":{
            "mechanism":{"type":"choice","instructions":"Which mechanism directly explains the second callback?","criteria":{
                "actor_redelivery":"Retry redelivered stable message m42 to the actor callback",
                "inbox_double_admit":"The inbox admitted two distinct records",
                "projection_duplication":"The projection itself invoked the actor callback",
                "unknown":"The supplied observations do not distinguish a mechanism"
            }},
            "next_tool":{"type":"choice","instructions":"Which focused code-exploration action best advances a fix for the callback mechanism?","criteria":{
                "actor_handler_references":"Find references and callers for the actor message handler and its retry boundary",
                "inbox_insert_references":"Inspect only inbox insertion call sites",
                "projection_styles":"Inspect history-row rendering styles",
                "broad_repository_search":"Run a broad unstructured repository search"
            }},
            "verification_target":{"type":"choice","instructions":"Which focused verification should be run first after changing callback deduplication?","criteria":{
                "actor_retry_fixture":"The actor retry fixture using stable message id m42",
                "inbox_admission_fixture":"A fixture testing only first-time inbox admission",
                "projection_snapshot":"A visual snapshot without retries",
                "full_workspace":"Every workspace test regardless of ownership"
            }},
            "wake_now":{"type":"noul","instructions":"Does the supplied state satisfy wake_policy for immediately waking the actor owner?"},
            "mechanism_supported":{"type":"noul","instructions":"Do the supplied observations support actor retry redelivery as the mechanism for the second callback?"},
            "visibility_contract_resolved":{"type":"noul","instructions":"Does accepted_contract decide whether a second visible history row is permitted?"},
            "visibility_decision_needed":{"type":"noul","instructions":"Is a semantic decision still needed before declaring the second visible history row correct or incorrect?"}
        }}),
        expected: Expected::Mixed {
            choices: BTreeMap::from([
                ("mechanism".into(), "actor_redelivery".into()),
                ("next_tool".into(), "actor_handler_references".into()),
                ("verification_target".into(), "actor_retry_fixture".into()),
            ]),
            nouls: BTreeMap::from([
                ("wake_now".into(), false),
                ("mechanism_supported".into(), true),
                ("visibility_contract_resolved".into(), false),
                ("visibility_decision_needed".into(), true),
            ]),
        },
    }
}

fn consistency_case(model: &str) -> Case {
    let expected = BTreeMap::from([
        ("approved".into(), false),
        ("not_approved".into(), true),
        ("receipt_proves_handling".into(), false),
        ("handling_can_be_unknown".into(), true),
        ("local_contract_resolves".into(), false),
        ("semantic_decision_needed".into(), true),
    ]);
    Case {
        name: "overlapping-judgments".into(),
        request: json!({"model":model,"state":{
            "candidate":{"revision":"r19","review":{"revision":"r18","verdict":"approved"}},
            "notification":{"status":"admitted","handling":"not observed"},
            "contract":{"accepted":"Retries may repeat delivery","duplicate_visibility":"undecided"},
            "issue":"Retry produces a second receiver callback and a second visible history row"
        },"questions":{
            "approved":{"type":"noul","instructions":"Does the supplied review establish approval of candidate revision r19 itself?"},
            "not_approved":{"type":"noul","instructions":"Is approval of candidate revision r19 absent from the supplied evidence?"},
            "receipt_proves_handling":{"type":"noul","instructions":"Does admitted notification status prove its message was handled?"},
            "handling_can_be_unknown":{"type":"noul","instructions":"Can message handling still be unknown given the supplied notification state?"},
            "local_contract_resolves":{"type":"noul","instructions":"Does the accepted contract determine whether the duplicate visible row is permitted?"},
            "semantic_decision_needed":{"type":"noul","instructions":"Is a new semantic decision required before declaring the duplicate visible row correct or incorrect?"}
        }}),
        expected: Expected::Nouls(expected),
    }
}

fn cases(model: &str) -> Vec<Case> {
    let mut out = Vec::new();
    for &(count, salt) in &[(8, 0), (32, 1), (128, 2), (255, 3)] {
        out.push(constraint_case(model, count, true, salt));
    }
    for salt in 4..8 {
        out.push(constraint_case(model, 32, true, salt));
    }
    for &count in &[8, 32, 128] {
        out.push(constraint_case(model, count, false, count));
    }
    for &depth in &[3, 5, 7] {
        out.push(graph_case(model, depth, true));
    }
    out.push(graph_case(model, 7, false));
    for &(depth, salt) in &[(1, 11), (2, 12), (4, 13), (8, 1), (16, 2), (32, 3), (64, 4)] {
        out.push(pointer_case(model, depth, salt));
    }
    out.push(temporal_case(model, false));
    out.push(temporal_case(model, true));
    out.push(consistency_case(model));
    for salt in 0..4 {
        out.push(ambiguity_case(model, salt));
    }
    for &count in &[8, 32, 64, 128, 256, 512, 640, 1024] {
        out.push(fanout_case(model, count));
    }
    for &count in &[0, 64, 256, 384, 512, 768, 896, 960, 1024] {
        out.push(noise_case(model, count));
    }
    for sample in 0..4 {
        out.push(microprogram_case(model, sample));
    }
    out
}

pub async fn run(
    output_dir: PathBuf,
    model: &str,
    only: Option<&str>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let key = std::env::var("TYPESAFE_API_KEY")
        .map_err(|_| "Set TYPESAFE_API_KEY before frontier runs")?;
    crate::bearer(&key)?;
    std::fs::create_dir(&output_dir)?;
    let mut summary_file = crate::new_evidence_file(&output_dir.join("summary.json"))?;
    let mut observations = Vec::new();
    let selected_cases: Vec<Case> = cases(model)
        .into_iter()
        .filter(|case| only.is_none_or(|needle| case.name.contains(needle)))
        .collect();
    if selected_cases.is_empty() {
        return Err(format!("no frontier cases matched {only:?}").into());
    }
    for case in selected_cases {
        let path = output_dir.join(format!("{}.json", case.name));
        let (evidence, valid) = crate::capture_exchange(
            "https://api.typesafe.ai/v1/systemone",
            Some(format!("frontier-{}", case.name)),
            Some(case.request),
            Some(key.clone()),
            path,
            Duration::from_secs(30),
        )
        .await?;
        let evaluation = evidence
            .interpretation
            .as_ref()
            .and_then(|i| i.evaluation.as_ref());
        let (passed, observed) = match case.expected {
            Expected::Choice { question, selected } => {
                match evaluation.and_then(|e| e.answers.get(question)) {
                    Some(Answer::Choice {
                        choice,
                        probabilities,
                        ..
                    }) => (
                        valid && choice == &selected,
                        json!({"expected":selected,"selected":choice,"probability":probabilities.get(choice)}),
                    ),
                    _ => (false, json!({"expected":selected,"error":"missing choice"})),
                }
            }
            Expected::ChoiceSet {
                question,
                allowed,
                min_allowed_mass,
                max_pair_gap,
            } => match evaluation.and_then(|e| e.answers.get(question)) {
                Some(Answer::Choice {
                    choice,
                    probabilities,
                    ..
                }) => {
                    let values: Vec<f64> = allowed
                        .iter()
                        .map(|key| probabilities.get(key).copied().unwrap_or(0.0))
                        .collect();
                    let mass: f64 = values.iter().sum();
                    let gap = values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                        - values.iter().copied().fold(f64::INFINITY, f64::min);
                    (
                        valid
                            && allowed.contains(choice)
                            && mass >= min_allowed_mass
                            && gap <= max_pair_gap,
                        json!({"allowed":allowed,"selected":choice,"allowed_probabilities":values,
                            "allowed_mass":mass,"pair_gap":gap,"min_allowed_mass":min_allowed_mass,
                            "max_pair_gap":max_pair_gap}),
                    )
                }
                _ => (false, json!({"allowed":allowed,"error":"missing choice"})),
            },
            Expected::Nouls(expected) => {
                let answers:Map<String,Value>=expected.iter().map(|(name,want)|{
                    let p=match evaluation.and_then(|e|e.answers.get(name)){Some(Answer::Noul{noul})=>Some(*noul),_=>None};
                    (name.clone(),json!({"expected":want,"p_yes":p,"correct_side":p.is_some_and(|x|(x>=0.5)==*want)}))}).collect();
                (
                    valid && answers.values().all(|v| v["correct_side"] == json!(true)),
                    Value::Object(answers),
                )
            }
            Expected::Mixed { choices, nouls } => {
                let choice_answers: Map<String, Value> = choices
                    .iter()
                    .map(|(name, want)| {
                        let got = match evaluation.and_then(|e| e.answers.get(name)) {
                            Some(Answer::Choice { choice, .. }) => Some(choice.as_str()),
                            _ => None,
                        };
                        (name.clone(), json!({"expected":want,"selected":got,"correct":got == Some(want.as_str())}))
                    })
                    .collect();
                let noul_answers: Map<String, Value> = nouls
                    .iter()
                    .map(|(name, want)| {
                        let p = match evaluation.and_then(|e| e.answers.get(name)) {
                            Some(Answer::Noul { noul }) => Some(*noul),
                            _ => None,
                        };
                        (name.clone(), json!({"expected":want,"p_yes":p,"correct_side":p.is_some_and(|x|(x>=0.5)==*want)}))
                    })
                    .collect();
                let answers_ok = choice_answers.values().all(|v| v["correct"] == json!(true))
                    && noul_answers
                        .values()
                        .all(|v| v["correct_side"] == json!(true));
                (
                    valid && answers_ok,
                    json!({"choices":choice_answers,"nouls":noul_answers}),
                )
            }
        };
        println!("{}: {}", case.name, if passed { "pass" } else { "FAIL" });
        observations.push(json!({"case":case.name,"passed":passed,"elapsed_ms":evidence.exchange.elapsed_ms,
            "usage":evidence.response_json.as_ref().map(|r|r["usage"].clone()),"observed":observed}));
    }
    let passed = observations
        .iter()
        .filter(|o| o["passed"] == json!(true))
        .count();
    let summary =
        json!({"model":model,"passed":passed,"total":observations.len(),"cases":observations});
    serde_json::to_writer_pretty(&mut summary_file, &summary)?;
    summary_file.write_all(b"\n")?;
    summary_file.sync_all()?;
    Ok(if passed == summary["total"].as_u64().unwrap() as usize {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_answer_keys_match_exact_traversal() {
        for &(depth, salt) in &[(1, 11), (2, 12), (4, 13), (8, 1), (16, 2), (32, 3), (64, 4)] {
            let case = pointer_case("test", depth, salt);
            let Expected::Choice { selected, .. } = case.expected else {
                panic!("pointer case must produce a Choice expectation")
            };
            let state = &case.request["state"];
            let mut node = state["start"].as_str().unwrap();
            for _ in 0..depth {
                node = state["links"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|link| link["node"] == node)
                    .unwrap()["next"]
                    .as_str()
                    .unwrap();
            }
            assert_eq!(
                case.request["questions"]["endpoint"]["criteria"][selected]["node"],
                node
            );
        }
    }

    #[test]
    fn suite_has_expected_boundary_cases() {
        let suite = cases("test");
        assert_eq!(suite.len(), 50);
        assert!(suite
            .iter()
            .any(|case| case.name == "constraints-255-valid-3"));
        assert!(suite.iter().any(|case| case.name == "pointer-depth-64"));
    }
}
