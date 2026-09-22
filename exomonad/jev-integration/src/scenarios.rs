//! Synthetic application judgments. Candidate IDs resolve to local fixtures;
//! no selected command, notification, or actor action is executed.
use serde_json::{json, Value};

fn request(model: &str, state: Value, questions: Value) -> Value {
    json!({"model": model, "state": state, "questions": questions})
}

pub fn route(model: &str, no_match: bool) -> Value {
    request(
        model,
        json!({
            "revision": "synthetic-r17",
            "update": if no_match {
                "The invoice export applies the wrong VAT category to overseas customers."
            } else {
                "Lookup behavior is unchanged, but callers retaining byte offsets across edits must switch to stable IDs before accepting this patch."
            },
            "context": "Choose an existing responsible worker; do not broaden their assignments."
        }),
        json!({"recipient": {
            "type": "choice",
            "instructions": {"question": "Which candidate's current assignment is directly affected by `update`?", "fallback": "Use unassigned if no assignment fits."},
            "criteria": {
                "actor_a": {"assignment": "Review editor navigation correctness", "current_work": "Checking whether bookmarks retain offsets after document edits", "paths": ["editor/bookmarks.rs"]},
                "actor_b": {"assignment": "Implement invoice PDF typography", "current_work": "Fonts and page margins only", "excludes": ["tax logic", "invoice export"]},
                "actor_c": {"assignment": "Measure index lookup speed", "current_work": "Benchmark fresh lookups; no offsets retained"},
                "unassigned": {"means": "None of these assignments owns the affected behavior"}
            }
        }}),
    )
}

pub fn attention(model: &str, routine: bool) -> Value {
    request(
        model,
        json!({
            "recipient": {"assignment": "Review bookmark validity after edits", "next_action": "Approve the offset-retaining caller at the current revision"},
            "update": if routine {
                "The README now spells bookmark consistently. Executable code and its documented contracts are unchanged."
            } else {
                "The new index invalidates retained byte offsets on edit. The caller under review still retains them; stable IDs are required."
            },
            "current_assumption": "Stored offsets remain valid after edits",
            "delivery": "Recipient has not yet approved; next ordinary checkpoint is after approval."
        }),
        json!({
            "changes_action": {"type": "noul", "instructions": "Does `update` require changing `recipient.next_action` before proceeding?"},
            "contradiction": {"type": "noul", "instructions": "Does `update` contradict `current_assumption` about executable behavior?"},
            "delay": {"type": "score", "instructions": "What consequence would delaying this update until the next ordinary checkpoint have for this recipient?", "criteria": [
                {"consequence": "Background information only", "definition": "Delaying this update has no effect on the recipient's next action or its correctness."},
                {"consequence": "Useful later", "definition": "The update helps later work, but the recipient's next action remains valid."},
                {"consequence": "Next action blocked", "definition": "The recipient cannot perform its next action correctly without this information."},
                {"consequence": "Incorrect approval", "definition": "Proceeding before reading this update would approve behavior contradicted by the new evidence."}
            ]}
        }),
    )
}

pub fn investigate(model: &str, fold: bool) -> Value {
    if fold {
        return request(
            model,
            json!({
                "inquiry": "Can a cancellation arriving after the first check but before publication suppress the reply?",
                "local": {"span": "worker.rs:40-43", "source": "if cancelled() { return; } let result = compute(); publish(result);"},
                "children": [{"span": "compute.rs:10-12", "fact": "compute returns a value; it does not inspect cancellation"}],
                "missing": ["publish implementation", "whether publish rechecks cancellation"],
                "revision": "synthetic-r17"
            }),
            json!({"conclusion": {
                "type": "choice", "instructions": "What conclusion about `inquiry` is supported by the supplied source evidence only?",
                "criteria": {
                    "suppressed": {"meaning": "Evidence establishes that late cancellation suppresses publication"},
                    "delivered": {"meaning": "Evidence establishes that late cancellation cannot suppress publication"},
                    "gap": {"meaning": "Publication behavior is missing; inspect publish before answering", "next_evidence": ["publish implementation"]}
                }
            }}),
        );
    }
    request(
        model,
        json!({
            "inquiry": "Where can cancellation prevent a computed result from reaching its requester?",
            "local": {"symbol": "worker", "source": "let value = compute(); publish(value);"},
            "visited": ["compute"], "remaining_depth": 3
        }),
        json!({"step": {
            "type": "choice", "instructions": "Select the next relationship most directly useful for `inquiry`, using the source observations in the alternatives.",
            "criteria": {
                "edge_12": {"destination": "cancel_metrics", "relation": "referenced telemetry helper", "source": "counter.increment();", "effect": "Records cancellation counts; no reply path"},
                "edge_7": {"destination": "publish", "relation": "called after compute", "source": "if token.cancelled() { return; } inbox.send(value);", "effect": "Handles transfer of the computed result"},
                "edge_9": {"destination": "compute", "relation": "already visited", "source": "parse(input)", "effect": "Constructs result"},
                "finish": {"meaning": "Current local source already identifies the relevant cancellation gate"},
                "unknown": {"meaning": "No supplied relationship helps answer the inquiry"}
            }
        }}),
    )
}

pub fn evidence(model: &str, missing: bool) -> Value {
    let mut candidates = json!({
        "group_1": {"span": "worker.rs:91", "diagnostic": "cannot find value result in this scope", "notes": ["Downstream use of the binding from line 70"]},
        "group_2": {"span": "worker.rs:70", "diagnostic": "expected Response<Report>, found Response<Text>", "notes": ["The assignment was instantiated with Text; receive_report requires Report"]},
        "group_3": {"span": "worker.rs:4", "diagnostic": "unused import: Report", "severity": "warning"},
        "no_match": {"meaning": "No supplied group directly documents the requested type mismatch"}
    });
    if missing {
        candidates.as_object_mut().unwrap().remove("group_2");
    }
    request(
        model,
        json!({
            "inquiry": "Find the diagnostic directly documenting why the assignment response cannot be passed to receive_report.",
            "candidate_coverage": if missing {"Partial excerpt; the original binding diagnostic is omitted"} else {"Includes the original binding diagnostic and downstream messages"},
            "rule": "Select existing evidence; do not infer a missing diagnostic from a downstream symptom."
        }),
        json!({"evidence": {"type": "choice", "instructions": "Which supplied diagnostic directly documents the type mismatch described in `inquiry`?", "criteria": candidates}}),
    )
}

pub fn relate(model: &str, distinct: bool) -> Value {
    request(
        model,
        json!({
            "existing": {"question": "Who retries a provider HTTP request after a connect timeout?", "scope": "HTTP transport", "revision": "synthetic-r17", "status": "unanswered"},
            "incoming": if distinct {
                json!({"question": "Who retries a settled actor assignment after its acceptance check fails?", "scope": "project orchestration", "revision": "synthetic-r17"})
            } else {
                json!({"question": "When connecting to the provider times out, is request retry owned by the HTTP transport or its caller?", "scope": "HTTP transport", "revision": "synthetic-r17"})
            }
        }),
        json!({"relationship": {
            "type": "choice", "instructions": {"question": "How does `incoming` relate to `existing`?", "rule": "Compare the actual operation and failure condition, not shared words."},
            "criteria": {
                "duplicate": {"meaning": "Same unresolved ownership decision for the same operation and failure"},
                "answered": {"meaning": "Existing record contains an accepted answer applicable to incoming"},
                "distinct": {"meaning": "Related wording but different operations, scopes, or failure conditions"},
                "conflict": {"meaning": "The records assert incompatible answers to the same ownership decision"}
            }
        }}),
    )
}

pub fn repair(model: &str, unavailable: bool) -> Value {
    let mut options = json!({
        "repair": {"kind": "RepairTask", "target": "retained-implementer", "task": "Replace offset retention with stable IDs at the observed caller"},
        "rerun": {"kind": "CommandPlan", "plan": "Repeat the exact same test at the same revision", "cost_seconds": 20},
        "inspect_setup": {"kind": "DiagnosticGroup", "task": "Inspect test infrastructure setup"},
        "owner": {"kind": "DesignQuestion", "task": "Ask coordinator to assign a repair worker; include the observed failure and contract"}
    });
    if unavailable {
        options.as_object_mut().unwrap().remove("repair");
    }
    request(
        model,
        json!({
            "check": {"status": "failed", "setup": "passed", "revision": "synthetic-r17", "observed": "bookmark points to wrong character after inserting text before it", "repeat": "Same failure reproduced twice"},
            "contract": "Bookmarks must retain logical identity after edits. Caller still stores byte offsets.",
            "worker": if unavailable {"Retained implementer is unavailable; coordinator must select a replacement"} else {"Retained implementer is available and owns this caller"},
            "actions": "All listed actions are eligible; choose the most useful one."
        }),
        json!({"next": {"type": "choice", "instructions": "Which available next action most directly advances resolution of the observed failure?", "criteria": options}}),
    )
}

pub fn experiment(model: &str, insufficient: bool) -> Value {
    let mut options = json!({
        "clear_cache": {"plan": "Clear the cache and retry", "prediction": {"stale_cache": "passes", "missing_invalidation": "passes"}, "cost_seconds": 5},
        "trace_invalidation": {"plan": "Record invalidation callback after an edit with cache enabled", "prediction": {"stale_cache": "callback is observed", "missing_invalidation": "callback absent"}, "cost_seconds": 8},
        "disable_cache": {"plan": "Disable the cache and retry", "prediction": {"stale_cache": "passes", "missing_invalidation": "passes"}, "cost_seconds": 4},
        "none": {"meaning": "No supplied plan distinguishes the two hypotheses; obtain another experiment"}
    });
    if insufficient {
        options
            .as_object_mut()
            .unwrap()
            .remove("trace_invalidation");
    }
    request(
        model,
        json!({
            "goal": "Distinguish the two supplied hypotheses, rather than merely making the symptom disappear",
            "hypotheses": {"stale_cache": "One stale entry from startup; subsequent edit invalidation works", "missing_invalidation": "Edit path fails to invoke invalidation callback"},
            "observation": "Cached lookup after an edit returns the old value", "budget_seconds": 10,
            "assumption": "Predictions supplied with plans are accurate for this synthetic example."
        }),
        json!({"experiment": {"type": "choice", "instructions": "Which plan within `budget_seconds` distinguishes `hypotheses` by different predicted observations? Choose none if no plan does.", "criteria": options}}),
    )
}
