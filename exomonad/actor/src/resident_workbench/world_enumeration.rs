//! `scopeBindings` over the actor's admitted compiler world.
//!
//! The runtime already materializes one occurrence table per admitted scope:
//! occurrence -> `SymbolIdentity` -> `SessionVarId`/defining scope -> checked
//! row. That table is what publication built; enumerating it needs no
//! inspection module, no compiler transaction and no extract worker.
//!
//! This path therefore never constructs an `InspectionRequest`. The answer
//! carries identity only (`IdentifierRef` plus observed/limit), because
//! presentation and row availability genuinely need a compiler and belong to
//! `info`. Staleness is refused before and after reading: a pinned provenance
//! that no longer matches the current compile view is `ScopeChanged`, never an
//! empty success.

use tidepool_runtime::session::{NameScope, QueryError, ScopeBindingsAnswer, ScopeProvenance};

use super::structured_introspection::{
    visit_introspection_identifier, visit_introspection_provenance,
    visit_introspection_query_error, visit_structural_list,
};
use super::to_haskell::{actor_haskell_int, visit_core, visit_named};
use super::*;

/// One guest value answering `scopeBindings`.
enum ScopeBindingsOutcome {
    Names(ScopeBindingsAnswer, ScopeProvenance),
    ScopeChanged {
        before: ScopeProvenance,
        after: ScopeProvenance,
    },
    Error(QueryError),
}

impl tidepool_bridge::sealed::ToHaskellSealed for ScopeBindingsOutcome {}

impl ToHaskell for ScopeBindingsOutcome {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let right = matches!(self, Self::Names(_, _));
        visit_named(
            table,
            visitor,
            "Data.Either",
            if right { "Right" } else { "Left" },
            |visitor| match self {
                Self::Names(answer, provenance) => {
                    visit_core(table, visitor, "NameRefList", |visitor| {
                        visit_structural_list(table, visitor, &answer.refs, |reference, visitor| {
                            visit_introspection_identifier(table, visitor, reference)
                        })?;
                        actor_haskell_int(answer.observed as u64, "observed binding count")?
                            .visit(table, visitor)?;
                        actor_haskell_int(answer.limit as u64, "binding list limit")?
                            .visit(table, visitor)?;
                        visit_introspection_provenance(table, visitor, provenance)
                    })
                }
                Self::ScopeChanged { before, after } => {
                    visit_core(table, visitor, "ScopeChanged", |visitor| {
                        visit_introspection_provenance(table, visitor, before)?;
                        visit_introspection_provenance(table, visitor, after)
                    })
                }
                Self::Error(error) => visit_introspection_query_error(table, visitor, error),
            },
        )
    }
}

/// What one machine visit observed. A pinned provenance that no longer
/// matches the current view never reads the world at all.
enum ScopeBindingsSnapshot {
    Stale {
        expected: ScopeProvenance,
        current: ScopeProvenance,
    },
    Ready {
        current: ScopeProvenance,
        answer: ScopeBindingsAnswer,
    },
    /// The actor does not hold a retained source owner for this scope.
    Refused(QueryError),
}


/// A caller-pinned view is stale when its publication generation or content
/// fingerprint no longer matches the view the machine currently holds.
/// `Nothing` never pins.
fn pinned_scope_moved(
    expected: Option<ScopeProvenance>,
    current: &ScopeProvenance,
) -> Option<ScopeProvenance> {
    expected.filter(|expected| !expected.same_view(current))
}

impl<H, O> ResidentActorWorkbench<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Answer `scopeBindings` from the admitted world without admitting any
    /// compiler work.
    pub(crate) async fn resume_world_scope_bindings(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        expected: Option<ScopeProvenance>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let source = self.access.source.config.clone();
        let snapshot_source = source.clone();
        let snapshot = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let current = match snapshot_source.inspection_provenance(
                    session,
                    context,
                    NameScope::Current,
                ) {
                    Ok(current) => current,
                    Err(error) => {
                        return Ok(ScopeBindingsSnapshot::Refused(
                            unobserved_inspection_source(error)?,
                        ))
                    }
                };
                if let Some(expected) = pinned_scope_moved(expected, &current) {
                    return Ok(ScopeBindingsSnapshot::Stale { expected, current });
                }
                // A retained captured source owner is what admitted the world
                // in the first place. Without one there is nothing to read, and
                // the answer refuses rather than manufacturing an empty scope.
                let layer = match public_module_query_source(&context.source_layer) {
                    Ok(layer) => layer,
                    Err(error) => return Ok(ScopeBindingsSnapshot::Refused(error)),
                };
                let evidence = layer.generation_evidence().ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "scopeBindings lacks retained source evidence".into(),
                    )
                })?;
                let original_source = context.source_dependency().source_lease()?.original_source().clone();
                let admitted = session
                    .capture_inspection_world_in(
                        context.placement.lexical_scope,
                        &evidence.reference,
                        &original_source,
                    )
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })?;
                // `WorldInspectionError` keeps the runtime's typed distinction
                // (no source, stale world, unreadable occurrence table) instead
                // of flattening every read failure into a protocol message.
                let answer = admitted
                    .scope_bindings()
                    .map_err(ResidentActorWorkbenchError::WorldInspection)?;
                Ok(ScopeBindingsSnapshot::Ready { current, answer })
            })
            .await?;
        self.access
            .with_continuation_machine(context, move |session, context, _, settlement| {
                let outcome = match snapshot {
                    ScopeBindingsSnapshot::Stale { expected, current } => {
                        ScopeBindingsOutcome::ScopeChanged {
                            before: expected,
                            after: current,
                        }
                    }
                    ScopeBindingsSnapshot::Refused(error) => ScopeBindingsOutcome::Error(error),
                    ScopeBindingsSnapshot::Ready { current, answer } => {
                        // Validate and resume under the same machine checkout so
                        // publication cannot move between the check and delivery.
                        match source.inspection_provenance(session, context, NameScope::Current) {
                            Ok(observed) if !observed.same_view(&current) => {
                                ScopeBindingsOutcome::ScopeChanged {
                                    before: current,
                                    after: observed,
                                }
                            }
                            Ok(_) => ScopeBindingsOutcome::Names(answer, current),
                            Err(error) => ScopeBindingsOutcome::Error(
                                unobserved_inspection_source(error)?,
                            ),
                        }
                    }
                };
                session
                    .resume_classified(hole, outcome, settlement)
                    .map_err(classify_resumption)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(generation: u64, fingerprint: &str) -> ScopeProvenance {
        ScopeProvenance {
            scope: NameScope::Current,
            generation,
            fingerprint: fingerprint.into(),
        }
    }

    #[test]
    fn scope_observation_refuses_only_unobserved_source_identity() {
        assert!(matches!(
            unobserved_inspection_source(ResidentActorWorkbenchError::CompileView(
                ActorCompileViewError::UnobservedSourceIdentity,
            )),
            Ok(QueryError::Unsupported(_)),
        ));
        let scope = ScopeId::ROOT;
        assert!(matches!(
            unobserved_inspection_source(ResidentActorWorkbenchError::Resident(
                ResidentError::Session(tidepool_runtime::session::SessionError::DeadScope(scope)),
            )),
            Err(ResidentActorWorkbenchError::Resident(ResidentError::Session(
                tidepool_runtime::session::SessionError::DeadScope(actual),
            ))) if actual == scope,
        ));
        assert!(matches!(
            unobserved_inspection_source(ResidentActorWorkbenchError::Compile(CompileError::Io(
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ))),
            Err(ResidentActorWorkbenchError::Compile(CompileError::Io(error)))
                if error.kind() == std::io::ErrorKind::PermissionDenied,
        ));
        assert!(matches!(
            unobserved_inspection_source(ResidentActorWorkbenchError::InvocationCancelled),
            Err(ResidentActorWorkbenchError::InvocationCancelled),
        ));
    }

    #[test]
    fn pinned_scope_moved_refuses_a_stale_view() {
        let current = provenance(7, "current");
        let stale = provenance(6, "current");
        let refused = pinned_scope_moved(Some(stale.clone()), &current)
            .expect("a moved generation is stale");
        assert!(refused.same_view(&stale));
        assert!(pinned_scope_moved(Some(current.clone()), &current).is_none());
        assert!(pinned_scope_moved(None, &current).is_none());
    }

    #[test]
    fn pinned_scope_moved_refuses_a_fingerprint_move_at_the_same_generation() {
        let current = provenance(7, "current");
        let stale = provenance(7, "previous");
        let refused = pinned_scope_moved(Some(stale.clone()), &current)
            .expect("a moved fingerprint is stale");
        assert!(refused.same_view(&stale));
    }

    #[test]
    fn pinned_public_module_scope_at_the_same_view_is_not_stale() {
        let current = provenance(7, "current");
        let pinned = ScopeProvenance {
            scope: NameScope::PublicModule("Data.Maybe".into()),
            ..current.clone()
        };
        // `None` routes the snapshot to `Names`; `ScopeChanged` is reserved for
        // a moved generation or fingerprint.
        assert!(pinned_scope_moved(Some(pinned), &current).is_none());
    }
}
