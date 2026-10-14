//! Capture of a public Haskell actor launch or replacement entry.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use std::collections::BTreeSet;
use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, FromHaskell};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_repr::{DataConTable, Generation};
use tidepool_runtime::session::{MaterializedFacade, OutputSink, ResidentHole, ResidentSession};

use crate::generated::actor::ActorReq;
use crate::ActorDescriptor;
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    tidepool_bridge_derive::FromHaskell,
    tidepool_bridge_derive::ToHaskell,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ForkEffort {
    #[haskell(module = "Tidepool.Effects.Core")]
    Low,
    #[haskell(module = "Tidepool.Effects.Core")]
    Medium,
    #[haskell(module = "Tidepool.Effects.Core")]
    High,
}

pub use tidepool_bridge_effects::Model;

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]
pub enum WorkerLifetime {
    InvocationOwned,
    ActorOwned,
    RunOwned,
    InScope(tidepool_bridge_effects::ResourceScopeId),
}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]
pub enum SpawnContextWire {
    CapturedSpawn(String),
    FreshSpawn(String),
}

#[derive(Debug, Clone, tidepool_bridge_derive::ToHaskell)]
pub enum SpawnRetainedResources {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnRetainedWorkspace(tidepool_bridge_effects::WtWorktreeHandle),
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnRetainedActor(
        (i64, i64),
        Option<tidepool_bridge_effects::WtWorktreeHandle>,
    ),
}

#[derive(Debug, Clone, tidepool_bridge_derive::ToHaskell)]
pub enum SpawnError {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnRefused(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnPartialFailure(
        SpawnRetainedResources,
        crate::lineage::SpawnCleanupOutcome,
        String,
    ),
}

pub(crate) struct SpawnDefinition {
    pub context: SpawnContextWire,
    pub workspace: crate::fork_workspace::SpawnWorkspaceWire,
}

pub(crate) struct ActorStartRequest {
    pub label: String,
    pub profile: ActorEffectProfileWire,
    pub workspace: Option<tidepool_bridge_effects::WtWorkspaceHandle>,
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub enum ActorEffectProfileWire {
    ActorSelectedProfile(Vec<ActorEffectKeyWire>),
}

impl ActorEffectProfileWire {
    fn resolve(self) -> Vec<ActorEffectKeyWire> {
        match self {
            Self::ActorSelectedProfile(keys) => keys,
        }
    }
}

pub use crate::nominal_keys::ActorEffectKeyWire;

/// One parked parent continuation paired with exclusive ownership of its child
/// entry. Its issuing session and compiler provenance travel with the value.
pub struct ResidentActorStart {
    pub(crate) parent_hole: ResidentHole,
    pub(crate) child: CapturedChildLaunch,
}

/// Rooted replacement recipe admitted by the controlling resident actor.
pub struct ActorReplacementDefinition {
    pub(crate) child: CapturedChildLaunch,
}

pub(crate) struct CapturedChildLaunch {
    pub lifetime: WorkerLifetime,
    pub descriptor: ActorDescriptor,
    pub spawn: Option<SpawnDefinition>,
    pub entry: crate::MailboxValue,
    pub launch_worktrees: Vec<String>,
    pub record_workspace: Option<tidepool_bridge_effects::WtWorkspaceHandle>,
    pub seed: Option<ChildSessionSeed>,
    pub exit_destination: Option<std::sync::Arc<crate::owned_result::RequestResultDestination>>,
}

/// Allocator floors captured before the parent checkout is released.
/// The descriptor separately retains the selected compiler interfaces.
pub(crate) struct ChildSessionSeed {
    pub val_generation: Generation,
    pub declaration_high_water: Option<Generation>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorStartCaptureError {
    #[error(transparent)]
    Session(#[from] tidepool_runtime::session::SessionError),
    #[error(transparent)]
    Resident(#[from] tidepool_runtime::session::ResidentError),
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error("actor start decoder received a non-start request")]
    UnexpectedRequest,
    #[error("actor start suspended without its child entry live payload")]
    MissingEntry,
    #[error(transparent)]
    ExactExports(#[from] tidepool_runtime::session::ExactExportError),
    #[error(transparent)]
    Facade(#[from] tidepool_runtime::session::ExactFacadeError),
    #[error("actor export `{head}` drifted from rooted definition {expected:?} to {actual:?}")]
    ShadowDrift {
        head: String,
        expected: tidepool_runtime::NominalHead,
        actual: tidepool_toolchain::declaration_join::ExportIdentity,
    },
    #[error("actor export `{head}` has more than one rooted nominal incarnation: {identities:?}")]
    AmbiguousIncarnation {
        head: String,
        identities: Vec<tidepool_runtime::NominalHead>,
    },
    #[error("actor start has no live declaration plane")]
    NoCompileView,
    #[error(transparent)]
    CompileView(#[from] crate::ActorCompileViewError),
    #[error("requested model must be a nonempty model identifier")]
    InvalidModel,
}

impl ResidentActorStart {
    /// Decode and claim a newly suspended start request while the resident
    /// machine is checked out. The entry root is born in the unpublished
    /// child's resource scope so parent cleanup cannot revoke a successfully accepted
    /// child computation.
    pub fn capture<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        request: &HaskellValue,
        table: &DataConTable,
        actor_context: &crate::ActorSessionContext,
        base_include: &[std::path::PathBuf],
        settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let session_id = actor_context.placement.session;
        let parent_actor = actor_context.actor;
        let (label, site, profile, workspace) = match ActorReq::from_value(request, table)? {
            ActorReq::ActorStartWith(label, site, _, profile, workspace) => {
                (label, site, profile, workspace)
            }
            ActorReq::ActorReplaceWith(_, site, _, label, profile) => (label, site, profile, None),
            _ => return Err(ActorStartCaptureError::UnexpectedRequest),
        };
        let site = u64::try_from(site).map_err(|_| ActorStartCaptureError::UnexpectedRequest)?;
        let witness = session.original_result_type_witness(site, &parent_hole)?;
        let destination = std::sync::Arc::new(crate::owned_result::RequestResultDestination::new(
            parent_actor,
            session_id,
            witness,
            session.lease_bindings(&[]),
        ));
        let mut captured = Self::capture_decoded(
            session,
            parent_hole,
            ActorStartRequest {
                label,
                profile,
                workspace,
            },
            actor_context,
            base_include,
            settlement,
        )?;
        captured.child.exit_destination = Some(destination);
        Ok(captured)
    }

    pub(crate) fn capture_spawn<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        context: SpawnContextWire,
        workspace: crate::fork_workspace::SpawnWorkspaceWire,
        effects: Vec<ActorEffectKeyWire>,
        label: Option<String>,
        model: Option<Model>,
        effort: Option<ForkEffort>,
        instructions: Option<String>,
        lifetime: WorkerLifetime,
        limits: Option<(i64, i64)>,
        actor_context: &crate::ActorSessionContext,
        base_include: &[std::path::PathBuf],
        settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let session_id = actor_context.placement.session;
        let parent_actor = actor_context.actor;
        if model.as_ref().is_some_and(|model| {
            model.value().is_empty() || model.value().chars().any(char::is_whitespace)
        }) {
            return Err(ActorStartCaptureError::InvalidModel);
        }
        let declaration_high_water = session.declaration_generation_high_water()?;
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)?
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        // Only the explicit installer closure's exact dependencies cross a
        // fresh context boundary; ambient parent lexical bindings do not.
        let facades = materialize_entry_facades(
            session,
            entry.provenance(),
            actor_context,
            base_include,
            settlement,
        )?;
        if !facades.is_empty() && declaration_high_water.is_none() {
            return Err(ActorStartCaptureError::NoCompileView);
        }
        let keys: Vec<_> = effects.into_iter().map(Into::into).collect();
        let fresh = matches!(&context, SpawnContextWire::FreshSpawn(_));
        let independent_machine = fresh && !keys.contains(&crate::ActorEffectKey::RepoEvent);
        let seed = independent_machine.then(|| ChildSessionSeed {
            val_generation: session.val_gen(),
            declaration_high_water,
        });
        let (child_session, lexical_scope) = if independent_machine {
            (
                tidepool_runtime::session::fresh_session_id(),
                tidepool_codegen::scope::ScopeId::ROOT,
            )
        } else {
            (session_id, session.mint_isolated_scope())
        };
        let checkpoint = match &context {
            SpawnContextWire::CapturedSpawn(token) => Some(token.clone()),
            SpawnContextWire::FreshSpawn(_) => None,
        };
        let mut descriptor = ActorDescriptor::new_optional(
            label,
            crate::ActorPlacement {
                session: child_session,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_capabilities(crate::ActorCapabilities::default().with_effect_keys(keys))
        .with_creator(parent_actor)
        .with_checkpoint_token(checkpoint)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades(&facades))
        .with_model(model)
        .with_fork_effort(effort)
        .with_instructions(instructions)
        .with_fork_budget(limits);
        if lifetime != WorkerLifetime::RunOwned {
            descriptor = descriptor.with_supervisor_parent(parent_actor);
        }
        Ok(Self {
            parent_hole,
            child: CapturedChildLaunch {
                lifetime,
                descriptor,
                spawn: Some(SpawnDefinition { context, workspace }),
                entry: crate::MailboxValue::new(session_id, entry),
                launch_worktrees: Vec::new(),
                record_workspace: None,
                seed,
                exit_destination: None,
            },
        })
    }

    pub(crate) fn capture_decoded<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        request: ActorStartRequest,
        actor_context: &crate::ActorSessionContext,
        base_include: &[std::path::PathBuf],
        settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let ActorStartRequest {
            label,
            profile,
            workspace,
        } = request;
        let session_id = actor_context.placement.session;
        let parent_actor = actor_context.actor;
        let effects = profile.resolve();
        let capabilities = crate::ActorCapabilities::default()
            .with_effect_keys(effects.into_iter().map(Into::into).collect());
        let declaration_high_water = session.declaration_generation_high_water()?;
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)?
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        let facades = materialize_entry_facades(
            session,
            entry.provenance(),
            actor_context,
            base_include,
            settlement,
        )?;
        if !facades.is_empty() && declaration_high_water.is_none() {
            return Err(ActorStartCaptureError::NoCompileView);
        }
        let independent_machine = !capabilities
            .effect_keys()
            .contains(&crate::ActorEffectKey::RepoEvent);
        let seed = independent_machine.then(|| ChildSessionSeed {
            val_generation: session.val_gen(),
            declaration_high_water,
        });
        let (launch_session, lexical_scope) = if independent_machine {
            (
                tidepool_runtime::session::fresh_session_id(),
                tidepool_codegen::scope::ScopeId::ROOT,
            )
        } else {
            (session_id, session.mint_isolated_scope())
        };
        let descriptor = ActorDescriptor::new(
            label,
            crate::ActorPlacement {
                session: launch_session,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_capabilities(capabilities)
        .with_creator(parent_actor)
        .with_supervisor_parent(parent_actor)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades(&facades));
        Ok(Self {
            parent_hole,
            child: CapturedChildLaunch {
                lifetime: WorkerLifetime::ActorOwned,
                descriptor,
                spawn: None,
                entry: crate::MailboxValue::new(session_id, entry),
                launch_worktrees: Vec::new(),
                record_workspace: workspace,
                seed,
                exit_destination: None,
            },
        })
    }
}

fn materialize_entry_facades<H, O>(
    session: &ResidentSession<H, O>,
    provenance: &tidepool_runtime::session::ProgramProvenance,
    context: &crate::ActorSessionContext,
    base_include: &[std::path::PathBuf],
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<Vec<MaterializedFacade>, ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    use tidepool_toolchain::declaration_join::{ExportIdentity, ExportNamespace};
    // Acquire once: parent release refuses new observations but cannot revoke
    // immutable custody already acquired for this unpublished launch.
    let mut inherited = context.source_imports.exact_facades()?;
    inherited.sort_by_key(MaterializedFacade::module_name);
    let rooted = rooted_incarnations(provenance)?;
    if rooted.is_empty() {
        return Ok(Vec::new());
    }
    let scope = session.run_context().lexical_scope;
    let view = session
        .compile_view_in(scope)
        .ok_or(ActorStartCaptureError::NoCompileView)?;
    let projections = inherited
        .iter()
        .map(|facade| {
            facade
                .projection()
                .cloned()
                .ok_or(tidepool_runtime::session::ExactFacadeError::UncertifiedSelection)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let inputs = tidepool_runtime::session::RuntimeCompileInputs::new(None, projections)
        .map_err(tidepool_runtime::session::SessionError::Compile)?;
    let actor_view = context.compile_view_with_inputs(view.clone(), inputs)?;
    let include = actor_view.include_paths(base_include);
    let mut local = Vec::new();
    let mut selections: std::collections::BTreeMap<usize, Vec<ExportIdentity>> = Default::default();
    for (name, expected) in rooted {
        let wanted = ExportIdentity {
            unit: expected.unit.clone(),
            module: expected.module.clone(),
            namespace: ExportNamespace::Type,
            occurrence: name.clone(),
            record_parent: None,
        };
        match session.exact_exports_in_namespace(scope, ExportNamespace::Type, &[&name]) {
            Ok(surface) => {
                let actual = surface
                    .declarations()?
                    .first()
                    .ok_or(tidepool_runtime::session::ExactExportError::UncertifiedExports)?;
                validate_original_head(&expected, &actual.head)?;
                local.push(name);
            }
            Err(tidepool_runtime::session::ExactExportError::UnknownExport { .. }) => {
                let selected_name = &name;
                let candidates = inherited
                    .iter()
                    .enumerate()
                    .flat_map(|(index, facade)| {
                        facade.projection().into_iter().flat_map(move |projection| {
                            projection
                                .receipt()
                                .exports()
                                .iter()
                                .filter(|export| {
                                    export.head.namespace == ExportNamespace::Type
                                        && export.head.occurrence == *selected_name
                                })
                                .map(move |export| (index, export))
                        })
                    })
                    .collect::<Vec<_>>();
                let identities: BTreeSet<_> = candidates
                    .iter()
                    .map(|(_, export)| tidepool_runtime::NominalHead {
                        unit: export.head.unit.clone(),
                        module: export.head.module.clone(),
                        name: export.head.occurrence.clone(),
                    })
                    .collect();
                if identities.len() > 1 {
                    return Err(ActorStartCaptureError::AmbiguousIncarnation {
                        head: name,
                        identities: identities.into_iter().collect(),
                    });
                }
                let Some((index, actual)) = candidates.first() else {
                    return Err(tidepool_runtime::session::ExactExportError::UnknownExport {
                        scope,
                        name,
                    }
                    .into());
                };
                validate_original_head(&expected, &actual.head)?;
                if candidates.iter().any(|(_, other)| *other != *actual) {
                    return Err(
                        tidepool_runtime::session::ExactExportError::UncertifiedExports.into(),
                    );
                }
                selections.entry(*index).or_default().push(wanted);
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut facades = Vec::new();
    if !local.is_empty() {
        let names = local.iter().map(String::as_str).collect::<Vec<_>>();
        let surface = session.exact_exports_in_namespace(scope, ExportNamespace::Type, &names)?;
        facades.push(surface.materialize(&view, settlement)?);
    }
    for (index, selected) in selections {
        if let Some(facade) = inherited[index].select_exact_exports(
            &selected,
            &include,
            actor_view.session_root(),
            settlement,
        )? {
            facades.push(facade);
        }
    }
    Ok(facades)
}

fn rooted_incarnations(
    provenance: &tidepool_runtime::session::ProgramProvenance,
) -> Result<std::collections::BTreeMap<String, tidepool_runtime::NominalHead>, ActorStartCaptureError>
{
    let mut rooted: std::collections::BTreeMap<String, BTreeSet<tidepool_runtime::NominalHead>> =
        Default::default();
    for head in provenance
        .sites()
        .iter()
        .flat_map(|site| {
            site.heads
                .iter()
                .chain(site.inputs.iter().flat_map(|input| input.heads.iter()))
        })
        .filter(|head| head.module.starts_with("Tidepool.Session.Lib.G"))
    {
        rooted
            .entry(head.name.clone())
            .or_default()
            .insert(head.clone());
    }
    rooted
        .into_iter()
        .map(|(head, identities)| {
            if identities.len() != 1 {
                Err(ActorStartCaptureError::AmbiguousIncarnation {
                    head,
                    identities: identities.into_iter().collect(),
                })
            } else {
                Ok((
                    head,
                    identities.into_iter().next().expect("one rooted identity"),
                ))
            }
        })
        .collect()
}

fn validate_original_head(
    expected: &tidepool_runtime::NominalHead,
    actual: &tidepool_toolchain::declaration_join::ExportIdentity,
) -> Result<(), ActorStartCaptureError> {
    use tidepool_toolchain::declaration_join::ExportNamespace;
    if actual.unit != expected.unit
        || actual.module != expected.module
        || actual.occurrence != expected.name
        || actual.namespace != ExportNamespace::Type
        || actual.record_parent.is_some()
    {
        return Err(ActorStartCaptureError::ShadowDrift {
            head: expected.name.clone(),
            expected: expected.clone(),
            actual: actual.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
fn validate_head_incarnations(
    declarations: &[tidepool_toolchain::declaration_join::DeclarationExport],
    provenance: &tidepool_runtime::session::ProgramProvenance,
) -> Result<(), ActorStartCaptureError> {
    for (head, expected) in rooted_incarnations(provenance)? {
        let actual = declarations
            .iter()
            .find(|export| {
                export.head.namespace == tidepool_toolchain::declaration_join::ExportNamespace::Type
                    && export.head.occurrence == head
            })
            .ok_or(tidepool_runtime::session::ExactExportError::UncertifiedExports)?;
        validate_original_head(&expected, &actual.head)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tidepool_codegen::suspension::RealmId;

    #[test]
    fn generated_actor_key_bridge_decodes_every_active_nominal_key() {
        use super::ActorEffectKeyWire;
        use tidepool_bridge::{FromHaskell, HaskellValue};
        use tidepool_repr::{DataCon, DataConId, DataConTable};
        for (index, wire) in ActorEffectKeyWire::ALL.iter().copied().enumerate() {
            let key = crate::ActorEffectKey::from(wire);
            assert_eq!(key, crate::ActorEffectKey::ALL[index]);
            assert_eq!(Some(wire.haskell_constructor()), key.haskell_constructor());
            let name = wire.haskell_constructor();
            let id = DataConId(index as u64 + 1);
            let mut table = DataConTable::new();
            let constructor = DataCon {
                identity: tidepool_repr::execution_schema::ConstructorIdentity {
                    symbol: tidepool_repr::execution_schema::SymbolIdentity {
                        unit: "fixture".into(),
                        module: "Tidepool.Effects.Core".into(),
                        namespace: "constructor".into(),
                        occurrence: name.into(),
                        record_parent: None,
                    },
                    family: tidepool_repr::execution_schema::TypeIdentity {
                        symbol: tidepool_repr::execution_schema::SymbolIdentity {
                            unit: "fixture".into(),
                            module: "Tidepool.Effects.Core".into(),
                            namespace: "type".into(),
                            occurrence: "ActorEffectKey".into(),
                            record_parent: None,
                        },
                        version: tidepool_repr::execution_schema::TypeVersion::GhcAbi([1; 16]),
                    },
                },
                id,
                name: name.into(),
                tag: index as u32 + 1,
                rep_arity: 0,
                field_bangs: vec![],
                qualified_name: Some(format!("Tidepool.Effects.Core.{name}")),
                type_name: "ActorEffectKey".into(),
            };
            table.insert_checked(constructor.clone()).unwrap();
            let decoded =
                ActorEffectKeyWire::from_value(&HaskellValue::Con(id, Vec::new()), &table).unwrap();
            assert_eq!(crate::ActorEffectKey::from(decoded), key);
            let foreign_id = DataConId(index as u64 + 10_000);
            let mut foreign = constructor;
            foreign.id = foreign_id;
            foreign.identity.symbol.module = "Foreign.Keys".into();
            foreign.identity.family.symbol.module = "Foreign.Keys".into();
            foreign.qualified_name = Some(format!("Foreign.Keys.{name}"));
            table.insert_checked(foreign).unwrap();
            assert!(ActorEffectKeyWire::from_value(
                &HaskellValue::Con(foreign_id, Vec::new()),
                &table
            )
            .is_err());
        }
    }

    proptest::proptest! {
        #[test]
        fn selected_rows_preserve_attenuation_through_descendant_histories(
            rows in proptest::collection::vec(proptest::collection::vec(0usize..5, 0..7), 1..7),
            permitted in 0u8..32,
            initial_depth in 0u16..7,
            initial_width in 0u16..4,
        ) {
            use crate::{ActorCapabilities, ActorEffectKey as Key, DescendantBudget};
            let keys = [Key::AgentLaunch, Key::Replies, Key::Actor, Key::Commands, Key::Reflect];
            let mut mask = permitted;
            let mut depth = initial_depth;
            let mut width = initial_width;
            let mut parent = ActorCapabilities::default()
                .with_effect_keys(keys.iter().enumerate()
                    .filter(|(index, _)| mask & (1 << index) != 0)
                    .map(|(_, key)| *key).collect())
                .with_descendant_budget(DescendantBudget {
                    maximum_depth: depth,
                    maximum_active_children: Some(width),
                });
            for row in rows {
                let requested = row.iter().fold(0u8, |bits, index| bits | (1 << index));
                let unique = requested.count_ones() as usize == row.len();
                let expected = depth > 0 && width > 0 && unique && requested & !mask == 0;
                let selected = row.iter().map(|index| keys[*index]).collect::<Vec<_>>();
                let admission = parent.preview_child(
                    ActorCapabilities::default().with_effect_keys(selected.clone()), None,
                );
                proptest::prop_assert_eq!(admission.is_ok(), expected);
                if let Ok(child) = admission {
                    mask = requested;
                    if mask & 1 != 0 {
                        depth -= 1;
                    } else {
                        depth = 0;
                        width = 0;
                    }
                    proptest::prop_assert_eq!(child.effect_keys(), selected.as_slice());
                    proptest::prop_assert_eq!(child.descendants(), DescendantBudget {
                        maximum_depth: depth,
                        maximum_active_children: Some(width),
                    });
                    parent = child;
                }
            }
        }

        #[test]
        fn rooted_nominal_references_converge_only_for_the_same_original(
            copies in 1usize..12,
            foreign in proptest::prelude::any::<bool>(),
            generation in 1u64..100,
        ) {
            let first = original_nominal();
            let mut heads = vec![first.clone(); copies];
            let mut other = first;
            if foreign { other.unit = "foreign".into(); }
            other.module = format!("Tidepool.Session.Lib.G{generation}");
            heads.push(other.clone());
            let same = !foreign && generation == 1;
            let result = super::rooted_incarnations(&nominal_provenance(heads));
            proptest::prop_assert_eq!(result.is_ok(), same);
            if let Ok(rooted) = result { proptest::prop_assert_eq!(rooted.len(), 1); }
        }

        #[test]
        fn selected_profiles_preserve_exact_keys_and_parent_refusal(
            selected in proptest::collection::vec(0usize..5, 0..9),
            allowed in proptest::prelude::any::<u8>(),
        ) {
            use super::ActorEffectKeyWire as Wire;
            use crate::ActorEffectKey as Key;
            let keys = [Key::Actor, Key::Commands, Key::Jev, Key::ModelCall, Key::Reflect];
            let expected = selected.iter().map(|index| keys[*index]).collect::<Vec<_>>();
            let wire = selected.iter().map(|index| match index {
                0 => Wire::EffectActor,
                1 => Wire::EffectCommands,
                2 => Wire::EffectJev,
                3 => Wire::EffectModelCall,
                _ => Wire::EffectReflect,
            }).collect();
            let actual = super::ActorEffectProfileWire::ActorSelectedProfile(wire).resolve();
            let actual = actual.into_iter().map(Key::from).collect::<Vec<_>>();
            proptest::prop_assert_eq!(&actual, &expected);
            let permitted = keys.into_iter().enumerate()
                .filter(|(index, _)| allowed & (1 << index) != 0)
                .map(|(_, key)| key).collect::<Vec<_>>();
            let parent = crate::ActorCapabilities::default().with_effect_keys(permitted.clone());
            let child = crate::ActorCapabilities::default().with_effect_keys(actual);
            let unique = expected.iter().copied().collect::<std::collections::HashSet<_>>().len()
                == expected.len();
            let admitted = unique && expected.iter().all(|key| permitted.contains(key));
            proptest::prop_assert_eq!(parent.preview_child(child, None).is_ok(), admitted);
        }
    }

    #[test]
    fn inherited_and_local_capture_preserves_originals_refuses_shadow_and_released_owner() {
        use tidepool_codegen::scope::ScopeId;
        use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
        use tidepool_runtime::session::{ModuleEnv, SessionLib, SessionRunContext};
        let root = tempfile::tempdir().unwrap();
        let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
        let mut lib = SessionLib::open(
            tidepool_repr::SessionId(17),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        let recovery = crate::recovery::recovery_test_fixture::RecoveryFixture::create(root.path());
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement));
        let mut session = tidepool_runtime::session::ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        tidepool_testing::with_settlement(|settlement| {
            session.define_scoped_in(
                ScopeId::ROOT,
                &[
                    "data Inherited = Inherited Int",
                    "data Withheld = Withheld Int",
                ],
                settlement,
            )
        })
        .unwrap();
        let surface = session
            .exact_exports_in(ScopeId::ROOT, &["Inherited", "Withheld"])
            .unwrap();
        let original = surface
            .declarations()
            .unwrap()
            .iter()
            .find(|export| export.head.occurrence == "Inherited")
            .unwrap()
            .head
            .clone();
        let facade = tidepool_testing::with_settlement(|settlement| {
            surface.materialize(&session.compile_view_in(ScopeId::ROOT).unwrap(), settlement)
        })
        .unwrap();
        let ceiling = session
            .declaration_generation_high_water()
            .unwrap()
            .unwrap();
        drop(session);
        let child_id = tidepool_repr::SessionId(18);
        let mut child_lib = SessionLib::open(
            child_id,
            root.path().join("child"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        tidepool_testing::with_settlement(|settlement| {
            recovery.attach(
                &mut child_lib,
                tidepool_runtime::session::RecoveryDomainId::Session(child_id),
                settlement,
            )
        });
        let mut session = tidepool_runtime::session::ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(child_lib),
        );
        session
            .initialize_captured_declaration_high_water(ceiling)
            .unwrap();
        let scope = ScopeId::ROOT;
        let mut context = selection_context();
        context.placement.session = child_id;
        context.source_imports = crate::ActorSourceImports::from_exact_facades([&facade, &facade]);
        session
            .set_actor_execution(
                SessionRunContext::ROOT,
                EffectRunPolicy::HandleOrSuspend,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .unwrap();
        assert!(session
            .compile_view_in(scope)
            .unwrap()
            .library_import_module()
            .is_none());
        assert_eq!(
            session.declaration_generation_high_water().unwrap(),
            Some(ceiling)
        );
        let inherited_head = tidepool_runtime::NominalHead {
            unit: original.unit.clone(),
            module: original.module.clone(),
            name: original.occurrence.clone(),
        };
        let inherited = nominal_provenance(vec![inherited_head.clone(), inherited_head.clone()]);
        let selected = tidepool_testing::with_settlement(|settlement| {
            super::materialize_entry_facades(
                &session,
                &inherited,
                &context,
                effects.include_paths(),
                settlement,
            )
        })
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].projection().unwrap().receipt().exports().len(),
            1
        );
        assert_eq!(
            selected[0].projection().unwrap().receipt().exports()[0].head,
            original
        );
        let local_generation = tidepool_testing::with_settlement(|settlement| {
            session.define_scoped_in(scope, &["data Local = Local Int"], settlement)
        })
        .unwrap();
        let local = session
            .exact_exports_in(scope, &["Local"])
            .unwrap()
            .declarations()
            .unwrap()[0]
            .head
            .clone();
        assert!(local_generation > ceiling);
        let mixed = nominal_provenance(vec![
            inherited_head.clone(),
            tidepool_runtime::NominalHead {
                unit: local.unit.clone(),
                module: local.module.clone(),
                name: local.occurrence.clone(),
            },
        ]);
        let selected = tidepool_testing::with_settlement(|settlement| {
            super::materialize_entry_facades(
                &session,
                &mixed,
                &context,
                effects.include_paths(),
                settlement,
            )
        })
        .unwrap();
        let actual = selected
            .iter()
            .flat_map(|facade| {
                facade
                    .projection()
                    .unwrap()
                    .receipt()
                    .exports()
                    .iter()
                    .map(|export| export.head.clone())
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, [original, local].into_iter().collect());
        let independent = crate::ActorSourceImports::from_exact_facades(&selected);
        let grandchild_id = tidepool_repr::SessionId(19);
        let mut grandchild_lib = SessionLib::open(
            grandchild_id,
            root.path().join("grandchild"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        tidepool_testing::with_settlement(|settlement| {
            recovery.attach(
                &mut grandchild_lib,
                tidepool_runtime::session::RecoveryDomainId::Session(grandchild_id),
                settlement,
            )
        });
        let mut grandchild = tidepool_runtime::session::ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(grandchild_lib),
        );
        grandchild
            .initialize_captured_declaration_high_water(
                session
                    .declaration_generation_high_water()
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        let mut descendant = context.clone();
        descendant.placement.session = grandchild_id;
        descendant.source_imports = independent;
        assert!(matches!(
            descendant.compile_view(session.compile_view_in(scope).unwrap()),
            Err(crate::ActorCompileViewError::WrongSession { .. })
        ));
        use std::sync::Arc;
        use tidepool_runtime::session::{turn, TemplateSelector};
        use tidepool_toolchain::checked_cell::CheckedCellSpecification;
        let execution = Arc::new(grandchild.begin_private_execution(scope).unwrap());
        let private_scope = execution.private_scope();
        assert!(matches!(
            descendant.compile_view(grandchild.compile_view_for_execution(&execution).unwrap()),
            Err(crate::ActorCompileViewError::WrongLexicalScope { .. })
        ));
        grandchild
            .set_run_context(SessionRunContext {
                lexical_scope: private_scope,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        descendant.placement.lexical_scope = private_scope;
        let view = descendant
            .compile_view(grandchild.compile_view_for_execution(&execution).unwrap())
            .unwrap();
        let imports = view.turn_imports();
        let templates = tidepool_runtime::session::resident_workbench_templates(
            effects.preamble(),
            "'[]",
            &imports,
        );
        let specification = Arc::new(CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "mixedAnswer <- pure (case (Inherited 20, Local 22) of (Inherited x, Local y) -> x + y)".into(),
            template_source: tidepool_runtime::session::resident_cell_check_template(
                effects.preamble(), "'[]", &imports,
            ),
            turn_templates: templates.iter().map(|template| {
                let kind = match template.kind {
                    TemplateSelector::Decl => "decl",
                    TemplateSelector::Bind => "bind",
                    TemplateSelector::BindDiscard => "binddiscard",
                    TemplateSelector::Expr => "expr",
                };
                (kind.into(), template.source.clone())
            }).collect(),
            injected_modules: Vec::new(),
            reserved_declaration_modules: Vec::new(),
        });
        let includes = view.include_paths(effects.include_paths());
        let plan = tidepool_testing::with_settlement(|settlement| {
            tidepool_toolchain::artifacts::parse_cell_plan(
                specification.clone(),
                &includes,
                settlement,
            )
        })
        .unwrap();
        let admission = grandchild
            .admit_planned_cell_for_execution(
                execution,
                plan,
                specification.clone(),
                specification.specification_digest(),
                [0; 32],
                includes,
                Some(view.compile_inputs().clone()),
            )
            .unwrap();
        let (checked, program) = tidepool_testing::with_settlement(|settlement| {
            turn::compile_cell_program_admitted(admission.clone(), settlement)
        })
        .unwrap();
        let prefix = grandchild
            .begin_cell_program(admission, program)
            .unwrap()
            .unwrap();
        let item = grandchild
            .admit_checked_item(prefix, checked.checked_item(0).unwrap())
            .unwrap();
        let turn::TurnResult::Bind {
            bound, compiled, ..
        } = turn::consume_cell_program_item(item.clone()).unwrap()
        else {
            panic!("mixed consumer must be a checked bind")
        };
        let tidepool_runtime::session::ResidentOutcome::Completed { result, .. } =
            tidepool_testing::with_settlement(|settlement| {
                grandchild.run_bind_with_sites(
                    "mixedAnswer",
                    compiled.code(),
                    &bound[0],
                    item.generation(),
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("mixed consumer must complete")
        };
        assert_eq!(result.to_json(), serde_json::json!(42));

        tidepool_testing::with_settlement(|settlement| {
            session.define_scoped_in(scope, &["data Inherited = Inherited Bool"], settlement)
        })
        .unwrap();
        assert!(matches!(
            tidepool_testing::with_settlement(|settlement| super::materialize_entry_facades(
                &session,
                &inherited,
                &context,
                effects.include_paths(),
                settlement
            )),
            Err(super::ActorStartCaptureError::ShadowDrift { .. })
        ));
        let shadow = tidepool_testing::with_settlement(|settlement| {
            session
                .exact_exports_in(scope, &["Inherited"])
                .unwrap()
                .materialize(&session.compile_view_in(scope).unwrap(), settlement)
        })
        .unwrap();
        let mut ambiguous = descendant.clone();
        ambiguous.placement.lexical_scope = scope;
        ambiguous.source_imports =
            crate::ActorSourceImports::from_exact_facades([&facade, &shadow]);
        grandchild.set_run_context(SessionRunContext::ROOT).unwrap();
        assert!(matches!(
            tidepool_testing::with_settlement(|settlement| super::materialize_entry_facades(
                &grandchild,
                &inherited,
                &ambiguous,
                effects.include_paths(),
                settlement,
            )),
            Err(super::ActorStartCaptureError::AmbiguousIncarnation { .. })
        ));
        context.source_imports.release_capture();
        assert!(matches!(
            tidepool_testing::with_settlement(|settlement| super::materialize_entry_facades(
                &session,
                &mixed,
                &context,
                effects.include_paths(),
                settlement
            )),
            Err(super::ActorStartCaptureError::CompileView(
                crate::ActorCompileViewError::ReleasedDeclarationCapture
            ))
        ));
    }

    #[test]
    fn empty_provenance_requires_no_child_facade() {
        let (session, root) = facade_selection_fixture();
        let facade = tidepool_testing::with_settlement(|settlement| {
            super::materialize_entry_facades(
                &session,
                &tidepool_runtime::session::ProgramProvenance::default(),
                &selection_context(),
                &[],
                settlement,
            )
        })
        .unwrap();
        assert!(facade.is_empty());
        assert_eq!(
            crate::ActorSourceImports::from_exact_facades(&facade),
            crate::ActorSourceImports::default()
        );
        assert!(!root.path().join("Tidepool/Actor/Surface").exists());
    }

    fn selection_context() -> crate::ActorSessionContext {
        crate::ActorDescriptor::new(
            "selection fixture",
            crate::ActorPlacement {
                session: tidepool_repr::SessionId(17),
                resource_scope: RealmId::ROOT,
                lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
            },
        )
        .session_context(crate::ActorRef::first(crate::ActorId(17)))
    }

    fn facade_selection_fixture() -> (
        tidepool_runtime::session::ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        tempfile::TempDir,
    ) {
        let root = tempfile::tempdir().unwrap();
        let lib = tidepool_runtime::session::SessionLib::open(
            tidepool_repr::SessionId(17),
            root.path(),
            tidepool_runtime::session::ModuleEnv::standalone_default(),
        )
        .unwrap();
        (
            tidepool_runtime::session::ResidentSession::unbootstrapped(
                frunk::HNil,
                tidepool_mcp::CapturedOutput::new(),
                tidepool_runtime::DEFAULT_NURSERY_SIZE,
                Some(lib),
            ),
            root,
        )
    }

    #[test]
    fn nonempty_facade_selection_still_refuses_unknown_exports() {
        let (session, root) = facade_selection_fixture();
        let provenance = tidepool_runtime::session::ProgramProvenance::from_sites(&[
            tidepool_runtime::YieldSite {
                input_type_witnesses: Vec::new(),
                site: 17,
                origin: "entry".into(),
                ordinal: 0,
                ty: "Missing".into(),
                modules: vec!["Tidepool.Session.Lib.G1".into()],
                heads: vec![tidepool_runtime::NominalHead {
                    unit: "main".into(),
                    module: "Tidepool.Session.Lib.G1".into(),
                    name: "Missing".into(),
                }],
                inputs: vec![],
                reply_declaration: None,
                request_type_signatures: None,
            },
        ])
        .unwrap();
        assert!(
            matches!(tidepool_testing::with_settlement(|settlement| super::materialize_entry_facades(&session, &provenance, &selection_context(), &[], settlement)),
            Err(super::ActorStartCaptureError::ExactExports(
                tidepool_runtime::session::ExactExportError::UnknownExport { name, .. },
            )) if name == "Missing")
        );
        assert!(!root.path().join("Tidepool/Actor/Surface").exists());
    }

    fn nominal_provenance(
        heads: Vec<tidepool_runtime::NominalHead>,
    ) -> tidepool_runtime::session::ProgramProvenance {
        tidepool_runtime::session::ProgramProvenance::from_sites(&[tidepool_runtime::YieldSite {
            input_type_witnesses: Vec::new(),
            site: 17,
            origin: "entry".into(),
            ordinal: 0,
            ty: "Original".into(),
            modules: heads.iter().map(|head| head.module.clone()).collect(),
            heads,
            inputs: vec![],
            reply_declaration: None,
            request_type_signatures: None,
        }])
        .unwrap()
    }

    fn original_nominal() -> tidepool_runtime::NominalHead {
        tidepool_runtime::NominalHead {
            unit: "main".into(),
            module: "Tidepool.Session.Lib.G1".into(),
            name: "Original".into(),
        }
    }

    fn original_declaration() -> tidepool_toolchain::declaration_join::DeclarationExport {
        use tidepool_toolchain::declaration_join::{
            DeclarationExport, DeclarationKind, ExportIdentity, ExportNamespace,
        };
        let head = original_nominal();
        DeclarationExport {
            kind: DeclarationKind::Type,
            head: ExportIdentity {
                unit: head.unit,
                module: head.module,
                namespace: ExportNamespace::Type,
                occurrence: head.name,
                record_parent: None,
            },
            children: Vec::new(),
        }
    }

    #[test]
    fn facade_nominal_identity_preserves_original_owner_after_join() {
        let provenance = nominal_provenance(vec![original_nominal()]);
        // The facade's current wrapper generation is deliberately not an input
        // to validation: the compiler export retains its original G1 owner.
        super::validate_head_incarnations(&[original_declaration()], &provenance).unwrap();
    }

    #[test]
    fn facade_nominal_identity_refuses_unit_module_namespace_and_ambiguity_drift() {
        use tidepool_toolchain::declaration_join::ExportNamespace;
        let provenance = nominal_provenance(vec![original_nominal()]);
        for foreign_unit in [false, true] {
            let mut actual = original_declaration();
            if foreign_unit {
                actual.head.unit = "foreign".into();
            } else {
                actual.head.module = "Tidepool.Session.Lib.G9".into();
            }
            assert!(matches!(
                super::validate_head_incarnations(&[actual], &provenance),
                Err(super::ActorStartCaptureError::ShadowDrift { .. })
            ));
        }
        let mut value = original_declaration();
        value.head.namespace = ExportNamespace::Value;
        assert!(matches!(
            super::validate_head_incarnations(&[value], &provenance),
            Err(super::ActorStartCaptureError::ExactExports(
                tidepool_runtime::session::ExactExportError::UncertifiedExports
            ))
        ));
        let mut other = original_nominal();
        other.unit = "foreign".into();
        let ambiguous = nominal_provenance(vec![original_nominal(), other]);
        assert!(matches!(
            super::validate_head_incarnations(&[original_declaration()], &ambiguous),
            Err(super::ActorStartCaptureError::AmbiguousIncarnation { .. })
        ));
    }
}
