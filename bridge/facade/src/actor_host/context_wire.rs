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

pub(super) fn to_wire(document: &ContextDocument) -> WireDocument {
    WireDocument {
        blocks: document.blocks.iter().map(block_to_wire).collect(),
    }
}

pub(super) fn from_wire(document: WireDocument) -> Result<ContextDocument, ContextError> {
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
        },
        text: text.text.clone(),
        editable: text.editable,
    }
}

fn text_from_wire(text: WireVisibleText) -> Result<ContextVisibleText, ContextError> {
    Ok(ContextVisibleText {
        reference: reference_from_wire(text.reference),
        selector: match text.selector {
            WireTextSelector::MessageText { part } => ContextTextSelector::MessageText {
                part: u32::try_from(part).map_err(|_| ContextError::InvalidReference)?,
            },
            WireTextSelector::ToolResultText => ContextTextSelector::ToolResultText,
        },
        text: text.text,
        editable: text.editable,
    })
}

fn reference_to_wire(reference: &ContextReference) -> WireReference {
    WireReference {
        raw: reference.as_str().to_owned(),
    }
}

fn reference_from_wire(reference: WireReference) -> ContextReference {
    ContextReference::from_raw(reference.raw)
}

fn role_to_wire(role: ContextRole) -> WireRole {
    match role {
        ContextRole::User => WireRole::User,
        ContextRole::Assistant => WireRole::Assistant,
    }
}

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
                        text: "Visible message in an opaque envelope".into(),
                        editable: true,
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
            };
            assert!(matches!(
                super::text_from_wire(text),
                Err(ContextError::InvalidReference)
            ));
        }
    }
}
