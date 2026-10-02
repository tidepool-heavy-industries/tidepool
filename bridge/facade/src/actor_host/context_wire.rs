//! Typed conversion between Store context documents and their Haskell wire form.

use harness::context::{
    ContextBlock, ContextDocument, ContextNativeKind, ContextReference, ContextRole,
};
use tidepool_bridge_effects::{
    ContextBlock as WireBlock, ContextDocument as WireDocument,
    ContextNativeKind as WireNativeKind, ContextReference as WireReference,
    ContextRole as WireRole,
};

pub(super) fn to_wire(document: &ContextDocument) -> WireDocument {
    WireDocument {
        blocks: document.blocks.iter().map(block_to_wire).collect(),
    }
}

pub(super) fn from_wire(document: WireDocument) -> ContextDocument {
    ContextDocument {
        blocks: document.blocks.into_iter().map(block_from_wire).collect(),
    }
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
        } => WireBlock::Native {
            reference: reference_to_wire(reference),
            kind: native_kind_to_wire(*kind),
            preview: preview.clone(),
            protected: *protected,
        },
    }
}

fn block_from_wire(block: WireBlock) -> ContextBlock {
    match block {
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
        } => ContextBlock::Native {
            reference: reference_from_wire(reference),
            kind: native_kind_from_wire(kind),
            preview,
            protected,
        },
    }
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
        ContextBlock, ContextDocument, ContextNativeKind, ContextReference, ContextRole,
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
                },
                ContextBlock::Native {
                    reference: ContextReference::from_raw("native:3".into()),
                    kind: ContextNativeKind::Opaque,
                    preview: "opaque".into(),
                    protected: true,
                },
                ContextBlock::Native {
                    reference: ContextReference::from_raw("native:4".into()),
                    kind: ContextNativeKind::Pending,
                    preview: "pending".into(),
                    protected: true,
                },
            ],
        };

        assert_eq!(from_wire(to_wire(&document)), document);
    }
}
