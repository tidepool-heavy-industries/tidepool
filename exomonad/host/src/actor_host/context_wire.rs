//! Typed conversion between Store context documents and their Haskell wire form.

use harness::context::{
    ContextBlock, ContextDocument, ContextError, ContextNativeKind, ContextReference, ContextRole,
    ContextTextSelector, ContextVisibleText,
};
use tidepool_bridge_effects::{
    ContextBlock as WireBlock, ContextDocument as WireDocument,
    ContextNativeKind as WireNativeKind, ContextReference as WireReference,
    ContextRole as WireRole, ContextTextSelector as WireTextSelector,
    ContextVisibleText as WireVisibleText,
};

pub(super) fn inspection_to_wire(
    inspection: &harness::context::ContextInspection,
) -> tidepool_bridge_effects::ContextInspection {
    tidepool_bridge_effects::ContextInspection {
        document: to_wire(&inspection.document),
        text_versions: inspection
            .text_versions
            .iter()
            .map(|version| tidepool_bridge_effects::ContextTextVersion {
                reference: reference_to_wire(&version.reference),
                selector: selector_to_wire(&version.selector),
                digest: version.digest.clone(),
            })
            .collect(),
        unit_versions: inspection
            .unit_versions
            .iter()
            .map(|version| tidepool_bridge_effects::ContextUnitVersion {
                reference: reference_to_wire(&version.reference),
                digest: version.digest.clone(),
            })
            .collect(),
        authorship: inspection
            .authorship
            .iter()
            .map(|metadata| {
                use harness::context::ContextTextAuthor as Author;
                use tidepool_bridge_effects::ContextTextAuthor as Wire;
                tidepool_bridge_effects::ContextTextAuthorship {
                    reference: reference_to_wire(&metadata.reference),
                    selector: selector_to_wire(&metadata.selector),
                    author: match metadata.author {
                        Author::User => Wire::ContextAuthorUser,
                        Author::Assistant => Wire::ContextAuthorAssistant,
                        Author::Tool => Wire::ContextAuthorTool,
                        Author::System => Wire::ContextAuthorSystem,
                        Author::Developer => Wire::ContextAuthorDeveloper,
                        Author::Unknown => Wire::ContextAuthorUnknown,
                    },
                    authored_note: metadata.authored_note,
                }
            })
            .collect(),
    }
}

pub(super) fn patch_from_wire(
    patch: tidepool_bridge_effects::ContextPatch,
) -> Result<harness::context::ContextPatch, ContextError> {
    use harness::context::ContextChange;
    use tidepool_bridge_effects::ContextChange as WireChange;
    Ok(harness::context::ContextPatch {
        changes: patch
            .changes
            .into_iter()
            .map(|change| {
                Ok(match change {
                    WireChange::ReplaceContextText {
                        reference,
                        selector,
                        expected,
                        text,
                    } => ContextChange::ReplaceContextText {
                        reference: reference_from_wire(reference),
                        selector: selector_from_wire(selector)?,
                        expected,
                        text,
                    },
                    WireChange::RemoveContextUnit {
                        reference,
                        expected,
                    } => ContextChange::RemoveContextUnit {
                        reference: reference_from_wire(reference),
                        expected,
                    },
                    WireChange::AppendRetainedExchange { source } => {
                        ContextChange::AppendRetainedExchange {
                            source: harness::context::RetainedExchangeReference::from_raw(
                                source.raw,
                            ),
                        }
                    }
                    WireChange::AppendContextNote { text, sources } => {
                        ContextChange::AppendContextNote {
                            text,
                            sources: sources.into_iter().map(reference_from_wire).collect(),
                        }
                    }
                })
            })
            .collect::<Result<Vec<_>, ContextError>>()?,
    })
}

pub(super) fn transaction_from_wire(
    transaction: tidepool_bridge_effects::ContextTransaction,
) -> harness::context::ContextTransaction {
    harness::context::ContextTransaction {
        raw: transaction.raw,
    }
}

pub(super) fn stage_accepted(
    transaction: &harness::context::ContextTransaction,
) -> tidepool_bridge_effects::ContextStageResult {
    tidepool_bridge_effects::ContextStageResult::ContextStaged(
        tidepool_bridge_effects::ContextTransaction {
            raw: transaction.raw.clone(),
        },
    )
}

pub(super) fn stage_refused(
    problems: Vec<harness::context::ContextProblem>,
) -> tidepool_bridge_effects::ContextStageResult {
    tidepool_bridge_effects::ContextStageResult::ContextStageRefused(
        problems.into_iter().map(problem_to_wire).collect(),
    )
}

fn problem_to_wire(
    problem: harness::context::ContextProblem,
) -> tidepool_bridge_effects::ContextProblem {
    use harness::context::ContextProblemCode as Code;
    use tidepool_bridge_effects::ContextProblemCode as Wire;
    tidepool_bridge_effects::ContextProblem {
        code: match problem.code {
            Code::Stale => Wire::ContextProblemStale,
            Code::Missing => Wire::ContextProblemMissing,
            Code::Ambiguous => Wire::ContextProblemAmbiguous,
            Code::Denied => Wire::ContextProblemDenied,
            Code::InvalidPatch => Wire::ContextProblemInvalidPatch,
            Code::AlreadyStaged => Wire::ContextProblemAlreadyStaged,
            Code::Ineligible => Wire::ContextProblemIneligible,
            Code::UnsupportedSelector => Wire::ContextProblemUnsupportedSelector,
        },
        reference: problem.reference.as_ref().map(reference_to_wire),
        detail: problem.detail,
    }
}

pub(super) fn status_to_wire(
    status: harness::context::ContextTransactionStatus,
) -> Result<tidepool_bridge_effects::ContextTransactionStatus, tidepool_effect::error::EffectError>
{
    use harness::context::ContextTransactionStatus as Status;
    use tidepool_bridge_effects::ContextTransactionStatus as Wire;
    Ok(match status {
        Status::Staged => Wire::ContextTransactionStaged,
        Status::Committed(receipt) => {
            Wire::ContextTransactionCommitted(tidepool_bridge_effects::ContextTransactionReceipt {
                head: receipt.head.0,
                generation: i64::try_from(receipt.generation).map_err(|_| {
                    tidepool_effect::error::EffectError::Handler(
                        "context generation exceeds wire range".into(),
                    )
                })?,
                changed: receipt.changed,
                appended: receipt.appended.iter().map(reference_to_wire).collect(),
            })
        }
        Status::Rejected(problems) => {
            Wire::ContextTransactionRejected(problems.into_iter().map(problem_to_wire).collect())
        }
        Status::Cancelled => Wire::ContextTransactionCancelled,
        Status::Unknown(detail) => Wire::ContextTransactionUnknown(detail),
    })
}

pub(super) fn original_to_wire(
    observed: Result<String, harness::context::ContextProblem>,
) -> tidepool_bridge_effects::OriginalTextObservation {
    use harness::context::ContextProblemCode as Code;
    use tidepool_bridge_effects::OriginalTextObservation as Wire;
    match observed {
        Ok(text) => Wire::OriginalTextAvailable(text),
        Err(problem) => match problem.code {
            Code::Missing => Wire::OriginalTextMissing,
            Code::UnsupportedSelector => Wire::OriginalTextUnsupported(problem.detail),
            _ => Wire::OriginalTextDenied(problem.detail),
        },
    }
}

pub(super) fn selector_from_wire(
    selector: WireTextSelector,
) -> Result<ContextTextSelector, ContextError> {
    match selector {
        WireTextSelector::MessageText { part } => Ok(ContextTextSelector::MessageText {
            part: u32::try_from(part).map_err(|_| ContextError::InvalidReference)?,
        }),
        WireTextSelector::ToolResultText => Ok(ContextTextSelector::ToolResultText),
        WireTextSelector::ReasoningText { part } => Ok(ContextTextSelector::ReasoningText {
            part: u32::try_from(part).map_err(|_| ContextError::InvalidReference)?,
        }),
    }
}

fn selector_to_wire(selector: &ContextTextSelector) -> WireTextSelector {
    match selector {
        ContextTextSelector::MessageText { part } => WireTextSelector::MessageText {
            part: i64::from(*part),
        },
        ContextTextSelector::ToolResultText => WireTextSelector::ToolResultText,
        ContextTextSelector::ReasoningText { part } => WireTextSelector::ReasoningText {
            part: i64::from(*part),
        },
    }
}

pub(super) fn to_wire(document: &ContextDocument) -> WireDocument {
    WireDocument {
        blocks: document.blocks.iter().map(block_to_wire).collect(),
    }
}

#[cfg(test)]
fn from_wire(document: WireDocument) -> Result<ContextDocument, ContextError> {
    Ok(ContextDocument {
        blocks: document
            .blocks
            .into_iter()
            .map(block_from_wire)
            .collect::<Result<_, _>>()?,
    })
}

fn block_to_wire(block: &ContextBlock) -> WireBlock {
    match block {
        ContextBlock::Text {
            reference,
            role,
            text,
            sources,
        } => WireBlock::Text {
            reference: reference.as_ref().map(reference_to_wire),
            role: role_to_wire(*role),
            text: text.clone(),
            sources: sources.iter().map(reference_to_wire).collect(),
        },
        ContextBlock::Native {
            reference,
            kind,
            preview,
            protected,
            texts,
        } => WireBlock::Native {
            reference: reference_to_wire(reference),
            kind: native_kind_to_wire(*kind),
            preview: preview.clone(),
            protected: *protected,
            texts: texts.iter().map(text_to_wire).collect(),
        },
    }
}

#[cfg(test)]
fn block_from_wire(block: WireBlock) -> Result<ContextBlock, ContextError> {
    Ok(match block {
        WireBlock::Text {
            reference,
            role,
            text,
            sources,
        } => ContextBlock::Text {
            reference: reference.map(reference_from_wire),
            role: role_from_wire(role),
            text,
            sources: sources.into_iter().map(reference_from_wire).collect(),
        },
        WireBlock::Native {
            reference,
            kind,
            preview,
            protected,
            texts,
        } => ContextBlock::Native {
            reference: reference_from_wire(reference),
            kind: native_kind_from_wire(kind),
            preview,
            protected,
            texts: texts
                .into_iter()
                .map(text_from_wire)
                .collect::<Result<_, _>>()?,
        },
    })
}

fn text_to_wire(text: &ContextVisibleText) -> WireVisibleText {
    WireVisibleText {
        reference: reference_to_wire(&text.reference),
        selector: match &text.selector {
            ContextTextSelector::MessageText { part } => WireTextSelector::MessageText {
                part: i64::from(*part),
            },
            ContextTextSelector::ToolResultText => WireTextSelector::ToolResultText,
            ContextTextSelector::ReasoningText { part } => WireTextSelector::ReasoningText {
                part: i64::from(*part),
            },
        },
        text: text.text.clone(),
        editable: text.editable,
        removable: text.removable,
    }
}

#[cfg(test)]
fn text_from_wire(text: WireVisibleText) -> Result<ContextVisibleText, ContextError> {
    Ok(ContextVisibleText {
        reference: reference_from_wire(text.reference),
        selector: match text.selector {
            WireTextSelector::MessageText { part } => ContextTextSelector::MessageText {
                part: u32::try_from(part).map_err(|_| ContextError::InvalidReference)?,
            },
            WireTextSelector::ReasoningText { part } => ContextTextSelector::ReasoningText {
                part: u32::try_from(part).map_err(|_| ContextError::InvalidReference)?,
            },
            WireTextSelector::ToolResultText => ContextTextSelector::ToolResultText,
        },
        text: text.text,
        editable: text.editable,
        removable: text.removable,
    })
}

fn reference_to_wire(reference: &ContextReference) -> WireReference {
    WireReference {
        raw: reference.as_str().to_owned(),
    }
}

pub(super) fn reference_from_wire(reference: WireReference) -> ContextReference {
    ContextReference::from_raw(reference.raw)
}

fn role_to_wire(role: ContextRole) -> WireRole {
    match role {
        ContextRole::User => WireRole::User,
        ContextRole::Assistant => WireRole::Assistant,
    }
}

#[cfg(test)]
fn role_from_wire(role: WireRole) -> ContextRole {
    match role {
        WireRole::User => ContextRole::User,
        WireRole::Assistant => ContextRole::Assistant,
    }
}

fn native_kind_to_wire(kind: ContextNativeKind) -> WireNativeKind {
    match kind {
        ContextNativeKind::CompletedExchange => WireNativeKind::CompletedExchange,
        ContextNativeKind::Opaque => WireNativeKind::Opaque,
        ContextNativeKind::Pending => WireNativeKind::Pending,
    }
}

#[cfg(test)]
fn native_kind_from_wire(kind: WireNativeKind) -> ContextNativeKind {
    match kind {
        WireNativeKind::CompletedExchange => ContextNativeKind::CompletedExchange,
        WireNativeKind::Opaque => ContextNativeKind::Opaque,
        WireNativeKind::Pending => ContextNativeKind::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::{from_wire, to_wire};
    use harness::context::{
        ContextBlock, ContextDocument, ContextError, ContextNativeKind, ContextReference,
        ContextRole, ContextTextSelector, ContextVisibleText,
    };

    #[test]
    fn round_trip_preserves_all_context_variants_and_provenance() {
        let document = ContextDocument {
            blocks: vec![
                ContextBlock::Text {
                    reference: Some(ContextReference::from_raw("editable:1".into())),
                    role: ContextRole::User,
                    text: "user note".into(),
                    sources: vec![
                        ContextReference::from_raw("native:2".into()),
                        ContextReference::from_raw("native:3".into()),
                    ],
                },
                ContextBlock::Text {
                    reference: None,
                    role: ContextRole::Assistant,
                    text: "assistant note".into(),
                    sources: vec![],
                },
                ContextBlock::Native {
                    reference: ContextReference::from_raw("native:2".into()),
                    kind: ContextNativeKind::CompletedExchange,
                    preview: "completed".into(),
                    protected: false,
                    texts: vec![ContextVisibleText {
                        reference: ContextReference::from_raw("output:2".into()),
                        selector: ContextTextSelector::ToolResultText,
                        text: "[Trimmed: repetitive output]\nsuccess".into(),
                        editable: true,
                        removable: false,
                    }],
                },
                ContextBlock::Native {
                    reference: ContextReference::from_raw("native:3".into()),
                    kind: ContextNativeKind::Opaque,
                    preview: "opaque".into(),
                    protected: true,
                    texts: vec![ContextVisibleText {
                        reference: ContextReference::from_raw("message:3".into()),
                        selector: ContextTextSelector::MessageText { part: 2 },
                        text: "Authored note in an opaque envelope".into(),
                        editable: true,
                        removable: true,
                    }],
                },
                ContextBlock::Native {
                    reference: ContextReference::from_raw("native:4".into()),
                    kind: ContextNativeKind::Pending,
                    preview: "pending".into(),
                    protected: true,
                    texts: vec![ContextVisibleText {
                        reference: ContextReference::from_raw("message:4".into()),
                        selector: ContextTextSelector::MessageText { part: 0 },
                        text: "Protected visible text".into(),
                        editable: false,
                        removable: false,
                    }],
                },
            ],
        };

        assert_eq!(from_wire(to_wire(&document)).unwrap(), document);
    }

    #[test]
    fn invalid_message_part_is_rejected_without_aliasing_another_part() {
        for part in [-1, i64::from(u32::MAX) + 1] {
            let text = super::WireVisibleText {
                reference: super::WireReference {
                    raw: "message:1".into(),
                },
                selector: super::WireTextSelector::MessageText { part },
                text: "replacement".into(),
                editable: true,
                removable: false,
            };
            assert!(matches!(
                super::text_from_wire(text),
                Err(ContextError::InvalidReference)
            ));
        }
    }

    #[test]
    fn original_observation_preserves_missing_denied_and_unsupported_distinctions() {
        use harness::context::{ContextProblem, ContextProblemCode};
        use tidepool_bridge_effects::OriginalTextObservation;
        for (code, expected) in [
            (
                ContextProblemCode::Missing,
                OriginalTextObservation::OriginalTextMissing,
            ),
            (
                ContextProblemCode::Denied,
                OriginalTextObservation::OriginalTextDenied("detail".into()),
            ),
            (
                ContextProblemCode::UnsupportedSelector,
                OriginalTextObservation::OriginalTextUnsupported("detail".into()),
            ),
        ] {
            assert_eq!(
                super::original_to_wire(Err(ContextProblem {
                    code,
                    reference: None,
                    detail: "detail".into(),
                })),
                expected
            );
        }
    }
}

pub(super) fn exchange_to_wire(
    observation: harness::context::RetainedExchangeObservation,
) -> tidepool_bridge_effects::RetainedExchangeObservation {
    use harness::context::RetainedExchangeObservation as Owner;
    use tidepool_bridge_effects::RetainedExchangeObservation as Wire;
    match observation {
        Owner::Available(source) => {
            Wire::RetainedExchangeAvailable(tidepool_bridge_effects::RetainedExchangeReference {
                raw: source.as_str().into(),
            })
        }
        Owner::Missing => Wire::RetainedExchangeMissing,
        Owner::Denied => Wire::RetainedExchangeDenied,
        Owner::Pending => Wire::RetainedExchangePending,
        Owner::Unpublished => Wire::RetainedExchangeUnpublished,
        Owner::NotStarted => Wire::RetainedExchangeNotStarted,
        Owner::Failure => Wire::RetainedExchangeFailure,
        Owner::Cancelled => Wire::RetainedExchangeCancelled,
        Owner::CancelledWithReceipt => Wire::RetainedExchangeCancelledWithReceipt,
        Owner::Interrupted => Wire::RetainedExchangeInterrupted,
        Owner::CancellationUnconfirmed => Wire::RetainedExchangeCancellationUnconfirmed,
    }
}
