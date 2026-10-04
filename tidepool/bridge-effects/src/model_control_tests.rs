use crate::{
    ModelAnnotationEnvelope, ModelControlStep, ModelEffortEnvelope, ModelLimitEnvelope,
    ModelOutcomeEnvelope, ModelReceiptEnvelope, ModelRequestEnvelope, ModelRequestLimits,
    ModelUsageEnvelope,
};
use serde_json::{json, Value};

fn hook() -> ModelControlStep {
    ModelControlStep::ModelHook {
        invocation: "invocation".into(),
        operation: "operation".into(),
        name: "tool".into(),
        arguments: json!({"input":true}),
        handle: "retained".into(),
        ordinal: 3,
        value: json!({"answer":42}),
        output: "{\"answer\":42}".into(),
    }
}
#[test]
fn model_control_steps_roundtrip_and_preserve_semantic_callback_value() {
    let counts = ModelUsageEnvelope {
        requests: 1,
        tools: 1,
        reported_tokens: 8,
        unknown_usage_requests: 0,
    };
    let receipt = ModelReceiptEnvelope {
        invocation_id: "invocation".into(),
        parent_cell: "cell".into(),
        requests: vec!["request".into()],
        counts: counts.clone(),
        cell_counts: counts,
        outcome: ModelOutcomeEnvelope::ModelTypedOutcome {
            value: json!({"answer":42}),
        },
    };
    for step in [
        hook(),
        ModelControlStep::ModelCallback {
            invocation: "invocation".into(),
            call_id: "operation".into(),
            name: "tool".into(),
            arguments: Value::Null,
        },
        ModelControlStep::ModelFinished { receipt },
    ] {
        let encoded = serde_json::to_value(&step).unwrap();
        assert_eq!(
            serde_json::from_value::<ModelControlStep>(encoded).unwrap(),
            step
        );
    }
    assert_eq!(
        serde_json::to_value(hook()).unwrap()["value"],
        json!({"answer":42})
    );
}
#[test]
fn model_hook_refuses_missing_value_unknown_fields_and_unknown_control_after_valid_control() {
    let valid = serde_json::to_value(hook()).unwrap();
    serde_json::from_value::<ModelControlStep>(valid.clone()).unwrap();
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("value");
    let mut surplus = valid.clone();
    surplus["foreign"] = json!(true);
    let mut unknown = valid;
    unknown["kind"] = json!("foreign");
    for refused in [missing, surplus, unknown] {
        assert!(serde_json::from_value::<ModelControlStep>(refused).is_err());
    }
}
#[test]
fn model_request_and_annotation_controls_are_closed_after_valid_controls() {
    let request = ModelRequestEnvelope {
        instructions: "instruction".into(),
        input: "input".into(),
        model: None,
        effort: Some(ModelEffortEnvelope::ModelLowEffort),
        limits: ModelRequestLimits {
            requests: None,
            tools: None,
            reported_tokens: None,
            seconds: None,
        },
        tools: Vec::new(),
        result_schema: None,
        after_tool: true,
    };
    let valid = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<ModelRequestEnvelope>(valid.clone()).unwrap(),
        request
    );
    let mut foreign = valid.clone();
    foreign["effort"] = json!("foreign");
    let mut negative = valid;
    negative["limits"]["requests"] = json!(-1);
    for refused in [foreign, negative] {
        assert!(serde_json::from_value::<ModelRequestEnvelope>(refused).is_err());
    }
    for annotation in [
        ModelAnnotationEnvelope::ModelNoAnnotation,
        ModelAnnotationEnvelope::ModelAbstained {
            reason: "reason".into(),
        },
        ModelAnnotationEnvelope::ModelAnnotated {
            text: "text".into(),
        },
        ModelAnnotationEnvelope::ModelPruned {
            handle: "retained".into(),
            text: "short".into(),
        },
    ] {
        let valid = serde_json::to_value(&annotation).unwrap();
        assert_eq!(
            serde_json::from_value::<ModelAnnotationEnvelope>(valid.clone()).unwrap(),
            annotation
        );
        let mut unknown = valid;
        unknown["foreign"] = json!(true);
        assert!(serde_json::from_value::<ModelAnnotationEnvelope>(unknown).is_err());
    }
    assert_eq!(
        serde_json::to_value(ModelOutcomeEnvelope::ModelExhaustedOutcome {
            value: ModelLimitEnvelope::ModelTokensLimit
        })
        .unwrap(),
        json!({"kind":"exhausted","value":"reported_tokens"})
    );
}
