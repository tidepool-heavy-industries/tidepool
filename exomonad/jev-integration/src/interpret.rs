use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        legend: BTreeMap<String, Value>,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

/// Hand-written rather than `#[derive(Deserialize)]` with the same
/// internally-tagged `type` attribute used for `Serialize` above.
///
/// `serde`'s internally-tagged representation reads an object ahead of time
/// into its own generic `Content` buffer so it can inspect the `type` field
/// before picking a variant. This workspace enables `serde_json`'s
/// `arbitrary_precision` feature (needed elsewhere, e.g. `bridge/mcp`, for
/// exact numeric round-tripping) — Cargo unifies that feature across every
/// workspace member built together, so it is active here too even though
/// this crate never asked for it. Under `arbitrary_precision`, a JSON number
/// decodes as a private one-key map rather than a plain number, and the
/// generic `Content` buffer (from the `serde` crate, not `serde_json`) does
/// not know how to unwrap that marker — so any internally-tagged enum with a
/// numeric field fails with "invalid type: map, expected f64", even on
/// perfectly ordinary input. Parsing into a `serde_json::Value` first and
/// dispatching by hand sidesteps `Content` entirely: `Value`'s own decoder is
/// the one place that DOES understand `arbitrary_precision`'s marker, so the
/// resulting `Value::Number` is a normal number and `from_value` on each
/// concrete variant struct deserializes it as such.
impl<'de> Deserialize<'de> for Answer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Noul {
            noul: f64,
        }
        #[derive(Deserialize)]
        struct Choice {
            choice: String,
            probabilities: BTreeMap<String, f64>,
            confidence: f64,
        }
        #[derive(Deserialize)]
        struct Score {
            score: f64,
            legend: BTreeMap<String, Value>,
            probabilities: BTreeMap<String, f64>,
            confidence: f64,
        }

        let value = Value::deserialize(deserializer)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string);
        match kind.as_deref() {
            Some("noul") => serde_json::from_value::<Noul>(value)
                .map(|Noul { noul }| Answer::Noul { noul })
                .map_err(serde::de::Error::custom),
            Some("choice") => serde_json::from_value::<Choice>(value)
                .map(
                    |Choice {
                         choice,
                         probabilities,
                         confidence,
                     }| Answer::Choice {
                        choice,
                        probabilities,
                        confidence,
                    },
                )
                .map_err(serde::de::Error::custom),
            Some("score") => serde_json::from_value::<Score>(value)
                .map(
                    |Score {
                         score,
                         legend,
                         probabilities,
                         confidence,
                     }| Answer::Score {
                        score,
                        legend,
                        probabilities,
                        confidence,
                    },
                )
                .map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "unknown or missing Answer \"type\": {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Evaluation {
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    // Usage is disputed across official contracts. Preserve it without assuming
    // token-only accounting or requiring the Python SDK's billing_units field.
    pub usage: Value,
}

#[derive(Debug, Serialize)]
pub struct Interpretation {
    pub evaluation: Option<Evaluation>,
    pub findings: Vec<String>,
}

/// Diagnostics are provisional observations, not a production decoder. Raw
/// response bytes remain authoritative even if deserialization fails here.
pub fn interpret(request: &Value, response: &[u8]) -> Interpretation {
    let evaluation: Evaluation = match serde_json::from_slice(response) {
        Ok(value) => value,
        Err(error) => {
            return Interpretation {
                evaluation: None,
                findings: vec![format!("typed decode: {error}")],
            }
        }
    };
    let mut findings = Vec::new();
    let Some(questions) = request["questions"].as_object() else {
        return Interpretation {
            evaluation: Some(evaluation),
            findings: vec!["request questions are not an object".into()],
        };
    };
    if questions.keys().collect::<BTreeSet<_>>() != evaluation.answers.keys().collect() {
        findings.push("answer keys differ from request question keys".into());
    }
    for (name, answer) in &evaluation.answers {
        let Some(question) = questions.get(name) else {
            continue;
        };
        let kind = match answer {
            Answer::Noul { .. } => "noul",
            Answer::Choice { .. } => "choice",
            Answer::Score { .. } => "score",
        };
        if question["type"].as_str() != Some(kind) {
            findings.push(format!("{name}: answer kind differs from question"));
            continue;
        }
        match answer {
            Answer::Noul { noul } => check_probability(name, *noul, &mut findings),
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            } => {
                check_distribution(name, probabilities, *confidence, &mut findings);
                if let Some(criteria) = question["criteria"].as_object() {
                    if criteria.keys().collect::<BTreeSet<_>>() != probabilities.keys().collect() {
                        findings.push(format!("{name}: probability keys differ from criteria"));
                    }
                    if !criteria.contains_key(choice) {
                        findings.push(format!("{name}: selected unknown alternative"));
                    }
                }
                if let Some(selected) = probabilities.get(choice) {
                    if probabilities.values().any(|p| *p > selected + 1e-6) {
                        findings.push(format!(
                            "{name}: selection is not a maximum-probability alternative"
                        ));
                    }
                }
            }
            Answer::Score {
                score,
                legend,
                probabilities,
                confidence,
            } => {
                check_distribution(name, probabilities, *confidence, &mut findings);
                if let Some(criteria) = question["criteria"].as_array() {
                    let expected: BTreeMap<String, Value> = criteria
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (i.to_string(), v.clone()))
                        .collect();
                    if *legend != expected {
                        findings.push(format!("{name}: legend differs from requested levels"));
                    }
                    if expected.keys().collect::<BTreeSet<_>>() != probabilities.keys().collect() {
                        findings.push(format!(
                            "{name}: probability keys differ from level indices"
                        ));
                    }
                    if !score.is_finite()
                        || *score < 0.0
                        || *score > criteria.len().saturating_sub(1) as f64
                    {
                        findings.push(format!("{name}: score outside requested range"));
                    }
                    let mean: f64 = (0..criteria.len())
                        .map(|i| {
                            i as f64 * probabilities.get(&i.to_string()).copied().unwrap_or(0.0)
                        })
                        .sum();
                    if (*score - mean).abs() > 0.01 {
                        findings.push(format!(
                            "{name}: score differs from distribution mean by more than 0.01"
                        ));
                    }
                }
            }
        }
    }
    Interpretation {
        evaluation: Some(evaluation),
        findings,
    }
}

fn check_probability(name: &str, value: f64, findings: &mut Vec<String>) {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        findings.push(format!("{name}: probability/confidence outside [0,1]"));
    }
}

fn check_distribution(
    name: &str,
    probabilities: &BTreeMap<String, f64>,
    confidence: f64,
    findings: &mut Vec<String>,
) {
    check_probability(name, confidence, findings);
    for value in probabilities.values() {
        check_probability(name, *value, findings);
    }
    // Provider says approximately one; tolerance is diagnostic policy only.
    if (probabilities.values().sum::<f64>() - 1.0).abs() > 0.01 {
        findings.push(format!(
            "{name}: distribution sum differs from one by more than 0.01"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::Probe;
    use serde_json::json;

    #[test]
    fn structured_legend_and_unknown_usage_are_preserved() {
        let request = Probe::Structured.request("test");
        let bytes = include_bytes!("../fixtures/structured-response.json");
        let report = interpret(&request, bytes);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        let decoded = report.evaluation.unwrap();
        assert_eq!(decoded.usage["billing_units"], 7);
        match &decoded.answers["urgency"] {
            Answer::Score { legend, .. } => assert!(legend["0"].is_object()),
            _ => panic!("expected score"),
        }
    }

    #[test]
    fn missing_answers_wrong_kind_and_unknown_selection_are_reported() {
        let request = Probe::Structured.request("test");
        let response = json!({"model":"test", "usage":{}, "answers": {
            "wake":{"type":"choice","choice":"alien","probabilities":{"alien":1.0},"confidence":1.0},
            "route":{"type":"choice","choice":"alien","probabilities":{"alien":1.0},"confidence":1.0}}});
        let report = interpret(&request, &serde_json::to_vec(&response).unwrap());
        assert_eq!(report.findings.len(), 4, "{:?}", report.findings);
    }

    #[test]
    fn malformed_and_out_of_range_results_are_observations() {
        assert!(interpret(&Probe::Structured.request("test"), b"not JSON")
            .evaluation
            .is_none());
        let response =
            json!({"model":"test", "usage":{}, "answers":{"wake":{"type":"noul","noul":1.1}}});
        assert_eq!(
            interpret(
                &Probe::NoulCriteriaOmitted.request("test"),
                &serde_json::to_vec(&response).unwrap()
            )
            .findings
            .len(),
            1
        );
    }
}
