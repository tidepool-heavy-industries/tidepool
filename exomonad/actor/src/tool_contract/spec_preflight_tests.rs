use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Accepted,
    Interpreter,
    Row,
    InputSchema,
    Provider,
}

#[test]
fn metadata_preflight_tracks_support_and_schema_histories() {
    let mut config = Config::with_cases(32);
    config.source_file = Some(file!());
    config.test_name = Some("metadata_preflight_tracks_support_and_schema_histories");
    TestRunner::new(config).run(
        &proptest::collection::vec((any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()), 1..9),
        |tail| {
            let mut history = vec![
                (true, true, true, true), (false, true, true, true),
                (true, false, true, true), (true, true, false, true),
                (true, true, true, false), (true, true, true, true),
            ];
            history.extend(tail);
            for (object, canonical, available, provider_accepts) in history {
                let mut keys = vec![ToolEffectKey::ContextReadWrite, ToolEffectKey::Reload, ActorEffectKey::Web.into()];
                if !canonical { keys.pop(); }
                let tool = ToolDeclaration {
                    name: "research".into(), description: "Research".into(),
                    input_schema: if object { serde_json::json!({"type":"object","properties":{"question":{"type":"string"}},"required":["question"],"additionalProperties":false}) } else { serde_json::json!({"type":"string"}) },
                    output_schema: Some(serde_json::json!({"type":"string"})),
                    kind: ToolKind::Call, schedule: ToolScheduling::BeforeNextInference,
                    implementation: ToolImplementation::ResidentHandler, effect_keys: keys,
                };
                let metadata = serde_json::json!({"tools":[tool],"slots":[],"slotEffectKeys":{}});
                let mut support = vec![ToolEffectKey::ContextReadWrite, ToolEffectKey::Reload];
                if available { support.push(ActorEffectKey::Web.into()); }
                let projected = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let observed = projected.clone();
                let provider = move |_: &[exomonad_tool::HostedTool]| {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if provider_accepts { Ok(()) } else { Err("controlled provider refusal".into()) }
                };
                let answer = preflight_spec_metadata(metadata, &[ActorEffectKey::Web], &support, Some(&provider));
                let actual = match answer {
                    Ok(SpecValidationReceipt::SpecValidationReceipt(names, effects, SpecProvenance::SpecProvenanceUnavailable(_), SpecProviderValidation::SpecProviderChecked)) => {
                        prop_assert_eq!(names, vec!["research"]);
                        prop_assert_eq!(effects, vec!["Web"]);
                        Outcome::Accepted
                    },
                    Err(SpecValidationError::SpecInterpreterUnavailable(_)) => Outcome::Interpreter,
                    Err(SpecValidationError::SpecEffectMismatch(_)) => Outcome::Row,
                    Err(SpecValidationError::SpecInputSchemaInvalid(_)) => Outcome::InputSchema,
                    Err(SpecValidationError::SpecProviderProjectionInvalid(_)) => Outcome::Provider,
                    other => return Err(TestCaseError::fail(format!("unexpected admission outcome: {other:?}"))),
                };
                let expected = if !available { Outcome::Interpreter } else if !canonical { Outcome::Row } else if !object { Outcome::InputSchema } else if !provider_accepts { Outcome::Provider } else { Outcome::Accepted };
                prop_assert_eq!(actual, expected);
                prop_assert_eq!(projected.load(std::sync::atomic::Ordering::SeqCst), usize::from(available && canonical && object));
            }
            Ok(())
        },
    ).unwrap();
}

#[test]
fn model_free_preflight_reports_provider_projection_not_checked() {
    let receipt = preflight_spec_metadata(
        serde_json::json!({"tools":[],"slots":[],"slotEffectKeys":{}}),
        &[],
        &[],
        None,
    )
    .unwrap();
    assert!(
        matches!(receipt, SpecValidationReceipt::SpecValidationReceipt(names, effects, SpecProvenance::SpecProvenanceUnavailable(_), SpecProviderValidation::SpecProviderNotChecked) if names.is_empty() && effects.is_empty())
    );
}
