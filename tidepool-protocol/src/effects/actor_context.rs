//! Typed facts about the currently executing actor.
//!
//! The Haskell row says that an actor may inspect its own context. Rust owns
//! the concrete identity, authority, workspace, and resource-policy facts and
//! projects them through this actor-serviced suspension.

use crate::hs::HsType;
use crate::schema::{
    Effect, HandlingClass, Helper, HelperBody, JsonInstance, Polymorphism, RecordField, SumVariant,
    TypeDef, TypeShape, VariantFields, Verb, WireDerives,
};

const NO_WIRE: WireDerives = WireDerives(&[]);

/// The actor-local context inspection effect.
#[must_use]
pub fn actor_context() -> Effect {
    Effect {
        name: "ActorContext",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "ActorContextDecodeHandler",
        handler_module: "actor_context",
        req_enum: "ActorContextReq",
        decl_fn: "actor_context_decl",
        description: &[
            "Inspect the executing actor's exact identity and effective role, workspace, ",
            "native-tool, prompt, and descendant policies. These are runtime facts, not ",
            "authority handles and not an inference from effect membership.",
        ],
        prompt_card: Some(&[
            "`actorContext` returns exact current-actor identity and effective policy facts.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: type_defs(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "ActorContextWith",
            method: "actor_context_with",
            args: vec![],
            ret: HsType::Named("ActorContextInfo"),
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: vec![Helper {
            name: "actorContext",
            ctor: Some("ActorContextWith"),
            substrate: false,
            doc: &["Inspect the current actor's exact identity and effective policy."],
            body: HelperBody::Nullary,
        }],
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}

fn type_defs() -> Vec<TypeDef> {
    vec![
        TypeDef {
            name: "ProviderUsageObservation", wire_rust: None,
            shape: TypeShape::Record { fields: vec![
                field("usageObservationId", HsType::Text),
                field("usageTimestamp", HsType::maybe(HsType::Text)),
                field("usageCachedInputTokens", HsType::Int),
                field("usageUncachedInputTokens", HsType::Int),
            ] },
            json: JsonInstance::None, derives: NO_WIRE, domain: None,
            doc: &["One durable provider usage observation; legacy sources may repeat the same response. No activation attribution is inferred."],
        },
        TypeDef {
            name: "ProviderUsageScope", wire_rust: None,
            shape: TypeShape::Sum { variants: vec![
                SumVariant { ctor: "UsageThread", fields: VariantFields::Positional(vec![HsType::Text]), doc: &[] },
                SumVariant { ctor: "UsageTurn", fields: VariantFields::Positional(vec![HsType::Text, HsType::Text]), doc: &[] },
            ] },
            json: JsonInstance::None, derives: NO_WIRE, domain: None,
            doc: &["Provider thread or provider turn identity; a provider turn is not an actor request."],
        },
        closed_sum("ProviderUsageCompleteness", &["UsagePartial", "UsageComplete"]),
        TypeDef {
            name: "ProviderUsageSummary", wire_rust: None,
            shape: TypeShape::Record { fields: vec![
                field("usageSummaryScope", HsType::Named("ProviderUsageScope")),
                field("usageSummaryCompleteness", HsType::Named("ProviderUsageCompleteness")),
                field("usageSummaryObservations", HsType::Int),
                field("usageSummaryCachedInputTokens", HsType::Int),
                field("usageSummaryUncachedInputTokens", HsType::Int),
                field("usageSummaryOutputTokens", HsType::Int),
                field("usageSummaryReasoningTokens", HsType::Int),
                field("usageSummaryTotalTokens", HsType::Int),
            ] },
            json: JsonInstance::None, derives: NO_WIRE, domain: None,
            doc: &["Totals from unique durable responses. UsageComplete covers the observed scope through a provider turn-completion boundary, not future thread work. Nothing means totals are unavailable."],
        },
        closed_sum(
            "ActorContextRole",
            &[
                "ContextRoot",
                "ContextResearch",
                "ContextCoding",
                "ContextScaffolding",
                "ContextIntegration",
                "ContextInherited",
            ],
        ),
        closed_sum(
            "ActorNativeTools",
            &[
                "NativeInspectionOnly",
                "NativeCoding",
                "NativeIntegration",
                "NativeInherited",
            ],
        ),
        closed_sum(
            "ActorWorkspaceAccess",
            &[
                "WorkspaceNone",
                "WorkspaceInspectOnly",
                "WorkspaceWritableBound",
            ],
        ),
        TypeDef {
            name: "ActivationKind",
            wire_rust: None,
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "ActivationRootStarted",
                        fields: VariantFields::Positional(Vec::new()),
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ActivationRequest",
                        fields: VariantFields::Positional(vec![HsType::Int, HsType::Int]),
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ActivationEvents",
                        fields: VariantFields::Positional(vec![HsType::List(Box::new(
                            HsType::Int,
                        ))]),
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: NO_WIRE,
            domain: None,
            doc: &["Why the current external application turn was activated."],
        },
        TypeDef {
            name: "ActorContextInfo",
            wire_rust: None,
            shape: TypeShape::Record {
                fields: vec![
                    field("contextActorId", HsType::Int),
                    field("contextActorIncarnation", HsType::Int),
                    field("contextParentId", HsType::maybe(HsType::Int)),
                    field("contextParentIncarnation", HsType::maybe(HsType::Int)),
                    field("contextActorPath", HsType::Text),
                    field("contextRole", HsType::Named("ActorContextRole")),
                    field("contextEffectRow", HsType::Text),
                    field("contextNativeTools", HsType::Named("ActorNativeTools")),
                    field(
                        "contextWorkspaceAccess",
                        HsType::Named("ActorWorkspaceAccess"),
                    ),
                    field("contextBoundWorktree", HsType::maybe(HsType::Text)),
                    field("contextForkGroup", HsType::maybe(HsType::Int)),
                    field("contextHaskellScope", HsType::Int),
                    field("contextActivationKind", HsType::Named("ActivationKind")),
                    field("contextEventWatermark", HsType::Int),
                    field("contextProviderThread", HsType::maybe(HsType::Text)),
                    field("contextProviderParentThread", HsType::maybe(HsType::Text)),
                    field("contextFirstUsage", HsType::maybe(HsType::Named("ProviderUsageObservation"))),
                    field("contextLatestUsage", HsType::maybe(HsType::Named("ProviderUsageObservation"))),
                    field("contextUsageSummary", HsType::maybe(HsType::Named("ProviderUsageSummary"))),
                    field("contextLatestTurnUsage", HsType::maybe(HsType::Named("ProviderUsageSummary"))),
                    field("contextMaximumDepth", HsType::Int),
                    field("contextMaximumActiveChildren", HsType::maybe(HsType::Int)),
                    field("contextPromptProfile", HsType::Text),
                ],
            },
            json: JsonInstance::None,
            derives: NO_WIRE,
            domain: None,
            doc: &["Exact runtime facts for the actor executing `actorContext`."],
        },
    ]
}

fn closed_sum(name: &'static str, constructors: &'static [&'static str]) -> TypeDef {
    TypeDef {
        name,
        wire_rust: None,
        shape: TypeShape::Sum {
            variants: constructors
                .iter()
                .map(|ctor| SumVariant {
                    ctor,
                    fields: VariantFields::Positional(Vec::new()),
                    doc: &[],
                })
                .collect(),
        },
        json: JsonInstance::None,
        derives: NO_WIRE,
        domain: None,
        doc: &[],
    }
}

fn field(hs_name: &'static str, ty: HsType) -> RecordField {
    RecordField {
        hs_name,
        rust_name: hs_name,
        ty,
        doc: &[],
    }
}
