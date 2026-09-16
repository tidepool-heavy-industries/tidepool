use clap::ValueEnum;
use serde_json::{json, Map, Value};

/// Experiments deliberately include disputed and invalid requests. This is not
/// the eventual validity-preserving production request API.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Probe {
    Structured,
    StateNull,
    StateArray,
    StateNumber,
    StateBoolean,
    InstructionsOmitted,
    InstructionsNull,
    InstructionsArray,
    InstructionsNumber,
    InstructionsBoolean,
    NoulCriteriaOmitted,
    NoulCriteriaNull,
    NoulCriteriaEmpty,
    NoulTrueOnly,
    NoulFalseOnly,
    NoulOutcomesNull,
    ScoreNullLevel,
    ScoreZero,
    ScoreOne,
    ScoreTwo,
    ScoreTen,
    ScoreEleven,
    ChoiceZero,
    ChoiceOne,
    Choice255,
    Choice256,
    ChoiceNullDescription,
    ChoiceArrayDescription,
    ChoiceNumberDescription,
    ChoiceBooleanDescription,
    EmptyQuestions,
    EscapedKeys,
    ExtraRequestProperty,
    ExtraQuestionProperty,
}

impl Probe {
    pub fn name(self) -> String {
        self.to_possible_value()
            .expect("all probes are named")
            .get_name()
            .to_owned()
    }

    pub fn request(self, model: &str) -> Value {
        let mut request = json!({
            "model": model,
            "state": {"message": "The worker cannot proceed until the missing configuration is supplied.",
                "context": {"active": true, "pending": 2, "previous": null}},
            "questions": {"wake": {
                "type": "noul",
                "instructions": {"question": "Does `message` describe a blocker for current work?"},
                "criteria": {"true": {"means": "Current work cannot proceed", "examples": ["Missing required input"]},
                    "false": {"means": "Work can proceed without this message"}}
            }}
        });
        match self {
            Self::Structured => {
                request["questions"]["route"] = json!({"type": "choice",
                    "instructions": {"question": "Who can resolve the missing configuration?", "focus": ["Current blocker"]},
                    "criteria": {"configuration_owner": {"handles": {"configuration": ["missing values", "invalid values"]}},
                        "reviewer": {"handles": ["completed work"]}, "neither": null}});
                request["questions"]["urgency"] = json!({"type": "score",
                    "instructions": "How urgently does this message need attention?",
                    "criteria": [{"means": "Useful information, work can continue"},
                        {"means": "Work cannot continue until someone responds"}]});
            }
            Self::StateNull => request["state"] = Value::Null,
            Self::StateArray => {
                request["state"] = json!([{"message": "Waiting for configuration"}, true, 2, null])
            }
            Self::StateNumber => request["state"] = json!(42),
            Self::StateBoolean => request["state"] = json!(true),
            Self::InstructionsOmitted => {
                request["questions"]["wake"]
                    .as_object_mut()
                    .unwrap()
                    .remove("instructions");
            }
            Self::InstructionsNull => request["questions"]["wake"]["instructions"] = Value::Null,
            Self::InstructionsArray => {
                request["questions"]["wake"]["instructions"] =
                    json!(["Is current work blocked?", {"inspect": "message"}])
            }
            Self::InstructionsNumber => request["questions"]["wake"]["instructions"] = json!(42),
            Self::InstructionsBoolean => request["questions"]["wake"]["instructions"] = json!(true),
            Self::NoulCriteriaOmitted => {
                request["questions"]["wake"]
                    .as_object_mut()
                    .unwrap()
                    .remove("criteria");
            }
            Self::NoulCriteriaNull => request["questions"]["wake"]["criteria"] = Value::Null,
            Self::NoulCriteriaEmpty => request["questions"]["wake"]["criteria"] = json!({}),
            Self::NoulTrueOnly => {
                request["questions"]["wake"]["criteria"] =
                    json!({"true": ["Current work is blocked"]})
            }
            Self::NoulFalseOnly => {
                request["questions"]["wake"]["criteria"] =
                    json!({"false": "Current work can proceed"})
            }
            Self::NoulOutcomesNull => {
                request["questions"]["wake"]["criteria"] = json!({"true": null, "false": null})
            }
            Self::ScoreNullLevel
            | Self::ScoreZero
            | Self::ScoreOne
            | Self::ScoreTwo
            | Self::ScoreTen
            | Self::ScoreEleven => {
                let count = match self {
                    Self::ScoreZero => 0,
                    Self::ScoreOne => 1,
                    Self::ScoreTen => 10,
                    Self::ScoreEleven => 11,
                    _ => 2,
                };
                let mut levels: Vec<Value> =
                    (0..count).map(|i| json!({"pending_messages": i})).collect();
                if matches!(self, Self::ScoreNullLevel) {
                    levels[0] = Value::Null;
                }
                request["questions"] = json!({"count": {"type": "score", "instructions": "How many messages are pending in `context.pending`?", "criteria": levels}});
            }
            Self::ChoiceZero
            | Self::ChoiceOne
            | Self::Choice255
            | Self::Choice256
            | Self::ChoiceNullDescription
            | Self::ChoiceArrayDescription
            | Self::ChoiceNumberDescription
            | Self::ChoiceBooleanDescription => {
                let count = match self {
                    Self::ChoiceZero => 0,
                    Self::ChoiceOne => 1,
                    Self::Choice255 => 255,
                    Self::Choice256 => 256,
                    _ => 2,
                };
                let mut options: Map<String, Value> = (0..count)
                    .map(|i| (format!("owner_{i}"), json!({"configuration_owner": i == 0})))
                    .collect();
                match self {
                    Self::ChoiceNullDescription => {
                        options.insert("owner_0".into(), Value::Null);
                    }
                    Self::ChoiceArrayDescription => {
                        options.insert(
                            "owner_0".into(),
                            json!(["Configuration owner", {"available": true}]),
                        );
                    }
                    Self::ChoiceNumberDescription => {
                        options.insert("owner_0".into(), json!(42));
                    }
                    Self::ChoiceBooleanDescription => {
                        options.insert("owner_0".into(), json!(true));
                    }
                    _ => {}
                }
                request["questions"] = json!({"route": {"type": "choice", "instructions": "Who owns configuration?", "criteria": options}});
            }
            Self::EmptyQuestions => request["questions"] = json!({}),
            Self::EscapedKeys => {
                request["questions"] = json!({"route/~. λ": {"type": "choice", "instructions": "Who owns configuration?",
                "criteria": {"configuration / ~ λ": {"owns": "configuration"}, "reviewer\n\"quoted\"": null}}})
            }
            Self::ExtraRequestProperty => {
                request["research_extension"] = json!({"synthetic": true})
            }
            Self::ExtraQuestionProperty => {
                request["questions"]["wake"]["research_extension"] = json!({"synthetic": true})
            }
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omissions_and_nulls_remain_distinct() {
        let omitted = Probe::InstructionsOmitted.request("test");
        let null = Probe::InstructionsNull.request("test");
        assert!(omitted["questions"]["wake"].get("instructions").is_none());
        assert_eq!(
            null["questions"]["wake"].get("instructions"),
            Some(&Value::Null)
        );
        assert!(
            Probe::NoulCriteriaOmitted.request("test")["questions"]["wake"]
                .get("criteria")
                .is_none()
        );
    }

    #[test]
    fn boundary_probes_reach_the_requested_cardinalities() {
        for (case, count) in [
            (Probe::ScoreZero, 0),
            (Probe::ScoreOne, 1),
            (Probe::ScoreTwo, 2),
            (Probe::ScoreTen, 10),
            (Probe::ScoreEleven, 11),
        ] {
            assert_eq!(
                case.request("test")["questions"]["count"]["criteria"]
                    .as_array()
                    .unwrap()
                    .len(),
                count
            );
        }
        for (case, count) in [
            (Probe::ChoiceZero, 0),
            (Probe::ChoiceOne, 1),
            (Probe::Choice255, 255),
            (Probe::Choice256, 256),
        ] {
            assert_eq!(
                case.request("test")["questions"]["route"]["criteria"]
                    .as_object()
                    .unwrap()
                    .len(),
                count
            );
        }
    }
}
