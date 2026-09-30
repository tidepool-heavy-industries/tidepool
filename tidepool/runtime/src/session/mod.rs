//! Session declaration accumulation.
//!
//! A [`SessionLib`] accumulates user declarations as **source text** across
//! turns. Each `define` turn:
//!   1. extracts the declaration's binder names **from GHC** (never a Rust-side
//!      Haskell parser) — see [`turn::run_turn`]'s `Decl` verdict;
//!   2. appends a [`render::DeclTurn`] to the ordered log and bumps the
//!      [`Generation`];
//!   3. regenerates the whole `Tidepool.Session.Lib.G<g>` module as a pure
//!      function of the log (selective re-export) and writes it atomically
//!      into the session include tree.
//!
//! Later turns see prior declarations by importing `Tidepool.Session.Lib.G<g>`
//! through the batch-compile pipeline ([`crate::compile_haskell`]) with the
//! session dir on the include path at highest precedence. The value and type
//! persistent stores are handled elsewhere; this module is a standalone, usable
//! declaration REPL on its own.

mod admission;
mod binding_table;
mod dialect;
#[cfg(test)]
#[path = "exact_recovery_acceptance_tests.rs"]
mod exact_recovery_acceptance_tests;
pub mod facade;
pub mod inspection;
pub mod kernel;
mod paired_publication;
pub mod persistent;
pub mod prepared;
mod publication;
mod recovery;
mod recovery_hydration;
pub use recovery_hydration::{RecoveryRunAuthority, RecoverySuccessorAuthority};
pub mod registry;
pub mod render;
pub mod resident;
pub mod supervisor;
pub mod turn;
pub mod view;
pub mod workbench;

pub use dialect::{
    declaration_pragmas, generated_support_pragmas, standalone_declaration_pragmas, EVAL_PRAGMAS,
};

pub use inspection::{
    run_inspections, ClassMethodInfo, ConstructorInfo, DeclarationInfo, FieldInfo, IdentifierInfo,
    IdentifierNamespace, IdentifierRef, InfoEntry, InspectionAvailability, InspectionQuery,
    InspectionRequest, InspectionResult, NameNamespace, NameQuery, NameScope, QueryError,
    ScopeProvenance, TypeExpression, TypeInfo, TypeMatch, TypeMatchQuality,
};
pub use kernel::{admit_checkout, Aged, SuspendableSession};

pub use persistent::{
    DeclarationPlaneCommit, MachineLease, MaterializationSetCommit, PersistentSession,
    ScopeRetirement, ValuePlaneCommit,
};

pub use admission::{
    CheckedDisplaySettlement, PrivateExecutionAdmission, RuntimeCellAdmission,
    RuntimeCheckedDisplayAdmission, RuntimeCheckedItemAdmission, RuntimeCheckedPrefix,
    RuntimeCheckedPrefixSnapshot, RuntimeLexicalScopeLease,
};
pub use paired_publication::FinalExecutionIntent;
pub use paired_publication::{
    AcceptedDeclarationPublication, CertifiedDeclarationPublication, DeclarationPublicationBase,
    DeclarationPublicationRejection, ExecutionPublication, RejectedDeclarationPublication,
};
pub use prepared::{
    CancelHandle, PreparedEngine, PreparedFailureKind, PreparedRuntimeError, PreparedSettlement,
    RealmId, SiteTypeEvidence,
};
pub use publication::{PublicationCancellation, PublicationDecision, PublicationPhase};
// Re-exported for callers that pass a value across two resident sessions'
// machines ([`resident::ResidentSession::export_custody`]/`import_parcel`)
// and any composition root that wires the sessions' shared image cache
// ([`resident::ResidentSession::set_image_registry`]).
pub use tidepool_codegen::prepared_program::{ImageRegistry, Parcel};

pub use recovery::{
    DeclarationRecoveryReport, LostDeclaration, RecoveryPublicOwner, ReplayedDeclaration,
};

pub use registry::{
    fresh_session_id, Checkout, CheckoutError, CheckoutReceipt, SessionRegistry, Slot, SlotKind,
};

/// The console-output buffer an effect handler writes into and a turn driver
/// drains when a turn yields. Abstracted so this crate stays below the server
/// crate that owns the concrete buffer (`tidepool_mcp::CapturedOutput`). The
/// buffer is `Clone` (Arc-backed) so the eval thread and the driver share one.
pub trait OutputSink: Clone + Send + 'static {
    /// Take all buffered lines, clearing the buffer.
    fn drain(&self) -> Vec<String>;
    /// Copy the buffered lines without clearing (a suspension keeps computing).
    fn snapshot(&self) -> Vec<String>;
}

pub use facade::{
    ExactExportError, ExactExportSurface, ExactFacadeError, FacadeIdentity, MaterializedFacade,
};

pub use supervisor::{GraceOutcome, TurnSupervisor};

pub use resident::{
    truncate_preview_at_line, HostBindingType, HostCarrier, HostPayload, PendingDisplayInstall,
    PendingPreparedInstall, PendingPreparedMode, ProgramProvenance, ProgramProvenanceError,
    ResidentContinuationEvent, ResidentDisplayBundle, ResidentError, ResidentHole, ResidentOutcome,
    ResidentResumeError, ResidentSession, RootCustody, SessionRunContext,
};

pub use view::{hide_preamble_exports, SessionCompileView, SourceImports};

pub use workbench::{
    classify_workbench_item, detect_hoisted_declaration_collision, escape_workbench_haskell_string,
    normalize_workbench_input, resident_cell_check_template, resident_workbench_templates,
    run_block_sequence, BlockExecution, BlockSequenceOutcome, CommittedBlock, MetaCommandLine,
    ParsedBlock, SourceOrderCollision, WorkSequence, WorkbenchBinding, WorkbenchBindingKind,
    WorkbenchCellItemKind, WorkbenchCellSourceItem, WorkbenchDiscovery, WorkbenchExecutionId,
    WorkbenchFailureLayer, WorkbenchForkBoundary, WorkbenchItem, WorkbenchItemReceipt,
    WorkbenchItemStatus, WorkbenchOperationDisposition, WorkbenchOperationId,
    WorkbenchOperationReceipt, WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus,
    WorkbenchTerminalTransfer,
};

pub use turn::{
    ambiguous_type_advice, assemble_bind_module, assemble_display_expression_module,
    assemble_expression_module, assemble_inspection_module, assemble_opaque_expression_module,
    check_cell, check_cell_with_fold, classify_block, constructor_advice,
    enable_no_monomorphism_restriction, insert_preamble_imports, place_turn_stmt,
    prepared_scaffold_binding, render_cell_compile_error, render_cell_compile_rejection,
    render_template, render_turn_compile_error, render_turn_compile_rejection,
    resume_import_targets, run_turn, run_turn_pinned, runtime_failure_advice,
    turn_user_code_line_range, turn_user_code_offset, with_resume_import, BoundBinder,
    CellAnalysisItem, CellAnalysisSourceItem, CellCheck, CellCheckFailure, CellCheckRequest,
    CellFoldTurn, CellSourceSpan, CheckedBinderPin, CheckedExpressionPlan, CompileRejection,
    CompiledTurn, DeclarationReceipt, DeclarationSource, ExpressionLift, ExpressionPresentation,
    HostBindingAuthority, LocatedImport, LocatedPragma, PragmaKind, SourcePrologue,
    TemplateSelector, TurnClassification, TurnCode, TurnFailure, TurnKind, TurnRequest, TurnResult,
    TurnTemplate, ValueTier, AMBIGUOUS_TYPE_ADVICE, CELL_PURE_DISPATCH_ADVICE,
    DECL_TEMPLATE_SOURCE, PREPARED_SCAFFOLD_TARGET,
};

/// Host-visible reentry state for one resident session.
///
/// This is an observation, not admission authority: the next operation must
/// still acquire the registry checkout and pass the machine's own reuse guard.
/// Source-only recovery is a separate declaration-environment transition and never
/// changes an unavailable machine into a reusable one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentSessionState {
    /// No machine has been bootstrapped yet; a first admitted entry may create it.
    Uninitialized,
    /// The existing machine can safely accept another explicitly submitted entry.
    Reusable,
    /// A checkout currently owns the machine, so reentry must not overtake it.
    Running,
    /// Heap or code integrity is uncertain, or the registry retained a terminal slot.
    Unavailable,
    /// The registry no longer owns this session incarnation.
    Gone,
}

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use tidepool_codegen::binding_table::SourceLeaseKey;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionId, SessionModule, SessionVarId};

pub use render::{
    subtract_import_list_names, DeclLog, DeclTurn, DeclarationKind, DeclarationRetraction,
    ExportItem, ModuleEnv, RenderedModule,
};

/// The stdlib include dir a candidate gen module needs at validation time, or
/// `None` when the caller's include path already carries one.
///
/// The candidate module imports stdlib sources (`Tidepool.Data.Text`, …), so a
/// stdlib root must be on the GHC search path or validation fails with a bare
/// "Could not find module" that reads like the user's declaration is wrong.
/// Production callers pass the server's stdlib dir through
/// [`SessionLib::with_validation_include`]; when they haven't, fall back to the
/// one locator ([`crate::toolchain::locate_stdlib`]).
///
/// A missing stdlib is a CONFIGURATION error and says so, instead of
/// surfacing as a downstream GHC scope error.
fn stdlib_include_for_validation(
    include: &[PathBuf],
) -> Result<Option<PathBuf>, crate::toolchain::ToolchainError> {
    if include.iter().any(|d| crate::toolchain::is_stdlib_root(d)) {
        return Ok(None);
    }
    crate::toolchain::locate_stdlib(&crate::toolchain::StdlibFallbacks::default())
        .map(|loc| Some(loc.dir))
}

/// Errors from the declaration-accumulation path.
#[derive(Debug)]
pub struct DeclarationValidationFailure {
    diagnostics: Vec<crate::diag::ExtractDiag>,
    anchor: String,
    line_offset: usize,
    source: String,
}

impl DeclarationValidationFailure {
    /// Render the structured GHC diagnostics for one frontend-owned location.
    #[must_use]
    pub fn render(&self, label: &str) -> String {
        self.render_with_offset(label, self.line_offset)
    }

    /// Render against the exact input unit a frontend submitted. Declaration
    /// modules may hoist trusted imports ahead of that unit, so the module's
    /// generic body offset can be intentionally unavailable. Matching the
    /// compiler-highlighted source line back to this one unit restores local
    /// coordinates without parsing Haskell or relabeling foreign diagnostics.
    #[must_use]
    pub fn render_for_input(&self, label: &str, input: &str) -> String {
        self.rejection_for_input(label, input).output
    }

    /// [`Self::render_for_input`]'s text plus the same GHC diagnostics as
    /// data, in the coordinates the text shows. The declaration path and the
    /// cell path converge on one receipt type, so they converge on one
    /// rejection shape too.
    #[must_use]
    pub fn rejection_for_input(
        &self,
        label: &str,
        input: &str,
    ) -> crate::session::CompileRejection {
        let matched = self
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.span.as_ref())
            .find_map(|span| {
                if !span.file.ends_with(&self.anchor) {
                    return None;
                }
                let generated_line = (span.start_line as usize).checked_sub(1)?;
                let generated = self.source.lines().nth(generated_line)?;
                input
                    .lines()
                    .position(|line| line == generated)
                    .and_then(|local| (span.start_line as usize).checked_sub(local + 1))
            });
        let offset = matched.unwrap_or(self.line_offset);
        let mut rejection = self.rejection_with_offset(label, offset);
        if matched.is_none() {
            // No GHC diagnostic span matched this anchor, or the generated line
            // it pointed at could not be found in the caller's own submitted
            // text. `self.line_offset` is a plausible but unverified fallback —
            // say so, rather than rendering a precise-looking line number that
            // may not correspond to anything the caller actually wrote.
            rejection.output.push_str(
                "\n\n(line numbers refer to the generated module; they could not be matched to \
                 your submitted text)",
            );
        }
        rejection
    }

    fn render_with_offset(&self, label: &str, line_offset: usize) -> String {
        self.rejection_with_offset(label, line_offset).output
    }

    fn rejection_with_offset(
        &self,
        label: &str,
        line_offset: usize,
    ) -> crate::session::CompileRejection {
        let rendered = crate::diag::render_diagnostics_structured(
            &self.diagnostics,
            &crate::diag::RenderOpts {
                anchor: &self.anchor,
                label,
                user_lines: None,
                line_offset,
                col_indent: 0,
                drop_foreign_gen_warnings_except: Some(&self.anchor),
                source: &self.source,
            },
        );
        crate::session::CompileRejection {
            output: rendered.text,
            diagnostics: rendered.diagnostics,
        }
    }
}

impl std::fmt::Display for DeclarationValidationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.render("<decl>").fmt(formatter)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    #[error(transparent)]
    InvalidBindingIdentity(#[from] tidepool_codegen::binding_table::BindingIdentityError),
    /// Filesystem I/O failure (creating the session root, writing/reading a
    /// gen module, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The compiler/extractor boundary rejected the declaration turn. Keep
    /// the original variant intact: callers must be able to distinguish real
    /// GHC diagnostics from missing, malformed, or unreadable artifacts.
    #[error(transparent)]
    Compile(#[from] crate::CompileError),
    /// The candidate gen module failed to type-check via GHC. The declaration
    /// log has been rolled back; the session remains usable.
    #[error("declaration type-check failed: {0}")]
    ValidationFailed(DeclarationValidationFailure),
    /// The toolchain itself is misconfigured — no extract, no stdlib, or a
    /// skewed extract/stdlib pair. Never caused by the user's declaration.
    #[error("toolchain: {0}")]
    Toolchain(#[from] crate::toolchain::ToolchainError),
    /// A scope-taking mutation (a mount, a scoped define/retract, a scope
    /// assignment) targeted a [`ScopeId`] that is not live — never minted, or
    /// already retired. A dead scope's lookup chain is empty, so anything
    /// written under it would be permanently unreachable and, for a mounted
    /// root, a permanent GC root by construction. Never the user's
    /// declaration — a stale or forged `ScopeId`.
    #[error("scope {0:?} is not live (never minted, or already retired)")]
    DeadScope(ScopeId),
    #[error("session has no persistent declaration library")]
    MissingDeclarationLibrary,
    #[error("staged public manifest belongs to a different session, actor, or manifest")]
    WrongPublicManifestTicket,
    #[error("private value replacement requires a certified lexical overlay")]
    UnsupportedPrivateValueReplacement,
    #[error("public binding promotion failed preflight: {0:?}")]
    InvalidPublicBindingPromotion(tidepool_codegen::binding_table::BindingPromotionError),
    #[error(
        "staged declaration no longer matches this session's live declaration or value environment"
    )]
    StaleStagedDeclaration,
    /// A durable source-recovery manifest was unreadable, from a future or
    /// retired schema, corrupt, or could not be published. This artifact is
    /// never silently reset: it is the authoritative record of which source
    /// may be replayed without repeating effects.
    #[error("declaration recovery manifest {}: {detail}", path.display())]
    RecoveryManifest { path: PathBuf, detail: String },
    /// The declaration and binding visibility swap has completed. Retry only
    /// [`SessionLib::confirm_recovery_durability`], never this declaration.
    #[error("declaration was published; recovery durability is unconfirmed at {}: {detail}", path.display())]
    PublishedDeclarationDurabilityUnconfirmed {
        commit: Box<DeclarationPlaneCommit>,
        path: PathBuf,
        detail: String,
    },
}

impl SessionError {
    #[must_use]
    pub fn published_declaration_commit(&self) -> Option<&DeclarationPlaneCommit> {
        match self {
            Self::PublishedDeclarationDurabilityUnconfirmed { commit, .. } => Some(commit),
            _ => None,
        }
    }
}

/// A published declaration must finish every owning layer's visibility swap
/// before its durability status crosses the public API boundary.
pub(crate) enum DeclarationAdmission<T> {
    Committed(T),
    PublishedDurabilityUnconfirmed {
        commit: T,
        path: PathBuf,
        detail: String,
    },
}

impl<T> DeclarationAdmission<T> {
    fn map_commit<U>(self, finalize: impl FnOnce(T) -> U) -> DeclarationAdmission<U> {
        match self {
            Self::Committed(commit) => DeclarationAdmission::Committed(finalize(commit)),
            Self::PublishedDurabilityUnconfirmed {
                commit,
                path,
                detail,
            } => DeclarationAdmission::PublishedDurabilityUnconfirmed {
                commit: finalize(commit),
                path,
                detail,
            },
        }
    }
}

impl DeclarationAdmission<DeclarationPlaneCommit> {
    fn into_result(self) -> Result<DeclarationPlaneCommit, SessionError> {
        match self {
            Self::Committed(commit) => Ok(commit),
            Self::PublishedDurabilityUnconfirmed {
                commit,
                path,
                detail,
            } => Err(SessionError::PublishedDeclarationDurabilityUnconfirmed {
                commit: Box::new(commit),
                path,
                detail,
            }),
        }
    }
}

/// Exact public lexical view captured under one resident-machine checkout.
/// The epoch is persistent-session owned and distinct from compiler allocation
/// generations; the declaration tip and full binding identities also permit
/// exact stale checks while concurrent publication is being introduced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicVisibilitySnapshot {
    pub scope: ScopeId,
    pub epoch: u64,
    pub declaration_tip: Generation,
    pub machine_incarnation: Option<SessionId>,
    pub bindings: Vec<(String, SessionVarId)>,
    pub source_instances: Vec<SourceLeaseKey>,
}

/// Immutable v2 manifest baseline captured under the owning session checkout.
/// Staging its replacement is fallible and may run after that checkout ends.
pub struct PublicManifestBase {
    admission_owner: Option<std::sync::Arc<admission::RuntimeAdmissionOwner>>,
    admission_owner_epoch: Option<u64>,
    manifest_owner: Option<std::sync::Arc<recovery_hydration::OwnedRecoveryManifest>>,
    session: SessionId,
    log_revision: u64,
    path: PathBuf,
    target: PublicPublicationBaseline,
    public_scope: ScopeId,
    private_scope: ScopeId,
    write_ids: Vec<SessionVarId>,
    source_keys: Vec<SourceLeaseKey>,
    expected_public: PublicVisibilitySnapshot,
    expected_private: PublicVisibilitySnapshot,
    final_bindings: Vec<(String, SessionVarId)>,
    final_source_instances: Vec<recovery::RecoveryPublicSourceInstance>,
}

enum PublicPublicationBaseline {
    Durable {
        owner: RecoveryPublicOwner,
        graph: recovery::RecoveryGraph,
    },
    Ephemeral,
}

enum StagedPublicationTarget {
    Durable {
        owner: RecoveryPublicOwner,
        base_checksum: String,
        base_high_water: Generation,
        staged: recovery::StagedRecoveryManifest,
    },
    Ephemeral,
}

/// A fully written, fsynced manifest candidate. Only the owning session may
/// compare its baseline and rename it while holding the machine checkout.
pub struct StagedPublicManifest {
    admission_owner: Option<std::sync::Arc<admission::RuntimeAdmissionOwner>>,
    admission_owner_epoch: Option<u64>,
    manifest_owner: Option<std::sync::Arc<recovery_hydration::OwnedRecoveryManifest>>,
    session: SessionId,
    log_revision: u64,
    path: PathBuf,
    target: StagedPublicationTarget,
    public_scope: ScopeId,
    private_scope: ScopeId,
    write_ids: Vec<SessionVarId>,
    source_keys: Vec<SourceLeaseKey>,
    expected_public: PublicVisibilitySnapshot,
    expected_private: PublicVisibilitySnapshot,
    declaration: Option<paired_publication::PreparedDeclarationPublication>,
}

/// A stale stage preserves the execution's intent; its caller can stage again
/// from the new public graph without repeating the execution's effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicManifestCommit {
    Stale,
    Cancelled,
    BeforeRename {
        detail: String,
    },
    Durable,
    /// The paired lexical/native swap is visible only in this runtime owner.
    Ephemeral,
    PublishedDurabilityUnconfirmed {
        detail: String,
    },
}

impl PublicManifestBase {
    /// Stage the exact binding and native source-instance winners captured by
    /// the session owner. This writes bytes without borrowing the machine.
    pub fn stage(self) -> Result<StagedPublicManifest, SessionError> {
        let target = match self.target {
            PublicPublicationBaseline::Ephemeral => StagedPublicationTarget::Ephemeral,
            PublicPublicationBaseline::Durable { owner, graph } => {
                let root = self
                    .path
                    .parent()
                    .ok_or_else(|| SessionError::RecoveryManifest {
                        path: self.path.clone(),
                        detail: "recovery manifest has no parent directory".into(),
                    })?;
                let surface = graph.public_surfaces.iter().find(|s| s.owner == owner);
                let epoch = surface.map_or(0, |s| s.epoch);
                let bindings = self
                    .final_bindings
                    .into_iter()
                    .map(|(name, id)| recovery::RecoveryPublicBinding {
                        name,
                        owner: recovery::RecoveryBindingId {
                            session: self.session.0,
                            variable: id.raw(),
                        },
                    })
                    .collect();
                let staged = recovery::stage_public_visibility_v2(
                    &self.path,
                    root,
                    &graph,
                    owner.clone(),
                    epoch,
                    bindings,
                    self.final_source_instances,
                    None,
                )
                .map_err(|error| SessionError::RecoveryManifest {
                    path: self.path.clone(),
                    detail: error.to_string(),
                })?;
                StagedPublicationTarget::Durable {
                    owner,
                    base_checksum: graph.checksum,
                    base_high_water: graph.high_water,
                    staged,
                }
            }
        };
        Ok(StagedPublicManifest {
            admission_owner: self.admission_owner,
            admission_owner_epoch: self.admission_owner_epoch,
            manifest_owner: self.manifest_owner,
            session: self.session,
            log_revision: self.log_revision,
            path: self.path,
            target,
            public_scope: self.public_scope,
            private_scope: self.private_scope,
            write_ids: self.write_ids,
            source_keys: self.source_keys,
            expected_public: self.expected_public,
            expected_private: self.expected_private,
            declaration: None,
        })
    }
}

fn authored_identity(
    identity: &tidepool_toolchain::declaration_join::ExportIdentity,
) -> Option<recovery::RecoverySymbolIdentity> {
    use tidepool_toolchain::declaration_join::ExportNamespace;

    let namespace = match identity.namespace {
        ExportNamespace::Value => "value",
        ExportNamespace::Type => "type",
        ExportNamespace::Constructor => "constructor",
        ExportNamespace::Field => "field",
    };
    Some(recovery::RecoverySymbolIdentity {
        unit: identity.unit.clone(),
        module: identity.module.clone(),
        namespace: namespace.into(),
        occurrence: identity.occurrence.clone(),
        record_parent: identity.record_parent.as_ref().map(|parent| {
            Box::new(recovery::RecoverySymbolIdentity {
                unit: identity.unit.clone(),
                module: identity.module.clone(),
                namespace: "type".into(),
                occurrence: parent.clone(),
                record_parent: None,
            })
        }),
    })
}

fn certified_recovery_export(
    export: &tidepool_toolchain::declaration_join::DeclarationExport,
) -> Option<recovery::RecoveryExport> {
    use tidepool_toolchain::declaration_join::DeclarationKind;

    Some(recovery::RecoveryExport {
        identity: authored_identity(&export.head)?,
        kind: match export.kind {
            DeclarationKind::Value => recovery::RecoveryExportKind::Value,
            DeclarationKind::Type => recovery::RecoveryExportKind::Type,
            DeclarationKind::Class => recovery::RecoveryExportKind::Class,
        },
        children: export
            .children
            .iter()
            .map(authored_identity)
            .collect::<Option<_>>()?,
    })
}

/// A resident session's declaration library. Owns the ordered decl log, the
/// monotonic generation, and the on-disk include tree.
pub struct SessionLib {
    id: SessionId,
    /// Local cache identity; replacing a library cannot reuse its old view.
    compile_view_identity: uuid::Uuid,
    /// Root of the session include tree: gen modules live at
    /// `<root>/Tidepool/Session/Lib/G<g>.hs`. Placed on the GHC include path at
    /// highest precedence so they shadow any same-named module.
    root: PathBuf,
    log: DeclLog,
    env: ModuleEnv,
    /// Extra `--include` dirs for decl binder-extraction + candidate validation,
    /// beyond `root` and the auto-derived stdlib `lib/`. When `env` imports
    /// modules that live outside the stdlib tree — notably the generated
    /// `Tidepool.Effects` (so a `session_def` helper can be `M`-typed and call
    /// effect verbs) — those dirs must be here or validation fails to resolve
    /// the import. Empty by default (the pure `standalone_default` surface needs
    /// only the stdlib). Set via [`with_validation_include`](Self::with_validation_include).
    extra_include: Vec<PathBuf>,
    /// Each scope's current persistent declaration environment tip: the generation a new turn in that
    /// scope chains its [`DeclTurn::parent`] from. `SessionLib`
    /// does not own the [`tidepool_codegen::scope::ScopeTree`] itself (that
    /// lives on `PersistentSession`) — it only keys this map by whatever
    /// [`ScopeId`] a caller passes to an `_in` method.
    ///
    /// ROOT is seeded at [`Self::open`] and every other scope is seeded from
    /// its PARENT's tip at mint time ([`Self::seed_scope`], called by
    /// `PersistentSession::mint_scope`, which owns the tree). That seeding is
    /// load-bearing, not bookkeeping: a scope absent from this map resolves to
    /// `Generation(0)` — the empty environment — and NEVER to the log's global
    /// tip. Falling back to the global tip would hand a scope whatever turn
    /// happened to be pushed last in ANY scope, so a sibling defining between
    /// a scope's mint and its first use would leak into it, and a child
    /// defining before its parent's next turn would leak UPWARD. Both would
    /// violate the rule that a scope's lookups are visible only to itself and
    /// its descendants, never sideways or upward; the regression is pinned by
    /// `session_decl_scope_tree.rs`.
    tips: HashMap<ScopeId, Generation>,
    /// Optional durable root-declaration manifest. It records source and
    /// retractions only; live values and scoped child state never enter it.
    recovery_manifest_path: Option<PathBuf>,
    recovery_turns: Vec<recovery::RecoveryTurn>,
    /// Persistence happens after a declaration has semantically committed.
    /// A write failure is observable here rather than returned as if retrying
    /// the declaration were safe.
    recovery_manifest_warning: Option<String>,
    recovery_report: Option<DeclarationRecoveryReport>,
    durable_graph: Option<DurableDeclarationGraph>,
    /// Actor incarnations in one session have distinct process-local lexical
    /// scopes, including inherited-context children.
    durable_public_scopes: BTreeMap<RecoveryPublicOwner, ScopeId>,
    #[cfg(test)]
    fail_recovery_durability_once: bool,
}

struct DurableDeclarationGraph {
    owner: Option<std::sync::Arc<recovery_hydration::OwnedRecoveryManifest>>,
    path: PathBuf,
    graph: recovery::RecoveryGraph,
    /// A visible rename whose directory sync still needs confirmation. The
    /// high-water identity is already burned, even while success is withheld.
    unconfirmed: Option<tidepool_atomic_write::PublishedWrite>,
}

/// Exact declaration module prepared for cell compilation without advancing
/// the live declaration log or scope tip.
#[derive(Clone, Debug)]
pub struct StagedDeclaration {
    generation: Generation,
    reserved: bool,
    persistence: DeclarationPersistence,
    module: SessionModule,
    receipt: DeclarationReceipt,
    exact_context:
        Option<std::sync::Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
    session_id: SessionId,
    root: PathBuf,
    scope: ScopeId,
    base_generation: Generation,
    base_tip: Generation,
    turn: DeclTurn,
    import_modules: Vec<String>,
    inject_modules: Vec<String>,
    visible_values: Vec<(SessionVarId, String)>,
    /// The exact bytes GHC validated. Installing this candidate ([`SessionLib::
    /// adopt_staged_batch_with_receipt_and_vals_in`]) writes precisely these
    /// bytes into the shared session root — never a re-render — so what lands
    /// in the include tree is provably what GHC already checked, whether that
    /// validation ran under this session's checkout (the single-checkout path)
    /// or off it, against a private candidate directory (a split cell
    /// preparation).
    rendered: RenderedModule,
    certified_authored: Option<tidepool_toolchain::declaration_join::CertifiedAuthoredDeclaration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeclarationPersistence {
    Durable,
    Ephemeral,
}

impl StagedDeclaration {
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }
    #[must_use]
    pub fn module(&self) -> SessionModule {
        self.module
    }
    #[must_use]
    pub fn receipt(&self) -> &DeclarationReceipt {
        &self.receipt
    }
    #[must_use]
    pub fn scope(&self) -> ScopeId {
        self.scope
    }
    #[must_use]
    pub fn items(&self) -> &[ExportItem] {
        &self.turn.items
    }
    /// Attach the live-value environment a candidate was rendered against, so
    /// [`SessionLib::adopt_staged_batch_with_receipt_and_vals_in`] can detect
    /// a binding change since. A split cell preparation calls this itself,
    /// off-checkout, after [`validate_declaration_candidate`] returns —
    /// [`PersistentSession::stage_declarations_in`] does the equivalent for
    /// the single-checkout path.
    #[must_use]
    pub fn with_visible_values(mut self, values: Vec<(SessionVarId, String)>) -> Self {
        self.visible_values = values;
        self
    }
}

/// A pure render of the next declaration candidate for `scope`: the exact
/// generation, module identity, rendered `.hs` text, and the log/binding
/// baseline it was rendered against — everything [`validate_declaration_candidate`]
/// needs to write and GHC-validate the candidate **off** the machine
/// checkout, and everything [`SessionLib::adopt_staged_batch_with_receipt_and_vals_in`]
/// needs to detect that baseline moving before installing it. Building this
/// touches no disk and runs no compiler; it is a pure function of
/// [`SessionLib`]'s in-memory log, so it is cheap enough to compute under a
/// checkout that is released immediately after.
#[derive(Clone, Debug)]
pub struct DeclarationCandidateRender {
    exact_context:
        Option<std::sync::Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
    session_id: SessionId,
    root: PathBuf,
    extra_include: Vec<PathBuf>,
    pragmas: String,
    scope: ScopeId,
    base_generation: Generation,
    base_tip: Generation,
    generation: Generation,
    reserved: bool,
    rendered: RenderedModule,
    turn: DeclTurn,
    receipt: DeclarationReceipt,
    import_modules: Vec<String>,
    inject_modules: Vec<String>,
}

impl DeclarationCandidateRender {
    /// Use the actor's source layer when validating a declaration import.
    /// The layer precedes shared roots, as it does for whole-cell checks.
    #[must_use]
    pub fn with_source_layer(mut self, roots: &[PathBuf]) -> Self {
        let mut include = roots.to_vec();
        include.extend(self.extra_include);
        self.extra_include = include;
        self
    }

    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }
    #[must_use]
    pub fn module(&self) -> SessionModule {
        self.rendered.module
    }
    #[must_use]
    pub fn scope(&self) -> ScopeId {
        self.scope
    }
}

impl SessionLib {
    #[must_use]
    pub fn next_module(&self) -> SessionModule {
        SessionModule::lib(self.log.generation().next())
    }

    /// Open a session rooted at `root` (created if absent). `env` controls the
    /// generated modules' pragma/import surface; pass
    /// [`ModuleEnv::standalone_default`] for the pure Lane-A surface.
    pub fn open(
        id: SessionId,
        root: impl Into<PathBuf>,
        env: ModuleEnv,
    ) -> Result<SessionLib, SessionError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        // Nothing but `ResidentSession::mount_carrier_in`'s hand-written
        // stub writes a `.hs` source under `Tidepool/Session/Val` (an
        // ordinary compiled bind is injected from its `.hi`, never a source
        // file at this path). A fresh incarnation always starts with
        // `stub_generations` empty and `val_gen` back at zero, so any file
        // already there belongs to a dead incarnation and its generation
        // number can be REISSUED for a real, `.hi`-backed bind -- which
        // would then coexist with the stale stub source on the include
        // path, an ambiguous or wrongly-resolved module for GHC's
        // downsweep. Sweeping every stub source on open (rather than
        // tracking which files are stale) is what guarantees none of them
        // can be found by a later turn, matching a resumed session's reset
        // bookkeeping exactly.
        if let Some(val_dir) = SessionModule::val(Generation(0))
            .relative_hs_path()
            .rsplit_once('/')
            .map(|(dir, _)| root.join(dir))
        {
            match std::fs::read_dir(&val_dir) {
                Ok(entries) => {
                    for entry in entries {
                        let path = entry?.path();
                        if path.extension().is_some_and(|extension| extension == "hs") {
                            std::fs::remove_file(&path)?;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(SessionLib {
            id,
            compile_view_identity: uuid::Uuid::new_v4(),
            root,
            log: DeclLog::new(),
            env,
            extra_include: Vec::new(),
            // ROOT starts at the empty environment. Seeding it explicitly is
            // what keeps `scope_tip`'s miss case meaning "empty" rather than
            // "whatever was pushed last anywhere" — see the field docs.
            tips: HashMap::from([(ScopeId::ROOT, Generation(0))]),
            recovery_manifest_path: None,
            recovery_turns: Vec::new(),
            recovery_manifest_warning: None,
            recovery_report: None,
            durable_graph: None,
            durable_public_scopes: BTreeMap::new(),
            #[cfg(test)]
            fail_recovery_durability_once: false,
        })
    }

    /// Bind one actor incarnation to its public lexical scope in this process.
    /// Another actor may share the manifest but cannot claim this scope.
    fn bind_durable_public_scope(
        &mut self,
        owner: RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<(), SessionError> {
        if self.durable_graph.is_none()
            || self
                .durable_public_scopes
                .get(&owner)
                .is_some_and(|bound| *bound != scope)
            || self
                .durable_public_scopes
                .iter()
                .any(|(bound_owner, bound_scope)| bound_owner != &owner && *bound_scope == scope)
        {
            return Err(SessionError::RecoveryManifest {
                path: self
                    .durable_graph
                    .as_ref()
                    .map_or_else(|| self.root.clone(), |state| state.path.clone()),
                detail: "declaration manifest has a conflicting public scope owner".into(),
            });
        }
        self.durable_public_scopes.insert(owner, scope);
        Ok(())
    }

    fn snapshot_public_manifest(
        &self,
        owner: RecoveryPublicOwner,
        public_scope: ScopeId,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
        expected_public: PublicVisibilitySnapshot,
        expected_private: PublicVisibilitySnapshot,
        final_bindings: Vec<(String, SessionVarId)>,
        final_source_instances: Vec<recovery::RecoveryPublicSourceInstance>,
    ) -> Result<PublicManifestBase, SessionError> {
        let state = self
            .durable_graph
            .as_ref()
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: self.root.clone(),
                detail: "v2 recovery graph is not attached".into(),
            })?;
        if state.unconfirmed.is_some()
            || self.durable_public_scopes.get(&owner) != Some(&public_scope)
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        Ok(PublicManifestBase {
            admission_owner: None,
            admission_owner_epoch: None,
            manifest_owner: state.owner.clone(),
            session: self.id,
            log_revision: self
                .log
                .publication_revision()
                .ok_or(SessionError::StaleStagedDeclaration)?,
            path: state.path.clone(),
            target: PublicPublicationBaseline::Durable {
                owner,
                graph: state.graph.clone(),
            },
            public_scope,
            private_scope,
            write_ids,
            source_keys,
            expected_public,
            expected_private,
            final_bindings,
            final_source_instances,
        })
    }

    fn public_manifest_ticket_is_current(
        &self,
        ticket: &StagedPublicManifest,
    ) -> Result<bool, SessionError> {
        if self.log.publication_revision() != Some(ticket.log_revision) {
            return Ok(false);
        }
        let StagedPublicationTarget::Durable {
            owner,
            base_checksum,
            base_high_water,
            ..
        } = &ticket.target
        else {
            return Ok(ticket.session == self.id
                && !self
                    .durable_public_scopes
                    .values()
                    .any(|scope| *scope == ticket.public_scope));
        };
        let live = self
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        if !recovery_hydration::same_manifest_owner(&live.owner, &ticket.manifest_owner) {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        self.public_manifest_baseline_is_current(
            ticket.session,
            &ticket.path,
            owner,
            ticket.public_scope,
            base_checksum,
            *base_high_water,
        )
    }

    fn publication_baseline_is_current(
        &self,
        base: &PublicManifestBase,
    ) -> Result<bool, SessionError> {
        if self.log.publication_revision() != Some(base.log_revision) {
            return Ok(false);
        }
        match &base.target {
            PublicPublicationBaseline::Durable { owner, graph } => self
                .public_manifest_baseline_is_current(
                    base.session,
                    &base.path,
                    owner,
                    base.public_scope,
                    &graph.checksum,
                    graph.high_water,
                ),
            PublicPublicationBaseline::Ephemeral => Ok(base.session == self.id
                && !self
                    .durable_public_scopes
                    .values()
                    .any(|scope| *scope == base.public_scope)),
        }
    }

    fn public_manifest_baseline_is_current(
        &self,
        session: SessionId,
        path: &Path,
        owner: &RecoveryPublicOwner,
        public_scope: ScopeId,
        checksum: &str,
        high_water: Generation,
    ) -> Result<bool, SessionError> {
        let state = self
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        if session != self.id
            || path != state.path
            || self.durable_public_scopes.get(owner) != Some(&public_scope)
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        Ok(!(state.unconfirmed.is_some()
            || state.graph.checksum != checksum
            || state.graph.high_water != high_water))
    }

    fn publish_staged_public_manifest_unchecked(
        &mut self,
        mut ticket: StagedPublicManifest,
    ) -> PublicManifestCommit {
        let declaration = ticket.declaration.take();
        let public_scope = ticket.public_scope;
        let StagedPublicationTarget::Durable { staged, .. } = ticket.target else {
            if let Some(declaration) = declaration {
                self.commit_prepared_declaration(public_scope, declaration);
            }
            return PublicManifestCommit::Ephemeral;
        };
        let outcome = self.publish_recovery_manifest(staged);
        let state = self
            .durable_graph
            .as_mut()
            .expect("ticket preflight found manifest");
        let committed = match outcome {
            recovery::RecoveryPublishOutcome::BeforeRename { detail, .. } => {
                PublicManifestCommit::BeforeRename { detail }
            }
            recovery::RecoveryPublishOutcome::Durable { graph, .. } => {
                state.graph = graph;
                PublicManifestCommit::Durable
            }
            recovery::RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                graph,
                publication,
                detail,
            } => {
                state.graph = graph;
                state.unconfirmed = Some(publication);
                PublicManifestCommit::PublishedDurabilityUnconfirmed { detail }
            }
        };
        if matches!(
            committed,
            PublicManifestCommit::Durable
                | PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
        ) {
            if let Some(declaration) = declaration {
                self.commit_prepared_declaration(public_scope, declaration);
            }
        }
        committed
    }

    /// Burn a unique authored or Join module identity before exposing it to the compiler.
    /// A failure before rename leaves the allocator untouched. After rename,
    /// the identity stays burned even if directory durability is uncertain.
    pub fn reserve_declaration_generation_durable(&mut self) -> Result<Generation, SessionError> {
        let state = self
            .durable_graph
            .as_mut()
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: self.root.clone(),
                detail: "v2 recovery graph is not attached".into(),
            })?;
        if state.unconfirmed.is_some() {
            return Err(SessionError::RecoveryManifest {
                path: state.path.clone(),
                detail: "previous recovery publication still needs durability confirmation".into(),
            });
        }
        let next = Generation(state.graph.high_water.0.checked_add(1).ok_or_else(|| {
            SessionError::RecoveryManifest {
                path: state.path.clone(),
                detail: "declaration generation space exhausted".into(),
            }
        })?);
        if self.log.generation() != state.graph.high_water {
            return Err(SessionError::RecoveryManifest {
                path: state.path.clone(),
                detail: "declaration allocator and durable high-water diverged".into(),
            });
        }
        let staged =
            recovery::stage_high_water_v2(&state.path, &state.graph, next).map_err(|error| {
                SessionError::RecoveryManifest {
                    path: state.path.clone(),
                    detail: error.to_string(),
                }
            })?;
        let outcome = staged.publish();
        match outcome {
            recovery::RecoveryPublishOutcome::BeforeRename { detail, .. } => {
                Err(SessionError::RecoveryManifest {
                    path: state.path.clone(),
                    detail,
                })
            }
            recovery::RecoveryPublishOutcome::Durable { graph, .. } => {
                state.graph = graph;
                assert_eq!(self.log.reserve(), next);
                Ok(next)
            }
            recovery::RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                graph,
                publication,
                detail,
            } => {
                state.graph = graph;
                assert_eq!(self.log.reserve(), next);
                state.unconfirmed = Some(publication);
                Err(SessionError::RecoveryManifest {
                    path: state.path.clone(),
                    detail,
                })
            }
        }
    }

    /// Reserve a Join through the same durable allocator used by authored
    /// declarations. A Join consumes its reserved slot only after publication.
    pub fn reserve_join_generation_durable(&mut self) -> Result<Generation, SessionError> {
        self.reserve_declaration_generation_durable()
    }

    /// Retry only the parent-directory sync for an already visible manifest.
    /// Never resubmit the declaration or reuse its reserved identity.
    pub fn confirm_recovery_durability(&mut self) -> Result<(), SessionError> {
        let state = self
            .durable_graph
            .as_mut()
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: self.root.clone(),
                detail: "v2 recovery graph is not attached".into(),
            })?;
        if let Some(publication) = &state.unconfirmed {
            publication
                .confirm_durability()
                .map_err(|error| SessionError::RecoveryManifest {
                    path: state.path.clone(),
                    detail: error.to_string(),
                })?;
            state.unconfirmed = None;
        }
        Ok(())
    }

    /// Reconstruct replayable root declarations from `path`, then attach that
    /// path for future source commits. Replay uses the ordinary GHC admission
    /// path and never repeats effects. Turns that depended on resident values,
    /// and declarations that no longer type-check without such a turn, are
    /// reported as lost rather than fabricated.
    pub fn attach_recovery_manifest(
        &mut self,
        path: impl Into<PathBuf>,
    ) -> Result<DeclarationRecoveryReport, SessionError> {
        let path = path.into();
        let manifest = recovery::read(&path)?;
        let mut report = DeclarationRecoveryReport {
            source_session: manifest.as_ref().map(|manifest| manifest.source_session),
            successor_session: self.id.0,
            replayed: Vec::new(),
            lost: Vec::new(),
        };

        if let Some(manifest) = manifest {
            if self.log.generation() != Generation(0) {
                return Err(SessionError::RecoveryManifest {
                    path,
                    detail: "recovery must attach before declarations are admitted".into(),
                });
            }
            let migration = recovery::migrate_v1(&manifest);
            if migration.high_water.0 == u64::MAX {
                return Err(SessionError::RecoveryManifest {
                    path,
                    detail: "legacy declaration generation space exhausted".into(),
                });
            }
            self.log.restore_high_water(migration.high_water);
            report.lost = migration.lost;
            let mut replay_failed = false;
            for turn in migration.replay_safe {
                if replay_failed {
                    report.lost.push(LostDeclaration {
                        origin_session: turn.origin_session,
                        source_generation: turn.generation,
                        source_hash: turn.source_hash,
                        sources: turn.sources,
                        reason: "an earlier legacy declaration could not be recovered".into(),
                    });
                    continue;
                }
                let sources: Vec<_> = turn.sources.iter().map(String::as_str).collect();
                match self.define_batch(&sources) {
                    Ok(generation) => {
                        report.replayed.push(ReplayedDeclaration {
                            origin_session: turn.origin_session,
                            source_generation: turn.generation,
                            successor_generation: generation.0,
                            source_hash: turn.source_hash,
                        });
                        self.recovery_turns.push(recovery::RecoveryTurn::new(
                            self.id.0,
                            generation.0,
                            turn.sources,
                            Vec::new(),
                            true,
                        ));
                    }
                    Err(error) => {
                        replay_failed = true;
                        report.lost.push(LostDeclaration {
                            origin_session: turn.origin_session,
                            source_generation: turn.generation,
                            source_hash: turn.source_hash,
                            sources: turn.sources,
                            reason: error.to_string(),
                        });
                    }
                }
            }
        }
        self.recovery_manifest_path = Some(path);
        self.recovery_report = Some(report.clone());
        Ok(report)
    }

    /// Last failure to publish source recovery state after a successful
    /// semantic declaration commit. Retrying the original declaration would
    /// be wrong; callers can surface this health fact and keep the session
    /// usable.
    #[must_use]
    pub fn recovery_manifest_warning(&self) -> Option<&str> {
        self.recovery_manifest_warning.as_deref()
    }

    /// Recovery facts for the current session incarnation, when a durable
    /// manifest has been attached. These are metadata about source replay,
    /// never reconstructed Haskell values.
    #[must_use]
    pub fn declaration_recovery_report(&self) -> Option<&DeclarationRecoveryReport> {
        self.recovery_report.as_ref()
    }

    /// Add include dirs used when extracting binders and validating candidate
    /// gen modules (e.g. the `Tidepool.Effects` dir + stdlib `lib/` when `env`
    /// is the full-eval [`session_decl_module_env`](crate) surface). Without
    /// these, a decl importing `Tidepool.Effects` fails validation with
    /// "Could not find module `Tidepool.Effects'". Chainable on `open`.
    #[must_use]
    pub fn with_validation_include(mut self, dirs: Vec<PathBuf>) -> Self {
        self.extra_include = dirs;
        self
    }

    /// Stable identity of the session whose exact gen modules this library
    /// renders. A [`SessionModule`] is only unique together with this id.
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// The include directory to place on the GHC search path (highest precedence).
    #[must_use]
    pub fn include_dir(&self) -> &Path {
        &self.root
    }

    /// The current generation (`Generation(0)` until the first `define`).
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.log.generation()
    }

    /// The generation `scope`'s persistent declaration environment currently stands at — the
    /// [`DeclTurn::parent`] a new turn in `scope` chains from. `Generation(0)`
    /// means `scope` has never had a turn pushed (nor inherited one): the same
    /// meaning as an empty session at ROOT. There is NO fallback to the log's
    /// global tip — see the `tips` field docs for why that is load-bearing, and
    /// why a scope minted outside `PersistentSession::mint_scope` therefore
    /// starts empty rather than inheriting anything.
    #[must_use]
    pub fn scope_tip(&self, scope: ScopeId) -> Generation {
        self.tips.get(&scope).copied().unwrap_or(Generation(0))
    }

    /// Seed `scope`'s decl tip with the generation it INHERITS — its parent's
    /// tip at mint time. Called by `PersistentSession::mint_scope`, which owns
    /// the [`ScopeTree`](tidepool_codegen::scope::ScopeTree) and is therefore
    /// the only place that knows a scope's parent.
    ///
    /// A no-op once `scope` has a tip of its own: seeding must never rewind a
    /// scope that has already pushed turns, and re-seeding a live scope would
    /// silently drop its declarations.
    pub fn seed_scope(&mut self, scope: ScopeId, inherited: Generation) {
        self.tips.entry(scope).or_insert(inherited);
    }

    /// The current session-library module, or `None` before any declaration.
    /// `current_module() == current_module_in(ScopeId::ROOT)`.
    #[must_use]
    pub fn current_module(&self) -> Option<SessionModule> {
        self.current_module_in(ScopeId::ROOT)
    }

    /// [`Self::current_module`], but for `scope`'s own tip.
    #[must_use]
    pub fn current_module_in(&self, scope: ScopeId) -> Option<SessionModule> {
        let g = self.scope_tip(scope);
        (g.0 > 0).then(|| SessionModule::lib(g))
    }

    /// Persistent imports visible at `scope`, derived from the same ordered
    /// declaration chain that is transferred during session recovery.
    #[must_use]
    pub fn workbench_imports_in(&self, scope: ScopeId) -> SourceImports {
        let mut imports = SourceImports::new();
        for generation in self.log.chain_from_root(self.scope_tip(scope)) {
            let turn = self.log.turn(generation).expect("scope tip is committed");
            imports.extend(&turn.workbench_imports);
        }
        imports
    }

    /// The `import Tidepool.Session.Lib.G<g>` line a turn should prepend to see
    /// the accumulated declarations, or `None` if the session is empty.
    /// `import_line() == import_line_in(ScopeId::ROOT)`.
    #[must_use]
    pub fn import_line(&self) -> Option<String> {
        self.import_line_in(ScopeId::ROOT)
    }

    /// [`Self::import_line`], but for `scope`'s own tip.
    #[must_use]
    pub fn import_line_in(&self, scope: ScopeId) -> Option<String> {
        self.current_module_in(scope)
            .map(|m| format!("import {}", m.module_name()))
    }

    /// The replayable decl half of a `:program` notebook repaint: turn source
    /// texts in log order, with fully-superseded turns dropped so a name
    /// redefined across separate turns emits only its LATEST definition
    /// instead of overlapping clauses GHC would reject. See
    /// [`DeclLog::replayable_sources`] for the exact latest-wins rule (it mirrors
    /// the eval-time module scoping).
    #[must_use]
    pub fn decl_sources(&self) -> Vec<&str> {
        self.log.replayable_sources()
    }

    /// Names of every value/function binder introduced across all declaration
    /// turns (all generations, not just the current one). Used by the eval
    /// assembler to hide session-defined names from the Prelude import so a
    /// user function named `over`/`view`/etc. resolves unambiguously to the
    /// session decl rather than the Prelude re-export.
    /// `decl_value_names() == decl_value_names_in(ScopeId::ROOT)`.
    #[must_use]
    pub fn decl_value_names(&self) -> Vec<&str> {
        self.decl_value_names_in(ScopeId::ROOT)
    }

    /// [`Self::decl_value_names`], but walking only `scope`'s own parent
    /// chain — a sibling scope's same-named value never shadows this one.
    #[must_use]
    pub fn decl_value_names_in(&self, scope: ScopeId) -> Vec<&str> {
        // Latest-wins with retraction: a name removed by a later retraction turn
        // (its binding migrated to the persistent binding store) is no longer a persistent declaration environment
        // value, so it drops out.
        let mut live: Vec<&str> = Vec::new();
        for g in self.log.chain_from_root(self.scope_tip(scope)) {
            let turn = self.log.turn(g).expect("scope tip is committed");
            for r in &turn.retracts {
                live.retain(|n| {
                    !r.selects(
                        tidepool_toolchain::declaration_join::ExportNamespace::Value,
                        n,
                    )
                });
            }
            for item in &turn.items {
                if let ExportItem::Value { name } = item {
                    live.retain(|n| *n != name.as_str());
                    live.push(name.as_str());
                }
            }
        }
        live
    }

    /// The currently in-scope declaration heads paired with the generation of
    /// their latest defining turn — the persistent declaration environment half of the live
    /// `tidepool://session/bindings` resource snapshot. Latest-wins across turns.
    /// `current_decl_heads() == current_decl_heads_in(ScopeId::ROOT)`.
    #[must_use]
    pub fn current_decl_heads(&self) -> Vec<(String, u64)> {
        self.current_decl_heads_in(ScopeId::ROOT)
    }

    /// [`Self::current_decl_heads`], but for `scope`'s own parent chain.
    #[must_use]
    pub fn current_decl_heads_in(&self, scope: ScopeId) -> Vec<(String, u64)> {
        self.log.current_heads_at(self.scope_tip(scope))
    }

    /// Current GHC declaration exports with their exact defining generations.
    /// Unlike [`Self::current_decl_heads`], this preserves value/type/class kind
    /// and constructor/method metadata.
    #[must_use]
    pub fn current_declarations(&self) -> Vec<(ExportItem, u64)> {
        self.current_declarations_in(ScopeId::ROOT)
    }

    /// Scoped [`Self::current_declarations`].
    #[must_use]
    pub fn current_declarations_in(&self, scope: ScopeId) -> Vec<(ExportItem, u64)> {
        self.log.current_items_at(self.scope_tip(scope))
    }

    /// A declaration value's compiler-rendered type, retained with its exact
    /// defining generation.
    #[must_use]
    pub fn declaration_value_type(&self, generation: u64, name: &str) -> Option<&str> {
        self.log.value_type_at(Generation(generation), name)
    }

    /// Cache one compatibility inspection batch against the exact visible
    /// generations it described. Stale entries are ignored.
    pub fn retain_declaration_value_types_in(
        &mut self,
        scope: ScopeId,
        types: &[(String, u64, String)],
    ) {
        let tip = self.scope_tip(scope);
        self.log.retain_value_types_at(tip, types);
    }

    /// Select a model-visible export membrane from the exact declaration
    /// module currently visible in `scope`. Names are declaration heads; a
    /// selected data type or class carries all GHC-reported constructors or
    /// methods through [`ExportItem`].
    pub fn exact_exports_in(
        &self,
        scope: ScopeId,
        heads: &[&str],
    ) -> Result<ExactExportSurface, ExactExportError> {
        let available = self.log.exports_at(self.scope_tip(scope));
        let mut selected = Vec::new();
        for head in heads
            .iter()
            .map(|head| head.trim())
            .filter(|head| !head.is_empty())
        {
            let item = available
                .iter()
                .find(|item| item.head_name() == head)
                .cloned()
                .ok_or_else(|| ExactExportError::UnknownExport {
                    scope,
                    name: head.to_string(),
                })?;
            if !selected
                .iter()
                .any(|prior: &ExportItem| prior.head_name() == item.head_name())
            {
                selected.push(item);
            }
        }
        Ok(ExactExportSurface::new(
            self.id,
            self.current_module_in(scope),
            selected,
        ))
    }

    /// Append a declaration turn. Extracts binder names from GHC, regenerates the
    /// gen-versioned module, writes it atomically, validates it type-checks via GHC,
    /// and returns the new generation.
    ///
    /// `decl_text` may contain several top-level declarations; their binders are
    /// classified together as this turn's introduced names.
    ///
    /// Empty / whitespace-only `decl_text` is a **no-op**: returns the current
    /// generation without bumping it.
    ///
    /// Syntactically-invalid declarations are rejected here as structured GHC
    /// diagnostics (`SessionError::Compile(CompileError::Diagnostics(_))`) and
    /// the log is left untouched.
    ///
    /// Declarations that parse but fail to type-check are also rejected: the
    /// candidate gen module is compiled via a thin wrapper; on failure the log is
    /// rolled back and the gen module file deleted so subsequent turns cannot pick
    /// up a stale poisoned module (`SessionError::ValidationFailed`). This covers
    /// ALL declaration kinds — `data`, `class`, `instance`, `type`, and values.
    pub fn define(&mut self, decl_text: &str) -> Result<Generation, SessionError> {
        self.define_batch(&[decl_text])
    }

    /// [`Self::define`], but against `scope`'s own persistent declaration environment rather than
    /// ROOT's — see [`Self::define_batch_with_vals_in`].
    pub fn define_scoped_in(
        &mut self,
        scope: ScopeId,
        decl_text: &str,
    ) -> Result<Generation, SessionError> {
        self.define_batch_with_vals_in(scope, &[decl_text], &[], &[])
    }

    /// [`Self::define`] plus scoping the declaration against live session
    /// values: `import_modules` (current `Val.G<g>` per still-live name) are
    /// imported unqualified into the rendered decl module; `inject_modules`
    /// (every still-live `Val.G<g>`, including shadowed gens) are passed to the
    /// extract as `--inject-val` so their `.hi` ifaces resolve at validation
    /// time. Lets a decl (`f x = … g …`) reference a prior session value `g`
    /// the way a genuine GHCi top-level definition would.
    pub fn define_with_vals(
        &mut self,
        decl_text: &str,
        import_modules: &[String],
        inject_modules: &[String],
    ) -> Result<Generation, SessionError> {
        self.define_batch_with_vals(&[decl_text], import_modules, inject_modules)
    }

    /// Define SEVERAL declarations as ONE generation — they land in one module
    /// and GHC typechecks them together, so a type signature and its binding,
    /// or a mutual-recursion SCC, split across separate block items still work
    /// (whole-block decl elaboration). `define` is the single-decl case.
    ///
    /// Binders are extracted from the concatenation (one parse), the sources
    /// ride as one `DeclTurn` (`render_module` already emits every source of a
    /// turn into one module), and validation/rollback are identical to
    /// `define`. Empty/whitespace sources are dropped; an all-empty batch is a
    /// no-op.
    ///
    /// Always shadows wildcard-imported names (`Library`, `Tidepool.Prelude`,
    /// …) with this session's own decl heads — GHCi parity for ANY session
    /// decl, pure or genuine: `f x = …` at the prompt always shadows an
    /// imported `f`, and a pure `let`/`<-` bind promoted into a decl
    /// (`tidepool-repl`'s `try_pure_bind_as_decl`) shadows exactly the same
    /// way, so pure and effectful binds stay interchangeable.
    pub fn define_batch(&mut self, decl_texts: &[&str]) -> Result<Generation, SessionError> {
        self.define_batch_with_vals(decl_texts, &[], &[])
    }

    /// [`Self::define_batch`] plus session-value scoping — see
    /// [`Self::define_with_vals`] for what `import_modules`/`inject_modules` do.
    /// `define_batch_with_vals(...) == define_batch_with_vals_in(ScopeId::ROOT, ...)`.
    pub fn define_batch_with_vals(
        &mut self,
        decl_texts: &[&str],
        import_modules: &[String],
        inject_modules: &[String],
    ) -> Result<Generation, SessionError> {
        self.define_batch_with_vals_in(ScopeId::ROOT, decl_texts, import_modules, inject_modules)
    }

    /// [`Self::define_batch_with_vals`], but the new turn chains from
    /// `scope`'s own tip instead of ROOT's — the ONE real define
    /// implementation; every other `define*` funnels into this one. On
    /// failure (write or GHC validation), `scope`'s tip is restored to
    /// exactly what it was before this call, and every OTHER scope's tip is
    /// left untouched — a failed define in a child scope never disturbs a
    /// sibling or the parent.
    pub fn define_batch_with_vals_in(
        &mut self,
        scope: ScopeId,
        decl_texts: &[&str],
        import_modules: &[String],
        inject_modules: &[String],
    ) -> Result<Generation, SessionError> {
        let Some(receipt) = self.declaration_receipt(decl_texts)? else {
            return Ok(self.scope_tip(scope));
        };
        self.define_batch_with_receipt_and_vals_in(
            scope,
            &SourceImports::new(),
            &receipt,
            import_modules,
            inject_modules,
        )
    }

    /// Ask GHC for the declaration facts that will authorize a commit. This is
    /// parse-only and mutates neither the log nor the generated module tree.
    /// The returned receipt must be passed unchanged to
    /// [`Self::define_batch_with_receipt_and_vals_in`].
    pub(crate) fn declaration_receipt(
        &self,
        decl_texts: &[&str],
    ) -> Result<Option<DeclarationReceipt>, SessionError> {
        let sources: Vec<String> = decl_texts
            .iter()
            .filter(|s| !s.trim().is_empty())
            .map(|s| (*s).to_string())
            .collect();
        if sources.is_empty() {
            return Ok(None);
        }

        let combined = sources.join("\n\n");
        let mut binder_include: Vec<&Path> = vec![self.root.as_path()];
        binder_include.extend(self.extra_include.iter().map(PathBuf::as_path));

        let decl_template = TurnTemplate {
            kind: TemplateSelector::Decl,
            source: format!(
                "{}\nmodule SessionDecls where\n{{{{TURN}}}}\n",
                self.env.pragmas
            ),
        };
        let turn_result = run_turn(TurnRequest {
            exact_context: None,
            session_id: Some(self.session_id()),
            turn_text: &combined,
            templates: std::slice::from_ref(&decl_template),
            include: &binder_include,
            session_root: self.root.as_path(),
            inject_modules: &[],
            gen: 0,
            verdict: Some(TurnClassification {
                kind: TurnKind::Decl,
                binders: Vec::new(),
                items: Vec::new(),
            }),
            target: None,
            retained_imports: &[],
        })
        .map_err(|failure| SessionError::Compile(failure.error))?;
        let receipt = match turn_result {
            TurnResult::Decl(receipt) => receipt,
            other => {
                return Err(SessionError::Compile(crate::CompileError::ExtractFailed(
                    format!("decl verdict produced an unexpected TurnResult variant: {other:?}"),
                )))
            }
        };
        Ok(Some(receipt))
    }

    /// Commit normalized source using the exact GHC receipt returned by
    /// [`Self::declaration_receipt`]. Keeping receipt acquisition separate from
    /// mutation lets [`PersistentSession`] derive binding-store eviction and
    /// validation imports from the same facts before this atomic commit.
    pub(crate) fn define_batch_with_receipt_and_vals_in(
        &mut self,
        scope: ScopeId,
        external: &SourceImports,
        receipt: &DeclarationReceipt,
        import_modules: &[String],
        inject_modules: &[String],
    ) -> Result<Generation, SessionError> {
        if self.durable_graph.is_some() {
            let candidate = self.render_admitted_candidate_in(
                scope,
                external,
                receipt,
                import_modules,
                inject_modules,
            )?;
            let staged = validate_declaration_candidate(candidate, &self.root)?;
            return self.adopt_staged_batch_with_receipt_and_vals_in(staged, &[]);
        }
        let sources = vec![receipt.source.replay_source(external)];
        let workbench_imports = receipt.source.prologue.workbench_imports();

        let tip_before = self.tips.get(&scope).copied();
        let gen = self.push_turn_in(
            scope,
            DeclTurn {
                normalized: receipt.source.clone(),
                external_imports: external.clone(),
                sources: sources.clone(),
                workbench_imports,
                items: receipt.items.clone(),
                value_types: BTreeMap::new(),
                retracts: Vec::new(),
                parent: None, // set inside push_turn_in from scope's tip
            },
        )?;
        let rendered = render::render_module_with_vals(&self.log, gen, &self.env, import_modules);
        // Roll the just-pushed turn back on a write failure, exactly as the
        // validation-failure path below does — a bare `?` here would bump the
        // generation permanently while leaving no on-disk module, poisoning
        // every later turn that imports the (missing) gen module.
        if let Err(e) = self.write_module(&rendered) {
            assert!(self.log.pop_latest_committed(gen));
            self.restore_tip(scope, tip_before);
            return Err(e);
        }

        // Validate ALL turns via GHC. On failure, roll back the log and delete
        // the gen module file so later turns don't import a poisoned module.
        let value_types = match self.validate_candidate(&rendered, &receipt.items, inject_modules) {
            Ok(types) => types,
            Err(error) => {
                assert!(self.log.pop_latest_committed(gen));
                let gen_path = self.root.join(rendered.module.relative_hs_path());
                if let Err(err) = std::fs::remove_file(&gen_path) {
                    tracing::warn!(
                        ?err,
                        path = %gen_path.display(),
                        "failed to delete poisoned generated module after validation failure"
                    );
                }
                self.restore_tip(scope, tip_before);
                return Err(error);
            }
        };
        self.log
            .turn_mut(gen)
            .expect("just committed declaration")
            .value_types = value_types;

        if scope == ScopeId::ROOT {
            self.record_recovery_turn(recovery::RecoveryTurn::new(
                self.id.0,
                gen.0,
                sources,
                Vec::new(),
                import_modules.is_empty() && inject_modules.is_empty(),
            ));
        }
        Ok(gen)
    }

    /// The pure half of declaration staging: render the next candidate
    /// module against a cloned log, without writing it to disk or invoking
    /// GHC. Cheap enough to run under a checkout that is released
    /// immediately after — see [`DeclarationCandidateRender`].
    pub(crate) fn render_candidate_in(
        &self,
        scope: ScopeId,
        external: &SourceImports,
        receipt: &DeclarationReceipt,
        import_modules: &[String],
        inject_modules: &[String],
    ) -> DeclarationCandidateRender {
        let sources = vec![receipt.source.replay_source(external)];
        let workbench_imports = receipt.source.prologue.workbench_imports();
        let mut log = self.log.clone();
        let turn = DeclTurn {
            normalized: receipt.source.clone(),
            external_imports: external.clone(),
            sources,
            workbench_imports,
            items: receipt.items.clone(),
            value_types: BTreeMap::new(),
            retracts: Vec::new(),
            parent: (self.scope_tip(scope).0 > 0).then_some(self.scope_tip(scope)),
        };
        let generation = log.push(turn.clone());
        let rendered = render::render_module_with_vals(&log, generation, &self.env, import_modules);
        DeclarationCandidateRender {
            exact_context: self.log.joined_context_at(self.scope_tip(scope)),
            session_id: self.id,
            root: self.root.clone(),
            extra_include: self.extra_include.clone(),
            pragmas: self.env.pragmas.clone(),
            scope,
            base_generation: self.log.generation(),
            base_tip: self.scope_tip(scope),
            generation,
            reserved: false,
            rendered,
            turn,
            receipt: receipt.clone(),
            import_modules: import_modules.to_vec(),
            inject_modules: inject_modules.to_vec(),
        }
    }

    /// In attached v2, burn the rendered candidate's identity durably before
    /// it leaves the checkout for compiler validation. Unattached sessions
    /// keep the existing sequential candidate path.
    pub(crate) fn render_admitted_candidate_in(
        &mut self,
        scope: ScopeId,
        external: &SourceImports,
        receipt: &DeclarationReceipt,
        import_modules: &[String],
        inject_modules: &[String],
    ) -> Result<DeclarationCandidateRender, SessionError> {
        let mut candidate =
            self.render_candidate_in(scope, external, receipt, import_modules, inject_modules);
        if self.durable_graph.is_some() {
            let reserved = self.reserve_declaration_generation_durable()?;
            assert_eq!(candidate.generation, reserved);
            candidate.base_generation = reserved;
            candidate.reserved = true;
        }
        Ok(candidate)
    }

    pub(crate) fn adopt_staged_batch_with_receipt_and_vals_in(
        &mut self,
        staged: StagedDeclaration,
        visible_values: &[(SessionVarId, String)],
    ) -> Result<Generation, SessionError> {
        let items = staged.items().to_vec();
        self.admit_staged_declaration_in(staged, visible_values)?
            .map_commit(|generation| DeclarationPlaneCommit {
                generation,
                module: SessionModule::lib(generation),
                items,
                evicted_values: Vec::new(),
            })
            .into_result()
            .map(|commit| commit.generation)
    }

    fn admit_staged_declaration_in(
        &mut self,
        staged: StagedDeclaration,
        visible_values: &[(SessionVarId, String)],
    ) -> Result<DeclarationAdmission<Generation>, SessionError> {
        let slot_matches = if staged.reserved {
            staged.certified_authored.is_some()
                && staged.base_generation == staged.generation
                && self.log.is_reserved(staged.generation)
                && staged.module == SessionModule::lib(staged.generation)
        } else {
            self.durable_graph.is_none()
                && staged.generation == self.log.generation().next()
                && staged.module == self.next_module()
        };
        if staged.session_id != self.id
            || staged.root != self.root
            || (!staged.reserved && staged.base_generation != self.log.generation())
            || staged.base_tip != self.scope_tip(staged.scope)
            || staged.exact_context != self.log.joined_context_at(staged.base_tip)
            || !slot_matches
            || staged.visible_values != visible_values
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let authored_context = staged
            .certified_authored
            .as_ref()
            .map(|certificate| {
                paired_publication::authored_context(self, staged.base_tip, certificate)
            })
            .transpose()?;
        let staged_graph = if staged.reserved
            && staged.persistence == DeclarationPersistence::Durable
            && self.durable_graph.is_some()
        {
            let (context, _) = authored_context
                .as_ref()
                .ok_or(SessionError::StaleStagedDeclaration)?;
            Some(self.stage_private_authored_graph(&staged, context)?)
        } else {
            None
        };
        // Install exactly the bytes GHC already validated — never a
        // re-render — into the shared session root. For the single-checkout
        // caller this rewrites the same bytes already written there at stage
        // time; for a split cell preparation this is the FIRST time the
        // candidate touches the shared root, moving it out of the private
        // directory it was validated against.
        self.write_module(&staged.rendered)?;
        let uncertain = if let Some(staged_graph) = staged_graph {
            let outcome = self.publish_recovery_manifest(staged_graph);
            let state = self
                .durable_graph
                .as_mut()
                .expect("reserved graph preflight");
            match outcome {
                recovery::RecoveryPublishOutcome::BeforeRename { detail, .. } => {
                    return Err(SessionError::RecoveryManifest {
                        path: state.path.clone(),
                        detail,
                    });
                }
                recovery::RecoveryPublishOutcome::Durable { graph, .. } => {
                    state.graph = graph;
                    None
                }
                recovery::RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                    graph,
                    publication,
                    detail,
                } => {
                    state.graph = graph;
                    state.unconfirmed = Some(publication);
                    Some(detail)
                }
            }
        } else {
            None
        };
        let generation = if staged.reserved {
            assert!(self.log.commit_reserved_certified_authored(
                staged.generation,
                staged.turn.clone(),
                staged
                    .certified_authored
                    .expect("authored evidence preflight"),
                authored_context
                    .as_ref()
                    .expect("authored context preflight")
                    .0
                    .clone(),
                authored_context.expect("authored context preflight").1,
            ));
            self.tips.insert(staged.scope, staged.generation);
            staged.generation
        } else {
            self.push_turn_in(staged.scope, staged.turn.clone())?
        };
        if staged.scope == ScopeId::ROOT {
            self.record_recovery_turn(recovery::RecoveryTurn::new(
                self.id.0,
                generation.0,
                staged.turn.sources,
                Vec::new(),
                staged.import_modules.is_empty() && staged.inject_modules.is_empty(),
            ));
        }
        if let Some(detail) = uncertain {
            return Ok(DeclarationAdmission::PublishedDurabilityUnconfirmed {
                commit: generation,
                path: self.durable_graph.as_ref().unwrap().path.clone(),
                detail,
            });
        }
        Ok(DeclarationAdmission::Committed(generation))
    }

    fn publish_recovery_manifest(
        &mut self,
        staged: recovery::StagedRecoveryManifest,
    ) -> recovery::RecoveryPublishOutcome {
        let outcome = staged.publish();
        #[cfg(test)]
        if std::mem::take(&mut self.fail_recovery_durability_once) {
            if let recovery::RecoveryPublishOutcome::Durable { graph, publication } = outcome {
                return recovery::RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                    graph,
                    publication,
                    detail: "injected post-rename durability failure".into(),
                };
            }
        }
        outcome
    }

    fn stage_private_authored_graph(
        &self,
        staged: &StagedDeclaration,
        context: &tidepool_toolchain::declaration_join::ExactDeclarationContext,
    ) -> Result<recovery::StagedRecoveryManifest, SessionError> {
        let state = self
            .durable_graph
            .as_ref()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let invalid = |detail: &str| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail: detail.into(),
        };
        if state.unconfirmed.is_some() || state.graph.high_water < staged.generation {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let certified = staged
            .certified_authored
            .as_ref()
            .ok_or_else(|| invalid("reserved declaration has no certified authored product"))?;
        if staged.turn.parent != (staged.base_tip.0 > 0).then_some(staged.base_tip)
            || staged.base_tip.0 > 0 && staged.exact_context.is_none()
        {
            return Err(invalid(
                "authored ancestry lacks its admitted exact context",
            ));
        }
        if certified.product().source_sha256() != Some(certified.source_sha256())
            || certified.product().owner().module != staged.module.module_name()
        {
            return Err(invalid(
                "authored recovery product does not match validated source",
            ));
        }
        let root = state
            .path
            .parent()
            .ok_or_else(|| invalid("recovery manifest has no parent"))?;
        let refs = tidepool_toolchain::recovery_artifacts::materialize_certified_products(
            root,
            certified.toolchain_identity_sha256(),
            certified.recovery_products(),
        )
        .map_err(|error| invalid(&error.to_string()))?;
        let own = certified.product().owner();
        if refs
            .iter()
            .filter(|reference| {
                reference.unit == own.unit
                    && reference.module == own.module
                    && reference.module_version == own.module_version.0
                    && reference.skinny_iface_sha256 == own.skinny_iface_sha256
                    && reference.product_sha256 == own.product_sha256
            })
            .count()
            != 1
        {
            return Err(invalid(
                "authored recovery closure lacks its exact declaration owner",
            ));
        }
        if certified.introduced_exports().iter().any(|export| {
            export.head.unit != own.unit
                || export.head.module != own.module
                || export
                    .children
                    .iter()
                    .any(|child| child.unit != own.unit || child.module != own.module)
        }) {
            return Err(invalid(
                "authored inventory includes nonlocal export identities",
            ));
        }
        let exports = certified
            .introduced_exports()
            .iter()
            .map(|export| {
                certified_recovery_export(export)
                    .ok_or_else(|| invalid("unsupported authored export identity"))
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        let artifacts = refs
            .into_iter()
            .map(recovery::RecoveryArtifactClosure::Home)
            .collect::<Vec<_>>();
        let mut graph = state.graph.clone();
        let artifact_refs = artifacts
            .iter()
            .map(recovery::RecoveryArtifactClosure::key)
            .collect();
        let mut live_dependencies = staged
            .turn
            .parent
            .and_then(|parent| graph.nodes.iter().find(|node| node.id == parent))
            .map(|node| node.live_dependencies.clone())
            .unwrap_or_default();
        live_dependencies.extend(staged.visible_values.iter().map(|(id, _)| {
            recovery::RecoveryLiveDependency::Binding {
                binding: recovery::RecoveryBindingId {
                    session: self.id.0,
                    variable: id.raw(),
                },
                name: id.raw().to_string(),
            }
        }));
        graph.nodes.push(recovery::RecoveryNode {
            id: staged.generation,
            parent: staged.turn.parent,
            kind: recovery::RecoveryNodeKind::Authored,
            implementation_refs: Vec::new(),
            artifact_refs,
            lexical_roots: vec![tidepool_toolchain::declaration_join::ExactModuleIdentity {
                unit: certified.product().owner().unit.clone(),
                module: staged.module.module_name(),
            }],
            lexical: context.lexical_graph().to_vec(),
            exports,
            retracts: paired_publication::exact_retractions(
                self,
                staged.base_tip,
                &staged.turn.retracts,
            )?
            .iter()
            .map(authored_identity)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid("unsupported exact retraction identity"))?,
            workbench_imports: staged.turn.workbench_imports.specs().to_vec(),
            instances: paired_publication::recovery_instances(
                certified.instances(),
                certified.family_closure(),
            ),
            state: if live_dependencies.is_empty() {
                recovery::RecoveryNodeState::ExactArtifactClosure
            } else {
                recovery::RecoveryNodeState::LiveValueDependency {
                    reason: "authored declaration retains exact live bindings".into(),
                }
            },
            live_dependencies,
        });
        for artifact in artifacts {
            if !graph
                .artifacts
                .iter()
                .any(|existing| existing.key() == artifact.key())
            {
                graph.artifacts.push(artifact);
            }
        }
        graph.seal().map_err(|error| invalid(&error.to_string()))?;
        recovery::stage_v2(&state.path, root, &graph).map_err(|error| invalid(&error.to_string()))
    }

    pub(crate) fn discard_staged(&self, staged: &StagedDeclaration) {
        if staged.session_id == self.id
            && staged.root == self.root
            && if staged.reserved {
                self.log.is_reserved(staged.generation)
            } else {
                staged.base_generation == self.log.generation()
                    && staged.generation == self.log.generation().next()
            }
        {
            // A no-op when the candidate was validated off-checkout, against
            // a private directory this session root never received — there
            // is nothing here to remove; the caller owns that directory's
            // lifetime (e.g. a `tempfile::TempDir` cleaned up on drop).
            remove_module_artifacts(&self.root, staged.module);
        }
    }

    /// Append `turn` as `scope`'s next turn: chains its `parent` from
    /// [`Self::scope_tip`], pushes it to the shared log, and advances
    /// `scope`'s tip to the new generation. Returns the new generation.
    fn push_turn_in(
        &mut self,
        scope: ScopeId,
        mut turn: DeclTurn,
    ) -> Result<Generation, SessionError> {
        let tip = self.scope_tip(scope);
        turn.parent = (tip.0 > 0).then_some(tip);
        let gen = if self.durable_graph.is_some() {
            let generation = self.reserve_declaration_generation_durable()?;
            assert!(self.log.commit_reserved_authored(generation, turn));
            generation
        } else {
            self.log.push(turn)
        };
        self.tips.insert(scope, gen);
        Ok(gen)
    }

    /// Undo [`Self::push_turn_in`]'s tip bump for `scope` — restores it to
    /// `tip_before` (the value read from `self.tips` immediately before the
    /// push), which may be `None` if `scope` had never been used yet. Paired
    /// with a reserved tombstone at the failed generation on every rollback
    /// path so its identity cannot be reused, and touches no other tip.
    fn restore_tip(&mut self, scope: ScopeId, tip_before: Option<Generation>) {
        match tip_before {
            Some(g) => {
                self.tips.insert(scope, g);
            }
            None => {
                self.tips.remove(&scope);
            }
        }
    }

    /// Retract `name` from the persistent declaration environment: after its binding migrates to the
    /// persistent binding store, the declaration module must stop exporting it, or a later
    /// `let`/`def` would compile against the stale decl (a value bound
    /// `findings <- pure []` then rebound `findings <- pure (findings ++ xs)`
    /// otherwise leaves `findings = []` defined forever). The dual of
    /// `tidepool-repl`'s binding-store eviction — call it when a persistent declaration environment name is
    /// materialized.
    ///
    /// No-op when `name` is not a current decl head. Otherwise appends a
    /// pure-retraction turn and re-renders the current module as a re-export
    /// shell minus `name`. The shell introduces NO new source (only subtracts an
    /// export), so it cannot fail to type-check — GHC validation is skipped,
    /// making retraction cheap (no ~6s compile).
    /// `retract(name) == retract_in(ScopeId::ROOT, name)`.
    pub fn retract(&mut self, name: &str) -> Result<(), SessionError> {
        self.retract_in(ScopeId::ROOT, name)
    }

    /// [`Self::retract`], but against `scope`'s own persistent declaration environment — a name
    /// retracted in a child scope never touches the parent's (or a sibling's)
    /// tip or heads.
    pub fn retract_in(&mut self, scope: ScopeId, name: &str) -> Result<(), SessionError> {
        self.retract_many_in(scope, &[name.to_string()])
    }

    /// Retract all current declaration heads in `names` with ONE durable
    /// generation.  This is the declaration-side commit used by an atomic
    /// materialization set: either the rendered re-export shell removes every
    /// requested current head, or the log/tip stay exactly as they were.
    pub fn retract_many_in(
        &mut self,
        scope: ScopeId,
        names: &[String],
    ) -> Result<(), SessionError> {
        self.retract_heads_in(scope, names, None)
    }

    pub(crate) fn retract_value_heads_in(
        &mut self,
        scope: ScopeId,
        names: &[String],
    ) -> Result<(), SessionError> {
        self.retract_heads_in(
            scope,
            names,
            Some(tidepool_toolchain::declaration_join::ExportNamespace::Value),
        )
    }

    fn retract_heads_in(
        &mut self,
        scope: ScopeId,
        names: &[String],
        namespace: Option<tidepool_toolchain::declaration_join::ExportNamespace>,
    ) -> Result<(), SessionError> {
        let tip = self.scope_tip(scope);
        let heads = self.log.current_items_at(tip);
        let mut retracts: Vec<DeclarationRetraction> = names
            .iter()
            .filter(|name| {
                heads.iter().any(|(head, _)| {
                    head.head_name() == name.as_str()
                        && namespace.is_none_or(|namespace| namespace == head.head_namespace())
                })
            })
            .map(|name| match namespace {
                Some(namespace) => DeclarationRetraction::Head {
                    namespace,
                    occurrence: name.clone(),
                },
                None => DeclarationRetraction::Name(name.clone()),
            })
            .collect();
        retracts.sort();
        retracts.dedup();
        if retracts.is_empty() {
            return Ok(());
        }
        if self.durable_graph.is_none()
            && self.recovery_manifest_path.is_some()
            && namespace.is_some()
            && heads.iter().any(|(head, _)| {
                retracts.iter().any(|retraction| {
                    retraction.occurrence() == head.head_name()
                        && !retraction.selects(head.head_namespace(), head.head_name())
                })
            })
        {
            return Err(SessionError::RecoveryManifest {
                path: self.recovery_manifest_path.clone().unwrap(),
                detail: "typed retraction requires the exact recovery graph format".into(),
            });
        }
        if let Some(state) = &self.durable_graph {
            let _ = state;
            paired_publication::exact_retractions(self, tip, &retracts)?;
            let receipt = DeclarationReceipt {
                source: DeclarationSource::default(),
                binders: Vec::new(),
                items: Vec::new(),
            };
            let mut candidate = self.render_admitted_candidate_in(
                scope,
                &SourceImports::new(),
                &receipt,
                &[],
                &[],
            )?;
            candidate.turn.retracts = retracts;
            let mut log = self.log.clone();
            assert!(log.commit_reserved_authored(candidate.generation, candidate.turn.clone()));
            candidate.rendered = render::render_module(&log, candidate.generation, &self.env);
            let scratch = tempfile::tempdir()?;
            let staged = validate_declaration_candidate(candidate, scratch.path())?;
            self.adopt_staged_batch_with_receipt_and_vals_in(staged, &[])?;
            return Ok(());
        }
        let tip_before = self.tips.get(&scope).copied();
        let gen = self.push_turn_in(
            scope,
            DeclTurn {
                normalized: Default::default(),
                external_imports: SourceImports::new(),
                sources: Vec::new(),
                workbench_imports: SourceImports::new(),
                items: Vec::new(),
                value_types: BTreeMap::new(),
                retracts: retracts.clone(),
                parent: None, // set inside push_turn_in from scope's tip
            },
        )?;
        let rendered = render::render_module(&self.log, gen, &self.env);
        if let Err(e) = self.write_module(&rendered) {
            assert!(self.log.pop_latest_committed(gen)); // keep the log consistent with disk
            self.restore_tip(scope, tip_before);
            return Err(e);
        }
        if scope == ScopeId::ROOT {
            self.record_recovery_turn(recovery::RecoveryTurn::new(
                self.id.0,
                gen.0,
                Vec::new(),
                retracts
                    .iter()
                    .map(|retraction| retraction.occurrence().to_owned())
                    .collect(),
                true,
            ));
        }
        Ok(())
    }

    fn record_recovery_turn(&mut self, turn: recovery::RecoveryTurn) {
        let Some(path) = self.recovery_manifest_path.as_ref() else {
            return;
        };
        self.recovery_turns.push(turn);
        match recovery::write(path, self.id.0, &self.recovery_turns) {
            Ok(()) => self.recovery_manifest_warning = None,
            Err(error) => {
                let warning = error.to_string();
                tracing::warn!(path = %path.display(), error = %warning, "could not publish declaration recovery manifest after semantic commit");
                self.recovery_manifest_warning = Some(warning);
            }
        }
    }

    /// Validate one declaration generation and capture its visible value types
    /// from the same checked environment. Importing the candidate forces GHC to
    /// check the complete module; no executable target is produced.
    fn validate_candidate(
        &self,
        rendered: &RenderedModule,
        items: &[ExportItem],
        inject_modules: &[String],
    ) -> Result<BTreeMap<String, String>, SessionError> {
        let stdlib_include = stdlib_include_for_validation(&self.extra_include)?;
        let mut includes = vec![self.root.clone()];
        includes.extend(self.extra_include.iter().cloned());
        if let Some(stdlib) = stdlib_include {
            includes.push(stdlib);
        }
        validate_rendered_module(
            rendered,
            items,
            inject_modules,
            &includes,
            &self.root,
            &self.env.pragmas,
            None,
        )
    }

    /// Atomically write a rendered module to its place in the include tree.
    /// Best-effort (no fsync): this is a regenerable compile artifact, not
    /// durable state — a write lost to a crash just recompiles on next use.
    fn write_module(&self, rendered: &RenderedModule) -> Result<(), SessionError> {
        write_module_at(&self.root, rendered)
    }
}

/// [`SessionLib::write_module`], generalized to an explicit root so a split
/// cell preparation can write a candidate into a private directory instead of
/// the shared session root.
fn write_module_at(root: &Path, rendered: &RenderedModule) -> Result<(), SessionError> {
    let rel = rendered.module.relative_hs_path();
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tidepool_atomic_write::write_best_effort(&path, rendered.source.as_bytes())
        .map_err(|e| SessionError::Io(e.source))?;
    Ok(())
}

/// [`SessionLib`]'s unpublished-artifact cleanup, generalized to an explicit
/// root. Shared compiler caches own their own content-based invalidation;
/// this only removes the source and any session-local compiler products
/// written beside it.
fn remove_module_artifacts(root: &Path, module: SessionModule) {
    let source = root.join(module.relative_hs_path());
    for extension in ["hs", "hi", "dyn_hi", "o", "dyn_o", "hie"] {
        let path = source.with_extension(extension);
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), %error, "could not remove unpublished declaration artifact");
            }
        }
    }
}

/// GHC-validate one already-rendered candidate module: importing it forces
/// GHC to check the complete module (no executable target is produced), and
/// this also captures each declared value's compiler-rendered type. `includes`
/// is the exact, ordered search path — callers assemble it (a private
/// candidate directory ahead of the session root and any extra include dirs,
/// or just the session root itself for the single-checkout caller) because
/// this function has no [`SessionLib`] of its own to derive one from.
/// `session_root` is separate from `includes`: it is where already-committed
/// `Tidepool.Session.Val.G<g>` interfaces live for `--inject-val` lookup, and
/// stays the session's real root even when `includes`' primary entry is a
/// private candidate directory — a candidate never has value interfaces of
/// its own.
fn validate_rendered_module(
    rendered: &RenderedModule,
    items: &[ExportItem],
    inject_modules: &[String],
    includes: &[PathBuf],
    session_root: &Path,
    pragmas: &str,
    exact_context: Option<
        &std::sync::Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    >,
) -> Result<BTreeMap<String, String>, SessionError> {
    let mut values = Vec::new();
    for item in items {
        let term_names: &[String] = match item {
            ExportItem::Value { name } => std::slice::from_ref(name),
            ExportItem::Type { cons, .. } => cons,
            ExportItem::Class { methods, .. } => methods,
        };
        for name in term_names {
            if !values.contains(name) {
                values.push(name.clone());
            }
        }
    }
    let expressions = if values.is_empty() {
        vec!["()".to_owned()]
    } else {
        values
            .iter()
            .map(|name| {
                let occurrence = name
                    .strip_prefix('(')
                    .and_then(|name| name.strip_suffix(')'))
                    .unwrap_or(name);
                match occurrence.chars().next() {
                    Some(c) if c.is_alphanumeric() || c == '_' => {
                        format!("TidepoolCandidate.{occurrence}")
                    }
                    _ => format!("(TidepoolCandidate.{occurrence})"),
                }
            })
            .collect()
    };
    let queries = expressions
        .iter()
        .cloned()
        .map(InspectionQuery::TypeOf)
        .collect::<Vec<_>>();
    let imports = format!(
        "{}\nqualified {} as TidepoolCandidate\n",
        rendered.module.module_name(),
        rendered.module.module_name()
    );
    let preamble = format!("{pragmas}\nmodule TidepoolDeclarationTypes where\n");
    let include_refs = includes.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let results = match inspection::run_inspections_strict(InspectionRequest {
        exact_context: exact_context.cloned(),
        preamble: &preamble,
        imports: &imports,
        include: &include_refs,
        session_root,
        inject_modules,
        queries: &queries,
        effects: None,
    }) {
        Ok(results) => results,
        Err(crate::CompileError::Diagnostics(diagnostics)) => {
            let line_offset = if rendered.body_line > 0 && !rendered.hoisted_lines {
                rendered.body_line
            } else {
                0
            };
            return Err(SessionError::ValidationFailed(
                DeclarationValidationFailure {
                    diagnostics,
                    anchor: rendered.module.relative_hs_path(),
                    line_offset,
                    source: rendered.source.clone(),
                },
            ));
        }
        Err(error) => return Err(SessionError::Compile(error)),
    };

    let mut types = BTreeMap::new();
    for (index, result) in results.into_iter().enumerate() {
        match result {
            InspectionResult::Type { display, .. } if index < values.len() => {
                types.insert(values[index].clone(), display);
            }
            InspectionResult::Type { .. } => {}
            InspectionResult::Rejected { diagnostic } => {
                return Err(SessionError::Compile(crate::CompileError::ExtractFailed(
                    format!("strict declaration type capture was rejected: {diagnostic}"),
                )));
            }
            other => {
                return Err(SessionError::Compile(crate::CompileError::ExtractFailed(
                    format!("declaration type capture returned {}", other.render()),
                )));
            }
        }
    }
    Ok(types)
}

/// Write `candidate`'s rendered module into `primary_root` and GHC-validate
/// it there — with `candidate`'s own session root and extra include dirs
/// available afterward on the search path (in that order) so prior
/// generations and configured vocabulary still resolve. Runs no checkout and
/// needs none: everything it reads is already owned by `candidate`.
///
/// `primary_root` may be a private, per-attempt directory (a split cell
/// preparation validating off-checkout) or the session's own root (the
/// single-checkout caller, where this is the whole of staging). Either way,
/// nothing is written to the *session's* root unless `primary_root` already
/// **is** that root — a private candidate never touches the shared include
/// tree; installing it for real is
/// [`SessionLib::adopt_staged_batch_with_receipt_and_vals_in`]'s job, from the
/// exact bytes recorded on the returned [`StagedDeclaration`].
pub fn validate_declaration_candidate(
    candidate: DeclarationCandidateRender,
    primary_root: &Path,
) -> Result<StagedDeclaration, SessionError> {
    write_module_at(primary_root, &candidate.rendered)?;

    let mut includes = vec![primary_root.to_path_buf()];
    if primary_root != candidate.root {
        includes.push(candidate.root.clone());
    }
    includes.extend(candidate.extra_include.iter().cloned());
    if let Some(stdlib) = stdlib_include_for_validation(&candidate.extra_include)? {
        includes.push(stdlib);
    }

    let value_types = match validate_rendered_module(
        &candidate.rendered,
        &candidate.turn.items,
        &candidate.inject_modules,
        &includes,
        &candidate.root,
        &candidate.pragmas,
        candidate.exact_context.as_ref(),
    ) {
        Ok(types) => types,
        Err(error) => {
            remove_module_artifacts(primary_root, candidate.rendered.module);
            return Err(error);
        }
    };

    let certified_authored = if candidate.reserved {
        let source_path = primary_root.join(candidate.rendered.module.relative_hs_path());
        let certified = match &candidate.exact_context {
            Some(context) => {
                tidepool_toolchain::declaration_join::certify_authored_declaration_in_context(
                    candidate.rendered.module,
                    &source_path,
                    &candidate.rendered.source,
                    &includes,
                    &candidate.root,
                    context.clone(),
                )
            }
            None => tidepool_toolchain::declaration_join::certify_authored_declaration(
                candidate.rendered.module,
                &source_path,
                &candidate.rendered.source,
                &includes,
                &candidate.root,
            ),
        };
        match certified {
            Ok(certified) => Some(certified),
            Err(error) => {
                remove_module_artifacts(primary_root, candidate.rendered.module);
                return Err(SessionError::Compile(error));
            }
        }
    } else {
        None
    };

    let mut turn = candidate.turn;
    turn.value_types = value_types;
    Ok(StagedDeclaration {
        generation: candidate.generation,
        reserved: candidate.reserved,
        persistence: DeclarationPersistence::Durable,
        module: candidate.rendered.module,
        receipt: candidate.receipt,
        exact_context: candidate.exact_context,
        session_id: candidate.session_id,
        root: candidate.root,
        scope: candidate.scope,
        base_generation: candidate.base_generation,
        base_tip: candidate.base_tip,
        turn,
        import_modules: candidate.import_modules,
        inject_modules: candidate.inject_modules,
        visible_values: Vec::new(),
        rendered: candidate.rendered,
        certified_authored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_session_has_no_module() {
        let dir = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();
        assert_eq!(lib.generation(), Generation(0));
        assert!(lib.current_module().is_none());
        assert!(lib.import_line().is_none());
    }

    #[test]
    fn durable_join_reservation_burns_identity_before_compiler_visibility() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let mut first = SessionLib::open(
            SessionId(72),
            dir.path().join("first"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        first.attach_recovery_graph_v2(&manifest).unwrap();
        assert!(!manifest.exists());
        assert_eq!(
            first.reserve_join_generation_durable().unwrap(),
            Generation(1)
        );
        assert!(manifest.exists());
        assert_eq!(first.scope_tip(ScopeId::ROOT), Generation(0));
        assert!(first.log.turn(Generation(1)).is_none());

        let mut second = SessionLib::open(
            SessionId(73),
            dir.path().join("second"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        second.attach_recovery_graph_v2(&manifest).unwrap();
        assert_eq!(second.generation(), Generation(1));
        assert_eq!(
            second.reserve_join_generation_durable().unwrap(),
            Generation(2)
        );
        assert_eq!(second.scope_tip(ScopeId::ROOT), Generation(0));
    }

    #[test]
    fn failed_durable_reservation_does_not_allocate_module_identity() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("missing-parent").join("declarations.json");
        let mut lib = SessionLib::open(
            SessionId(74),
            dir.path().join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_recovery_graph_v2(&manifest).unwrap();
        assert!(lib.reserve_join_generation_durable().is_err());
        assert_eq!(lib.generation(), Generation(0));
        assert!(!manifest.exists());
    }

    /// An unseeded scope resolves to the EMPTY environment, never to the
    /// log's global tip. The global-tip fallback is the leak forbidden in
    /// both directions: a sibling pushing a turn between a scope's mint and
    /// its first use would leak into that scope, and a child defining before
    /// its parent's next turn would leak upward
    /// into the parent. (The GHC-validated end-to-end form of this lives in
    /// `tests/session_decl_scope_tree.rs`; this pins the pure tip algebra.)
    #[test]
    fn an_unseeded_scope_is_empty_not_the_global_tip() {
        let dir = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();
        assert_eq!(lib.scope_tip(ScopeId::ROOT), Generation(0));
        assert_eq!(
            lib.scope_tip(ScopeId(42)),
            Generation(0),
            "a scope nobody seeded sees nothing, not the last turn pushed anywhere"
        );
    }

    /// Seeding carries the PARENT's environment down and is idempotent — it
    /// must never rewind a scope that has already pushed turns, which would
    /// silently drop that scope's own declarations.
    #[test]
    fn seed_scope_inherits_once_and_never_rewinds() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib =
            SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();

        lib.seed_scope(ScopeId(1), Generation(3));
        assert_eq!(lib.scope_tip(ScopeId(1)), Generation(3));

        lib.seed_scope(ScopeId(1), Generation(9));
        assert_eq!(
            lib.scope_tip(ScopeId(1)),
            Generation(3),
            "re-seeding a live scope is a no-op, not a rewind"
        );

        // Siblings seeded from one parent tip start identical and independent.
        lib.seed_scope(ScopeId(2), Generation(3));
        assert_eq!(lib.scope_tip(ScopeId(2)), lib.scope_tip(ScopeId(1)));
    }

    #[test]
    fn workbench_imports_follow_the_scoped_declaration_chain() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib =
            SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();
        let root = lib
            .push_turn_in(
                ScopeId::ROOT,
                DeclTurn {
                    normalized: Default::default(),
                    external_imports: SourceImports::new(),
                    sources: vec!["import qualified Data.Set as Set".into()],
                    workbench_imports: SourceImports::from_specs(["qualified Data.Set as Set"]),
                    items: Vec::new(),
                    value_types: BTreeMap::new(),
                    retracts: Vec::new(),
                    parent: None,
                },
            )
            .unwrap();
        let child = ScopeId(1);
        lib.seed_scope(child, root);
        lib.push_turn_in(
            child,
            DeclTurn {
                normalized: Default::default(),
                external_imports: SourceImports::new(),
                sources: vec!["import Data.Proxy (Proxy (..))".into()],
                workbench_imports: SourceImports::from_specs(["Data.Proxy (Proxy (..))"]),
                items: Vec::new(),
                value_types: BTreeMap::new(),
                retracts: Vec::new(),
                parent: None,
            },
        )
        .unwrap();
        lib.push_turn_in(
            ScopeId::ROOT,
            DeclTurn {
                normalized: Default::default(),
                external_imports: SourceImports::new(),
                sources: vec!["import qualified Data.Map.Strict as Map".into()],
                workbench_imports: SourceImports::from_specs(["qualified Data.Map.Strict as Map"]),
                items: Vec::new(),
                value_types: BTreeMap::new(),
                retracts: Vec::new(),
                parent: None,
            },
        )
        .unwrap();

        assert_eq!(
            lib.workbench_imports_in(child).source_lines(),
            [
                "import qualified Data.Set as Set",
                "import Data.Proxy (Proxy (..))"
            ]
        );
        assert_eq!(
            lib.workbench_imports_in(ScopeId::ROOT).source_lines(),
            [
                "import qualified Data.Set as Set",
                "import qualified Data.Map.Strict as Map"
            ]
        );
    }

    fn validated_staged_declaration(lib: &SessionLib, source: &str) -> StagedDeclaration {
        let receipt = lib
            .declaration_receipt(&[source])
            .expect("extract declaration receipt")
            .expect("non-empty declaration receipt");
        let candidate =
            lib.render_candidate_in(ScopeId::ROOT, &SourceImports::new(), &receipt, &[], &[]);
        validate_declaration_candidate(candidate, lib.include_dir())
            .expect("stage and validate declaration")
    }

    #[test]
    fn declaration_validation_uses_the_actor_source_layer() {
        let root = tempfile::tempdir().unwrap();
        let helper = tempfile::tempdir().unwrap();
        std::fs::write(
            helper.path().join("SessionHelpers.hs"),
            "module SessionHelpers where\nhelper :: Int\nhelper = 42\n",
        )
        .unwrap();
        let lib = staged_test_lib(&root);
        let receipt = lib
            .declaration_receipt(&["import SessionHelpers\nanswer :: Int\nanswer = helper"])
            .unwrap()
            .unwrap();
        let candidate =
            lib.render_candidate_in(ScopeId::ROOT, &SourceImports::new(), &receipt, &[], &[]);
        assert!(validate_declaration_candidate(candidate.clone(), lib.include_dir()).is_err());
        let staged = validate_declaration_candidate(
            candidate.with_source_layer(&[helper.path().to_path_buf()]),
            lib.include_dir(),
        )
        .expect("actor helper import must resolve while staging the declaration");
        assert!(staged
            .items()
            .iter()
            .any(|item| item.head_name() == "answer"));
    }

    fn validated_staged_answer(lib: &SessionLib) -> StagedDeclaration {
        validated_staged_declaration(lib, "data Flag = On\nanswer :: Int\nanswer = 42")
    }

    fn staged_test_lib(root: &tempfile::TempDir) -> SessionLib {
        tidepool_testing::eval_harness::require_extract();
        SessionLib::open(SessionId(991), root.path(), ModuleEnv::standalone_default())
            .expect("open declaration library")
            .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()])
    }

    #[test]
    fn attached_v2_private_authored_commit_can_reserve_join_and_restart_without_reuse() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let mut lib = staged_test_lib(&root);
        lib.attach_recovery_graph_v2(&manifest).unwrap();
        let private = ScopeId(2);
        lib.seed_scope(private, Generation(0));
        let receipt = lib
            .declaration_receipt(&["data PrivateFlag = PrivateFlag"])
            .unwrap()
            .unwrap();
        let candidate = lib
            .render_admitted_candidate_in(private, &SourceImports::new(), &receipt, &[], &[])
            .unwrap();
        assert_eq!(candidate.generation, Generation(1));
        assert!(lib.log.is_reserved(Generation(1)));
        assert_eq!(lib.scope_tip(private), Generation(0));
        assert_eq!(
            recovery::read_v2(&manifest, root.path())
                .unwrap()
                .unwrap()
                .graph
                .high_water,
            Generation(1)
        );
        let staged = validate_declaration_candidate(candidate, lib.include_dir()).unwrap();
        let exact_evidence = staged.certified_authored.clone().unwrap();
        let committed_copy = staged.clone();
        assert_eq!(
            lib.reserve_join_generation_durable().unwrap(),
            Generation(2)
        );
        assert_eq!(lib.scope_tip(private), Generation(0));
        assert_eq!(
            lib.adopt_staged_batch_with_receipt_and_vals_in(staged, &[])
                .unwrap(),
            Generation(1)
        );
        assert_eq!(lib.scope_tip(private), Generation(1));
        assert_eq!(
            lib.log.certified_authored_at(Generation(1)),
            Some(&exact_evidence)
        );
        assert_eq!(lib.scope_tip(ScopeId::ROOT), Generation(0));
        assert!(matches!(
            lib.adopt_staged_batch_with_receipt_and_vals_in(committed_copy.clone(), &[]),
            Err(SessionError::StaleStagedDeclaration)
        ));
        lib.discard_staged(&committed_copy);
        assert!(root
            .path()
            .join(committed_copy.module().relative_hs_path())
            .exists());
        let retained = recovery::read_v2(&manifest, root.path()).unwrap().unwrap();
        assert!(retained.artifact_losses.is_empty());
        assert_eq!(retained.graph.high_water, Generation(2));
        assert_eq!(retained.graph.nodes.len(), 1);
        assert_eq!(retained.graph.nodes[0].id, Generation(1));
        assert_eq!(
            retained.graph.nodes[0].kind,
            recovery::RecoveryNodeKind::Authored
        );
        assert!(!retained.graph.nodes[0].artifact_refs.is_empty());
        assert!(retained.graph.public_surfaces.is_empty());
        assert_eq!(
            lib.reserve_join_generation_durable().unwrap(),
            Generation(3)
        );
        drop(lib);

        let mut restarted = staged_test_lib(&root);
        restarted.attach_recovery_graph_v2(&manifest).unwrap();
        assert_eq!(restarted.generation(), Generation(3));
        assert_eq!(
            restarted.reserve_join_generation_durable().unwrap(),
            Generation(4)
        );
        assert!(restarted.current_declarations().is_empty());
    }

    #[test]
    fn attached_v2_direct_define_uses_certified_private_graph_admission() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let mut lib = staged_test_lib(&root);
        lib.attach_recovery_graph_v2(&manifest).unwrap();
        assert_eq!(
            lib.define_batch(&["data DirectFlag = DirectFlag"]).unwrap(),
            Generation(1)
        );
        let retained = recovery::read_v2(&manifest, root.path()).unwrap().unwrap();
        assert!(retained.artifact_losses.is_empty());
        assert_eq!(retained.graph.nodes.len(), 1);
        assert_eq!(
            retained.graph.nodes[0].kind,
            recovery::RecoveryNodeKind::Authored
        );
        assert_eq!(retained.graph.nodes[0].id, Generation(1));
        assert!(retained.graph.public_surfaces.is_empty());
        assert!(!retained.graph.artifacts.is_empty());
        assert!(lib.retract("DirectFlag").is_err());
        assert_eq!(lib.scope_tip(ScopeId::ROOT), Generation(1));
        assert_eq!(lib.generation(), Generation(1));
    }

    #[test]
    fn attached_v2_failed_validation_and_reservation_keep_burned_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let mut lib = staged_test_lib(&root);
        lib.attach_recovery_graph_v2(&manifest).unwrap();
        let receipt = lib
            .declaration_receipt(&["answer :: Int\nanswer = missingHelper"])
            .unwrap()
            .unwrap();
        let candidate = lib
            .render_admitted_candidate_in(ScopeId::ROOT, &SourceImports::new(), &receipt, &[], &[])
            .unwrap();
        assert!(validate_declaration_candidate(candidate, lib.include_dir()).is_err());
        assert!(lib.log.is_reserved(Generation(1)));
        assert_eq!(lib.scope_tip(ScopeId::ROOT), Generation(0));
        drop(lib);
        let mut restarted = staged_test_lib(&root);
        restarted.attach_recovery_graph_v2(&manifest).unwrap();
        assert_eq!(restarted.generation(), Generation(1));
        assert_eq!(
            restarted.reserve_join_generation_durable().unwrap(),
            Generation(2)
        );

        let separate = tempfile::tempdir().unwrap();
        let path = separate.path().join("declarations.json");
        let mut failed = SessionLib::open(
            SessionId(992),
            separate.path().join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        failed.attach_recovery_graph_v2(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(failed.reserve_declaration_generation_durable().is_err());
        assert_eq!(failed.generation(), Generation(0));
    }

    #[test]
    fn legacy_recovery_does_not_replay_past_an_ambiguous_turn_or_reuse_its_modules() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("recovery.json");
        recovery::write(
            &manifest,
            41,
            &[
                recovery::RecoveryTurn::new(41, 1, vec!["data Old = Old".into()], Vec::new(), true),
                recovery::RecoveryTurn::new(41, 2, Vec::new(), vec!["Old".into()], true),
                recovery::RecoveryTurn::new(
                    41,
                    3,
                    vec!["later :: Int\nlater = 3".into()],
                    Vec::new(),
                    true,
                ),
            ],
        )
        .unwrap();
        let mut lib = staged_test_lib(&root);
        let report = lib.attach_recovery_manifest(&manifest).unwrap();
        assert!(report.replayed.is_empty());
        assert_eq!(report.lost.len(), 3);
        assert_eq!(lib.generation(), Generation(3));
        assert!(lib.declaration_value_type(3, "later").is_none());
        assert_eq!(
            lib.define_batch(&["fresh :: Int\nfresh = 4"]).unwrap(),
            Generation(4)
        );
        assert_eq!(lib.declaration_value_type(4, "fresh"), Some("Int"));
    }

    #[test]
    fn safe_legacy_replay_renumbers_and_rebinds_future_manifest_to_successor() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("recovery.json");
        recovery::write(
            &manifest,
            41,
            &[recovery::RecoveryTurn::new(
                41,
                3,
                vec!["answer :: Int\nanswer = 42".into()],
                Vec::new(),
                true,
            )],
        )
        .unwrap();
        let mut lib = staged_test_lib(&root);
        let report = lib.attach_recovery_manifest(&manifest).unwrap();
        assert_eq!(report.replayed.len(), 1);
        assert_eq!(report.replayed[0].source_generation, 3);
        assert_eq!(report.replayed[0].successor_generation, 4);
        assert_eq!(lib.declaration_value_type(4, "answer"), Some("Int"));
        assert_eq!(
            lib.define_batch(&["next :: Int\nnext = answer + 1"])
                .unwrap(),
            Generation(5)
        );
        let successor = recovery::read(&manifest).unwrap().unwrap();
        assert_eq!(successor.source_session, lib.id.0);
        assert_eq!(successor.turns.len(), 2);
        assert!(successor
            .turns
            .iter()
            .all(|turn| turn.origin_session == lib.id.0));
    }

    #[test]
    fn adopting_a_validated_candidate_commits_it_once_and_keeps_its_artifact() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("recovery.json");
        let mut lib = staged_test_lib(&root);
        lib.attach_recovery_manifest(&manifest)
            .expect("attach empty recovery manifest");
        let staged = validated_staged_answer(&lib);
        assert_eq!(
            staged.turn.value_types.get("answer").map(String::as_str),
            Some("Int")
        );
        assert_eq!(
            staged.turn.value_types.get("On").map(String::as_str),
            Some("Flag")
        );
        let module_path = root.path().join(staged.module().relative_hs_path());
        assert!(
            module_path.exists(),
            "validation wrote the candidate module"
        );

        assert_eq!(
            lib.adopt_staged_batch_with_receipt_and_vals_in(staged.clone(), &[])
                .expect("adopt the validated candidate"),
            Generation(1)
        );
        assert_eq!(lib.generation(), Generation(1));
        assert_eq!(lib.declaration_value_type(1, "answer"), Some("Int"));
        assert!(
            manifest.exists(),
            "adoption records the durable recovery turn"
        );
        assert!(lib.recovery_manifest_warning().is_none());

        lib.discard_staged(&staged);
        assert!(
            module_path.exists(),
            "a copied post-adoption token cannot delete a committed module"
        );
    }

    #[test]
    fn failed_staging_keeps_structured_diagnostics_and_publishes_no_types() {
        let root = tempfile::tempdir().unwrap();
        let lib = staged_test_lib(&root);
        let receipt = lib
            .declaration_receipt(&["bad :: Int\nbad = True"])
            .expect("extract declaration receipt")
            .expect("non-empty declaration receipt");
        let candidate =
            lib.render_candidate_in(ScopeId::ROOT, &SourceImports::new(), &receipt, &[], &[]);
        let error = validate_declaration_candidate(candidate, lib.include_dir())
            .expect_err("ill-typed declaration must fail staging");
        let SessionError::ValidationFailed(failure) = error else {
            panic!("expected structured validation failure, got {error:?}");
        };
        assert!(!failure.diagnostics.is_empty());
        assert_eq!(lib.generation(), Generation(0));
        assert_eq!(lib.declaration_value_type(1, "bad"), None);
    }

    #[test]
    fn later_data_declaration_shadows_a_type_referenced_by_a_live_value() {
        let root = tempfile::tempdir().unwrap();
        let mut lib = staged_test_lib(&root);
        let first =
            validated_staged_declaration(&lib, "data Version = OldVersion Int deriving Show");
        lib.adopt_staged_batch_with_receipt_and_vals_in(first, &[])
            .expect("commit the original Version declaration");

        let bind_template = TurnTemplate {
            kind: TemplateSelector::Bind,
            source: assemble_bind_module(
                concat!(
                    "{-# LANGUAGE DataKinds, TypeOperators #-}\n",
                    "module SessionBind where\n",
                    "import Tidepool.Prelude\n",
                    "import Tidepool.Effects\n",
                    "import Control.Monad.Freer (Eff)\n",
                    "import Tidepool.Session.Lib.G1\n",
                ),
                "",
                "__result",
                "'[]",
                "{{TURN_STMT}}",
                "{{BINDERS}}",
                false,
            ),
        };
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let includes = [
            root.path(),
            prelude.as_path(),
            effects[0].as_path(),
            effects[1].as_path(),
        ];
        let bound = run_turn(TurnRequest {
            exact_context: None,
            session_id: None,
            turn_text: "old <- pure (OldVersion 1)",
            templates: std::slice::from_ref(&bind_template),
            include: &includes,
            session_root: root.path(),
            inject_modules: &[],
            gen: 1,
            verdict: Some(TurnClassification {
                kind: TurnKind::Bind,
                binders: vec!["old".to_owned()],
                items: Vec::new(),
            }),
            target: None,
            retained_imports: &[],
        })
        .expect("compile the value against the original type");
        let TurnResult::Bind { bound, .. } = bound else {
            panic!("the original value must compile as a bind")
        };
        let injected = vec![bound[0].module.clone()];
        let receipt = lib
            .declaration_receipt(&["data Version = NewVersion Bool deriving Show"])
            .expect("extract replacement declaration")
            .expect("non-empty replacement declaration");
        let candidate = lib.render_candidate_in(
            ScopeId::ROOT,
            &SourceImports::new(),
            &receipt,
            &injected,
            &injected,
        );
        validate_declaration_candidate(candidate, lib.include_dir())
            .expect("a replacement type may shadow the type of a live value");
    }

    #[test]
    fn stale_candidate_cannot_remove_a_sibling_committed_artifact() {
        let root = tempfile::tempdir().unwrap();
        let mut lib = staged_test_lib(&root);
        let stale = validated_staged_declaration(&lib, "stale :: Int\nstale = 1");
        // A second checkout prepares and commits the same next generation
        // before the first candidate can adopt. Its module supersedes the
        // provisional artifact at that generation.
        let sibling = validated_staged_declaration(&lib, "winner :: Int\nwinner = 7");
        let module_path = root.path().join(sibling.module().relative_hs_path());
        assert_eq!(
            lib.adopt_staged_batch_with_receipt_and_vals_in(sibling, &[])
                .expect("adopt sibling candidate"),
            Generation(1)
        );
        assert!(
            std::fs::read_to_string(&module_path)
                .expect("read sibling module")
                .contains("winner = 7"),
            "the sibling committed source owns G1"
        );
        assert!(matches!(
            lib.adopt_staged_batch_with_receipt_and_vals_in(stale.clone(), &[]),
            Err(SessionError::StaleStagedDeclaration)
        ));
        lib.discard_staged(&stale);
        assert!(
            std::fs::read_to_string(&module_path)
                .expect("read sibling module after stale discard")
                .contains("winner = 7"),
            "a stale candidate cannot delete its sibling's committed module"
        );
    }
}
