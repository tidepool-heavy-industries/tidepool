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
    Nouls(BTreeMap<&'static str, bool>),
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

fn consistency_case(model: &str) -> Case {
    let expected = BTreeMap::from([
        ("approved", false),
        ("not_approved", true),
        ("receipt_proves_handling", false),
        ("handling_can_be_unknown", true),
        ("local_contract_resolves", false),
        ("semantic_decision_needed", true),
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
    out
}

pub async fn run(output_dir: PathBuf, model: &str) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let key = std::env::var("TYPESAFE_API_KEY")
        .map_err(|_| "Set TYPESAFE_API_KEY before frontier runs")?;
    crate::bearer(&key)?;
    std::fs::create_dir(&output_dir)?;
    let mut summary_file = crate::new_evidence_file(&output_dir.join("summary.json"))?;
    let mut observations = Vec::new();
    for case in cases(model) {
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
            Expected::Nouls(expected) => {
                let answers:Map<String,Value>=expected.iter().map(|(name,want)|{
                    let p=match evaluation.and_then(|e|e.answers.get(*name)){Some(Answer::Noul{noul})=>Some(*noul),_=>None};
                    ((*name).into(),json!({"expected":want,"p_yes":p,"correct_side":p.is_some_and(|x|(x>=0.5)==*want)}))}).collect();
                (
                    valid && answers.values().all(|v| v["correct_side"] == json!(true)),
                    Value::Object(answers),
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
        assert_eq!(suite.len(), 25);
        assert!(suite
            .iter()
            .any(|case| case.name == "constraints-255-valid-3"));
        assert!(suite.iter().any(|case| case.name == "pointer-depth-64"));
    }
}
