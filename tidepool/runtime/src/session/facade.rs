//! Compiler-issued selected declaration interfaces. Source-only compatibility
//! facades remain separate and cannot authorize nominal actor payloads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_repr::{SessionId, SessionModule};
use tidepool_toolchain::declaration_join::{
    DeclarationExport, ExactModuleIdentity, ExportIdentity,
};

use super::lexical_projection::{issue_projection, DeclarationProjectionBaseline};
use super::{CertifiedDeclarationProjection, ExportItem, SessionCompileView};

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExportAuthority {
    Empty,
    Legacy,
    Certified {
        baseline: Arc<DeclarationProjectionBaseline>,
        exports: Vec<DeclarationExport>,
        includes: Vec<PathBuf>,
    },
}

/// Selected compiler identities with their original owned lexical closure.
/// Callers cannot turn names or available interfaces into export authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactExportSurface {
    session: SessionId,
    source: Option<SessionModule>,
    items: Vec<ExportItem>,
    authority: ExportAuthority,
}

impl ExactExportSurface {
    pub(crate) fn legacy(
        session: SessionId,
        source: Option<SessionModule>,
        mut items: Vec<ExportItem>,
    ) -> Self {
        items.sort_by_key(ExportItem::render_entry);
        items.dedup_by(|left, right| {
            left.head_namespace() == right.head_namespace() && left.head_name() == right.head_name()
        });
        let authority = if items.is_empty() {
            ExportAuthority::Empty
        } else {
            ExportAuthority::Legacy
        };
        Self {
            session,
            source,
            items,
            authority,
        }
    }

    pub(super) fn certified(
        session: SessionId,
        source: Option<SessionModule>,
        baseline: DeclarationProjectionBaseline,
        mut exports: Vec<DeclarationExport>,
        includes: Vec<PathBuf>,
    ) -> Self {
        for export in &mut exports {
            export.children.sort();
        }
        exports.sort_by(|left, right| left.head.cmp(&right.head));
        Self {
            session,
            source,
            items: exports.iter().map(ExportItem::from).collect(),
            authority: ExportAuthority::Certified {
                baseline: Arc::new(baseline),
                exports,
                includes,
            },
        }
    }

    pub fn session(&self) -> SessionId {
        self.session
    }
    pub fn source_module(&self) -> Option<SessionModule> {
        self.source
    }
    pub fn items(&self) -> &[ExportItem] {
        &self.items
    }

    pub fn declarations(&self) -> Result<&[DeclarationExport], ExactExportError> {
        match &self.authority {
            ExportAuthority::Empty => Ok(&[]),
            ExportAuthority::Legacy => Err(ExactExportError::UncertifiedExports),
            ExportAuthority::Certified { exports, .. } => Ok(exports),
        }
    }

    /// Issue a selected interface through the same compiler join owner used by
    /// cumulative declaration views. The original modules remain dependencies;
    /// only this interface is a new lexical root.
    pub(super) fn materialize(
        &self,
        view: &SessionCompileView,
        admit: &dyn Fn() -> Result<Option<super::RuntimeSourceClaim>, super::SourceAdmissionError>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<MaterializedFacade, ExactFacadeError> {
        if view.session() != self.session {
            return Err(ExactFacadeError::WrongSession {
                surface: self.session,
                view: view.session(),
            });
        }
        if let ExportAuthority::Certified {
            baseline,
            exports,
            includes,
        } = &self.authority
        {
            let _source_claim = admit().map_err(super::SessionError::SourceAdmission)?;
            let projection = issue_projection(
                &baseline.context,
                &baseline.owner,
                &baseline.surface,
                exports,
                &baseline.instances,
                &baseline.families,
                includes,
                view.session_root(),
                settlement,
            )?;
            let identity = FacadeIdentity {
                digest: projection.receipt().expected_public_version().to_owned(),
            };
            return Ok(MaterializedFacade {
                identity,
                artifact: FacadeArtifact::Certified(projection),
            });
        }
        let identity = FacadeIdentity::for_legacy_surface(self);
        let source = render_legacy_facade(&identity, self);
        let path = view.session_root().join(identity.relative_hs_path());
        if !std::fs::read(&path).is_ok_and(|existing| existing == source.as_bytes()) {
            let _source_claim = admit().map_err(super::SessionError::SourceAdmission)?;
            let parent = path
                .parent()
                .ok_or_else(|| ExactFacadeError::InvalidPath(path.clone()))?;
            std::fs::create_dir_all(parent).map_err(|source| ExactFacadeError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
            tidepool_atomic_write::write_best_effort(&path, source.as_bytes()).map_err(
                |error| ExactFacadeError::Io {
                    path: error.path,
                    source: error.source,
                },
            )?;
        }
        Ok(MaterializedFacade {
            identity,
            artifact: FacadeArtifact::Source { path, source },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FacadeIdentity {
    digest: String,
}

impl FacadeIdentity {
    fn for_legacy_surface(surface: &ExactExportSurface) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"tidepool-source-only-export-facade-v2\0");
        hash.update(&surface.session.0.to_le_bytes());
        if let Some(module) = surface.source {
            hash.update(module.module_name().as_bytes());
        }
        hash.update(b"\0");
        for item in &surface.items {
            hash.update(item.render_entry().as_bytes());
            hash.update(b"\0");
        }
        Self {
            digest: hash.finalize().to_hex().to_string(),
        }
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn module_name(&self) -> String {
        format!("Tidepool.Actor.Surface.H{}", self.digest)
    }
    pub fn relative_hs_path(&self) -> PathBuf {
        PathBuf::from(format!("Tidepool/Actor/Surface/H{}.hs", self.digest))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FacadeArtifact {
    Source { path: PathBuf, source: String },
    Certified(Arc<CertifiedDeclarationProjection>),
}

/// The same issued projection survives descriptor, installation and seed
/// transfers. Certified interfaces are never reduced to source import strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializedFacade {
    identity: FacadeIdentity,
    artifact: FacadeArtifact,
}
impl MaterializedFacade {
    /// Narrow this issued interface by original identities. The selected groups
    /// and implementation closure come from this owner, never caller evidence.
    pub(super) fn select_exact_exports(
        &self,
        selected: &[ExportIdentity],
        includes: &[PathBuf],
        root: &Path,
        admit: &dyn Fn() -> Result<Option<super::RuntimeSourceClaim>, super::SourceAdmissionError>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<Option<Self>, ExactFacadeError> {
        let inherited = self
            .projection()
            .ok_or(ExactFacadeError::UncertifiedSelection)?;
        let wanted: std::collections::BTreeSet<_> = selected.iter().collect();
        if wanted.is_empty() {
            return Ok(None);
        }
        let exports = wanted
            .into_iter()
            .map(|head| {
                inherited
                    .receipt()
                    .exports()
                    .iter()
                    .find(|export| &export.head == head)
                    .cloned()
                    .ok_or_else(|| ExactFacadeError::UnselectedExport(head.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if exports == inherited.receipt().exports() {
            return Ok(Some(self.clone()));
        }
        let owner = ExactModuleIdentity {
            unit: inherited.receipt().reserved().unit.clone(),
            module: inherited.module_name().to_owned(),
        };
        let surface = super::render::AdmittedDeclarationSurface {
            roots: vec![owner.clone()],
            lexical: inherited.context().lexical_graph().to_vec(),
        };
        let _source_claim = admit().map_err(super::SessionError::SourceAdmission)?;
        let projection = issue_projection(
            inherited.context(),
            &owner,
            &surface,
            &exports,
            inherited.receipt().instances(),
            inherited.receipt().family_closure(),
            includes,
            root,
            settlement,
        )?;
        Ok(Some(Self {
            identity: FacadeIdentity {
                digest: projection.receipt().expected_public_version().to_owned(),
            },
            artifact: FacadeArtifact::Certified(projection),
        }))
    }

    pub fn identity(&self) -> &FacadeIdentity {
        &self.identity
    }
    pub fn module_name(&self) -> String {
        self.identity.module_name()
    }
    pub fn projection(&self) -> Option<&Arc<CertifiedDeclarationProjection>> {
        match &self.artifact {
            FacadeArtifact::Certified(projection) => Some(projection),
            FacadeArtifact::Source { .. } => None,
        }
    }
    pub fn source_artifact(&self) -> Option<(&Path, &str)> {
        match &self.artifact {
            FacadeArtifact::Source { path, source } => Some((path, source)),
            FacadeArtifact::Certified(_) => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExactFacadeError {
    #[error("facade selection requires compiler-certified original identities")]
    UncertifiedSelection,
    #[error("declaration {0:?} was not selected by this facade owner")]
    UnselectedExport(ExportIdentity),
    #[error(transparent)]
    Projection(#[from] super::SessionError),
    #[error("exact export surface belongs to session {surface}, not compile view {view}")]
    WrongSession { surface: SessionId, view: SessionId },
    #[error("facade target has no parent directory: {}", .0.display())]
    InvalidPath(PathBuf),
    #[error("facade I/O at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExactExportError {
    #[error("selected declaration exports have no compiler-certified original identities")]
    UncertifiedExports,
    #[error("session has no declaration plane")]
    NoDeclarationPlane,
    #[error("scope {0:?} is not live")]
    DeadScope(tidepool_codegen::scope::ScopeId),
    #[error("`{name}` is not an exported declaration in scope {scope:?}")]
    UnknownExport {
        scope: tidepool_codegen::scope::ScopeId,
        name: String,
    },
}

fn render_legacy_facade(identity: &FacadeIdentity, surface: &ExactExportSurface) -> String {
    let entries = surface
        .items
        .iter()
        .map(ExportItem::render_entry)
        .collect::<Vec<_>>();
    let mut source = format!("-- GENERATED — source-only export membrane.\n{{-# LANGUAGE NoImplicitPrelude, ExplicitNamespaces, TypeOperators #-}}\nmodule {}", identity.module_name());
    if entries.is_empty() {
        source.push_str(" () where\n");
        return source;
    }
    source.push_str(&format!("\n  ( {}\n  ) where\n", entries.join("\n  , ")));
    if let Some(module) = surface.source {
        source.push_str(&format!(
            "import {} ({})\n",
            module.module_name(),
            entries.join(", ")
        ));
    }
    source
}

impl super::PersistentSession {
    fn admit_facade_source(
        &self,
        scope: tidepool_codegen::scope::ScopeId,
    ) -> Result<Option<super::RuntimeSourceClaim>, super::SourceAdmissionError> {
        let owner = self
            .source_transaction
            .as_ref()
            .ok_or(super::SourceAdmissionError::MissingTransaction)?;
        let target = (scope != tidepool_codegen::scope::ScopeId::ROOT)
            .then(|| self.source_lease_in(scope))
            .transpose()?;
        owner.admit(target)?;
        Ok(None)
    }

    pub fn materialize_facade(
        &self,
        surface: &ExactExportSurface,
        view: &SessionCompileView,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<MaterializedFacade, ExactFacadeError> {
        if self.lib().session_id() != view.session() {
            return Err(ExactFacadeError::WrongSession {
                surface: self.lib().session_id(),
                view: view.session(),
            });
        }
        surface.materialize(
            view,
            &|| self.admit_facade_source(view.lexical_scope()),
            settlement,
        )
    }
    pub fn select_facade_exact_exports(
        &self,
        scope: tidepool_codegen::scope::ScopeId,
        facade: &MaterializedFacade,
        selected: &[ExportIdentity],
        includes: &[PathBuf],
        root: &Path,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<Option<MaterializedFacade>, ExactFacadeError> {
        facade.select_exact_exports(
            selected,
            includes,
            root,
            &|| self.admit_facade_source(scope),
            settlement,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_repr::Generation;

    fn source_action<T>(
        session: &mut super::super::PersistentSession,
        source: &Arc<super::super::RuntimeLexicalScopeLease>,
        action: impl FnOnce(
            &mut super::super::PersistentSession,
            &mut dyn FnMut(crate::CompilerTransactionClose),
        ) -> T,
    ) -> T {
        session
            .with_source_compiler_transaction(
                source.clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                action,
            )
            .unwrap()
            .into_outcome()
            .action
    }

    #[test]
    fn inherited_certified_facade_subset_preserves_original_owners_and_default_visibility() {
        use crate::session::{ModuleEnv, PersistentSession, SessionLib};
        use tidepool_toolchain::artifacts::{compile_invocation_in_context, CompileInvocation};
        use tidepool_toolchain::declaration_join::ExactModuleIdentity;

        let root = tempfile::tempdir().unwrap();
        let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
        let mut env = ModuleEnv::standalone_default();
        env.pragmas.push_str("\n{-# LANGUAGE TypeFamilies #-}");
        let mut lib = SessionLib::open(SessionId(9917), root.path(), env)
            .unwrap()
            .with_validation_include(effects.include_paths().to_vec());
        let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), crate::DEFAULT_NURSERY_SIZE);
        let expected_context = session
            .lib()
            .current_exact_context_in(ScopeId::ROOT)
            .expect("recovered root has its issued exact context");
        let scope = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        let scope_lease = session.adopt_initial_notebook_scope(scope).unwrap();
        let captured_context = session
            .lib()
            .current_exact_context_in(scope)
            .expect("detached capture retains the root's exact context");
        assert!(Arc::ptr_eq(&captured_context, &expected_context));
        assert_eq!(
            captured_context.semantic_sha256(),
            expected_context.semantic_sha256()
        );
        let declarations = include_str!("fixtures/inherited-facade-subset.hs")
            .trim()
            .split("\n\n")
            .collect::<Vec<_>>();
        let generation = source_action(&mut session, &scope_lease, |session, settlement| {
            session.define_scoped_in(scope, &declarations, settlement)
        })
        .unwrap();
        let full_surface = session
            .exact_exports_in(
                scope,
                &[
                    "Kept",
                    "KeptClass",
                    "KeptPayload",
                    "Hidden",
                    "HiddenClass",
                    "HiddenPayload",
                ],
            )
            .unwrap();
        let view = session.compile_view_in(scope).unwrap();
        let facade = source_action(&mut session, &scope_lease, |session, settlement| {
            session.materialize_facade(&full_surface, &view, settlement)
        })
        .unwrap();
        let inherited = facade.projection().unwrap();
        for class in ["KeptClass", "HiddenClass"] {
            assert!(inherited
                .receipt()
                .instances()
                .classes
                .iter()
                .any(|instance| {
                    instance.class.occurrence == class
                        && instance.class.module == SessionModule::lib(generation).module_name()
                }));
        }
        assert!(inherited.receipt().instances().families.len() >= 2);
        let all = inherited
            .receipt()
            .exports()
            .iter()
            .map(|export| export.head.clone())
            .collect::<Vec<_>>();
        let reused = source_action(&mut session, &scope_lease, |session, settlement| {
            session.select_facade_exact_exports(scope, &facade, &all, &[], root.path(), settlement)
        })
        .unwrap()
        .unwrap();
        assert!(Arc::ptr_eq(inherited, reused.projection().unwrap()));
        let selected = full_surface
            .declarations()
            .unwrap()
            .iter()
            .filter(|export| {
                matches!(
                    export.head.occurrence.as_str(),
                    "Kept" | "KeptClass" | "KeptPayload"
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 3);
        assert!(selected.iter().all(|export| {
            export.head.unit == "main"
                && export.head.module == SessionModule::lib(generation).module_name()
        }));
        let inherited_owner = ExactModuleIdentity {
            unit: inherited.receipt().reserved().unit.clone(),
            module: inherited.module_name().to_owned(),
        };
        let include = view.include_paths(effects.include_paths());
        let wanted = selected
            .iter()
            .map(|export| export.head.clone())
            .collect::<Vec<_>>();
        let narrowed_facade = source_action(&mut session, &scope_lease, |session, settlement| {
            session.select_facade_exact_exports(
                scope,
                &facade,
                &wanted,
                &include,
                root.path(),
                settlement,
            )
        })
        .unwrap()
        .unwrap();
        let narrowed = narrowed_facade.projection().unwrap();
        let mut wrong = wanted[0].clone();
        wrong.unit = "foreign".into();
        assert!(
            matches!(source_action(&mut session, &scope_lease, |session, settlement| {
            session.select_facade_exact_exports(scope, &facade, &[wrong.clone()], &include, root.path(), settlement)
        }), Err(ExactFacadeError::UnselectedExport(identity)) if identity == wrong)
        );
        assert_eq!(narrowed.receipt().exports(), selected);
        assert_eq!(
            narrowed.receipt().instances(),
            inherited.receipt().instances()
        );
        assert_eq!(
            narrowed.receipt().family_closure(),
            inherited.receipt().family_closure()
        );
        assert_ne!(narrowed.module_name(), inherited.module_name());
        let nested_root = narrowed
            .context()
            .lexical_graph()
            .iter()
            .find(|node| node.owner.module == narrowed.module_name())
            .unwrap();
        assert_eq!(nested_root.imports, vec![inherited_owner]);

        let preamble =
            crate::session::insert_preamble_imports(effects.preamble(), narrowed.module_name());
        let source = crate::session::assemble_expression_module(
            &preamble,
            "entry",
            "'[]",
            "pure (keptNumber (Kept (41 :: KeptPayload Bool)))",
            crate::session::ExpressionLift::Effectful,
        );
        let invocation = CompileInvocation {
            source: &source,
            targets: &[crate::session::PREPARED_SCAFFOLD_TARGET],
            include: &include,
            fallback_module_name: "InheritedFacadeConsumer",
        };
        let compiled = tidepool_testing::with_settlement(|settlement| {
            compile_invocation_in_context(
                &invocation,
                Arc::clone(narrowed.context()),
                |_, _, _| {},
                settlement,
            )
        })
        .unwrap();
        let value = crate::run_compiled_target(
            &compiled,
            crate::session::PREPARED_SCAFFOLD_TARGET,
            crate::DEFAULT_NURSERY_SIZE,
            &mut frunk::HNil,
            &(),
            |_| {},
        )
        .unwrap();
        assert_eq!(value.to_json(), serde_json::json!(42));

        let refused_source = source.replace(
            "keptNumber (Kept (41 :: KeptPayload Bool))",
            "hiddenNumber (Hidden (41 :: HiddenPayload Bool))",
        );
        let refused = tidepool_testing::with_settlement(|settlement| {
            compile_invocation_in_context(
                &CompileInvocation {
                    source: &refused_source,
                    ..invocation
                },
                Arc::clone(narrowed.context()),
                |_, _, _| {},
                settlement,
            )
        });
        assert!(
            matches!(&refused, Err(crate::CompileError::Diagnostics(_))),
            "{:?}",
            refused.err()
        );
        // Selection controls the default surface. Explicit access to an already
        // captured interface remains valid in the trusted source environment.
        let explicit_source =
            refused_source.replace(narrowed.module_name(), inherited.module_name());
        let explicit = tidepool_testing::with_settlement(|settlement| {
            compile_invocation_in_context(
                &CompileInvocation {
                    source: &explicit_source,
                    ..invocation
                },
                Arc::clone(narrowed.context()),
                |_, _, _| {},
                settlement,
            )
        })
        .unwrap();
        let value = crate::run_compiled_target(
            &explicit,
            crate::session::PREPARED_SCAFFOLD_TARGET,
            crate::DEFAULT_NURSERY_SIZE,
            &mut frunk::HNil,
            &(),
            |_| {},
        )
        .unwrap();
        assert_eq!(value.to_json(), serde_json::json!(141));

        let original_source = refused_source.replace(
            narrowed.module_name(),
            &SessionModule::lib(generation).module_name(),
        );
        let refused = tidepool_testing::with_settlement(|settlement| {
            compile_invocation_in_context(
                &CompileInvocation {
                    source: &original_source,
                    ..invocation
                },
                Arc::clone(narrowed.context()),
                |_, _, _| {},
                settlement,
            )
        });
        assert!(
            matches!(&refused, Err(crate::CompileError::InputRejected(_))),
            "{:?}",
            refused.err()
        );
        scope_lease
            .source_admission()
            .fence()
            .unwrap()
            .retire(
                crate::session::NotebookGenerationId::INITIAL
                    .next()
                    .unwrap(),
            )
            .unwrap();
        assert!(session.scope_tree().is_live(scope));
        source_action(&mut session, &scope_lease, |session, _| {
            let native_ends = std::cell::Cell::new(0);
            let mut observe = |_| native_ends.set(native_ends.get() + 1);
            assert!(matches!(
                session.materialize_facade(&full_surface, &view, &mut observe),
                Err(ExactFacadeError::Projection(
                    crate::session::SessionError::SourceAdmission(
                        crate::session::SourceAdmissionError::Closed {
                            phase: crate::session::SourceAdmissionPhase::Retired { .. },
                            ..
                        }
                    )
                ))
            ));
            assert!(matches!(
                session.select_facade_exact_exports(
                    scope,
                    &facade,
                    &wanted,
                    &include,
                    root.path(),
                    &mut observe
                ),
                Err(ExactFacadeError::Projection(
                    crate::session::SessionError::SourceAdmission(
                        crate::session::SourceAdmissionError::Closed {
                            phase: crate::session::SourceAdmissionPhase::Retired { .. },
                            ..
                        }
                    )
                ))
            ));
            let unchanged = session
                .select_facade_exact_exports(
                    scope,
                    &facade,
                    &all,
                    &include,
                    root.path(),
                    &mut observe,
                )
                .unwrap()
                .unwrap();
            assert!(Arc::ptr_eq(inherited, unchanged.projection().unwrap()));
            assert!(session
                .select_facade_exact_exports(
                    scope,
                    &facade,
                    &[],
                    &include,
                    root.path(),
                    &mut observe
                )
                .unwrap()
                .is_none());
            let mut foreign_view = view.clone();
            foreign_view.session = SessionId(999_917);
            assert!(matches!(
                session.materialize_facade(&full_surface, &foreign_view, &mut observe),
                Err(ExactFacadeError::WrongSession { .. })
            ));
            assert!(
                matches!(session.select_facade_exact_exports(scope, &facade, &[wrong.clone()], &include, root.path(), &mut observe), Err(ExactFacadeError::UnselectedExport(identity)) if identity == wrong)
            );
            assert_eq!(
                native_ends.get(),
                0,
                "retired refusals and pure reads must precede compiler effects"
            );
        });
    }

    #[test]
    fn materialized_facade_reexports_only_selected_exact_items() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lib = crate::session::SessionLib::open(
            SessionId(8),
            dir.path(),
            crate::session::ModuleEnv::standalone_default(),
        )
        .unwrap();
        let mut session =
            crate::session::PersistentSession::new(Some(lib), crate::DEFAULT_NURSERY_SIZE);
        let scope = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        let source = session.adopt_initial_notebook_scope(scope).unwrap();
        let view = session.compile_view_in(scope).unwrap();
        let surface = ExactExportSurface::legacy(
            SessionId(8),
            Some(SessionModule::lib(Generation(4))),
            vec![
                ExportItem::Value {
                    name: "review".into(),
                },
                ExportItem::Type {
                    name: "Finding".into(),
                    cons: vec!["Finding".into()],
                },
            ],
        );

        assert!(matches!(
            surface.declarations(),
            Err(ExactExportError::UncertifiedExports)
        ));
        let facade = source_action(&mut session, &source, |session, settlement| {
            session.materialize_facade(&surface, &view, settlement)
        })
        .expect("materialize facade");
        let (path, source) = facade.source_artifact().unwrap();
        assert!(source.contains("Finding(..)"));
        assert!(source.contains("review"));
        assert!(source.contains("import Tidepool.Session.Lib.G4 (Finding(..), review)"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}
