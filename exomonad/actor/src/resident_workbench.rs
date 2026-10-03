//! Actor binding for the shared resident Haskell workbench.
//!
//! This adapter owns no machine and holds no checkout between calls. Each
//! fenced block checks out the actor's registered resident session, installs
//! the exact actor context, compiles and runs one segment on the blocking
//! pool, then restores the machine before the actor loop continues.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

#[cfg(test)]
mod display_callback_tests;
mod structured_introspection;
pub(crate) mod to_haskell;

use structured_introspection::StructuredIntrospectionAnswer;
#[cfg(test)]
use to_haskell::visit_usage_summary;
use to_haskell::{
    ActorContextProjection, CallStatus, ForkCleanupAnswer, LifecycleAnswer, ProgressAnswer,
    RouteStateAnswer,
};

use serde::Serialize;
use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, FromHaskell, HaskellVisitor, ToHaskell};
use tidepool_bridge_derive::FromHaskell as DeriveFromHaskell;
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{request_constructor, DispatchEffect};
use tidepool_repr::execution_schema::SymbolIdentity;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::registry::{CheckoutError, SessionRegistry};
use tidepool_runtime::session::{
    check_cell, hide_preamble_exports, insert_preamble_imports, render_turn_compile_rejection,
    resident_cell_check_template, resident_workbench_templates, run_inspections, run_turn,
    run_turn_pinned, validate_declaration_candidate, BoundBinder, CellCheck, CellCheckRequest,
    CheckedBinderPin, CheckedExpressionPlan, CompiledTurn, DeclarationCandidateRender,
    DeclarationReceipt, ExpressionPresentation, HostBindingAuthority, HostBindingType, HostCarrier,
    HostPayload, InspectionQuery, InspectionRequest, OutputSink, ParsedBlock,
    PendingPreparedInstall, PendingPreparedMode, ResidentContinuationEvent, ResidentError,
    ResidentHole, ResidentOutcome, ResidentResumeError, ResidentSession, RootCustody,
    SourceImports, StagedDeclaration, TurnClassification, TurnCode, TurnKind, TurnRequest,
    TurnResult,
};
use tidepool_runtime::{
    classify_compile, classify_session, spawn_blocking_in_span, CompileError, FailureClass,
};
use tracing::Instrument;

use crate::mailbox::{InstalledReceiver, KernelValue, ResidentOutbound, ResidentWaitRequest};
use crate::request_effect::{
    CancellationAcknowledgement, RepliesReq, ReplyAttempt, ReplyPoll, RequestCancellation,
    RequestReservation, RequestSubmission, ResponseAbandonment, ResponseForget, ResponsePoll,
    WatchForget, WatchPoll, WatchRegistration, WatchesReq,
};
use crate::{ActorCompileViewError, ResponseExpectation};

tokio::task_local! {
    static SLOT_CONTINUATION_OWNER: ParkedHoleAbortRegistration;
    static INVOCATION_CANCEL: Arc<std::sync::atomic::AtomicBool>;
    static EXECUTION_CONTROL: Arc<crate::WorkbenchExecutionControl>;
}

pub(crate) async fn with_invocation_cancellation<T>(
    cancel: Arc<std::sync::atomic::AtomicBool>,
    operation: impl std::future::Future<Output = T>,
) -> T {
    INVOCATION_CANCEL.scope(cancel, operation).await
}

pub(crate) async fn with_execution_control<T>(
    control: Arc<crate::WorkbenchExecutionControl>,
    operation: impl std::future::Future<Output = T>,
) -> T {
    let cancel = control.native_cancel();
    EXECUTION_CONTROL
        .scope(control, with_invocation_cancellation(cancel, operation))
        .await
}

pub(crate) fn execution_control() -> Option<Arc<crate::WorkbenchExecutionControl>> {
    EXECUTION_CONTROL.try_with(Arc::clone).ok()
}

impl ResponseExpectation {
    fn request_preamble(
        &self,
        preamble: &str,
        request: crate::RequestId,
        effects_alias: &str,
    ) -> String {
        let mut preamble = insert_preamble_imports(
            preamble,
            "qualified Tidepool.Agent.Reply.Internal as TidepoolReplies",
        );
        preamble = insert_preamble_imports(&preamble, "qualified Data.Void as TidepoolVoid");
        preamble = insert_preamble_imports(&preamble, "qualified Prelude as TidepoolPrelude");
        preamble.push_str(&format!(
            "\nsessionReply :: TidepoolReplies.Reply ({0})\nsessionReply = TidepoolReplies.Reply (TidepoolReplies.RequestId {1})\n{2}\nrespond = TidepoolReplies.reply sessionReply\n",
            self.expected_type(),
            request.0,
            self.respond_signature(effects_alias),
        ));
        if let Some(progress_type) = &self.progress_type {
            preamble.push_str(&format!(
                "\nreportProgress :: ({progress_type}) -> Eff {effects_alias} ()\nreportProgress value = do\n  outcome <- TidepoolReplies.reportRequestProgress (TidepoolReplies.ProgressSink (TidepoolReplies.RequestId {})) value\n  case outcome of\n    Left failure -> TidepoolPrelude.error (TidepoolPrelude.show failure)\n    Right () -> pure ()\n", request.0,
            ));
        }
        preamble
    }
}

fn actor_preamble(preamble: &str, context: &crate::ActorSessionContext) -> String {
    let preamble =
        insert_preamble_imports(preamble, "qualified Tidepool.Agent.Ref as TidepoolAgentRef");
    format!(
        "{preamble}\nme :: TidepoolAgentRef.AgentRef\nme = TidepoolAgentRef.internalAgentRef {} {}\n",
        context.actor.id.0, context.actor.incarnation.0
    )
}

// This header belongs to the trusted generated template, never authored cell text.
fn cell_module_preamble(
    preamble: &str,
    module: &str,
) -> Result<String, ResidentActorWorkbenchError> {
    let mut replaced = false;
    let rendered = preamble
        .split_inclusive('\n')
        .map(|line| {
            if !replaced
                && line.trim_start().starts_with("module ")
                && line.trim_end().ends_with(" where")
            {
                replaced = true;
                let newline = if line.ends_with('\n') { "\n" } else { "" };
                format!("module {module} where{newline}")
            } else {
                line.to_owned()
            }
        })
        .collect::<String>();
    if replaced {
        Ok(rendered)
    } else {
        Err(ResidentActorWorkbenchError::CompileInfrastructure(
            "resident cell preamble has no module declaration".into(),
        ))
    }
}

/// Add `hiding (names)` to the exact line in `imports` (a
/// [`crate::mount::ActorCompileView::turn_imports`]-shaped spec text, one
/// entry per line, no leading `import`) that unqualifiedly names
/// `library_module` bare — the shape it always has here, since this patch
/// only ever runs against the FIRST (unstaged) whole-cell preflight
/// attempt, before any per-item staging has had a chance to hide anything.
/// Returns `None` (no retry) when `names` is empty or that exact bare line
/// isn't found, so a caller that can't safely patch simply keeps the
/// original diagnostic instead of silently doing nothing.
fn hide_same_cell_collisions(
    imports: &str,
    library_module: &str,
    names: &[String],
) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let mut found = false;
    let hidden = names.join(", ");
    let patched = imports
        .lines()
        .map(|line| {
            if line == library_module {
                found = true;
                format!("{library_module} hiding ({hidden})")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    found.then_some(patched)
}

/// The same cell-check failure mapping every `prepare_cell` exit uses:
/// a genuine Haskell error becomes a rejectable [`CellCheck`] failure the
/// caller can present, anything else is infrastructure trouble.
/// When a cell GHC has already rejected has a declaration reaching for a name
/// an earlier statement binds, say so beside GHC's own diagnostics.
///
/// The scan that finds this shape is lexical and cannot see every way Haskell
/// binds a name (a case alternative, a record field, an operator containing
/// `<-`), so it explains a failure and never causes one: a cell GHC accepts is
/// never refused on its word. It speaks only when GHC's diagnostics name the
/// same binder, which keeps it quiet on failures it has nothing to do with.
fn explain_source_order(
    failure: &mut tidepool_runtime::session::CellCheckFailure,
    cell_source: &str,
) {
    let CompileError::Diagnostics(diagnostics) = &mut failure.error else {
        return;
    };
    let Some(collision) =
        tidepool_runtime::session::detect_hoisted_declaration_collision(cell_source)
    else {
        return;
    };
    if !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains(&collision.binder_name))
    {
        return;
    }
    let line = u32::try_from(collision.declaration_line).unwrap_or(u32::MAX);
    diagnostics.push(tidepool_toolchain::diag::ExtractDiag {
        span: Some(tidepool_toolchain::diag::DiagSpan {
            file: "<cell>".to_string(),
            start_line: line,
            start_col: 1,
            end_line: line,
            end_col: 1,
        }),
        severity: tidepool_toolchain::diag::DiagnosticSeverity::Warning,
        message: collision.message(),
    });
}

fn cell_check_error(
    failure: tidepool_runtime::session::CellCheckFailure,
    cell_source: &str,
) -> ResidentActorWorkbenchError {
    if classify_compile(&failure.error).class == FailureClass::UserHaskell {
        let mut failure = failure;
        explain_source_order(&mut failure, cell_source);
        ResidentActorWorkbenchError::CellCheck(failure)
    } else {
        let mut diagnostic = classify_compile(&failure.error);
        diagnostic.message =
            tidepool_runtime::session::render_cell_compile_error(&failure.error, cell_source);
        ResidentActorWorkbenchError::CompileInfrastructure(diagnostic)
    }
}

#[cfg(test)]
mod same_cell_collision_tests {
    use super::hide_same_cell_collisions;

    #[test]
    fn hides_the_named_collisions_from_the_bare_library_line() {
        let imports = "qualified Data.Set as Set\nTidepool.Session.Lib.G7\nqualified Tidepool.Inspection as TidepoolInspection";
        let patched =
            hide_same_cell_collisions(imports, "Tidepool.Session.Lib.G7", &["sh".to_string()])
                .expect("the bare library line is present and must be patched");
        assert_eq!(
            patched,
            "qualified Data.Set as Set\nTidepool.Session.Lib.G7 hiding (sh)\nqualified Tidepool.Inspection as TidepoolInspection"
        );
        // Every other line is untouched.
        assert!(patched.contains("qualified Data.Set as Set"));
        assert!(patched.contains("qualified Tidepool.Inspection as TidepoolInspection"));
    }

    #[test]
    fn multiple_collisions_join_into_one_hiding_clause() {
        let patched = hide_same_cell_collisions(
            "Tidepool.Session.Lib.G7",
            "Tidepool.Session.Lib.G7",
            &["sh".to_string(), "symA".to_string()],
        )
        .unwrap();
        assert_eq!(patched, "Tidepool.Session.Lib.G7 hiding (sh, symA)");
    }

    #[test]
    fn no_collisions_or_no_matching_line_means_no_retry() {
        assert_eq!(
            hide_same_cell_collisions("Tidepool.Session.Lib.G7", "Tidepool.Session.Lib.G7", &[]),
            None
        );
        // The library line isn't bare (already qualified/hidden some other
        // way) — patching it here could silently do the wrong thing, so this
        // conservatively declines the retry rather than guessing.
        assert_eq!(
            hide_same_cell_collisions(
                "qualified Tidepool.Session.Lib.G7 as Prev",
                "Tidepool.Session.Lib.G7",
                &["sh".to_string()],
            ),
            None
        );
    }
}

#[cfg(test)]
mod failure_diagnostic_tests {
    use super::*;

    #[test]
    fn preflight_infrastructure_failure_keeps_compiler_class_and_cause() {
        let failure =
            CompileError::MissingOutput(std::path::PathBuf::from("missing-output")).into();
        let error = cell_check_error(failure, "authored cell");
        let diagnostic = error
            .failure_diagnostic()
            .expect("compile failure classified");
        assert_eq!(diagnostic.class, FailureClass::Infra);
        assert_eq!(
            diagnostic.phase,
            tidepool_toolchain::failclass::Phase::Compile
        );
        assert_eq!(
            diagnostic.cause,
            Some(tidepool_toolchain::failclass::CompileFailureCause::MissingOutput)
        );
        assert!(error.to_string().contains("missing-output"));
        assert!(error
            .to_string()
            .starts_with("resident workbench compiler infrastructure failed:"));
    }
}

/// Trusted source environment supplied by actor deployment. The canonical
/// `ActorEffects` alias itself lives in the imported Haskell facade; Rust does
/// not reflect or authorize its row entries.
#[derive(Clone)]
pub struct ActorWorkbenchSource {
    preamble: Arc<str>,
    base_include: Arc<[PathBuf]>,
    workbench_imports: SourceImports,
    /// The workspace's `[haskell] spec` key, when it names one. Rule two of
    /// spec discovery; rule one is `AgentSpec.hs` in the run graph.
    spec: Option<Arc<str>>,
    workspace_modules: Arc<[String]>,
    installed_effect_support: Arc<[exomonad_tool::ToolEffectKey]>,
}

/// One prepared import environment for evaluation and inspection. Name
/// resolution comes from the exact lexical view before either path builds
/// a compiler request.
struct WorkbenchCompilation {
    preamble: String,
    imports: String,
    include: Vec<PathBuf>,
    injected: Vec<String>,
}

impl ActorWorkbenchSource {
    fn prepare(&self, scope: &crate::ActorCompileView) -> WorkbenchCompilation {
        WorkbenchCompilation {
            preamble: scope.shadow_preamble(&hide_preamble_exports(
                &self.preamble,
                &["print"]
                    .map(|name| tidepool_runtime::session::ExportItem::Value { name: name.into() }),
            )),
            imports: scope.turn_imports(),
            include: scope.include_paths(&self.base_include),
            injected: scope.injected_module_names(),
        }
    }

    fn prepare_effectful(
        &self,
        scope: &crate::ActorCompileView,
        effects: &str,
    ) -> Result<WorkbenchCompilation, ResidentActorWorkbenchError> {
        let mut prepared = self.prepare(scope);
        let shim = tidepool_mcp::ensure_selected_effects_shim(effects).map_err(|error| {
            ResidentActorWorkbenchError::ActorProtocol(format!("selected effect profile: {error}"))
        })?;
        prepared.include.insert(0, shim);
        Ok(prepared)
    }

    #[must_use]
    pub fn new(preamble: impl Into<Arc<str>>, base_include: Vec<PathBuf>) -> Self {
        Self {
            preamble: preamble.into(),
            base_include: base_include.into(),
            workbench_imports: SourceImports::from_specs([
                "qualified Data.Set as Set",
                "qualified Tidepool.Inspection as TidepoolInspection",
                "Tidepool.Inspection (display, expand, expansions)",
                "qualified Tidepool.Effects.Core",
            ]),
            spec: None,
            workspace_modules: Arc::from([]),
            installed_effect_support: Arc::from([]),
        }
    }

    /// Capture support from the actual assembled interpreter instances and
    /// installed host services, before admitting source-authored tools.
    #[must_use]
    pub fn with_installed_effect_support(
        mut self,
        keys: impl IntoIterator<Item = exomonad_tool::ToolEffectKey>,
    ) -> Self {
        let mut installed = Vec::new();
        for key in keys {
            if !installed.contains(&key) {
                installed.push(key);
            }
        }
        self.installed_effect_support = installed.into();
        self
    }

    pub(crate) fn installed_effect_support(&self) -> &[exomonad_tool::ToolEffectKey] {
        &self.installed_effect_support
    }

    /// Name the workspace-authored Haskell modules already compiled into
    /// every session and imported into every actor's preamble, so the doc
    /// catalog can list them beside the built-in topics. Absent for the
    /// operator workbench and tests, which have no `FrozenWorkspace`.
    #[must_use]
    pub fn with_workspace_modules(
        mut self,
        modules: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.workspace_modules = modules.into_iter().map(Into::into).collect();
        self
    }

    /// Preload the small quasiquoter vocabulary promised by a hosted
    /// workbench. Deployment selects this policy; the generic actor engine
    /// does not inspect input text or depend on an MCP-specific parser.
    #[must_use]
    pub fn with_default_quasiquoters(mut self) -> Self {
        self.workbench_imports
            .extend_text("Tidepool.QQ (fmt, j, patch, uri)");
        self
    }

    /// The import block every cell of this workbench compiles against, before
    /// a session view adds its own declaration and value modules. The compiler
    /// warm-up reads it so the graph it primes is the graph a cell reaches.
    #[must_use]
    pub fn workbench_import_text(&self) -> String {
        self.workbench_imports.template_text()
    }

    /// Additional shared imports must enter declaration compilation as well as
    /// expression templates, so bindings keep the same vocabulary after a fork.
    #[must_use]
    pub fn with_imports(mut self, imports: &str) -> Self {
        self.workbench_imports.extend_text(imports);
        self
    }

    /// Select the workspace's `[haskell] spec` value — rule two of spec
    /// discovery, for a workspace that wants a name other than
    /// `AgentSpec.agentSpec`.
    #[must_use]
    pub fn with_spec(mut self, entry: impl Into<Arc<str>>) -> Self {
        let entry = entry.into();
        if let Some((module, _)) = entry.rsplit_once('.') {
            self.workbench_imports
                .extend_text(&format!("qualified {module}"));
        }
        self.workbench_imports
            .extend_text("qualified Tidepool.Agent.Contract");
        self.spec = Some(entry);
        self
    }

    /// [`Self::with_spec`] when the workspace named one, and nothing when it
    /// did not.
    #[must_use]
    pub fn with_spec_if(self, entry: Option<&str>) -> Self {
        match entry {
            Some(entry) => self.with_spec(entry),
            None => self,
        }
    }
}

/// One installed spec: the surface it declares, the retained value every call
/// and every slot is an application of, and the identity of this install.
pub(crate) struct ResidentWorkbenchTools {
    pub(crate) declarations: Vec<exomonad_tool::HostedTool>,
    pub(crate) dispatch: Arc<RootCustody>,
    /// Installer source roots belong to the installed implementation, not the
    /// actor's published declaration and value surface.
    _installation_scope: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    /// Exact installer row retained with its rooted dispatcher.
    pub(crate) dispatcher_effects: String,
    /// Which slots the installed record fills, by name, as the same compile
    /// declared them.
    pub(crate) slots: Vec<String>,
    /// How the spec was found, and where. Reported in status and in every
    /// reload receipt.
    pub(crate) resolved: crate::agent_spec::ResolvedSpec,
    /// Which install this record is, counting from one within this actor
    /// incarnation. A completed call names it, so a receipt says which record
    /// served the call and not merely which record is active now.
    pub(crate) install: u64,
    /// The source revision this record was built from, when the actor has a
    /// layer of its own to name one.
    ///
    /// This says which installed record served a call and what revision that
    /// record was compiled against. It does NOT say that every function
    /// reachable through the call belongs to one revision, which would be a
    /// different and possibly false claim.
    pub(crate) revision: Option<String>,
}

/// Authority over one actor's installed handler and immutable source revision.
/// The handler root remains live while an issued request retains this value.
#[derive(Clone)]
pub struct InstalledToolLease {
    actor: crate::ActorRef,
    source: crate::CheckpointSourceLayer,
    tools: Option<Arc<ResidentWorkbenchTools>>,
}

impl InstalledToolLease {
    pub(crate) fn new(
        actor: crate::ActorRef,
        source: crate::CheckpointSourceLayer,
        tools: Option<Arc<ResidentWorkbenchTools>>,
    ) -> Self {
        Self {
            actor,
            source,
            tools,
        }
    }

    pub(crate) fn actor(&self) -> crate::ActorRef {
        self.actor
    }

    pub(crate) fn source(&self) -> &crate::CheckpointSourceLayer {
        &self.source
    }

    pub(crate) fn tools(&self) -> Option<&ResidentWorkbenchTools> {
        self.tools.as_deref()
    }

    pub(crate) fn tools_arc(&self) -> Option<Arc<ResidentWorkbenchTools>> {
        self.tools.clone()
    }

    pub(crate) fn with_source(&self, source: crate::CheckpointSourceLayer) -> Self {
        Self {
            source,
            ..self.clone()
        }
    }

    pub(crate) fn with_tools(&self, tools: Option<Arc<ResidentWorkbenchTools>>) -> Self {
        Self {
            tools,
            ..self.clone()
        }
    }
}

/// The actor's single current installation. Requests clone an immutable lease;
/// reload replaces only this pointer after publishing source or handler state.
#[derive(Default)]
pub(crate) struct InstalledToolsState(Mutex<Option<InstalledToolLease>>);

impl InstalledToolsState {
    pub(crate) fn current(&self) -> Option<InstalledToolLease> {
        self.0.lock().clone()
    }

    pub(crate) fn publish(&self, lease: InstalledToolLease) {
        *self.0.lock() = Some(lease);
    }

    pub(crate) fn current_tools(&self) -> Option<Arc<ResidentWorkbenchTools>> {
        self.current()?.tools_arc()
    }

    pub(crate) fn publish_source(
        &self,
        actor: crate::ActorRef,
        source: crate::CheckpointSourceLayer,
    ) {
        let mut current = self.0.lock();
        *current = Some(match current.as_ref() {
            Some(lease) => {
                assert_eq!(
                    lease.actor(),
                    actor,
                    "source publication belongs to the installed actor"
                );
                lease.with_source(source)
            }
            None => InstalledToolLease::new(actor, source, None),
        });
    }

    pub(crate) fn publish_tools(&self, tools: Option<Arc<ResidentWorkbenchTools>>) {
        let mut current = self.0.lock();
        if let Some(lease) = current.as_ref() {
            *current = Some(lease.with_tools(tools));
        }
    }

    pub(crate) fn clear(&self) {
        *self.0.lock() = None;
    }
}

pub(crate) use crate::tool_contract::{decode_installation, SpecInstallation};

impl ResidentWorkbenchTools {
    /// The provenance a completed call records: this record, and the revision
    /// it was built from.
    pub(crate) fn provenance(&self) -> String {
        format!(
            "install={} revision={} {}",
            self.install,
            self.revision.as_deref().unwrap_or("(run)"),
            self.resolved.describe()
        )
    }
}

/// Shared-machine registry shape used by actors. String holes are only the
/// registry's checkout index; obligation-carrying `ResidentHole` values stay
/// inside each running segment.
pub type ActorMachineRegistry<H, O> = SessionRegistry<ResidentSession<H, O>, String>;

/// Read-only counters for matched resident performance measurements.
///
/// Every value comes from the session's existing codegen, heap, or residency
/// authority. `None` means the prepared machine has not bootstrapped yet;
/// callers must preserve that distinction instead of reporting zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ResidentMachineMeasurement {
    pub native_functions: Option<u64>,
    pub native_code_bytes: Option<u64>,
    pub live_old_bytes: Option<usize>,
    pub major_collections: Option<u64>,
    pub programs: Option<usize>,
    pub block_words: Option<usize>,
    pub persistent_roots: Option<usize>,
    pub handles: Option<usize>,
    pub code_exports: Option<usize>,
    pub parked: Option<usize>,
    pub static_regions: Option<usize>,
    pub descriptor_rows: Option<usize>,
    pub callable_rows: Option<usize>,
    pub enter_rows: Option<usize>,
}

impl ResidentMachineMeasurement {
    fn of<H, O>(session: &ResidentSession<H, O>) -> Self
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let residency = session.residency();
        let codegen = session.codegen_totals();
        let heap = session.heap_stats();
        Self {
            native_functions: codegen.map(|(functions, _)| functions),
            native_code_bytes: codegen.map(|(_, bytes)| bytes),
            live_old_bytes: heap.map(|stats| stats.live_bytes),
            major_collections: heap.map(|stats| stats.gc_count),
            programs: residency.map(|counts| counts.programs),
            block_words: residency.map(|counts| counts.block_words),
            persistent_roots: residency.map(|counts| counts.persistent_roots),
            handles: residency.map(|counts| counts.handles),
            code_exports: residency.map(|counts| counts.code_exports),
            parked: residency.map(|counts| counts.parked),
            static_regions: residency.map(|counts| counts.static_regions),
            descriptor_rows: residency.map(|counts| counts.descriptor_rows),
            callable_rows: residency.map(|counts| counts.callable_rows),
            enter_rows: residency.map(|counts| counts.enter_rows),
        }
    }
}

/// Which built-in host carrier shape a mount fills. Each kind names one
/// fixed nominal type; a carrier built for a kind mounts any number of
/// values of that type with no further GHC call (see [`HostCarrier`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum HostCarrierKind {
    Job,
    Json,
    Text,
}

impl HostCarrierKind {
    fn host_binding_type(self) -> HostBindingType {
        match self {
            HostCarrierKind::Job => HostBindingType::COMMAND_JOB,
            HostCarrierKind::Json => HostBindingType::JSON_VALUE,
            HostCarrierKind::Text => HostBindingType::TEXT,
        }
    }

    fn type_name(self) -> &'static str {
        match self {
            HostCarrierKind::Job => COMMAND_JOB_TYPE_NAME,
            HostCarrierKind::Json => JSON_INPUT_TYPE_NAME,
            HostCarrierKind::Text => TEXT_BINDING_TYPE_NAME,
        }
    }

    fn anchor(self) -> &'static str {
        match self {
            HostCarrierKind::Job => COMMAND_JOB_ANCHOR,
            HostCarrierKind::Json => JSON_INPUT_ANCHOR,
            HostCarrierKind::Text => TEXT_BINDING_ANCHOR,
        }
    }

    fn imports(self) -> SourceImports {
        match self {
            HostCarrierKind::Job => command_job_carrier_imports(),
            HostCarrierKind::Json => json_input_carrier_imports(),
            HostCarrierKind::Text => text_binding_carrier_imports(),
        }
    }

    fn retain_text_constructor(self) -> bool {
        match self {
            HostCarrierKind::Job | HostCarrierKind::Text => true,
            HostCarrierKind::Json => false,
        }
    }

    /// The throwaway binding name the one compile that builds this kind's
    /// carrier uses. Discarded immediately: [`HostCarrier::from_compiled`]
    /// keeps only the compiled turn's table/program and the binder's shape,
    /// never this name or its generation.
    fn carrier_binding_name(self) -> &'static str {
        match self {
            HostCarrierKind::Job => "__tidepoolJobCarrier",
            HostCarrierKind::Json => "__tidepoolJsonCarrier",
            HostCarrierKind::Text => "__tidepoolTextCarrier",
        }
    }
}

/// A carrier this workbench already built, and the ordered source revisions it
/// was built against.
struct CachedHostCarrier {
    revision: Vec<Option<String>>,
    carrier: Arc<HostCarrier>,
}

/// Carriers this workbench has built, by kind, shared for as long as the
/// owning [`ResidentActorRunner`]/[`ResidentActorWorkbench`] lineage lives —
/// see [`ResidentMachineAccess::sharing`].
///
/// The key is the kind alone, not the session, because a carrier carries no
/// session state: each kind compiles a fixed anchor against fixed imports
/// (never the actor's own view), and its constructor ids are content hashes
/// of their qualified names (`tidepool_repr::datacon_table`), so the same
/// carrier installs into any session or restart; a genuine id collision is
/// a loud `merge_table` error, never a silent overwrite.
type HostCarrierCache =
    Arc<std::sync::Mutex<std::collections::BTreeMap<HostCarrierKind, CachedHostCarrier>>>;

/// Shared checkout boundary for every actor machine entry path. Fenced
/// fragments and installed actor programs differ above this layer, but use
/// exactly the same admission and settlement mechanism. Every checkout
/// installs the supplied actor context before invoking its operation, so a
/// child cannot leak its lexical scope, runtime resource scope, or principal into the
/// next parent or sibling entry.
///
/// An immutable observation of the actual handler instances in a checkout.
pub type HandlerEffectSupport<H> =
    Arc<dyn Fn(&H) -> Vec<exomonad_tool::ToolEffectKey> + Send + Sync + 'static>;

struct ResidentMachineAccess<H, O> {
    machines: Arc<ActorMachineRegistry<H, O>>,
    source: ActorWorkbenchSource,
    handler_effect_support: HandlerEffectSupport<H>,
    /// Builds a fresh, idle machine for a session id this host has decided
    /// deserves its own — installed once by the composition root; `None`
    /// keeps every launch on the shared session, unchanged, which is also
    /// today's behavior everywhere this hasn't been wired up yet. See
    /// [`crate::start::child_session_eligibility`] for the one caller that
    /// consults this.
    child_session_factory: Option<ChildSessionFactory<H, O>>,
    /// The compiled turn a fresh child session bootstraps with before
    /// anything imports into it — the SAME program the composition root
    /// itself bootstrapped with (`bridge/facade`'s `compile_root`), so a
    /// child's image matches the parent's for whatever a transferred entry's
    /// constituent objects need to resolve. Installed once by the
    /// composition root, alongside [`Self::child_session_factory`]; `None`
    /// makes [`ResidentActorRunner::provision_child_session`] refuse
    /// (there's a factory but nothing to bootstrap the machine it builds
    /// with — `ResidentSession::import_parcel` and `set_image_registry` both
    /// require an already-bootstrapped engine).
    child_bootstrap_program: Option<Arc<tidepool_runtime::session::CompiledTurn>>,
    /// This run's shared [`tidepool_runtime::session::ImageRegistry`], if
    /// the composition root installed one — `None` leaves every session
    /// compiling its own images, unchanged. Applied to a session's engine
    /// on every checkout ([`Self::with_host_machine`]): a no-op before that
    /// session's own first turn bootstraps its engine (there is nothing yet
    /// to share an image with — see
    /// [`tidepool_runtime::session::ResidentSession::set_image_registry`]),
    /// so a session's bootstrap install is never a hit, but every later
    /// install on that session, root or child alike, can be.
    image_registry: Option<Arc<tidepool_runtime::session::ImageRegistry>>,
    /// Sessions `provision_child_session` built — a launch's OWN
    /// dedicated machine, as opposed to the run's single shared session
    /// (never a member here, and never torn down). Membership is what makes
    /// a session id eligible for `ResidentActorRunner::retire_child_session`
    /// at all.
    child_sessions: Arc<std::sync::Mutex<std::collections::HashSet<tidepool_repr::SessionId>>>,
    /// Dedicated child sessions whose actor has retired but whose machine
    /// still held at least one live value handle. Each entry owns the one
    /// cleanup worker and its custody-drop signal until checkout settlement
    /// removes the entry.
    pending_child_teardown: Arc<
        std::sync::Mutex<
            std::collections::HashMap<tidepool_repr::SessionId, Arc<PendingChildTeardown>>,
        >,
    >,
    // The `compile_blocking` span omits `include_roots` from every line (the
    // full search path is long and rarely changes turn to turn); this tracks
    // the last-logged roots per session so a diagnostic reader still sees
    // them once, and again whenever they actually change.
    logged_include_roots:
        std::sync::Mutex<std::collections::HashMap<tidepool_repr::SessionId, String>>,
    /// Compiled host carriers, by kind, built off checkout on first use and
    /// reused by every later mount of that kind — see
    /// [`ResidentActorWorkbench::carrier_for`]. Invalidated the same way
    /// `prepare_tools` invalidates its compiled record: by the actor's own
    /// source-layer revision, so an edited layer is never served through a
    /// carrier compiled against its stale imports.
    carriers: HostCarrierCache,
}

/// One detached cleanup owner for a retired dedicated child session. The
/// worker handle remains here so tests can prove the actual task finished,
/// rather than treating session removal alone as sufficient evidence.
struct PendingChildTeardown {
    wake: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    initial_checkout_complete: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    worker: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PendingChildTeardown {
    fn new() -> Self {
        Self {
            wake: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            initial_checkout_complete: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            worker: std::sync::Mutex::new(None),
        }
    }
}

fn reject_activation_declaration<H, O>(
    session: &ResidentSession<H, O>,
    scope: ScopeId,
) -> Result<(), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if session
        .current_decl_heads_in(scope)
        .iter()
        .any(|(name, _)| name == "sessionInput")
    {
        return Err(ResidentActorWorkbenchError::InputMount(
            "sessionInput is reserved for activation input; remove its authored declaration before activating".into(),
        ));
    }
    Ok(())
}

struct CancelCompilerTransactionOnDrop(Option<tidepool_runtime::CompilerTransactionCancellation>);

impl Drop for CancelCompilerTransactionOnDrop {
    fn drop(&mut self) {
        if let Some(cancellation) = self.0.take() {
            cancellation.cancel();
        }
    }
}

impl<H, O> ResidentMachineAccess<H, O> {
    fn new(machines: Arc<ActorMachineRegistry<H, O>>, source: ActorWorkbenchSource) -> Self {
        Self {
            machines,
            source,
            handler_effect_support: Arc::new(|_| Vec::new()),
            child_session_factory: None,
            child_bootstrap_program: None,
            image_registry: None,
            child_sessions: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            pending_child_teardown: Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            logged_include_roots: std::sync::Mutex::new(std::collections::HashMap::new()),
            carriers: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
        }
    }

    /// Another entry point into the same machine registry, source, and
    /// carrier cache as `self` — used wherever the runner mints another
    /// workbench or runner for the same actor rather than an independent
    /// one, so a carrier built for one turn serves every later turn instead
    /// of being rebuilt each time a fresh `ResidentActorWorkbench` is
    /// constructed per call.
    fn sharing(&self) -> Self {
        Self {
            machines: Arc::clone(&self.machines),
            source: self.source.clone(),
            handler_effect_support: self.handler_effect_support.clone(),
            child_session_factory: self.child_session_factory.clone(),
            child_bootstrap_program: self.child_bootstrap_program.clone(),
            image_registry: self.image_registry.clone(),
            child_sessions: Arc::clone(&self.child_sessions),
            pending_child_teardown: Arc::clone(&self.pending_child_teardown),
            logged_include_roots: std::sync::Mutex::new(std::collections::HashMap::new()),
            carriers: Arc::clone(&self.carriers),
        }
    }

    fn owns_pending_child_teardown(
        &self,
        session_id: tidepool_repr::SessionId,
        owner: &Arc<PendingChildTeardown>,
    ) -> bool {
        self.pending_child_teardown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .is_some_and(|current| Arc::ptr_eq(current, owner))
    }

    /// Complete terminal child cleanup under the same membership/owner lock
    /// order as retirement admission. An older worker cannot erase a different
    /// pending owner; terminal checkout settlement may complete the current one.
    fn finish_child_session_teardown(
        &self,
        session_id: tidepool_repr::SessionId,
        expected_owner: Option<&Arc<PendingChildTeardown>>,
    ) -> bool {
        let (removed_child, owner) = {
            let mut children = self
                .child_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut pending = self
                .pending_child_teardown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if expected_owner.is_some_and(|owner| {
                !pending
                    .get(&session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, owner))
            }) {
                return false;
            }
            (children.remove(&session_id), pending.remove(&session_id))
        };
        if let Some(owner) = &owner {
            owner.wake.notify_one();
        }
        removed_child || owner.is_some()
    }
}

/// Builds a fresh, idle [`ResidentSession`] for a session id the caller has
/// already minted — the composition root's one factory, installed once and
/// called by [`ResidentActorRunner::provision_child_session`] with the child's
/// fixed source layer. `Fn`, not `FnOnce`: called once per eligible child for
/// the life of the host.
pub type ChildSessionFactory<H, O> = Arc<
    dyn Fn(tidepool_repr::SessionId, &[PathBuf]) -> Result<Box<ResidentSession<H, O>>, String>
        + Send
        + Sync,
>;

/// Keeps `prepare_cell`'s split-mounted request-input host binding alive
/// (leased, never retired) across every checkout the split releases and
/// re-acquires, and guarantees it is retired exactly once. A normal exit
/// path calls [`Self::retire`] directly, folding the retirement into a
/// checkout already in hand. If this guard is instead dropped still armed —
/// an error propagated through `?`, or `prepare_cell`'s own future being
/// cancelled — [`Drop`] spawns one more checkout in the background to
/// retire the binding there, since `Drop` cannot itself run the `async`
/// checkout. Not generic over `H`/`O`: a `Drop` impl cannot add bounds
/// beyond the type's own definition, so the checkout this performs is
/// captured, fully monomorphized, as a boxed closure at construction time
/// instead (see [`Self::new`]).
struct HostInputRetirement {
    context: crate::ActorSessionContext,
    input: MountedHostInput,
    // Retained only so the leased identity survives at least until
    // `retire` (or the background cleanup) removes its owner; dropping the
    // lease needs no checkout of its own.
    lease: Option<tidepool_runtime::session::resident::BindingLease>,
    retired: bool,
    background_retire: Box<dyn FnOnce() + Send>,
}

impl HostInputRetirement {
    fn new<H, O>(
        access: &ResidentMachineAccess<H, O>,
        context: crate::ActorSessionContext,
        input: MountedHostInput,
        lease: tidepool_runtime::session::resident::BindingLease,
    ) -> Self
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        let background_retire: Box<dyn FnOnce() + Send> = {
            let machines = Arc::clone(&access.machines);
            let source = access.source.clone();
            let context = context.clone();
            let binder = input.binder.clone();
            Box::new(move || {
                tokio::spawn(async move {
                    let access = ResidentMachineAccess::new(machines, source);
                    if let Err(error) = access
                        .with_machine(context, move |session, ctx, _| {
                            let session_root =
                                carrier_mount_session_root(session, ctx.placement.lexical_scope)?;
                            session.retire_host_binding_owner(&session_root, &binder);
                            Ok(())
                        })
                        .await
                    {
                        tracing::warn!(
                            %error,
                            "failed to retire an abandoned split cell's request input binding"
                        );
                    }
                });
            })
        };
        Self {
            context,
            input,
            lease: Some(lease),
            retired: false,
            background_retire,
        }
    }

    fn mounted_input(&self) -> &MountedHostInput {
        &self.input
    }

    /// Retire the mounted binding through the checkout `access` gives,
    /// consuming this guard so its `Drop` never spawns background cleanup
    /// afterward.
    async fn retire<H, O>(mut self, access: &ResidentMachineAccess<H, O>)
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        self.retired = true;
        self.lease = None;
        let binder = self.input.binder.clone();
        let context = self.context.clone();
        if let Err(error) = access
            .with_machine(context, move |session, ctx, _| {
                let session_root =
                    carrier_mount_session_root(session, ctx.placement.lexical_scope)?;
                session.retire_host_binding_owner(&session_root, &binder);
                Ok(())
            })
            .await
        {
            tracing::warn!(%error, "failed to retire a split cell's request input binding");
        }
    }
}

impl Drop for HostInputRetirement {
    fn drop(&mut self) {
        if self.retired {
            return;
        }
        let background = std::mem::replace(&mut self.background_retire, Box::new(|| {}));
        background();
    }
}

/// Owns one invocation's exact parked continuations across machine checkouts.
/// A slot can suspend a nested helper while its earlier hole remains parked,
/// so every created id stays owned until the session reports its retirement.
/// The registration is cloned into blocking checkouts; a late suspension
/// observes abandonment and queues cleanup of only that invocation's ids.
pub(crate) struct ParkedHoleAbortGuard {
    shared: Arc<ParkedHoleAbortState>,
}

struct ParkedHoleAbortState {
    owner: Option<(crate::ActorRef, crate::ActorPlacement)>,
    abort: Arc<dyn Fn(String) + Send + Sync>,
    state: Mutex<ParkedHoleState>,
    reason: String,
    retained_authority: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

enum ParkedHoleState {
    Owned(std::collections::BTreeSet<String>),
    Abandoned(std::collections::BTreeSet<String>),
    Settled,
}

impl ParkedHoleAbortGuard {
    fn new<H, O>(
        access: &ResidentMachineAccess<H, O>,
        context: crate::ActorSessionContext,
        cont_id: String,
        reason: String,
    ) -> Self
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        Self::with_latest(access, context, Some(cont_id), reason)
    }

    fn with_latest<H, O>(
        access: &ResidentMachineAccess<H, O>,
        context: crate::ActorSessionContext,
        latest: Option<String>,
        reason: String,
    ) -> Self
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        Self::with_retained_latest(access, context, latest, reason, None)
    }

    fn with_retained_latest<H, O>(
        access: &ResidentMachineAccess<H, O>,
        context: crate::ActorSessionContext,
        latest: Option<String>,
        reason: String,
        retained_authority: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> Self
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        let machines = Arc::clone(&access.machines);
        let source = access.source.clone();
        let cleanup_context = context.clone();
        let cleanup_reason = reason.clone();
        let runtime = tokio::runtime::Handle::current();
        let shared = Arc::new_cyclic(|weak: &std::sync::Weak<ParkedHoleAbortState>| {
            let weak = weak.clone();
            let abort: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |cont_id| {
                let machines = Arc::clone(&machines);
                let source = source.clone();
                let context = cleanup_context.clone();
                let reason = cleanup_reason.clone();
                let cleanup_id = cont_id.clone();
                let owner = weak.upgrade();
                runtime.spawn(async move {
                    let access = ResidentMachineAccess::new(machines, source);
                    let result = access
                        .with_machine(context, move |session, _, _| {
                            abort_owned_hole(session, cont_id.clone(), reason)
                        })
                        .await;
                    if let Err(error) = result {
                        tracing::warn!(cont_id = %cleanup_id, %error, "failed to abort an abandoned continuation");
                    } else if let Some(owner) = owner {
                        ParkedHoleAbortRegistration(owner)
                            .observe(ResidentContinuationEvent::Retired(cleanup_id));
                    }
                });
            });
            ParkedHoleAbortState {
                owner: Some((context.actor, context.placement)),
                abort,
                state: Mutex::new(ParkedHoleState::Owned(latest.into_iter().collect())),
                reason,
                retained_authority,
            }
        });
        Self { shared }
    }

    pub(crate) fn registration(&self) -> ParkedHoleAbortRegistration {
        ParkedHoleAbortRegistration(Arc::clone(&self.shared))
    }

    /// The owning checkout is in hand: the hole's fate is now decided
    /// synchronously within it, so no background cleanup is needed.
    pub(crate) fn disarm(self) {
        let mut state = self.shared.state.lock();
        *state = ParkedHoleState::Settled;
        drop(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationHandoffFailure {
    WrongOwner,
    Abandoned,
    Settled,
    NotOwned,
}

impl Drop for ParkedHoleAbortGuard {
    fn drop(&mut self) {
        let abandoned = {
            let mut state = self.shared.state.lock();
            match std::mem::replace(&mut *state, ParkedHoleState::Settled) {
                ParkedHoleState::Owned(current) => {
                    *state = ParkedHoleState::Abandoned(current.clone());
                    current
                }
                ParkedHoleState::Abandoned(current) => {
                    *state = ParkedHoleState::Abandoned(current);
                    std::collections::BTreeSet::new()
                }
                ParkedHoleState::Settled => std::collections::BTreeSet::new(),
            }
        };
        for cont_id in abandoned {
            (self.shared.abort)(cont_id);
        }
    }
}

pub(crate) struct ParkedHoleAbortRegistration(Arc<ParkedHoleAbortState>);

impl Clone for ParkedHoleAbortRegistration {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl ParkedHoleAbortRegistration {
    fn abandon_for_acknowledgement(
        &self,
        context: &crate::ActorSessionContext,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let owner_matches = self.0.owner.is_some_and(|(actor, placement)| {
            actor == context.actor && placement == context.placement
        });
        let private_matches = self
            .0
            .retained_authority
            .as_ref()
            .and_then(|authority| {
                authority.downcast_ref::<crate::resident_actor::ExecutionResourceOwners>()
            })
            .is_some_and(|resources| resources.authorizes_cleanup_context(context));
        if !owner_matches && !private_matches {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "continuation cleanup differs from its admitted actor placement".into(),
            ));
        }
        let mut state = self.0.state.lock();
        if let ParkedHoleState::Owned(current) = &mut *state {
            *state = ParkedHoleState::Abandoned(std::mem::take(current));
        }
        Ok(())
    }

    fn awaiting_acknowledgement(&self) -> Vec<String> {
        match &*self.0.state.lock() {
            ParkedHoleState::Owned(current) | ParkedHoleState::Abandoned(current) => {
                current.iter().cloned().collect()
            }
            ParkedHoleState::Settled => Vec::new(),
        }
    }

    fn confirm_acknowledgement_in_checkout(&self) -> Result<(), ResidentActorWorkbenchError> {
        let mut state = self.0.state.lock();
        match &*state {
            ParkedHoleState::Owned(current) | ParkedHoleState::Abandoned(current)
                if !current.is_empty() =>
            {
                Err(ResidentActorWorkbenchError::ActorProtocol(
                    "native continuation cleanup remains unconfirmed".into(),
                ))
            }
            _ => {
                *state = ParkedHoleState::Owned(Default::default());
                Ok(())
            }
        }
    }

    pub(crate) async fn scope<F: std::future::Future>(&self, operation: F) -> F::Output {
        SLOT_CONTINUATION_OWNER.scope(self.clone(), operation).await
    }

    pub(crate) fn sync_scope<T>(&self, operation: impl FnOnce() -> T) -> T {
        SLOT_CONTINUATION_OWNER.sync_scope(self.clone(), operation)
    }

    fn observe(&self, event: ResidentContinuationEvent) {
        let abort = {
            let mut state = self.0.state.lock();
            match event {
                ResidentContinuationEvent::Parked(cont_id) => match &mut *state {
                    ParkedHoleState::Owned(current) => {
                        current.insert(cont_id);
                        None
                    }
                    ParkedHoleState::Abandoned(current) => {
                        current.insert(cont_id.clone()).then_some(cont_id)
                    }
                    ParkedHoleState::Settled => None,
                },
                ResidentContinuationEvent::Retired(cont_id) => {
                    match &mut *state {
                        ParkedHoleState::Owned(current) | ParkedHoleState::Abandoned(current) => {
                            current.remove(&cont_id);
                        }
                        ParkedHoleState::Settled => {}
                    }
                    None
                }
            }
        };
        if let Some(cont_id) = abort {
            (self.0.abort)(cont_id);
        }
    }

    /// Transfer ownership to the exact continuation returned by this
    /// checkout. If the async owner was dropped while the blocking operation
    /// ran, settle the late successor before releasing the checked-out
    /// session.
    fn replace_in_checkout<H, O>(
        &self,
        session: &mut ResidentSession<H, O>,
        outcome: &ResidentOutcome,
    ) where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let successor = outcome_continuation_id(outcome);
        let abandoned = {
            let mut state = self.0.state.lock();
            match std::mem::replace(&mut *state, ParkedHoleState::Settled) {
                ParkedHoleState::Abandoned(_) => {
                    *state = ParkedHoleState::Abandoned(successor.clone().into_iter().collect());
                    true
                }
                ParkedHoleState::Owned(_) => {
                    *state = successor
                        .clone()
                        .map_or(ParkedHoleState::Settled, |cont_id| {
                            ParkedHoleState::Owned(std::iter::once(cont_id).collect())
                        });
                    false
                }
                ParkedHoleState::Settled => false,
            }
        };
        if abandoned {
            let reason = self.0.reason.clone();
            if let Some(cont_id) = successor {
                if let Err(error) = abort_owned_hole(session, cont_id.clone(), reason) {
                    tracing::warn!(%cont_id, %error, "failed to abort a late continuation after its owner was dropped");
                    (self.0.abort)(cont_id);
                } else {
                    self.observe(ResidentContinuationEvent::Retired(cont_id));
                }
            }
        }
    }
}

fn outcome_continuation_id(outcome: &ResidentOutcome) -> Option<String> {
    match outcome {
        ResidentOutcome::Suspended { hole, .. } | ResidentOutcome::Deferred { hole, .. } => {
            Some(hole.cont_id().to_owned())
        }
        ResidentOutcome::Completed { .. } | ResidentOutcome::BindingsCommitted { .. } => None,
    }
}

fn abort_owned_hole<H, O>(
    session: &mut ResidentSession<H, O>,
    cont_id: String,
    reason: String,
) -> Result<(), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if !session.parked_holes().contains(&cont_id.as_str()) {
        return Ok(());
    }
    let outcome = session.abort(&cont_id, reason);
    if !session.parked_holes().contains(&cont_id.as_str()) {
        return Ok(());
    }
    match outcome {
        Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
        Ok(_) => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
            "aborting continuation {cont_id} left it parked"
        ))),
    }
}

/// Concrete resident workbench for one typed agent-session obligation.
pub struct ResidentActorWorkbench<H, O> {
    access: ResidentMachineAccess<H, O>,
    response: Option<ResponseExpectation>,
    request: Option<crate::RequestId>,
    type_modules: Arc<[String]>,
    json_input: Option<serde_json::Value>,
    compilation_authority: Option<Arc<crate::resident_actor::WorkbenchCompilationAuthority>>,
    private_execution: Option<Arc<ExecutionPrivateScope>>,
}

#[derive(tidepool_bridge_derive::ToHaskell)]
enum HostCommandJob {
    #[haskell(module = "Tidepool.Command.Types", name = "Job")]
    Job(String),
}

/// Live execution state for one workbench item that suspended on an actor
/// effect. The continuation and any value it binds remain owned by the
/// actor's resource scope; this value only carries the item-local rendering
/// state needed while the actor interpreter settles nominal effects.
pub(crate) struct ResidentWorkbenchFragment {
    display: WorkbenchDisplay,
    output: Vec<String>,
    presented: Vec<String>,
    recovered_jobs: Vec<String>,
    warnings: Vec<String>,
    /// Job ids returned by `Cmd.start` effects resolved while this exact
    /// item's Haskell computation was running. A single unambiguous job here
    /// against a single command-job-typed binder in `display` identifies
    /// the authored job binding — see
    /// `settle_fragment`'s tagging of `host_text_bindings` on completion.
    started_jobs: Vec<String>,
}

impl ResidentWorkbenchFragment {
    pub(crate) fn summarizes_bound_commands(&self) -> bool {
        matches!(&self.display, WorkbenchDisplay::Binding(_))
    }

    pub(crate) fn retain_job_binding(&mut self, binding: String) {
        self.recovered_jobs.push(binding);
    }

    /// Record that a `Cmd.start` effect resolved to this job id while this
    /// item's computation was running. Only tagged onto a binding at
    /// completion when it is the item's sole started job and its sole
    /// command-job-typed binder — see `settle_fragment`.
    pub(crate) fn record_started_job(&mut self, job: String) {
        self.started_jobs.push(job);
    }

    /// Charge streamed output and the eventual value display to the same
    /// allowance. Transport limits count bytes; Haskell tree limits count chars.
    pub(crate) fn present_output(&mut self, text: &str, remaining: &mut usize) -> String {
        let text = crate::workbench_display::bounded_output(text, *remaining);
        *remaining = remaining.saturating_sub(text.len() + 1);
        if let WorkbenchDisplay::Observation { budget, .. } = &mut self.display {
            *budget = budget.saturating_sub(text.chars().count() + 1);
        }
        text
    }

    pub(crate) fn present_command(
        &mut self,
        job: String,
        presentation: tidepool_bridge_effects::CommandPresentation,
        remaining: &mut usize,
    ) -> String {
        if !self.presented.contains(&job) {
            self.presented.push(job);
        }
        if let tidepool_bridge_effects::CommandPresentation::CommandVisible(text, _) = presentation
        {
            self.present_output(&text, remaining)
        } else {
            String::new()
        }
    }
}

#[derive(Clone)]
struct ProtectedObservation {
    prefix: Arc<tidepool_runtime::session::RuntimeCheckedPrefix>,
    execution: Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
    binder: BoundBinder,
}

fn protected_observation(
    compiled: &CompiledTurn,
    bound: &[BoundBinder],
) -> Result<Option<ProtectedObservation>, ResidentActorWorkbenchError> {
    let Some(execution) = compiled
        .certification
        .as_ref()
        .and_then(|certificate| certificate.checked_execution())
    else {
        return Ok(None);
    };
    if execution.item().kind() != tidepool_toolchain::checked_cell::CheckedItemKind::Expression {
        return Ok(None);
    }
    let prefix = compiled
        .certification
        .as_ref()
        .and_then(|certificate| certificate.checked_prefix())
        .ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "checked observation lacks its private prefix".into(),
            )
        })?;
    let [binder] = bound else {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "checked observation has no exact single capture binder".into(),
        ));
    };
    if execution.observation_name() != Some(binder.name.as_str()) {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "checked observation capture name differs".into(),
        ));
    }
    Ok(Some(ProtectedObservation {
        prefix: prefix.clone(),
        execution: execution.clone(),
        binder: binder.clone(),
    }))
}

/// Matched-build envelope. The authored output stays inside `Success`;
/// refusals never become values of a tool's advertised output schema.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolDispatchReply {
    Success {
        output: serde_json::Value,
    },
    Refused {
        #[serde(flatten)]
        error: ToolDispatchError,
    },
}

#[derive(Debug, serde::Deserialize, serde::Serialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolDispatchError {
    #[error("{error}")]
    UnknownTool {
        #[serde(skip_serializing)]
        error: String,
        tool: String,
    },
    #[error("{error}")]
    InvalidInput {
        #[serde(skip_serializing)]
        error: String,
        tool: String,
        detail: String,
    },
}

impl ToolDispatchReply {
    pub fn into_output(self) -> Result<serde_json::Value, ToolDispatchError> {
        match self {
            Self::Success { output } => Ok(output),
            Self::Refused { error } => Err(error),
        }
    }
}
impl ToolDispatchError {
    pub fn tool(&self) -> &str {
        match self {
            Self::UnknownTool { tool, .. } | Self::InvalidInput { tool, .. } => tool,
        }
    }
}

enum WorkbenchDisplay {
    Binding(Vec<BoundBinder>),
    Opaque,
    Tool,
    ToolDispatch,
    Observation {
        name: String,
        budget: usize,
        presentation: ExpressionPresentation,
        source: ActorWorkbenchSource,
        type_modules: Vec<String>,
        protected: Option<ProtectedObservation>,
    },
}

#[derive(Clone, Copy)]
struct RequestWorkbenchScope<'a> {
    response: Option<&'a ResponseExpectation>,
    request: Option<crate::RequestId>,
    type_modules: &'a [String],
}

impl RequestWorkbenchScope<'_> {
    fn source(
        &self,
        source: &ActorWorkbenchSource,
        context: &crate::ActorSessionContext,
    ) -> ActorWorkbenchSource {
        let mut source = source.clone();
        source.preamble = match (self.response, self.request) {
            (Some(response), Some(request)) => {
                response.request_preamble(&source.preamble, request, &context.haskell_effects_alias)
            }
            (None, None) => source.preamble.to_string(),
            _ => unreachable!("request workbench scope is constructed atomically"),
        }
        .into();
        source.preamble = actor_preamble(&source.preamble, context).into();
        source
    }
}

pub(crate) enum ResidentWorkbenchStep {
    Committed {
        output: String,
        warnings: Vec<String>,
        installed_bindings: Vec<String>,
    },
    Rejected(tidepool_runtime::session::CompileRejection),
    Running {
        fragment: Box<ResidentWorkbenchFragment>,
        outcome: Box<ResidentOutcome>,
    },
    Replied {
        request: crate::RequestId,
        result: RootCustody,
        preview: Option<String>,
    },
    CancellationAcknowledged {
        request: crate::RequestId,
    },
}

/// The one machine-entry component for installed actor program segments.
/// It shares checkout/context installation with the fenced workbench; startup
/// and later mailbox scheduling therefore cannot grow a second dispatcher.
pub struct ResidentActorRunner<H, O> {
    access: ResidentMachineAccess<H, O>,
}

/// Preparation owns a fresh session until an exact actor takes custody. The
/// runner's existing synchronous discard operation also covers a dropped await.
pub(crate) struct ChildSessionStartupLease {
    discard: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl ChildSessionStartupLease {
    pub(crate) fn admitted(mut self) {
        self.discard = None;
    }
}

impl Drop for ChildSessionStartupLease {
    fn drop(&mut self) {
        if let Some(discard) = self.discard.take() {
            discard();
        }
    }
}

pub(crate) struct ExecutionPrivateScope {
    pub owner: Arc<crate::resident_actor::WorkbenchPublicOwner>,
    pub public_scope: tidepool_codegen::scope::ScopeId,
    pub private_scope: tidepool_codegen::scope::ScopeId,
    pub admitted_public: tidepool_runtime::session::PublicVisibilitySnapshot,
    pub decision: Arc<tidepool_runtime::session::PublicationDecision>,
    pub admission: Arc<tidepool_runtime::session::PrivateExecutionAdmission>,
}

pub(crate) enum PrivateExecutionPublication {
    Manifest(tidepool_runtime::session::PublicManifestCommit),
    Rejected {
        reason: tidepool_toolchain::declaration_join::JoinRejection,
        diagnostic: String,
    },
}

enum PreparedExecutionPublication {
    Manifest(tidepool_runtime::session::StagedPublicManifest),
    Rejected(tidepool_runtime::session::RejectedDeclarationPublication),
}

impl<H, O> Clone for ResidentActorRunner<H, O> {
    fn clone(&self) -> Self {
        Self {
            access: self.access.sharing(),
        }
    }
}

/// A private readiness continuation validated while its actor is still
/// unpublished. Construction proves both the nominal request and owning
/// runtime resource scope; consuming it is the only way the runner enters the
/// installed program.
pub(crate) struct ResidentActorReadiness {
    hole: ResidentHole,
}

pub(crate) struct ResidentActorShutdown {
    continuation: ResidentHole,
    hook: RootCustody,
}

impl ResidentActorShutdown {
    pub(crate) fn into_parts(self) -> (ResidentHole, RootCustody) {
        (self.continuation, self.hook)
    }
}

/// Suspensions admitted while installing the trusted actor entry.
pub(crate) enum ResidentActorStartupStep {
    InstallSource {
        continuation: ResidentHole,
        source: crate::request::sources::SourceBinding,
    },
    InstallShutdown(ResidentActorShutdown),
    Attach(ResidentAgentAttachment),
    Ready(ResidentActorReadiness),
}

pub(crate) struct ResidentAgentAttachment {
    pub(crate) continuation: ResidentHole,
    pub(crate) initial_user_message: Option<String>,
}

pub(crate) struct AgentRosterProjection {
    pub(crate) received: (u64, u64),
    pub(crate) requests: (Vec<crate::RequestId>, Vec<crate::RequestId>),
    pub(crate) actor: crate::ActorRef,
    pub(crate) descriptor: crate::ActorDescriptor,
    pub(crate) bound_worktree: Option<String>,
    pub(crate) terminal: Option<crate::ActorTerminal>,
    pub(crate) runtime: crate::ActorRuntimeObservation,
}

pub(crate) struct AgentInspectionBoundary {
    pub(crate) target: crate::ActorRef,
    pub(crate) continuation: ResidentHole,
}

pub(crate) enum AgentForgetProjection {
    Forgotten,
    Running,
    Retained {
        requests: Vec<crate::RequestId>,
        watches: Vec<crate::WatchId>,
    },
    Unavailable,
    OutputPending {
        displays: usize,
    },
}

#[derive(Clone)]
pub(crate) enum AgentStopProjection {
    /// Stopped, and every host resource the actor held is released.
    StoppedNow,
    /// Stopped, but the host retained resources; the text names them.
    StoppedRetaining(String),
    /// Stopped; the host's release had not settled within the wait, and a
    /// notice will report it.
    StoppedReleasing,
    AlreadyStopped,
    Unavailable,
    Unauthorized,
    Failed(String),
}

#[derive(Clone)]
pub(crate) struct CleanupActorProjection {
    pub actor: crate::ActorRef,
    pub label: String,
    pub terminal: bool,
    pub revision: u64,
}

#[derive(Clone)]
pub(crate) struct CleanupPlanProjection {
    pub group: crate::ForkGroupId,
    pub actors: Vec<CleanupActorProjection>,
    pub pending_responses: Vec<crate::RequestId>,
    pub pending_watches: Vec<crate::WatchId>,
    pub refusal: Option<String>,
}

pub(crate) enum CleanupStepProjection {
    ForgotResponses(Vec<crate::RequestId>),
    ForgotWatches(Vec<crate::WatchId>),
    StoppedActor(crate::ActorRef, AgentStopProjection),
    ForgotActor(crate::ActorRef),
    ActorRetained {
        actor: crate::ActorRef,
        requests: Vec<crate::RequestId>,
        watches: Vec<crate::WatchId>,
    },
    ActorOutputPending {
        actor: crate::ActorRef,
        displays: usize,
    },
    GroupRetired(crate::ForkGroupId),
    Blocked(String),
    StalePlan,
}

pub(crate) struct CleanupReceiptProjection {
    pub plan: CleanupPlanProjection,
    pub steps: Vec<CleanupStepProjection>,
    pub complete: bool,
}

/// One fully captured boundary reached by an installed actor program.
/// Variants own every linear runtime value needed to service that boundary;
/// downstream orchestration never re-decodes the suspended request.
#[allow(
    clippy::large_enum_variant,
    reason = "boundaries deliberately retain linear runtime custody without a second allocation layer"
)]
pub(crate) enum ResidentActorBoundary {
    External {
        continuation: ResidentHole,
        work: tidepool_effect::DeferredEffect,
    },
    Console {
        continuation: ResidentHole,
        text: String,
    },
    DisplayAllowance {
        continuation: ResidentHole,
    },
    DisplayAllowanceGranted {
        continuation: ResidentHole,
        allowance: i64,
    },
    DisplayPublish {
        continuation: ResidentHole,
        output: tidepool_runtime::session::WorkbenchDisplayPage,
        callback: RootCustody,
    },
    DisplayExpand {
        continuation: ResidentHole,
        identity: (i64, i64, i64),
        key: i64,
    },
    DisplayPublished {
        continuation: ResidentHole,
        identity: (i64, i64, i64),
    },
    DisplayExpanded {
        continuation: ResidentHole,
        keys: Vec<(i64, String)>,
    },
    Sleep {
        continuation: ResidentHole,
        duration: Duration,
    },
    Jev {
        continuation: ResidentHole,
        request: String,
    },
    Model {
        continuation: ResidentHole,
        request: crate::generated::model_call::ModelReq,
        table: DataConTable,
    },
    Context {
        continuation: ResidentHole,
        request: crate::ContextReq,
        table: DataConTable,
    },
    Command {
        continuation: ResidentHole,
        request: crate::generated::commands::CommandsReq,
    },
    Replace {
        target: crate::ActorRef,
        candidate: crate::ResidentActorStart,
    },
    Drain {
        continuation: ResidentHole,
        target: crate::ActorRef,
    },
    NotificationSend {
        continuation: ResidentHole,
        target: crate::ActorRef,
        message: String,
    },
    NotificationPoll {
        continuation: ResidentHole,
        receipt: crate::notification::NotificationReceiptWire,
    },
    Completed,
    ActorContext(ResidentHole),
    ActorLocalContext(ResidentHole),
    AttachSource {
        continuation: ResidentHole,
        owner: crate::ActorRef,
        source: crate::request::sources::SourceBinding,
    },
    ForkGroup(ForkGroupBoundary),
    Start(crate::ResidentActorStart),
    Outbound(ResidentOutbound),
    Wait(ResidentWaitRequest),
    Poll(crate::wait::ResidentPollRequest),
    Receive(InstalledReceiver),
    Checkpoint {
        continuation: ResidentHole,
        site: u64,
        value: RootCustody,
    },
    ToolAwait(crate::resident_tools::ResidentToolAwait),
    ToolReply(crate::resident_tools::ResidentToolReply),
    AgentSession(crate::ResidentInteractiveSession),
    AgentAttachment(ResidentAgentAttachment),
    Lookup {
        continuation: ResidentHole,
        request: crate::lookup::LookupRequest,
    },
    Introspection {
        continuation: ResidentHole,
        query: tidepool_runtime::session::NameQuery,
        kind: StructuredInspectionKind,
    },
    AgentInspect(AgentInspectionBoundary),
    AgentList(ResidentHole),
    /// The caller's own recent conversation. It carries no actor argument:
    /// the boundary reads the executing actor's conversation or none.
    ReflectConversation {
        continuation: ResidentHole,
        count: i64,
    },
    AgentShareObservation {
        continuation: ResidentHole,
        recipient: crate::ActorRef,
        scope: crate::ActorRef,
    },
    AgentGroupList {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    AgentForget(AgentInspectionBoundary),
    AgentStop(AgentInspectionBoundary),
    CleanupPlan {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    CleanupExecute {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
        inspected: Vec<(crate::ActorRef, u64)>,
    },
    RequestReservation(RequestReservation),
    CurrentRequest {
        continuation: ResidentHole,
        site: Option<u64>,
    },
    RequestSubmission(RequestSubmission),
    ReplyAttempt(ReplyAttempt),
    ResponsePoll(ResponsePoll),
    ProgressPublication {
        continuation: ResidentHole,
        request: crate::RequestId,
        value: RootCustody,
    },
    ProgressPoll(ResponsePoll),
    RequestUpdate {
        continuation: ResidentHole,
        request: crate::RequestId,
        message: String,
    },
    RequestUpdatePoll {
        continuation: ResidentHole,
        update: crate::RequestUpdateId,
    },
    WatchProgressPoll {
        continuation: ResidentHole,
        watch: crate::WatchId,
        request: crate::RequestId,
        after: u64,
    },
    CommandReportPoll {
        continuation: ResidentHole,
        job: String,
    },
    RequestDetachment {
        continuation: ResidentHole,
        request: crate::RequestId,
    },
    RequestCancellation(RequestCancellation),
    ResponseAbandonment(ResponseAbandonment),
    ResponseForget(ResponseForget),
    ReplyPoll(ReplyPoll),
    CancellationAcknowledgement(CancellationAcknowledgement),
    WatchRegistration(WatchRegistration),
    RouteRegistration {
        registration: WatchRegistration,
        entry: RootCustody,
    },
    RoutePoll(WatchPoll),
    RouteList(ResidentHole),
    WatchPoll(WatchPoll),
    WatchAwait(WatchPoll),
    WatchForget(WatchForget),
}

pub(crate) enum ForkGroupBoundary {
    CheckCheckpoint {
        continuation: ResidentHole,
        token: String,
    },
    ReleaseCheckpoint {
        continuation: ResidentHole,
        token: String,
    },
    Checkpoint {
        continuation: ResidentHole,
        name: String,
    },
    Preview {
        continuation: ResidentHole,
        role: crate::ActorLaunchRoleWire,
        effect_keys: Vec<crate::ActorEffectKeyWire>,
        budget: Option<(i64, i64)>,
        model: Option<crate::Model>,
        effort: Option<crate::ForkEffort>,
        context: crate::ForkContext,
        instructions: Option<String>,
        lifetime: crate::WorkerLifetime,
    },
    Begin {
        continuation: ResidentHole,
        relative: bool,
        group: String,
        branches: Vec<String>,
    },
    Commit {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    CommitCaptured {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    Abort {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    Cleanup {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResidentKernelBoundary {
    Reply,
    Continue,
}

impl ResidentKernelBoundary {
    fn operation(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Continue => "continue",
        }
    }
}

impl ResidentActorBoundary {
    pub(crate) fn success_disposition(
        &self,
    ) -> tidepool_runtime::session::WorkbenchOperationDisposition {
        use tidepool_runtime::session::WorkbenchOperationDisposition;
        match self {
            Self::Context {
                request: crate::ContextReq::GetContextWith,
                ..
            } => WorkbenchOperationDisposition::Read,
            Self::Context { .. } => WorkbenchOperationDisposition::Staged,
            Self::DisplayAllowance { .. } | Self::DisplayAllowanceGranted { .. } => {
                WorkbenchOperationDisposition::Read
            }
            _ => WorkbenchOperationDisposition::Committed,
        }
    }

    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::Completed => "program completion",
            Self::External { .. } => "external effect",
            Self::Sleep { .. } => "sleep",
            Self::Jev { .. } => "jev",
            Self::Model { .. } => "model call",
            Self::Context { .. } => "context transformation",
            Self::Command { .. } => "command job",
            Self::Console { .. } => "print",
            Self::DisplayPublish { .. } | Self::DisplayPublished { .. } => "display",
            Self::DisplayExpand { .. } | Self::DisplayExpanded { .. } => "expand",
            Self::DisplayAllowance { .. } | Self::DisplayAllowanceGranted { .. } => {
                "display allowance"
            }
            Self::NotificationSend { .. } => "notify",
            Self::NotificationPoll { .. } => "pollNotification",
            Self::ActorContext(_) => "actorContext",
            Self::ActorLocalContext(_) => "actor local context",
            Self::AttachSource { .. } => "attach source",
            Self::ForkGroup(ForkGroupBoundary::Preview { .. }) => "preview context-fork policy",
            Self::ForkGroup(ForkGroupBoundary::Checkpoint { .. }) => "capture context checkpoint",
            Self::ForkGroup(ForkGroupBoundary::CheckCheckpoint { .. }) => {
                "check context checkpoint"
            }
            Self::ForkGroup(ForkGroupBoundary::ReleaseCheckpoint { .. }) => {
                "release context checkpoint"
            }
            Self::ForkGroup(ForkGroupBoundary::Begin { .. }) => "begin context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Commit { .. }) => "commit context-fork group",
            Self::ForkGroup(ForkGroupBoundary::CommitCaptured { .. }) => {
                "commit captured context-fork group"
            }
            Self::ForkGroup(ForkGroupBoundary::Abort { .. }) => "abort context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Cleanup { .. }) => "cleanup context-fork group",
            Self::Start(_) => "startActor",
            Self::Replace { .. } => "replaceActor",
            Self::Outbound(ResidentOutbound::Call { .. }) => "call",
            Self::Outbound(ResidentOutbound::TryCall { .. }) => "tryCall",
            Self::Outbound(ResidentOutbound::Cast { .. }) => "cast",
            Self::Outbound(ResidentOutbound::TryCast { .. }) => "tryCast",
            Self::Drain { .. } => "drainActor",
            Self::Wait(_) => "awaitExit",
            Self::Poll(_) => "pollExit",
            Self::Receive(_) => "receive",
            Self::Checkpoint { .. } => "state checkpoint",
            Self::ToolAwait(_) => "agent tool await",
            Self::ToolReply(_) => "agent tool reply",
            Self::AgentSession(_) => "agent session",
            Self::AgentAttachment(_) => "agent attachment",
            Self::Lookup { .. } => "lookup",
            Self::Introspection { kind, .. } => match kind {
                StructuredInspectionKind::Info => "structured info",
                StructuredInspectionKind::Type => "structured type",
            },
            Self::AgentInspect(_) => "observeAgent",
            Self::AgentList(_) => "listAgents",
            Self::ReflectConversation { .. } => "reflect",
            Self::AgentShareObservation { .. } => "shareObservation",
            Self::AgentGroupList { .. } => "observeForkGroup",
            Self::AgentForget(_) => "forgetAgent",
            Self::AgentStop(_) => "stopAgent",
            Self::CleanupPlan { .. } => "planCleanup",
            Self::CleanupExecute { .. } => "executeCleanup",
            Self::RequestReservation(_) => "request",
            Self::CurrentRequest { .. } => "currentRequest",
            Self::RequestSubmission(_) => "request",
            Self::ReplyAttempt(_) => "reply",
            Self::ResponsePoll(_) => "pollResponse",
            Self::ProgressPublication { .. } => "reportProgress",
            Self::ProgressPoll(_) => "pollProgress",
            Self::RequestUpdate { .. } => "updateRequest",
            Self::RequestUpdatePoll { .. } => "pollRequestUpdate",
            Self::WatchProgressPoll { .. } => "pollWatch progress",
            Self::CommandReportPoll { .. } => "pollWatch command",
            Self::RequestDetachment { .. } => "detachRequest",
            Self::RequestCancellation(_) => "cancelRequest",
            Self::ResponseAbandonment(_) => "abandonResponse",
            Self::ResponseForget(_) => "forgetResponse",
            Self::ReplyPoll(_) => "pollReply",
            Self::CancellationAcknowledgement(_) => "acknowledgeCancellation",
            Self::WatchRegistration(_) => "watch",
            Self::RouteRegistration { .. } => "route",
            Self::RoutePoll(_) => "pollRoute",
            Self::RouteList(_) => "listRoutes",
            Self::WatchPoll(_) => "pollWatch",
            Self::WatchAwait(_) => "awaitWatch",
            Self::WatchForget(_) => "forgetWatch",
        }
    }
}

#[derive(Clone, Copy)]
enum BoundaryCapture {
    Execution,
    Replacement,
}

#[derive(Clone, Copy)]
pub(crate) enum StructuredInspectionKind {
    Info,
    Type,
}

#[derive(DeriveFromHaskell)]
#[haskell(name = "NameQuery")]
struct IntrospectionNameQuery {
    scope: IntrospectionNameScope,
    namespace: IntrospectionNameNamespace,
    name: String,
}

#[derive(DeriveFromHaskell)]
enum IntrospectionNameScope {
    CurrentScope,
    PublicModule(String),
}

#[derive(DeriveFromHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the wire truth: FromHaskell matches them \
              verbatim against Haskell constructor names, so the shared \
              `Name` suffix must stay exactly as spelled, not be trimmed"
)]
enum IntrospectionNameNamespace {
    AnyName,
    ValueName,
    TypeName,
    ConstructorName,
}

impl From<IntrospectionNameQuery> for tidepool_runtime::session::NameQuery {
    fn from(query: IntrospectionNameQuery) -> Self {
        use tidepool_runtime::session::{NameNamespace, NameScope};
        Self {
            scope: match query.scope {
                IntrospectionNameScope::CurrentScope => NameScope::Current,
                IntrospectionNameScope::PublicModule(module) => NameScope::PublicModule(module),
            },
            namespace: match query.namespace {
                IntrospectionNameNamespace::AnyName => NameNamespace::Any,
                IntrospectionNameNamespace::ValueName => NameNamespace::Value,
                IntrospectionNameNamespace::TypeName => NameNamespace::Type,
                IntrospectionNameNamespace::ConstructorName => NameNamespace::Constructor,
            },
            name: query.name,
        }
    }
}

/// The one nominal roster for requests interpreted at actor execution
/// boundaries. Generated request enums own constructor recognition and field
/// shape; this sum owns orchestration routing.
macro_rules! resident_request_roster {
    ($($variant:ident($request:ty) => $key:expr,)*) => {
        enum ResidentRequest { $($variant($request),)* }

        pub(crate) fn intrinsic_effect_families() -> Vec<exomonad_tool::ToolEffectKey> {
            [$($key,)*].into_iter().flatten().collect()
        }
    };
}

resident_request_roster! {
    Console(crate::generated::console::ConsoleReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Console)),
    Sleep(crate::generated::sleep::SleepReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Sleep)),
    Commands(crate::generated::commands::CommandsReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Commands)),
    Notifications(crate::generated::notifications::NotificationsReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Notifications)),
    Jev(crate::generated::jev::JevReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Jev)),
    Model(crate::generated::model_call::ModelReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::ModelCall)),
    Context(crate::ContextReq) => None,
    Actor(crate::generated::actor::ActorReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Actor)),
    ActorContext(crate::generated::actor_context::ActorContextReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::ActorContext)),
    AgentControl(crate::generated::agent_control::AgentControlReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::AgentControl)),
    AgentInspection(crate::generated::agent_inspection::AgentInspectionReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::AgentInspection)),
    Lookup(crate::generated::lookup::LookupReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Lookup)),
    Introspection(crate::generated::introspection::IntrospectionReq) => None,
    AgentLaunch(crate::generated::agent_launch::AgentLaunchReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::AgentLaunch)),
    Forks(crate::generated::forks::ForksReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Forks)),
    ActorKernel(crate::generated::actor_kernel::ActorKernelReq) => None,
    ActorLocal(crate::generated::actor_local::ActorLocalReq) => None,
    AgentTools(crate::generated::agent_tools::AgentToolsReq) => None,
    AgentSession(crate::generated::agent_session::AgentSessionReq) => None,
    Reflect(crate::generated::reflect::ReflectReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Reflect)),
    Replies(RepliesReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Replies)),
    Watches(WatchesReq) => Some(exomonad_tool::ToolEffectKey::Actor(exomonad_tool::ActorEffectKey::Watches)),
}

impl ResidentRequest {
    fn decode(
        request: &HaskellValue,
        table: &DataConTable,
    ) -> Result<Self, ResidentActorWorkbenchError> {
        tracing::trace!(
            constructor = %request_constructor(request, table),
            "decoding resident request"
        );
        macro_rules! try_member {
            ($variant:path, $request:ty) => {
                match <$request as FromHaskell>::from_value(request, table) {
                    Ok(decoded) => return Ok($variant(decoded)),
                    Err(BridgeError::UnknownDataCon(_)) => {}
                    Err(source) => {
                        return Err(ResidentActorWorkbenchError::RequestDecode {
                            constructor: request_constructor(request, table),
                            source,
                        });
                    }
                }
            };
        }

        try_member!(
            Self::Notifications,
            crate::generated::notifications::NotificationsReq
        );
        try_member!(Self::Sleep, crate::generated::sleep::SleepReq);
        try_member!(Self::Jev, crate::generated::jev::JevReq);
        try_member!(Self::Model, crate::generated::model_call::ModelReq);
        try_member!(Self::Context, crate::ContextReq);
        try_member!(Self::Commands, crate::generated::commands::CommandsReq);
        try_member!(Self::Console, crate::generated::console::ConsoleReq);
        try_member!(Self::Actor, crate::generated::actor::ActorReq);
        try_member!(
            Self::ActorContext,
            crate::generated::actor_context::ActorContextReq
        );
        try_member!(
            Self::AgentControl,
            crate::generated::agent_control::AgentControlReq
        );
        try_member!(
            Self::AgentInspection,
            crate::generated::agent_inspection::AgentInspectionReq
        );
        try_member!(Self::Lookup, crate::generated::lookup::LookupReq);
        try_member!(
            Self::Introspection,
            crate::generated::introspection::IntrospectionReq
        );
        try_member!(
            Self::AgentLaunch,
            crate::generated::agent_launch::AgentLaunchReq
        );
        try_member!(Self::Forks, crate::generated::forks::ForksReq);
        try_member!(
            Self::ActorKernel,
            crate::generated::actor_kernel::ActorKernelReq
        );
        try_member!(
            Self::ActorLocal,
            crate::generated::actor_local::ActorLocalReq
        );
        try_member!(
            Self::AgentTools,
            crate::generated::agent_tools::AgentToolsReq
        );
        try_member!(
            Self::AgentSession,
            crate::generated::agent_session::AgentSessionReq
        );
        try_member!(Self::Reflect, crate::generated::reflect::ReflectReq);
        try_member!(Self::Replies, RepliesReq);
        try_member!(Self::Watches, WatchesReq);

        Err(ResidentActorWorkbenchError::UnsupportedRequest {
            constructor: request_constructor(request, table),
        })
    }

    fn operation(&self) -> &'static str {
        match self {
            Self::Sleep(crate::generated::sleep::SleepReq::SleepWith(..)) => "sleep",
            Self::Jev(crate::generated::jev::JevReq::JevAskWith(..)) => "jev",
            Self::Model(_) => "model call",
            Self::Context(_) => "context transformation",
            Self::Commands(_) => "command job",
            Self::Console(_) => "console output",
            Self::Notifications(crate::generated::notifications::NotificationsReq::NotifyWith(
                ..,
            )) => "notify",
            Self::Notifications(
                crate::generated::notifications::NotificationsReq::PollNotificationWith(..),
            ) => "pollNotification",
            Self::Actor(crate::generated::actor::ActorReq::ActorBeginForkGroupWith(..)) => {
                "begin context-fork group"
            }
            Self::ActorContext(
                crate::generated::actor_context::ActorContextReq::ActorContextWith,
            ) => "actorContext",
            Self::Reflect(crate::generated::reflect::ReflectReq::ReflectWith(..)) => "reflect",
            Self::AgentControl(
                crate::generated::agent_control::AgentControlReq::AgentControlStopWith(..),
            ) => "stopAgent",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentInspectCleanupWith(..),
            ) => "planCleanup",
            Self::AgentControl(
                crate::generated::agent_control::AgentControlReq::AgentControlExecuteCleanupWith(
                    ..,
                ),
            ) => "executeCleanup",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentInspectWith(..),
            ) => "observeAgent",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentListWith,
            ) => "listAgents",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentShareObservationWith(
                    ..,
                ),
            ) => "shareObservation",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentGroupListWith(..),
            ) => "observeForkGroup",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentForgetWith(..),
            ) => "forgetAgent",
            Self::Lookup(_) => "lookup",
            Self::Introspection(
                crate::generated::introspection::IntrospectionReq::IntrospectionInfoWith(..),
            ) => "structured info",
            Self::Introspection(
                crate::generated::introspection::IntrospectionReq::IntrospectionTypeOfWith(..),
            ) => "structured type",
            Self::AgentLaunch(crate::generated::agent_launch::AgentLaunchReq::AgentLaunchWith(
                ..,
            )) => "startAgent",
            Self::Forks(crate::generated::forks::ForksReq::ForksBeginWith(..)) => {
                "begin context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksStartWith(..)) => "context fork",
            Self::Forks(crate::generated::forks::ForksReq::ForksCheckpointWith(..)) => "checkpoint",
            Self::Forks(crate::generated::forks::ForksReq::ForksCheckCheckpointWith(..)) => {
                "checkCheckpoint"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksReleaseCheckpointWith(..)) => {
                "releaseCheckpoint"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksPreviewWith(..)) => {
                "preview context-fork policy"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksCommitWith(..)) => {
                "commit context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksCommitCapturedWith(..)) => {
                "commit captured context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksAbortWith(..)) => {
                "abort context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksCleanupWith(..)) => {
                "cleanup context-fork group"
            }
            Self::Actor(crate::generated::actor::ActorReq::ActorStartWith(..)) => "startActor",
            Self::Actor(crate::generated::actor::ActorReq::ActorForkWith(..)) => "context fork",
            Self::Actor(crate::generated::actor::ActorReq::ActorCommitForkGroupWith(..)) => {
                "commit context-fork group"
            }
            Self::Actor(crate::generated::actor::ActorReq::ActorAbortForkGroupWith(..)) => {
                "abort context-fork group"
            }
            Self::Actor(crate::generated::actor::ActorReq::ActorWaitWith(..)) => "awaitExit",
            Self::Actor(crate::generated::actor::ActorReq::ActorPollWith(..)) => "pollExit",
            Self::Actor(crate::generated::actor::ActorReq::ActorCallWith(..)) => "call",
            Self::Actor(crate::generated::actor::ActorReq::ActorTryCallWith(..)) => "tryCall",
            Self::Actor(crate::generated::actor::ActorReq::ActorCastWith(..)) => "cast",
            Self::Actor(crate::generated::actor::ActorReq::ActorTryCastWith(..)) => "tryCast",
            Self::Actor(crate::generated::actor::ActorReq::ActorDrainWith(..)) => "drainActor",
            Self::Actor(crate::generated::actor::ActorReq::ActorReplaceWith(..)) => "replaceActor",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(..),
            ) => "installShutdown",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(..),
            ) => "install progress source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(
                    ..,
                ),
            ) => "install settlement source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(..),
            ) => "command source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith(..),
            ) => "install lifecycle source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorLifecycleInputWith,
            ) => "lifecycle source input",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorCommandInputWith,
            ) => "command source input",
            Self::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith) => {
                "ready"
            }
            Self::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorReplyWith(
                ..,
            )) => "reply",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorContinueWith(..),
            ) => "continue",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorLocalContextWith,
            ) => "actor local context",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorLocalAttachProgressSourceWith(
                    ..,
                ),
            ) => "attach progress source",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorLocalAttachSettlementSourceWith(
                    ..,
                ),
            ) => "attach settlement source",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorLocalAttachCommandSourceWith(..),
            ) => "attach command source",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorLocalAttachLifecycleSourceWith(
                    ..,
                ),
            ) => "attach lifecycle source",
            Self::ActorLocal(crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(
                ..,
            )) => "receive",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorCheckpointWith(..),
            ) => "state checkpoint",
            Self::AgentTools(
                crate::generated::agent_tools::AgentToolsReq::AgentToolsInstallWith(..),
            ) => "agent tool installation",
            Self::AgentTools(crate::generated::agent_tools::AgentToolsReq::AgentToolsInputWith) => {
                "agent tool input"
            }
            Self::AgentTools(
                crate::generated::agent_tools::AgentToolsReq::AgentToolsAwaitWith(..),
            ) => "agent tool await",
            Self::AgentTools(
                crate::generated::agent_tools::AgentToolsReq::AgentToolsReplyWith(..),
            ) => "agent tool reply",
            Self::AgentSession(
                crate::generated::agent_session::AgentSessionReq::AgentSessionWith(..),
            ) => "agent session",
            Self::AgentSession(
                crate::generated::agent_session::AgentSessionReq::AgentAttachWith(..),
            ) => "agent attachment",
            Self::Replies(RepliesReq::ReserveRequestWith(..)) => "request reservation",
            Self::Replies(RepliesReq::CurrentRequestWith(..)) => "currentRequest",
            Self::Replies(RepliesReq::SubmitRequestWith(..)) => "request submission",
            Self::Replies(RepliesReq::AttemptReplyWith(..)) => "attemptReply",
            Self::Replies(RepliesReq::PublishProgressWith(..)) => "reportProgress",
            Self::Replies(RepliesReq::ObserveProgressWith(..)) => "pollProgress",
            Self::Replies(RepliesReq::UpdateRequestWith(..)) => "updateRequest",
            Self::Replies(RepliesReq::ObserveRequestUpdateWith(..)) => "pollRequestUpdate",
            Self::Replies(RepliesReq::ReplyWith(..)) => "reply",
            Self::Replies(RepliesReq::ObserveResponseWith(..)) => "pollResponse",
            Self::Replies(RepliesReq::CancelRequestWith(..)) => "cancelRequest",
            Self::Replies(RepliesReq::DetachRequestWith(..)) => "detachRequest",
            Self::Replies(RepliesReq::AbandonResponseWith(..)) => "abandonResponse",
            Self::Replies(RepliesReq::ForgetResponseWith(..)) => "forgetResponse",
            Self::Replies(RepliesReq::ObserveReplyWith(..)) => "pollReply",
            Self::Replies(RepliesReq::AttemptAcknowledgeCancellationWith(..)) => {
                "attemptAcknowledgeCancellation"
            }
            Self::Replies(RepliesReq::AcknowledgeCancellationWith(..)) => "acknowledgeCancellation",
            Self::Watches(WatchesReq::RegisterWatchWith(..)) => "watch",
            Self::Watches(WatchesReq::RegisterWatchGroupsWith(..)) => "watch",
            Self::Watches(WatchesReq::RegisterAwaitWith(..)) => "waitFor",
            Self::Watches(WatchesReq::RegisterRouteWith(..)) => "route",
            Self::Watches(WatchesReq::RegisterRouteGroupsWith(..)) => "route",
            Self::Watches(WatchesReq::ObserveRouteWith(..)) => "pollRoute",
            Self::Watches(WatchesReq::ListRoutesWith) => "listRoutes",
            Self::Watches(WatchesReq::ObserveWatchWith(..)) => "pollWatch",
            Self::Watches(WatchesReq::AwaitWatchWith(..)) => "awaitWatch",
            Self::Watches(WatchesReq::ObserveWatchProgressWith(..)) => "pollWatch progress",
            Self::Watches(WatchesReq::ObserveCommandWith(..)) => "pollWatch command",
            Self::Watches(WatchesReq::ForgetWatchWith(..)) => "forgetWatch",
        }
    }
}

/// Write `source` to `path`, creating parent directories as needed — the
/// same shape [`tidepool_runtime::session::ExactExportSurface::materialize`]
/// itself uses to put a facade under the launching session's own root, used
/// here to put a copy of that same content (and the `Lib.G<n>` sources it
/// re-exports from) under a CHILD session's own root instead.
fn write_seed_source(path: &std::path::Path, source: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!("child session seed directory {}: {error}", parent.display())
        })?;
    }
    tidepool_atomic_write::write_best_effort(path, source.as_bytes())
        .map_err(|error| format!("child session seed file {}: {error}", error.path.display()))
}

impl<H, O> ResidentActorRunner<H, O> {
    #[must_use]
    pub fn new(machines: Arc<ActorMachineRegistry<H, O>>, source: ActorWorkbenchSource) -> Self {
        Self {
            access: ResidentMachineAccess::new(machines, source),
        }
    }

    /// Inspect support from this checkout's actual handler stack, including
    /// the distinct stack constructed for a dedicated child session.
    #[must_use]
    pub fn with_handler_effect_support(
        mut self,
        observer: impl Fn(&H) -> Vec<exomonad_tool::ToolEffectKey> + Send + Sync + 'static,
    ) -> Self {
        self.access.handler_effect_support = Arc::new(observer);
        self
    }

    /// Install the composition root's child-session factory. Omitted, every
    /// launch keeps running on the session that admitted it — today's
    /// Install the composition root's child-session factory. Omitted, every
    /// launch keeps running on the session that admitted it.
    #[must_use]
    pub fn with_child_session_factory(mut self, factory: ChildSessionFactory<H, O>) -> Self {
        self.access.child_session_factory = Some(factory);
        self
    }

    /// Install the compiled turn a fresh child session bootstraps with —
    /// see [`ResidentMachineAccess::child_bootstrap_program`]. Required
    /// alongside [`Self::with_child_session_factory`] for
    /// [`Self::provision_child_session`] to succeed.
    #[must_use]
    pub fn with_child_bootstrap_program(
        mut self,
        program: Arc<tidepool_runtime::session::CompiledTurn>,
    ) -> Self {
        self.access.child_bootstrap_program = Some(program);
        self
    }

    /// Whether this host can give a launch its own dedicated machine at
    /// all: both [`Self::with_child_session_factory`] and
    /// [`Self::with_child_bootstrap_program`] were installed.
    /// `try_start_child`/`replacement.rs`'s matching resolution consult
    /// this before calling [`Self::provision_child_session`], so a host
    /// that never opted into per-actor machines (most test harnesses
    /// included) still runs every launch on the session that admitted it,
    /// exactly as before this capability existed, rather than failing an
    /// otherwise-ordinary fork over a host capability nothing asked for.
    #[must_use]
    pub fn supports_child_sessions(&self) -> bool {
        self.access.child_session_factory.is_some() && self.access.child_bootstrap_program.is_some()
    }

    /// Install this run's shared [`tidepool_runtime::session::ImageRegistry`].
    /// Omitted, every session compiles its own images unchanged. Applied to
    /// a session's engine on every checkout, root and child alike — see
    /// [`ResidentMachineAccess::image_registry`].
    #[must_use]
    pub fn with_image_registry(
        mut self,
        registry: Arc<tidepool_runtime::session::ImageRegistry>,
    ) -> Self {
        self.access.image_registry = Some(registry);
        self
    }

    /// Build a fresh machine for `session_id` and register it idle in the
    /// shared registry, using the installed [`ChildSessionFactory`]. Returns
    /// an error naming why when no factory is installed, the factory itself
    /// fails, or `session_id` somehow already names a live entry (it is
    /// minted fresh by the caller — `tidepool_repr::SessionId` collision is
    /// not expected, but silently overwriting a live session is never safe).
    #[allow(
        dead_code,
        reason = "installed by the composition root (actor_host.rs's compile_root); \
                  every production launch path now goes through \
                  Self::provision_child_session instead (an eligible launch \
                  always has an entry to import, never an empty child), so this \
                  stays exercised by tests only"
    )]
    pub(crate) fn spawn_child_session(
        &self,
        session_id: tidepool_repr::SessionId,
    ) -> Result<(), String>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let factory = self
            .access
            .child_session_factory
            .clone()
            .ok_or_else(|| "no child-session factory installed".to_string())?;
        let machine = factory(session_id, &[])?;
        self.access
            .machines
            .try_insert_idle(session_id, machine)
            .map_err(|_machine| {
                format!("session {session_id} already had a live entry; refusing to overwrite it")
            })
    }

    /// [`Self::spawn_child_session`], but the one runtime-owned operation
    /// that gives an eligible `SelectedContext` launch a genuinely running
    /// machine of its own (`crate::start::child_session_eligibility`;
    /// called by `try_start_child`/`replacement.rs`'s matching resolution
    /// once the checkout that captured the launch has long since been
    /// released): construct the private session, bootstrap it (install the
    /// same compiled program the root itself bootstrapped with — required
    /// before either `set_image_registry` or a later
    /// [`ResidentActorRunner::transfer_custody`] import will do anything
    /// but no-op/refuse on a virgin engine), install this run's shared
    /// image registry, and mint the child's OWN lexical scope on the
    /// machine that will actually own it (never the parent's — copying a
    /// `ScopeId` across sessions is not evidence of destination scope
    /// membership). Only once every one of those succeeds does the machine
    /// publish into the shared registry. A failure at any step drops the
    /// private machine and the reserved session id without ever publishing
    /// it — the shared registry is unchanged.
    ///
    /// `source_layer` is the descriptor's fixed helper snapshot. The factory
    /// puts it on the validation path before bootstrap compiles inherited
    /// declarations.
    ///
    /// `resource_scope` is the descriptor's own resource scope, minted at
    /// capture time (`crate::start::capture_decoded`'s `child_realm`) — the
    /// SAME realm `transfer_custody` will later import the entry under, not
    /// a second one of this call's own minting nothing afterward would use.
    ///
    /// `seed`, when the launch carried one (`crate::start::ChildSessionSeed`
    /// — an eligible launch always does), is written to the child's own
    /// session root BEFORE the bootstrap install: any declaration facade
    /// `capture_decoded` materialized under the PARENT's session root (its
    /// `import Tidepool.Session.Lib.G<n> (...)` line names generations that
    /// otherwise exist nowhere the child can find them) and every
    /// `Lib.G<n>.hs` source it might reference, at the same relative paths
    /// (`tidepool_atomic_write`, exactly as `ExactExportSurface::materialize`
    /// itself writes the facade on the parent). The child's own value-binding
    /// generation counter is then raised to the parent's
    /// (`ResidentSession::set_val_gen`'s own monotonic-max, never lowers it)
    /// so nothing the child declares on its own later can mint a generation
    /// number a just-copied file already uses.
    ///
    /// Returns the child's own freshly minted lexical scope
    /// (`ActorDescriptor::with_lexical_scope` replaces the placeholder the
    /// parent minted at capture time with this one). The caller still owns
    /// crossing the entry itself into this machine — `transfer_custody`,
    /// not this call, which never touches any value's custody.
    pub(crate) async fn provision_child_session(
        &self,
        session_id: tidepool_repr::SessionId,
        resource_scope: RealmId,
        seed: Option<&crate::start::ChildSessionSeed>,
        source_layer: &[PathBuf],
    ) -> Result<tidepool_codegen::scope::ScopeId, String>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        let factory = self
            .access
            .child_session_factory
            .clone()
            .ok_or_else(|| "no child-session factory installed".to_string())?;
        let bootstrap_program = self
            .access
            .child_bootstrap_program
            .clone()
            .ok_or_else(|| "no child bootstrap program installed".to_string())?;
        let source_layer = source_layer.to_vec();
        let seed = seed.map(|seed| {
            (
                seed.facade.as_ref().map(|facade| {
                    (
                        facade.identity().relative_hs_path(),
                        facade.source().to_owned(),
                    )
                }),
                seed.lib_sources.clone(),
                seed.val_generation,
            )
        });
        let image_registry = self.access.image_registry.clone();
        // The blocking task owns only an unregistered session. Cancellation
        // drops its eventual result, so it cannot register an orphan after the
        // awaiting launch's cleanup has already run.
        let (machine, lexical_scope) = spawn_blocking_in_span(move || {
            let mut machine = factory(session_id, &source_layer)?;
            let lexical_scope = machine.mint_isolated_scope();
            machine
                .set_actor_execution(
                    tidepool_runtime::session::SessionRunContext {
                        resource_scope,
                        lexical_scope,
                        ..tidepool_runtime::session::SessionRunContext::ROOT
                    },
                    tidepool_effect::EffectRunPolicy::HandleOrSuspend,
                    tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE,
                )
                .map_err(|error| format!("child session bootstrap context: {error}"))?;
            if let Some((facade, lib_sources, val_generation)) = seed {
                let root = machine
                    .compile_view_in(tidepool_codegen::scope::ScopeId::ROOT)
                    .ok_or_else(|| {
                        "child session has no compile view for inherited source seeding".to_string()
                    })?
                    .session_root()
                    .to_path_buf();
                if let Some((facade_path, facade_source)) = facade {
                    write_seed_source(&root.join(facade_path), &facade_source)?;
                }
                for (relative, source) in &lib_sources {
                    write_seed_source(&root.join(relative), source)?;
                }
                machine.set_val_gen(val_generation);
            }
            if let Some(registry) = &image_registry {
                // Before the bootstrap, so the child's first install is the
                // run's shared driver image rather than a second compile of it.
                machine.set_image_registry(Arc::clone(registry));
            }
            machine
                .run_with_sites("child_session_bootstrap", bootstrap_program.code())
                .map_err(|error| format!("child session bootstrap install: {error}"))?;
            Ok::<_, String>((machine, lexical_scope))
        })
        .await
        .map_err(|error| format!("child session preparation task: {error}"))??;
        // Atomic check-and-insert (`try_insert_idle`, not `insert_idle`):
        // the collision check must happen before any registry mutation, not
        // after — `insert_idle` would already have replaced whatever was at
        // `session_id` by the time its return value said so.
        self.access
            .machines
            .try_insert_idle(session_id, machine)
            .map_err(|_machine| {
                format!("session {session_id} already had a live entry; refusing to overwrite it")
            })?;
        self.access
            .child_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id);
        Ok(lexical_scope)
    }

    /// Attempt to release `session_id`'s dedicated machine now that its
    /// actor has retired — the other half of
    /// [`Self::provision_child_session`]'s lifecycle. A no-op,
    /// immediately, for any session that call never built (the run's
    /// shared session in particular is never a member of
    /// [`ResidentMachineAccess::child_sessions`] and is never removed).
    ///
    /// The invariant has two parts, and teardown needs both: (a) no live
    /// [`RootCustody`] for this session is held outside it anywhere in the
    /// process ([`ResidentSession::outstanding_custody`] — NOT
    /// `value_handle_count`, which also counts the session's own private
    /// bindings, e.g. the crossed-entry identities `ResidentSession::import_parcel`
    /// records, and so never reaches zero on its own); (b) no live actor in
    /// the forest directory is still PLACED on this session — an inherited
    /// context fork shares its parent's session rather than getting a
    /// dedicated one of its own, so a dedicated session's owner retiring
    /// does not mean every actor using it has. `other_actor_still_on_session`
    /// is that second check, made by the caller against the actor
    /// directory (this runner owns no directory of its own) immediately
    /// before this call.
    ///
    /// A live occupant (b) skips straight to `Ok(())`: nothing to defer,
    /// since another actor's own eventual retirement checks again. Passing
    /// (b), (a)'s check happens under the SAME checkout the settlement
    /// below marks pending for, so a caller who finds custody still
    /// outstanding logs it once here and the deferred release completes
    /// when that session's custody owner signals a later root release. One
    /// waiter for this pending session then checks the existing checkout
    /// settlement path. This call itself never
    /// waits for that: it performs one checkout, one check, and returns
    /// either way, so an actor's retirement is never blocked on whoever
    /// still needs this machine's output.
    pub(crate) async fn retire_child_session(
        &self,
        session_id: tidepool_repr::SessionId,
        session_retained: bool,
    ) -> Result<(), ResidentActorWorkbenchError>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        if session_retained {
            return Ok(());
        }
        // The map entry is also the cleanup task owner. Only the call that
        // creates it starts a task; concurrent retirements share that task
        // and cannot replace the session's notifier with another waiter.
        let (owner, start_worker) = {
            // Keep membership and pending-owner admission in one lock order.
            // Checkout settlement removes both under this same pair, so a
            // caller that raced a completed teardown cannot reinsert stale
            // pending state after the machine is gone.
            let children = self
                .access
                .child_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !children.contains(&session_id) {
                return Ok(());
            }
            let mut pending = self
                .access
                .pending_child_teardown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match pending.entry(session_id) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    (Arc::clone(entry.get()), false)
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let owner = Arc::new(PendingChildTeardown::new());
                    entry.insert(Arc::clone(&owner));
                    (owner, true)
                }
            }
        };
        if !start_worker {
            return Ok(());
        }

        // Start the owner before the first await. Cancellation of this caller
        // may stop its initial-check response, but cannot strand the pending
        // session after the blocking checkout settles.
        let (start_worker_tx, start_worker_rx) = tokio::sync::oneshot::channel();
        let (initial_check_tx, initial_check_rx) = tokio::sync::oneshot::channel();
        let access = self.access.sharing();
        let worker_owner = Arc::clone(&owner);
        let worker = tokio::spawn(async move {
            let _ = start_worker_rx.await;
            let session_wake = Arc::clone(&worker_owner.wake);
            let initial = access
                .with_host_machine("child-teardown", session_id, None, move |session, _| {
                    session.set_custody_cleanup_notifier(Arc::new(move || {
                        session_wake.notify_one();
                    }));
                    Ok(session.outstanding_custody())
                })
                .await;
            match initial {
                Ok(_) => {
                    #[cfg(test)]
                    worker_owner
                        .initial_checkout_complete
                        .store(true, std::sync::atomic::Ordering::Release);
                    let _ = initial_check_tx.send(Ok(()));
                }
                Err(error) => {
                    let still_owned = access.owns_pending_child_teardown(session_id, &worker_owner);
                    if still_owned {
                        access.finish_child_session_teardown(session_id, Some(&worker_owner));
                    }
                    let result = if still_owned { Err(error) } else { Ok(()) };
                    let _ = initial_check_tx.send(result);
                    return;
                }
            }

            while access.owns_pending_child_teardown(session_id, &worker_owner) {
                worker_owner.wake.notified().await;
                if !access.owns_pending_child_teardown(session_id, &worker_owner) {
                    break;
                }
                if let Err(error) = access
                    .with_host_machine(
                        "child-teardown-custody-release",
                        session_id,
                        None,
                        |_, _| Ok(()),
                    )
                    .await
                {
                    tracing::debug!(session = ?session_id, %error,
                        "deferred child-session cleanup could not check out its machine");
                    access.finish_child_session_teardown(session_id, Some(&worker_owner));
                    break;
                }
            }
        });
        #[cfg(test)]
        {
            *owner
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        }
        #[cfg(not(test))]
        drop(worker);
        let _ = start_worker_tx.send(());

        initial_check_rx.await.map_err(|_| {
            ResidentActorWorkbenchError::ActorProtocol(
                "child-session cleanup worker stopped before its initial checkout".to_string(),
            )
        })??;
        // This is idempotent when several actors retire from one session: the
        // single owner continues until checkout settlement removes its entry.
        Ok(())
    }

    pub(crate) fn child_session_startup_lease(
        &self,
        session_id: tidepool_repr::SessionId,
    ) -> ChildSessionStartupLease
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        let runner = self.clone();
        ChildSessionStartupLease {
            discard: Some(Box::new(move || runner.discard_child_session(session_id))),
        }
    }

    /// Discard `session_id`'s dedicated machine immediately, unconditionally
    /// — for a `provision_child_session` that succeeded (the machine is
    /// registered) but something later in the same launch failed before any
    /// actor ever admitted onto it (a `transfer_custody`/shared-import
    /// failure, most likely). No actor exists yet to retire, so there is no
    /// "outstanding custody" worth deferring for, unlike
    /// [`Self::retire_child_session`]: this call site is the one place a
    /// child session with nothing depending on it is removed outright, so
    /// a failed launch never orphans a registered-but-unowned machine.
    /// Called from both `try_start_child` and `replacement.rs`'s matching
    /// resolution on any error after their own `provision_child_session`
    /// call succeeds.
    pub(crate) fn discard_child_session(&self, session_id: tidepool_repr::SessionId) {
        self.access.finish_child_session_teardown(session_id, None);
        if self
            .access
            .machines
            .remove(
                session_id,
                "child session launch failed before any actor admitted",
            )
            .is_some()
        {
            tracing::info!(session = ?session_id, "discarded a dedicated child session after a failed launch");
        }
    }

    pub(crate) fn resident_session_state(
        &self,
        session: tidepool_repr::SessionId,
    ) -> tidepool_runtime::session::ResidentSessionState
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        use tidepool_runtime::session::{ResidentSessionState, SlotKind};

        match self.access.machines.kind(session) {
            None => ResidentSessionState::Gone,
            Some(SlotKind::Running) => ResidentSessionState::Running,
            Some(SlotKind::Wedged) => ResidentSessionState::Unavailable,
            Some(SlotKind::Idle | SlotKind::Suspended) => self
                .access
                .machines
                .peek(session, |resident| resident.is_bootstrapped())
                .map_or(ResidentSessionState::Running, |bootstrapped| {
                    if bootstrapped {
                        ResidentSessionState::Reusable
                    } else {
                        ResidentSessionState::Uninitialized
                    }
                }),
        }
    }

    /// Snapshot the resident machine without checking it out. Returns `None`
    /// while a turn owns the machine or after the session has retired.
    #[must_use]
    pub fn measurement_snapshot(
        &self,
        session: tidepool_repr::SessionId,
    ) -> Option<ResidentMachineMeasurement>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        self.access
            .machines
            .peek(session, ResidentMachineMeasurement::of)
    }

    pub(crate) fn workbench(
        &self,
        response: ResponseExpectation,
        request: crate::RequestId,
        type_modules: Vec<String>,
    ) -> ResidentActorWorkbench<H, O> {
        ResidentActorWorkbench::from_access(
            self.access.sharing(),
            Some(response),
            Some(request),
            type_modules,
        )
    }

    pub(crate) fn application_workbench(&self) -> ResidentActorWorkbench<H, O> {
        ResidentActorWorkbench::from_access(self.access.sharing(), None, None, Vec::new())
    }
}

impl<H, O> ResidentActorWorkbench<H, O> {
    #[must_use]
    pub fn new(
        machines: Arc<ActorMachineRegistry<H, O>>,
        source: ActorWorkbenchSource,
        response: Option<ResponseExpectation>,
        request: Option<crate::RequestId>,
        type_modules: Vec<String>,
    ) -> Self {
        Self::from_access(
            ResidentMachineAccess::new(machines, source),
            response,
            request,
            type_modules,
        )
    }

    fn from_access(
        access: ResidentMachineAccess<H, O>,
        response: Option<ResponseExpectation>,
        request: Option<crate::RequestId>,
        type_modules: Vec<String>,
    ) -> Self {
        Self {
            access,
            response,
            request,
            type_modules: type_modules.into(),
            json_input: None,
            compilation_authority: None,
            private_execution: None,
        }
    }

    pub(crate) fn with_intrinsic_effect_support(
        mut self,
        keys: Vec<exomonad_tool::ToolEffectKey>,
    ) -> Self {
        let mut support = self.access.source.installed_effect_support.to_vec();
        for key in keys {
            if !support.contains(&key) {
                support.push(key);
            }
        }
        self.access.source.installed_effect_support = support.into();
        self
    }

    #[must_use]
    pub(crate) fn with_json_input(mut self, input: Option<serde_json::Value>) -> Self {
        self.json_input = input;
        self
    }

    pub(crate) fn with_compilation_authority(
        mut self,
        authority: Arc<crate::resident_actor::WorkbenchCompilationAuthority>,
    ) -> Self {
        self.compilation_authority = Some(authority);
        self
    }

    pub(crate) fn with_private_execution(mut self, execution: Arc<ExecutionPrivateScope>) -> Self {
        self.private_execution = Some(execution);
        self
    }
}

/// The owning boundary that refused a whole-cell public publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivatePublicationPhase {
    Freeze,
    Restage,
    CertifyAndStage,
    Publish,
    RevalidateRejection,
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorWorkbenchError {
    #[error(
        "actor {actor:?} public scope {scope:?} publication failed during {phase:?}: {source}"
    )]
    PrivatePublication {
        actor: crate::ActorRef,
        scope: ScopeId,
        phase: PrivatePublicationPhase,
        source: Box<ResidentActorWorkbenchError>,
    },
    #[error("actor continuation handoff refused: actor {actor:?}, placement {placement:?}, continuation {continuation}, reason {reason:?}")]
    ContinuationHandoff {
        actor: crate::ActorRef,
        placement: crate::ActorPlacement,
        continuation: String,
        reason: ContinuationHandoffFailure,
    },
    #[error("actor retired before machine admission: {0:?}")]
    RetiredBeforeAdmission(crate::ActorTerminal),
    #[error(transparent)]
    CompileView(#[from] ActorCompileViewError),
    #[error("resident machine checkout failed: {0}")]
    Checkout(CheckoutError<String>),
    #[error("resident workbench compiler failed: {0}")]
    Compile(CompileError),
    #[error("resident cell check failed: {0}")]
    CellCheck(tidepool_runtime::session::CellCheckFailure),
    #[error("resident workbench compiler infrastructure failed:\n{0}")]
    CompileInfrastructure(tidepool_toolchain::failclass::FailureEnvelope),
    #[error("resident workbench execution failed: {0}")]
    Resident(#[from] ResidentError),
    /// The response that answers a boundary's effect was already handed to
    /// the resident machine — `ResidentSession::resume`,
    /// `resume_handle`, or `resume_framed_custody` was actually called with
    /// the computed answer — and driving the resumed fragment onward from
    /// there failed. Distinguished from `Resident` (which also covers
    /// failures BEFORE that call, e.g. establishing actor execution context
    /// or computing the answer value) so a caller can tell a failure after
    /// delivery from one before or during it. See
    /// `tidepool_runtime::session::workbench::WorkbenchOperationDisposition::Unknown`
    /// (workbench.rs:265-268): that disposition means "failed after dispatch
    /// without proving whether its mutation crossed the commit point" — this
    /// variant proves that it did.
    #[error("resident workbench execution failed after delivering a response: {0}")]
    Delivered(ResidentError),
    /// A completed machine prefix could not be published after the authored
    /// cell had already failed. The original failure remains authoritative.
    #[error("{original}; completed-prefix publication also failed: {publication}")]
    PrefixPublication {
        #[source]
        original: Box<ResidentActorWorkbenchError>,
        publication: Box<ResidentActorWorkbenchError>,
    },
    /// An earlier cell hit an integrity failure; this actor's machine refuses
    /// all further execution.
    #[error(
        "this actor's Haskell machine was lost to an earlier integrity failure (reported by that \
         cell); no further cells can run here. Restart requires verified durable declarations; \
         live values are lost and effects are not replayed"
    )]
    MachineLost,
    #[error("resident workbench task panicked or was cancelled: {0}")]
    Join(tokio::task::JoinError),
    #[error("could not mount the typed completion input: {0}")]
    InputMount(String),
    #[error("could not inspect the saved value: {0}")]
    Inspection(String),
    #[error("actor protocol violation: {0}")]
    ActorProtocol(String),
    #[error(transparent)]
    ToolDeclaration(#[from] exomonad_tool::ToolDeclarationError),
    #[error("unsupported resident actor request `{constructor}`")]
    UnsupportedRequest { constructor: String },
    #[error("could not decode resident actor request `{constructor}`: {source}")]
    RequestDecode {
        constructor: String,
        source: BridgeError,
    },
    #[error("could not bridge an actor protocol value: {0}")]
    Bridge(#[from] tidepool_bridge::BridgeError),
    #[error(transparent)]
    InteractiveSessionCapture(#[from] crate::InteractiveSessionCaptureError),
    #[error(transparent)]
    StartCapture(#[from] crate::ActorStartCaptureError),
    #[error(transparent)]
    WaitCapture(#[from] crate::ActorWaitError),
    #[error("tool dispatch refused: {0}")]
    ToolDispatch(ToolDispatchError),
    #[error("invalid agent tool declaration set: {0}")]
    ToolDeclarations(serde_json::Error),
}

impl ResidentActorWorkbenchError {
    pub(crate) fn primary_failure(&self) -> &Self {
        match self {
            Self::PrefixPublication { original, .. } => original.primary_failure(),
            original => original,
        }
    }

    pub(crate) fn failure_diagnostic(
        &self,
    ) -> Option<tidepool_toolchain::failclass::FailureEnvelope> {
        match self.primary_failure() {
            Self::PrivatePublication { source, .. } => source.failure_diagnostic(),
            Self::Compile(error) => Some(classify_compile(error)),
            Self::CellCheck(failure) => Some(classify_compile(&failure.error)),
            Self::CompileInfrastructure(diagnostic) => Some(diagnostic.clone()),
            Self::Resident(ResidentError::Session(error))
            | Self::Delivered(ResidentError::Session(error)) => {
                Some(tidepool_runtime::failclass::classify_session(error))
            }
            Self::Resident(ResidentError::Run(error))
            | Self::Delivered(ResidentError::Run(error)) => {
                Some(tidepool_runtime::failclass::classify(error))
            }
            Self::Resident(ResidentError::Prepared(error))
            | Self::Delivered(ResidentError::Prepared(error)) => {
                Some(tidepool_runtime::failclass::classify_prepared(error))
            }
            _ => None,
        }
    }

    /// Whether this failure is only an observation budget running out
    /// somewhere in the turn, rather than a fault in the program, the heap,
    /// the value, or the actor protocol around it. `Resident` and
    /// `Delivered` are the two shapes that wrap a `ResidentError`, at
    /// either position (before or after an effect's answer was consumed);
    /// every other variant reports something that is actually wrong.
    pub(crate) fn is_observation_budget_exhausted(&self) -> bool {
        match self {
            Self::Resident(error) | Self::Delivered(error) => {
                error.is_observation_budget_exhausted()
            }
            _ => false,
        }
    }
}

/// Render an actor's ordered include roots for the `compile_blocking` span,
/// in search order, so a diagnostic reader sees the exact search path a
/// compile ran against without cross-referencing the actor registry.
fn render_include_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Preserve the resident session's authoritative boundary classification.
/// A rejected response leaves the parked effect available for retry; a
/// consumed response has crossed that boundary even if running onward failed.
fn classify_resumption(error: ResidentResumeError) -> ResidentActorWorkbenchError {
    match error {
        ResidentResumeError::Rejected(error) => ResidentActorWorkbenchError::Resident(error),
        ResidentResumeError::Consumed(error) => ResidentActorWorkbenchError::Delivered(error),
    }
}

enum MachineCheckoutAdmission {
    Wait(Option<Duration>),
    UntilRetirement(crate::RetainedActorExit),
}

impl From<Option<Duration>> for MachineCheckoutAdmission {
    fn from(wait: Option<Duration>) -> Self {
        Self::Wait(wait)
    }
}

impl<H, O> ResidentMachineAccess<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    async fn with_machine<ResultValue>(
        &self,
        context: crate::ActorSessionContext,
        operation: impl FnOnce(
                &mut ResidentSession<H, O>,
                &crate::ActorSessionContext,
                &ActorWorkbenchSource,
            ) -> Result<ResultValue, ResidentActorWorkbenchError>
            + Send
            + 'static,
    ) -> Result<ResultValue, ResidentActorWorkbenchError>
    where
        ResultValue: Send + 'static,
    {
        self.with_machine_wait(context, None, operation).await
    }

    /// Log the ordered include-root search path a session's compiles run
    /// against, once, and again whenever it actually changes — never on
    /// every `compile_blocking` line, where it would dominate the log.
    fn log_include_roots_if_changed(
        &self,
        session_id: tidepool_repr::SessionId,
        source_layer: &[PathBuf],
    ) {
        let rendered = render_include_roots(source_layer);
        let mut logged = self
            .logged_include_roots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if logged.get(&session_id) != Some(&rendered) {
            tracing::info!(
                session = ?session_id,
                include_roots = %rendered,
                "resident compile include roots"
            );
            logged.insert(session_id, rendered);
        }
    }

    async fn with_machine_wait<ResultValue>(
        &self,
        context: crate::ActorSessionContext,
        admission: impl Into<MachineCheckoutAdmission>,
        operation: impl FnOnce(
                &mut ResidentSession<H, O>,
                &crate::ActorSessionContext,
                &ActorWorkbenchSource,
            ) -> Result<ResultValue, ResidentActorWorkbenchError>
            + Send
            + 'static,
    ) -> Result<ResultValue, ResidentActorWorkbenchError>
    where
        ResultValue: Send + 'static,
    {
        // `compile_blocking` carries the context a compile-cost diagnostic
        // needs and that `spawn_blocking` would otherwise drop: which actor
        // and input unit (session) this turn belongs to. `Instrument`ing the
        // whole `with_host_machine` future (not just its `spawn_blocking`
        // closure) keeps the span active across every `.await` point inside
        // it, so `Span::current()` is correct wherever `spawn_blocking_in_span`
        // is eventually called. This is `info`, not `debug`: the actor/session
        // context needs to reach the INFO run log every cell logs at, not
        // just the detailed trace. `include_roots` stays out of the span
        // (and every line it prefixes) because the full search path is long
        // and rarely changes turn to turn; it is logged separately, once per
        // session and again only when it changes.
        let compile_span = tracing::info_span!(
            "compile_blocking",
            actor = %context.actor,
            session = %context.placement.session,
            operation = "resident_turn",
        );
        self.log_include_roots_if_changed(context.placement.session, &context.source_layer);
        let slot_owner = SLOT_CONTINUATION_OWNER.try_with(Clone::clone).ok();
        let invocation_cancel = INVOCATION_CANCEL.try_with(Arc::clone).ok();
        self.with_host_machine(
            context.actor.to_string(),
            context.placement.session,
            admission,
            move |session, source| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let admitted = |session: &mut ResidentSession<H, O>| {
                    if let Some(owner) = slot_owner {
                        let authority = owner.0.retained_authority.clone();
                        let observer: Arc<dyn Fn(ResidentContinuationEvent) + Send + Sync> =
                            Arc::new(move |event| owner.observe(event));
                        let observed = |session: &mut ResidentSession<H, O>| {
                            session.with_continuation_observer(observer, |session| {
                                operation(session, &context, source)
                            })
                        };
                        if let Some(authority) = authority {
                            session.with_continuation_resource_owner(authority, observed)
                        } else {
                            observed(session)
                        }
                    } else {
                        operation(session, &context, source)
                    }
                };
                match invocation_cancel {
                    Some(cancel) => session.with_invocation_cancel(cancel, admitted),
                    None => admitted(session),
                }
            },
        )
        .instrument(compile_span)
        .await
    }

    async fn with_host_machine<T: Send + 'static>(
        &self,
        actor: impl Into<String>,
        session_id: tidepool_repr::SessionId,
        admission: impl Into<MachineCheckoutAdmission>,
        operation: impl FnOnce(
                &mut ResidentSession<H, O>,
                &ActorWorkbenchSource,
            ) -> Result<T, ResidentActorWorkbenchError>
            + Send
            + 'static,
    ) -> Result<T, ResidentActorWorkbenchError> {
        let actor = actor.into();
        let request = tidepool_runtime::session::registry::CheckoutRequest::Run;
        let admission_started = std::time::Instant::now();
        let retirement = match admission.into() {
            MachineCheckoutAdmission::Wait(wait) => (wait, None),
            MachineCheckoutAdmission::UntilRetirement(retirement) => (None, Some(retirement)),
        };
        let checkout = match &retirement {
            (_, Some(retirement)) => {
                tokio::select! {
                    biased;
                    terminal = retirement.wait_requested_shutdown() => {
                        return Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(terminal));
                    }
                    checkout = self.machines.checkout_queued(session_id, request) => checkout,
                }
            }
            (Some(limit), None) => {
                self.machines
                    .checkout_wait(session_id, request, *limit)
                    .await
            }
            (None, None) => self.machines.checkout_queued(session_id, request).await,
        }
        .map_err(|error| {
            if matches!(
                error,
                tidepool_runtime::session::registry::CheckoutError::Unknown(_)
                    | tidepool_runtime::session::registry::CheckoutError::Retired { .. }
                    | tidepool_runtime::session::registry::CheckoutError::Terminal { .. }
            ) {
                self.finish_child_session_teardown(session_id, None);
            }
            ResidentActorWorkbenchError::Checkout(error)
        })?;
        // Every cell holds the run's resident machine exclusively, so the
        // wait for it is a primary per-cell cost. `info`, not `debug`: this
        // needs to reach the run's INFO log, not just the detailed trace.
        tracing::info!(actor = %actor, session = ?session_id,
            waited_ms = admission_started.elapsed().as_millis(),
            "resident machine checkout admitted");
        crate::call_timing::add_checkout_wait_ms(admission_started.elapsed().as_millis());
        let held_since = std::time::Instant::now();
        let (mut session, receipt) = checkout.into_parts();
        if let Some(registry) = &self.image_registry {
            // No-op before this session's own first turn bootstraps its
            // engine; cheap (an `Arc` clone into a `Option` slot) and safe
            // to repeat on every checkout otherwise — see
            // `ResidentMachineAccess::image_registry`.
            session.set_image_registry(Arc::clone(registry));
        }
        let source = self.source.clone();
        let machines = Arc::clone(&self.machines);
        let child_cleanup = self.sharing();

        let task = spawn_blocking_in_span(move || {
            // The blocking task owns the machine and its linear checkout
            // receipt together. Its async caller may be cooperatively
            // cancelled while this closure is running; settlement must not
            // depend on that caller continuing to poll the JoinHandle.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(retirement) = &retirement.1 {
                    retirement
                        .claim_before_shutdown(|| true)
                        .map_err(ResidentActorWorkbenchError::RetiredBeforeAdmission)?;
                }
                operation(&mut session, &source)
            }));
            match outcome {
                Ok(outcome) => {
                    if session.compilation_failed() {
                        let reason = match &outcome {
                            Err(detail) => format!(
                                "machine became unavailable after a runtime fault: {detail}"
                            ),
                            Ok(_) => "machine became unavailable after a runtime fault \
                                       (no further detail was reported)"
                                .to_string(),
                        };
                        machines.settle_retire(receipt, reason);
                        child_cleanup.finish_child_session_teardown(session_id, None);
                        tracing::info!(actor = %actor, session = ?session_id,
                            held_ms = held_since.elapsed().as_millis(),
                            "resident machine checkout released (retired)");
                        return outcome;
                    }

                    // A dedicated child session whose actor already retired
                    // gets another look on every checkout, including the
                    // cleanup owner's custody-release retry. Once nothing is
                    // left, the machine is removed outright instead of
                    // settling back to idle.
                    let pending_owner = child_cleanup
                        .pending_child_teardown
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(&session_id)
                        .cloned();
                    if let Some(owner) = pending_owner {
                        let outstanding = session.outstanding_custody();
                        if outstanding == 0 {
                            child_cleanup.finish_child_session_teardown(session_id, Some(&owner));
                            machines.settle_retire(
                                receipt,
                                "actor retired, deferred custody now released",
                            );
                            tracing::info!(actor = %actor, session = ?session_id,
                                held_ms = held_since.elapsed().as_millis(),
                                "dedicated child session released (deferred teardown completed)");
                            return outcome;
                        }
                        tracing::info!(actor = %actor, session = ?session_id,
                            outstanding, "dedicated child session release still deferred");
                    }

                    let holes = session
                        .parked_holes()
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                    machines.settle_suspended(receipt, session, holes);
                    tracing::info!(actor = %actor, session = ?session_id,
                        held_ms = held_since.elapsed().as_millis(),
                        "resident machine checkout released");
                    outcome
                }
                Err(payload) => {
                    // Peek at the panic message without consuming the
                    // payload — `resume_unwind` below still needs it intact.
                    let message = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic payload".to_string());
                    machines.settle_retire(
                        receipt,
                        format!("machine became unavailable: a resident turn panicked ({message})"),
                    );
                    child_cleanup.finish_child_session_teardown(session_id, None);
                    tracing::info!(actor = %actor, session = ?session_id,
                        held_ms = held_since.elapsed().as_millis(),
                        "resident machine checkout released (panic)");
                    std::panic::resume_unwind(payload);
                }
            }
        })
        .await;
        // Measured here, not inside the blocking closure above: the closure
        // runs on a blocking-pool thread, which does not inherit this task's
        // `call_timing` task-local. All three exit branches inside it (retired,
        // suspended, panic-then-resume_unwind) log their own `held_ms` before
        // returning or unwinding, so this elapsed-since-`held_since` read,
        // taken right after the join, covers every one of them in one place.
        crate::call_timing::add_checkout_hold_ms(held_since.elapsed().as_millis());

        match task {
            Ok(outcome) => outcome,
            Err(error) => Err(ResidentActorWorkbenchError::Join(error)),
        }
    }
}

impl<H, O> ResidentActorWorkbench<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) fn actor_initialization_cleanup(
        &self,
        context: crate::ActorSessionContext,
    ) -> ParkedHoleAbortGuard {
        // Inline actor admission must never borrow the creating cell's observer.
        ParkedHoleAbortGuard::with_retained_latest(
            &self.access,
            context,
            None,
            "actor initialization abandoned before standing custody".into(),
            None,
        )
    }

    pub(crate) async fn settle_initialization_custody(
        &self,
        context: crate::ActorSessionContext,
        registration: ParkedHoleAbortRegistration,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                if registration.0.owner != Some((context.actor, context.placement)) {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "initialization custody differs from its original actor placement".into(),
                    ));
                }
                let mut state = registration.0.state.lock();
                let ParkedHoleState::Owned(current) = &*state else {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "actor initialization lost its continuation custody".into(),
                    ));
                };
                for cont_id in current {
                    if session.parked_realm_named(cont_id) != Some(context.placement.resource_scope)
                    {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "actor initialization frame {cont_id} differs from owning realm {:?}",
                            context.placement.resource_scope,
                        )));
                    }
                }
                // The registered standing and actor shutdown now own this exact realm.
                *state = ParkedHoleState::Settled;
                Ok(())
            })
            .await
    }

    pub(crate) fn continuation_cleanup_owner(
        &self,
        context: crate::ActorSessionContext,
        reason: String,
        authority: Arc<crate::resident_actor::ExecutionResourceOwners>,
    ) -> ParkedHoleAbortGuard {
        ParkedHoleAbortGuard::with_retained_latest(
            &self.access,
            context,
            None,
            reason,
            Some(authority),
        )
    }

    /// Abandon this invocation before waiting for checkout. A blocking native
    /// action can still create a successor; its existing observer registers it
    /// with this same abandoned owner. Publication may continue only after the
    /// checkout proves every registered continuation has actually retired.
    pub(crate) async fn abort_owned_continuations(
        &self,
        context: crate::ActorSessionContext,
        registration: ParkedHoleAbortRegistration,
        reason: String,
    ) -> Result<(), ResidentActorWorkbenchError> {
        registration.abandon_for_acknowledgement(&context)?;
        self.access
            .with_machine(context, move |session, _, _| {
                for cont_id in registration.awaiting_acknowledgement() {
                    abort_owned_hole(session, cont_id.clone(), reason.clone())?;
                    registration.observe(ResidentContinuationEvent::Retired(cont_id));
                }
                registration.confirm_acknowledgement_in_checkout()
            })
            .await
    }

    pub(crate) async fn with_exact_continuation_cleanup<T>(
        &self,
        context: crate::ActorSessionContext,
        reason: String,
        operation: impl std::future::Future<Output = T>,
    ) -> T {
        let retained_authority = SLOT_CONTINUATION_OWNER
            .try_with(|owner| owner.0.retained_authority.clone())
            .ok()
            .flatten();
        let guard = ParkedHoleAbortGuard::with_retained_latest(
            &self.access,
            context,
            None,
            reason,
            retained_authority,
        );
        SLOT_CONTINUATION_OWNER
            .scope(guard.registration(), operation)
            .await
    }

    /// Resolve this actor's spec without compiling anything, for status and
    /// for a reload receipt that must name the rule before it knows whether
    /// the spec compiles.
    pub(crate) fn resolve_spec(
        &self,
        context: &crate::ActorSessionContext,
    ) -> crate::agent_spec::ResolvedSpec {
        // The roots this actor's cells resolve a module in, in GHC's order:
        // private helpers, then the run graph. Helpers cannot define
        // AgentSpec, so every actor discovers the run's current spec.
        let mut roots = context.source_layer.to_vec();
        roots.extend(self.access.source.base_include.iter().cloned());
        crate::agent_spec::resolve(roots, self.access.source.spec.as_deref())
    }

    /// Compile one spec and keep both of its products.
    ///
    /// One fragment produces the declarations — read out of the
    /// `AgentToolsInstallWith` suspension — and the retained dispatcher, kept
    /// as an `Arc<RootCustody>` heap root, which covers ordinary tool calls
    /// and every slot the spec fills. They are two products of one compile of
    /// one module against one include list, so a schema can never advertise a
    /// handler from another revision, and a slot can never be a revision ahead
    /// of the tools beside it.
    pub(crate) fn prepare_tools<'a>(
        &'a self,
        context: crate::ActorSessionContext,
        install: u64,
        granted_effects: Vec<exomonad_tool::ActorEffectKey>,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<ResidentWorkbenchTools, ResidentActorWorkbenchError>,
    > {
        Box::pin(async move {
            let source_support = self.access.source.installed_effect_support().to_vec();
            let observer = self.access.handler_effect_support.clone();
            let admitted_support = self
                .access
                .with_machine(context.clone(), move |session, _, _| {
                    let mut support = source_support;
                    for key in observer(session.handlers()) {
                        if !support.contains(&key) {
                            support.push(key);
                        }
                    }
                    Ok(support)
                })
                .await?;
            let admitted_base: Vec<_> = granted_effects
                .into_iter()
                .filter(|key| admitted_support.contains(&exomonad_tool::ToolEffectKey::Actor(*key)))
                .collect();
            let resolved = self.resolve_spec(&context);
            let revision = resolved.source_revision();
            let entry = resolved.entry.clone().unwrap_or_else(|| {
                if admitted_support.contains(&exomonad_tool::ToolEffectKey::ContextReadWrite) {
                    "Tidepool.Agent.Contract.defaultWorkbenchSpec".into()
                } else {
                    "Tidepool.Agent.Contract.defaultAsyncWorkbenchSpec".into()
                }
            });
            let installation = crate::agent_spec::installation_expression(&entry, &admitted_base);
            let authored_effects = installation.effect_row;
            let dispatcher_effects = format!("(Tidepool.Effects.Core.AgentTools ': Tidepool.Effects.Core.ContextReadWrite ': {authored_effects})");
            let mut source = self.access.source.clone();
            // A spec found by convention is named by no configured key, so its
            // module is not in the shared workbench vocabulary; the fragment that
            // installs it brings its own qualified import.
            if let Some((module, _)) = entry.rsplit_once('.') {
                source
                    .workbench_imports
                    .extend_text(&format!("qualified {module}"));
            }
            source
                .workbench_imports
                .extend_text("qualified Tidepool.Agent.Contract");
            source.preamble =
                insert_preamble_imports(&source.preamble, "qualified Tidepool.Effects.Core").into();
            let mut compile_context = context.clone();
            compile_context.haskell_effects_alias = dispatcher_effects.clone();
            let installation_scope = self
                .access
                .with_machine(context.clone(), |session, context, _| {
                    session
                        .retain_lexical_scope(context.placement.lexical_scope)
                        .map_err(Into::into)
                })
                .await?;
            compile_context.placement.lexical_scope = installation_scope.scope();
            let abort_guard = ParkedHoleAbortGuard::with_retained_latest(
                &self.access,
                compile_context.clone(),
                None,
                "tool installation was abandoned before settlement".into(),
                Some(installation_scope.clone()),
            );
            let registration = abort_guard.registration();
            let block = ParsedBlock {
                ordinal: 1,
                total: 1,
                source: installation.expression,
            };
            let authority = self.compilation_authority.clone().ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "tool installation requires its admitted source authority".into(),
                )
            })?;
            let mut installer = ResidentActorWorkbench {
                access: self.access.sharing(),
                response: None,
                request: None,
                type_modules: Arc::from([]),
                json_input: None,
                compilation_authority: Some(authority.clone()),
                private_execution: None,
            };
            installer.access.source = source;
            let publication_resolved = resolved;
            let installed_effect_support =
                installer.access.source.installed_effect_support().to_vec();
            let handler_effect_support = self.access.handler_effect_support.clone();
            // Setup uses the existing public scope while the bootstrap owner
            // retains publication authority. The checked program seals its
            // captured interfaces and protected templates before native execution.
            // Box compiler state separately from the tool-preparation future.
            let (checked, prepared) = Box::pin(installer.prepare_checked_cell(
                compile_context.clone(),
                block.source,
                authority,
                None,
                None,
            ))
            .await
            .map_err(|error| {
                tracing::error!(?error, "agent spec installation check failed");
                error
            })?;
            if checked.items.len() != 1
                || checked.items[0].verdict.kind != TurnKind::Bind
                || !checked.items[0].verdict.binders.is_empty()
            {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "tool installer must be one checked bind without public binders".into(),
                ));
            }
            let PreparedCell::Ready {
                mut items,
                dependencies,
            } = prepared
            else {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "tool installer checked program was rejected before execution".into(),
                ));
            };
            let item = items.pop().ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    "tool installer checked program has no executable item".into(),
                )
            })?;
            let block = ParsedBlock {
                ordinal: 1,
                total: 1,
                source: checked.items[0].source.clone(),
            };
            // Execution stays inside the registration while its state is boxed.
            let step = registration
                .scope(Box::pin(installer.begin_prepared_cell_item(
                    compile_context.clone(),
                    block,
                    item,
                    0,
                )))
                .await?;
            let ResidentWorkbenchStep::Running { outcome, .. } = step else {
                let detail = match step {
                    ResidentWorkbenchStep::Rejected(detail) => detail.output,
                    _ => "installer completed without publishing its handler".into(),
                };
                return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "tool installation: {detail}"
                )));
            };
            let ResidentOutcome::Suspended { hole, request, .. } = *outcome else {
                unreachable!("running fragment has a suspension")
            };
            // `hole` is plain session-held data held across the checkout below
            // being acquired; if this future is dropped while still awaiting
            // that checkout, nothing else resumes or aborts the continuation it
            // parked. `ParkedHoleAbortGuard` covers that gap the same way
            // `HostInputRetirement` covers the split cell's mounted input:
            // Drop can't await, so it spawns one more checkout in the
            // background to abort the hole there, retaining the installer
            // scope until retirement. Successful publication disarms it.
            let resumption_registration = registration.clone();
            let publication = registration.scope(self
                .access
                .with_machine(compile_context, move |session, context, _| {
                    let publication = (|| {
                        let ResidentRequest::AgentTools(
                            crate::generated::agent_tools::AgentToolsReq::AgentToolsInstallWith(
                                declarations,
                                _,
                            ),
                        ) = ResidentRequest::decode(&request, session.data_con_table())?
                        else {
                            return Err(ResidentActorWorkbenchError::ActorProtocol(
                                "tool installer crossed an unexpected effect boundary".into(),
                            ));
                        };
                        let installation = tidepool_runtime::value_to_json(
                            &declarations,
                            session.data_con_table(),
                            0,
                        );
                        let SpecInstallation {
                            tools,
                            slots,
                            slot_effect_keys,
                        } = decode_installation(installation)?;
                        let mut installed_effect_support = installed_effect_support.to_vec();
                        for key in handler_effect_support(session.handlers()) {
                            if !installed_effect_support.contains(&key) {
                                installed_effect_support.push(key);
                            }
                        }
                        crate::tool_contract::validate_installation(
                            &tools,
                            &slots,
                            &slot_effect_keys,
                            &admitted_base,
                            &installed_effect_support,
                        )
                        .map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                        })?;
                        let declarations = crate::resident_interactive::project_tools(tools)
                            .map_err(|error| {
                                ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                            })?;
                        let dispatch = session
                            .live_payload_handle_owned_by(
                                hole.cont_id(),
                                context.placement.resource_scope,
                            )?
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "tool installer did not retain its dispatcher".into(),
                                )
                            })?;
                        Ok((declarations, slots, dispatch))
                    })();
                    let (declarations, slots, dispatch) = match publication {
                        Ok(publication) => publication,
                        Err(error) => {
                            match session.abort(hole.cont_id(), "tool publication rejected".into())
                            {
                                Ok(outcome) => resumption_registration.replace_in_checkout(session, &outcome),
                                Err(abort_error) => tracing::warn!(
                                    hole = hole.cont_id(),
                                    %abort_error,
                                    "failed to abort parked hole after tool publication rejection"
                                ),
                            }
                            return Err(error);
                        }
                    };
                    let settled = session
                        .resume_classified(hole, ())
                        .map_err(classify_resumption)?;
                    resumption_registration.replace_in_checkout(session, &settled);
                    if !matches!(
                        settled,
                        ResidentOutcome::Completed { .. }
                            | ResidentOutcome::BindingsCommitted { .. }
                    ) {
                        if let ResidentOutcome::Suspended { hole, .. } = settled {
                            match session.abort(
                                hole.cont_id(),
                                "tool installer must finish after publication".into(),
                            ) {
                                Ok(outcome) => resumption_registration.replace_in_checkout(session, &outcome),
                                Err(abort_error) => tracing::warn!(
                                    hole = hole.cont_id(),
                                    %abort_error,
                                    "failed to abort parked hole after tool installer overrun"
                                ),
                            }
                        }
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "tool installer did not finish after publication".into(),
                        ));
                    }
                    Ok(ResidentWorkbenchTools {
                        declarations,
                        dispatch: Arc::new(dispatch),
                        _installation_scope: installation_scope,
                        dispatcher_effects,
                        slots,
                        resolved: publication_resolved,
                        install,
                        revision,
                    })
                }))
                .await;
            if publication.is_ok() {
                abort_guard.disarm();
            }
            drop(dependencies);
            publication
        })
    }

    /// Drive a display callback using its actor-issued input. No source is compiled.
    pub(crate) async fn expand_display(
        &self,
        context: crate::ActorSessionContext,
        callback: Arc<RootCustody>,
        identity: (i64, i64, i64),
        key: i64,
        allowance: i64,
    ) -> Result<
        (tidepool_runtime::session::WorkbenchDisplayPage, RootCustody),
        ResidentActorWorkbenchError,
    > {
        let allowance = allowance.clamp(0, 8192);
        let mut context = context;
        context.effect_policy = tidepool_effect::EffectRunPolicy::SuspendAll;
        self.access.with_machine(context, move |session, context, _| {
            let scope = context.placement.resource_scope;
            let mut outcome = session.run_rooted_entry_borrowed("display_expansion", &callback, 0, scope, None)?;
            let mut input_received = false;
            let mut published = None;
            loop {
                match outcome {
                    ResidentOutcome::Completed { .. } if input_received => return published.ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("display callback completed without publishing detail".into())),
                    ResidentOutcome::Suspended { hole, request, .. } => {
                        let next = (|| {
                            match ResidentRequest::decode(&request, session.data_con_table())? {
                                ResidentRequest::Console(crate::generated::console::ConsoleReq::DisplayExpansionInputWith) if !input_received => {
                                    input_received = true;
                                    Ok(session.resume(hole.clone(), (identity, key, allowance)))
                                }
                                ResidentRequest::Console(crate::generated::console::ConsoleReq::DisplayWith((issued, text, expansions, unavailable), _)) if input_received && published.is_none() && issued == identity => {
                                    let output = tidepool_runtime::session::WorkbenchDisplayPage { identity, text, expansions, unavailable };
                                    crate::resident_actor::validate_display_page(&output.text, allowance)?;
                                    crate::resident_actor::validate_display_metadata(&output)?;
                                    let callback = session.live_payload_handle_owned_by(hole.cont_id(), scope)?.ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("display update has no retained callback".into()))?;
                                    published = Some((output, callback));
                                    Ok(session.resume(hole.clone(), identity))
                                }
                                _ => Err(ResidentActorWorkbenchError::ActorProtocol("display callback crossed an unauthorized boundary".into())),
                            }
                        })();
                        match next {
                            Ok(next) => outcome = next?,
                            Err(error) => {
                                let _ = session.abort(hole.cont_id(), "display expansion rejected".into());
                                return Err(error);
                            }
                        }
                    }
                    _ => return Err(ResidentActorWorkbenchError::ActorProtocol("display callback did not follow its input/publication protocol".into())),
                }
            }
        }).await
    }

    /// Apply the retained handler with invocation data. No source compiler is involved.
    pub(crate) async fn begin_tool(
        &self,
        context: crate::ActorSessionContext,
        dispatch: Arc<RootCustody>,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let outcome = session
                    .run_rooted_entry_borrowed(
                        "hosted_tool",
                        &dispatch,
                        0,
                        context.placement.resource_scope,
                        None,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "tool dispatcher completed without requesting invocation data".into(),
                    ));
                };
                let input = (|| {
                    if !matches!(
                        ResidentRequest::decode(&request, session.data_con_table())?,
                        ResidentRequest::AgentTools(
                            crate::generated::agent_tools::AgentToolsReq::AgentToolsInputWith
                        )
                    ) {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "tool dispatcher crossed an unexpected input boundary".into(),
                        ));
                    }
                    Ok((name, arguments))
                })();
                let answer = match input {
                    Ok(answer) => answer,
                    Err(error) => {
                        if let Err(abort_error) =
                            session.abort(hole.cont_id(), "tool invocation input rejected".into())
                        {
                            tracing::warn!(
                                hole = hole.cont_id(),
                                %abort_error,
                                "failed to abort parked hole after tool invocation input rejection"
                            );
                        }
                        return Err(error);
                    }
                };
                let outcome = session.resume(hole, answer);
                start_fragment_settlement(
                    session,
                    context,
                    1,
                    // A tool invocation has no submitted cell to point at.
                    String::new(),
                    WorkbenchDisplay::ToolDispatch,
                    Vec::new(),
                    outcome,
                )
            })
            .await
    }

    /// Apply the retained after-tool slot to a finished call and what it
    /// answered.
    ///
    /// The same `Arc<RootCustody>` an ordinary call is an application of,
    /// entered at the slot's own index instead of zero. Nothing compiles here,
    /// and nothing is retained per call: a slot invocation costs exactly what
    /// a tool invocation costs.
    pub(crate) async fn begin_after_tool(
        &self,
        context: crate::ActorSessionContext,
        dispatch: Arc<RootCustody>,
        tool: String,
        payload: serde_json::Value,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let outcome = session
                    .run_rooted_entry_borrowed(
                        "after_tool_slot",
                        &dispatch,
                        crate::after_tool::AFTER_TOOL_ENTRY,
                        context.placement.resource_scope,
                        None,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "after-tool slot completed without requesting its input".into(),
                    ));
                };
                let input = (|| {
                    if !matches!(
                        ResidentRequest::decode(&request, session.data_con_table())?,
                        ResidentRequest::AgentTools(
                            crate::generated::agent_tools::AgentToolsReq::AgentToolsInputWith
                        )
                    ) {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "after-tool slot crossed an unexpected input boundary".into(),
                        ));
                    }
                    Ok((tool, payload))
                })();
                let answer = match input {
                    Ok(answer) => answer,
                    Err(error) => {
                        if let Err(abort_error) =
                            session.abort(hole.cont_id(), "after-tool slot input rejected".into())
                        {
                            tracing::warn!(
                                hole = hole.cont_id(),
                                %abort_error,
                                "failed to abort parked hole after after-tool slot input rejection"
                            );
                        }
                        return Err(error);
                    }
                };
                let outcome = session.resume(hole, answer);
                start_fragment_settlement(
                    session,
                    context,
                    1,
                    // A slot invocation has no submitted cell to point at.
                    String::new(),
                    WorkbenchDisplay::Tool,
                    Vec::new(),
                    outcome,
                )
            })
            .await
    }

    /// Bind one tool result under the handle the slot was shown, so a pruned
    /// view keeps the whole of what it selected from addressable.
    ///
    /// A value the model may want back belongs in the lexical environment it
    /// already computes in. The handle is chosen before the slot runs, and
    /// defined only when the slot actually prunes.
    pub(crate) async fn bind_tool_result(
        &self,
        context: crate::ActorSessionContext,
        binding: String,
        result: String,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let carrier = self.carrier_for(&context, HostCarrierKind::Text).await?;
        self.access
            .with_machine(context, move |session, context, source| {
                mount_text_binding(
                    session,
                    context,
                    source,
                    &[],
                    &binding,
                    &result,
                    Some(&carrier),
                )
                .map_err(|error| {
                    ResidentActorWorkbenchError::InputMount(format!(
                        "the whole result could not be bound as {binding}: {error}"
                    ))
                })
            })
            .await
    }

    /// Compile the input interface and preview together, commit the input,
    /// then render it. A failed preview leaves the mounted binding available.
    pub(crate) async fn mount_activation_input(
        &self,
        context: crate::ActorSessionContext,
        input: tidepool_runtime::session::RuntimeActivationInput,
        reply_type: String,
        reply_declaration: Option<String>,
        reply_declaration_modules: Vec<String>,
    ) -> Result<(String, String, tidepool_repr::SessionVarId), ResidentActorWorkbenchError> {
        let reply_declaration = reply_declaration.filter(|_| {
            reply_declaration_modules.iter().any(|module| {
                declaration_worth_showing(module, &self.access.source.workspace_modules)
            })
        });
        let mut source = self.access.source.clone();
        if let (Some(response), Some(request)) = (&self.response, self.request) {
            source.preamble = response
                .request_preamble(&source.preamble, request, &context.haskell_effects_alias)
                .into();
        }
        source.preamble = actor_preamble(&source.preamble, &context).into();
        let type_modules = self.type_modules.clone();
        let authority = self.compilation_authority.clone().ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "activation requires its original source authority".into(),
            )
        })?;
        let preview = self
            .access
            .with_machine(context, move |session, context, _| {
                use tidepool_runtime::session::turn::{check_activation_input, compile_activation_input};
                reject_activation_declaration(session, context.placement.lexical_scope)?;
                let view = actor_compile_view(session, context, &source, &type_modules)?;
                let prepared = source.prepare_effectful(&view, &context.haskell_effects_alias)?;
                let preamble = insert_preamble_imports(&prepared.preamble, &prepared.imports);
                let candidate = session.next_declaration_module().ok_or_else(|| {
                    ResidentActorWorkbenchError::InputMount("activation has no declaration plane".into())
                })?;
                let check_preamble = cell_module_preamble(&prepared.preamble, &candidate.module_name())?;
                let template = resident_cell_check_template(
                    &check_preamble,
                    &context.haskell_effects_alias,
                    &prepared.imports,
                );
                let evidence = cell_check_evidence(&view, &template, &prepared);
                let owner = session.admit_activation_input_in(
                    context.placement.lexical_scope,
                    input,
                    &preamble,
                    &context.haskell_effects_alias,
                    ACTIVATION_INPUT_LIMIT,
                    template,
                    authority.clone(),
                    authority.authority_digest(),
                    prepared.include,
                    evidence,
                ).map_err(ResidentActorWorkbenchError::Resident)?;
                let checked = check_activation_input(&owner).map_err(|failure| {
                    ResidentActorWorkbenchError::InputMount(failure.error.to_string())
                })?;
                let item = session.admit_activation_input_item(&owner, &checked)
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let compiled = compile_activation_input(&owner, item).map_err(|failure| {
                    ResidentActorWorkbenchError::InputMount(failure.error.to_string())
                })?;
                let mounted = session.mount_activation_input(owner, compiled)
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let binding = mounted.binding();
                let preview = match session.run_activation_preview(mounted) {
                    Ok(ResidentOutcome::Suspended { hole, .. } | ResidentOutcome::Deferred { hole, .. }) => {
                        if let Err(abort_error) = session
                            .abort(hole.cont_id(), "pure activation preview suspended".into())
                        {
                            tracing::warn!(
                                hole = hole.cont_id(),
                                %abort_error,
                                "failed to abort parked hole after pure activation preview suspended"
                            );
                        }
                        return Err(ResidentActorWorkbenchError::Inspection(
                            "pure activation preview suspended".into(),
                        ));
                    }
                    Ok(outcome) => decode_activation_observation(outcome),
                    Err(error) if error.is_observation_budget_exhausted()
                        || matches!(&error,
                            ResidentError::Prepared(prepared)
                                if matches!(prepared,
                                    tidepool_runtime::session::PreparedRuntimeError::Run(
                                        tidepool_codegen::prepared_program::ExecutionError::Runtime(_)
                                    )) && prepared.kind()
                                        == tidepool_runtime::session::PreparedFailureKind::Language
                        ) => {
                        Err(ResidentActorWorkbenchError::Resident(error))
                    }
                    Err(error) => return Err(ResidentActorWorkbenchError::Resident(error)),
                };
                Ok((preview, binding))
            })
            .await?;
        let (preview, input_binding) = preview;
        let input = match preview {
            Ok((text, omitted)) => bounded_activation_text(
                text, ACTIVATION_INPUT_LIMIT, omitted, "inspectFull sessionInput",
            ),
            Err(_) => "<input rendering unavailable; use lookup for sessionInput, then select or apply the value>".into(),
        };
        let reply = match reply_declaration {
            Some(declaration) => declaration,
            None if reply_declaration_modules.is_empty() => {
                format!("{reply_type} (no declaration captured at the request site)")
            }
            // A library/stdlib reply type: "reply type X" above already
            // names it, and its internal representation would not help a
            // model construct one.
            None => String::new(),
        };
        Ok((
            input,
            bounded_activation_text(reply, 4 * 1024, false, &format!("lookup {reply_type}")),
            input_binding,
        ))
    }

    /// Prepare one resident cell, splitting `check_cell` (GHC) and every
    /// per-item compile off the machine checkout: snapshot under a short
    /// checkout
    /// ([`snapshot_cell_split`]), check off-checkout
    /// ([`check_cell_off_checkout`]), re-checkout to reserve generations and,
    /// for a cell with a `Decl` item, render its next declaration candidate
    /// ([`reserve_cell_generations`]), GHC-validate that candidate
    /// off-checkout against a private directory nobody else's compile has on
    /// its include path and compile every other item against it
    /// ([`validate_declaration_candidate`], [`compile_cell_items_off_checkout`]),
    /// then re-checkout once more to revalidate and install — writing the
    /// already-validated declaration into the shared session root for the
    /// first time only now ([`finalize_cell_install`]). Each re-checkout
    /// detects a stale snapshot via
    /// [`crate::ActorCompileView::is_current_for`] and an unchanged
    /// `next_declaration_module()` ([`split_staleness`]).
    ///
    /// The split is attempted once. The first stale re-checkout falls
    /// straight through to [`Self::prepare_cell_single_checkout`], whose one
    /// exclusive checkout stays held across its own declaration staging and
    /// every item compile and so cannot go stale. An inherited-context fork
    /// shares its parent's scope chain, and a parent committing cells goes
    /// stale faster than a split compile completes: a second split attempt
    /// would repeat the same GHC work against a view the parent is still
    /// moving, not wait for it to settle (see [`CHEAP_RETRY_ATTEMPTS`] for
    /// the paths whose retry is cheap enough to make).
    pub(crate) async fn prepare_cell(
        &self,
        context: crate::ActorSessionContext,
        cell_source: String,
    ) -> Result<(CellCheck, PreparedCell), ResidentActorWorkbenchError> {
        let response = self.response.clone();
        let request = self.request;
        let type_modules = Arc::clone(&self.type_modules);
        let effects = context.haskell_effects_alias.clone();

        // Mount the request's JSON input carrier once, before any checkout
        // this split releases, and keep it leased — never retired — across
        // every one of them, so a later checkout's freshly re-derived view
        // still resolves it. `HostInputRetirement` retires it on every exit
        // path below (explicitly on the ones taken here; in the background,
        // on drop, for an error `?` return or this future's own
        // cancellation).
        let mut leased_input = match &self.json_input {
            Some(input) => {
                let carrier = self.carrier_for(&context, HostCarrierKind::Json).await?;
                let mount_context = context.clone();
                let mount_source = self.access.source.clone();
                let mount_type_modules = Arc::clone(&type_modules);
                let mount_input = input.clone();
                let (mounted, lease) = self
                    .access
                    .with_machine(mount_context, move |session, context, _| {
                        let mounted = mount_json_input(
                            session,
                            context,
                            &mount_source,
                            &mount_type_modules,
                            &mount_input,
                            Some(&carrier),
                        )?;
                        let lease =
                            session.lease_bindings(&[tidepool_repr::VarId(mounted.binder.var_id)]);
                        Ok((mounted, lease))
                    })
                    .await?;
                Some(HostInputRetirement::new(
                    &self.access,
                    context.clone(),
                    mounted,
                    lease,
                ))
            }
            None => None,
        };

        match (&self.compilation_authority, &self.private_execution) {
            (Some(authority), Some(execution)) => {
                return self
                    .prepare_checked_cell(
                        context,
                        cell_source,
                        authority.clone(),
                        Some(execution.clone()),
                        leased_input,
                    )
                    .await;
            }
            (None, None) => {}
            _ => return Err(ResidentActorWorkbenchError::ActorProtocol(
                "private cell compilation requires both source authority and execution admission"
                    .into(),
            )),
        }

        // Carries one cancellation edge across every off-checkout GHC call
        // this split makes, the same shape `prepare_cell_single_checkout`
        // arms for its own (single-checkout) compile: dropping this future
        // before it settles interrupts whichever compile is in flight,
        // rather than leaving it to finish unobserved.
        let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));

        let (stage, changed) = 'split: {
            let snapshot_source = self.access.source.clone();
            let snapshot_type_modules = Arc::clone(&type_modules);
            let snapshot_response = response.clone();
            let snapshot_mounted_input = leased_input
                .as_ref()
                .map(|guard| guard.mounted_input().clone());
            let (source, snapshot) = self
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    snapshot_cell_split(
                        session,
                        context,
                        snapshot_source,
                        &snapshot_type_modules,
                        snapshot_response.as_ref(),
                        request,
                        snapshot_mounted_input.as_ref(),
                    )
                })
                .await?;

            // No checkout held here: the GHC whole-cell check runs
            // concurrently with every other actor's turn against this
            // session.
            let check_source = source.clone();
            let check_effects = effects.clone();
            let check_cell_source = cell_source.clone();
            let check_cancellation = cancellation.clone();
            let (snapshot, checked, folded) =
                crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                    tidepool_runtime::with_compiler_transaction_cancellable(
                        check_cancellation,
                        || {
                            let checked = check_cell_off_checkout(
                                &snapshot,
                                &check_source,
                                &check_effects,
                                &check_cell_source,
                            );
                            checked.map(|(checked, folded)| (snapshot, checked, folded))
                        },
                    )
                }))
                .await
                .map_err(ResidentActorWorkbenchError::Join)??;

            let declaration_index = checked
                .items
                .iter()
                .position(|item| item.verdict.kind == TurnKind::Decl);
            // Trust the speculative fold only when the check's OWN
            // classification confirms the shape it was built for: exactly
            // one item, and it's a bind (never a declaration — which stages
            // through a private candidate this fold never touched — and
            // never a bare expression, whose install goes through an
            // observation wrapper this fold does not build; see
            // `check_cell_off_checkout`'s doc comment).
            let folded = folded.filter(|_| {
                declaration_index.is_none()
                    && checked.items.len() == 1
                    && checked.items[0].verdict.kind == TurnKind::Bind
            });
            let declaration_receipt = declaration_index.map(|index| {
                let item = &checked.items[index];
                DeclarationReceipt {
                    binders: item.verdict.binders.clone(),
                    items: item.verdict.items.clone(),
                    source: tidepool_runtime::session::DeclarationSource {
                        prologue: checked.prologue.clone(),
                        body: item.source.clone(),
                    },
                }
            });
            let value_item_count = checked
                .items
                .iter()
                .filter(|item| item.verdict.kind != TurnKind::Decl)
                .count();

            let reserve_source = source.clone();
            let reserve_type_modules = Arc::clone(&type_modules);
            let reserve_receipt = declaration_receipt;
            let (snapshot, reservation) = self
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    reserve_cell_generations(
                        session,
                        context,
                        &reserve_source,
                        &reserve_type_modules,
                        &snapshot,
                        value_item_count,
                        reserve_receipt.as_ref(),
                    )
                    .map(|reservation| (snapshot, reservation))
                })
                .await?;
            let (c_view, retained, visible_names, declaration_candidate) = match reservation {
                CellReservation::Stale(changed) => break 'split ("reservation", changed),
                CellReservation::Ready(ready) => {
                    let CellReservationReady {
                        view,
                        retained,
                        visible_names,
                        declaration,
                    } = *ready;
                    (view, retained, visible_names, declaration)
                }
            };

            // No checkout held here either: every item's GHC compile also
            // runs with the machine released — including, for a cell with a
            // `Decl` item, GHC-validating that declaration against a private
            // candidate directory nobody else's compile has on its include
            // path (`validate_declaration_candidate`), never the shared
            // session root.
            let (checked, outcome, staged) = match folded {
                // A single bind was compiled using the snapshot's reserved
                // identity. Verify the artifact agrees before installing it.
                Some(folded)
                    if fold_result_matches_generation(&folded, c_view.next_value_generation()) =>
                {
                    let ready = ReadyBlock {
                        result: folded,
                        generation: c_view.next_value_generation(),
                        declaration_source: checked.items[0].source.clone(),
                        declaration_imports: c_view.workbench_imports(),
                        observation: None,
                    };
                    (
                        checked,
                        CellItemsOutcome::Ready(vec![PreparedCellItem {
                            ready: PreparedCellStep::Executable(Box::new(ready)),
                        }]),
                        None,
                    )
                }
                _ => {
                    let compile_source = source.clone();
                    let compile_effects = effects.clone();
                    let compile_cell_source = cell_source.clone();
                    let compile_context = context.clone();
                    let compile_view = c_view.clone();
                    let compile_cancellation = cancellation.clone();
                    crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                        tidepool_runtime::with_compiler_transaction_cancellable(
                            compile_cancellation,
                            || {
                                let candidate_dir = declaration_candidate
                                    .is_some()
                                    .then(tempfile::tempdir)
                                    .transpose()
                                    .map_err(|error| {
                                        ResidentActorWorkbenchError::CompileInfrastructure(
                                            format!("declaration candidate directory: {error}")
                                                .into(),
                                        )
                                    })?;
                                let staged = match (declaration_candidate, &candidate_dir) {
                                    (Some((candidate, visible_values)), Some(candidate_dir)) => {
                                        match validate_declaration_candidate(
                                            candidate,
                                            candidate_dir.path(),
                                        ) {
                                            Ok(staged) => {
                                                Some(staged.with_visible_values(visible_values))
                                            }
                                            Err(error)
                                                if classify_session(&error).class
                                                    == FailureClass::UserHaskell =>
                                            {
                                                let diagnostic =
                                                    classify_session(&error).message.into();
                                                let index = declaration_index.unwrap_or(0);
                                                return Ok((
                                                    checked,
                                                    CellItemsOutcome::Rejected {
                                                        index,
                                                        diagnostic,
                                                    },
                                                    None,
                                                ));
                                            }
                                            Err(error) => {
                                                return Err(ResidentActorWorkbenchError::Resident(
                                                    ResidentError::Session(error),
                                                ))
                                            }
                                        }
                                    }
                                    _ => None,
                                };
                                let compile_view = match &staged {
                                    Some(staged) => compile_view
                                        .with_staged_library(staged.module(), staged.items()),
                                    None => compile_view,
                                };
                                let outcome = compile_cell_items_off_checkout(
                                    &compile_context,
                                    &compile_source,
                                    &compile_effects,
                                    &checked,
                                    &compile_cell_source,
                                    compile_view,
                                    &retained,
                                    &visible_names,
                                    staged.as_ref(),
                                    candidate_dir.as_ref().map(|dir| dir.path()),
                                );
                                outcome.map(|outcome| (checked, outcome, staged))
                            },
                        )
                    }))
                    .await
                    .map_err(ResidentActorWorkbenchError::Join)??
                }
            };

            let items = match outcome {
                CellItemsOutcome::Rejected { index, diagnostic } => {
                    // The rejection was derived from `c_view`, taken before
                    // the machine was released for this compile. Re-derive
                    // the view once more before trusting it: if something
                    // else wrote to a scope this compile actually read from
                    // in the meantime, the rejection is stale and the cell
                    // recompiles under one checkout, exactly as this same
                    // split's install-time mismatch does. A rejected
                    // declaration item's own candidate was validated against
                    // a private directory dropped when this compile
                    // finished, so there is nothing further to discard here.
                    let revalidate_source = source.clone();
                    let revalidate_type_modules = Arc::clone(&type_modules);
                    let revalidate_against = c_view.clone();
                    let revalidate_candidate_module =
                        declaration_index.map(|_| snapshot.candidate_module);
                    let revalidation = self
                        .access
                        .with_machine(context.clone(), move |session, context, _| {
                            revalidate_cell_rejection(
                                session,
                                context,
                                &revalidate_source,
                                &revalidate_type_modules,
                                revalidate_candidate_module,
                                &revalidate_against,
                            )
                        })
                        .await?;
                    match revalidation {
                        CellRejectionRevalidation::StillCurrent => {
                            if let Some(guard) = leased_input.take() {
                                guard.retire(&self.access).await;
                            }
                            cancel_on_drop.0 = None;
                            return Ok((checked, PreparedCell::Rejected { index, diagnostic }));
                        }
                        CellRejectionRevalidation::Stale(changed) => {
                            break 'split ("rejection revalidation", changed)
                        }
                    }
                }
                CellItemsOutcome::Ready(items) => items,
            };

            #[cfg(test)]
            split_probe::before_install().await;
            let install_source = source.clone();
            let install_type_modules = Arc::clone(&type_modules);
            let candidate_module = snapshot.candidate_module;
            let install_view = c_view;
            let install_checked = checked.clone();
            let install = self
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    finalize_cell_install(
                        session,
                        context,
                        &install_source,
                        &install_type_modules,
                        candidate_module,
                        &install_view,
                        &install_checked,
                        items,
                        staged,
                    )
                })
                .await?;
            match install {
                CellInstall::Ready(prepared) => {
                    if let Some(guard) = leased_input.take() {
                        guard.retire(&self.access).await;
                    }
                    cancel_on_drop.0 = None;
                    return Ok((checked, prepared));
                }
                CellInstall::Stale(changed) => ("install", changed),
            }
        };
        log_split_stale("cell", &context, stage, changed, false);

        if let Some(guard) = leased_input.take() {
            guard.retire(&self.access).await;
        }
        cancel_on_drop.0 = None;
        self.prepare_cell_single_checkout(context, cell_source)
            .await
    }

    /// Retain the admitted source and input owners while one compiler
    /// transaction prepares all items before native execution becomes possible.
    async fn prepare_checked_cell(
        &self,
        context: crate::ActorSessionContext,
        cell_source: String,
        authority: Arc<crate::resident_actor::WorkbenchCompilationAuthority>,
        execution: Option<Arc<ExecutionPrivateScope>>,
        leased_input: Option<HostInputRetirement>,
    ) -> Result<(CellCheck, PreparedCell), ResidentActorWorkbenchError> {
        if execution
            .as_ref()
            .is_some_and(|execution| context.placement.lexical_scope != execution.private_scope)
            || context.source_layer.as_ref() != authority.source().include_paths()
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "checked cell preparation requires its admitted context and source revision".into(),
            ));
        }
        let admission_execution = execution.clone();
        let snapshot_source = self.access.source.clone();
        let type_modules = self.type_modules.clone();
        let response = self.response.clone();
        let request = self.request;
        let mounted_input = leased_input
            .as_ref()
            .map(|guard| guard.mounted_input().clone());
        let snapshot_cell_source = cell_source.clone();
        let specification = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let (source, snapshot) = snapshot_cell_split_owned(
                    session,
                    context,
                    snapshot_source,
                    &type_modules,
                    response.as_ref(),
                    request,
                    mounted_input.as_ref(),
                    execution
                        .as_ref()
                        .map_or(CellSnapshotAdmission::ProtectedSetup, |execution| {
                            CellSnapshotAdmission::PrivateExecution(execution.admission.as_ref())
                        }),
                )?;
                let prepared =
                    source.prepare_effectful(&snapshot.view, &context.haskell_effects_alias)?;
                let preamble = cell_module_preamble(
                    &prepared.preamble,
                    &snapshot.candidate_module.module_name(),
                )?;
                let template = resident_cell_check_template(
                    &preamble,
                    &context.haskell_effects_alias,
                    &prepared.imports,
                );
                let evidence = cell_check_evidence(&snapshot.view, &template, &prepared);
                let templates = resident_workbench_templates(
                    &prepared.preamble,
                    &context.haskell_effects_alias,
                    &prepared.imports,
                );
                let cell = tidepool_toolchain::checked_cell::CheckedCellSpecification {
                    admission_digest: [0; 32],
                    cell_source: snapshot_cell_source,
                    template_source: template,
                    turn_templates: templates
                        .iter()
                        .map(|template| {
                            let kind = match template.kind {
                                tidepool_runtime::session::TemplateSelector::Decl => "decl",
                                tidepool_runtime::session::TemplateSelector::Bind => "bind",
                                tidepool_runtime::session::TemplateSelector::BindDiscard => {
                                    "binddiscard"
                                }
                                tidepool_runtime::session::TemplateSelector::Expr => "expr",
                            };
                            (kind.to_owned(), template.source.clone())
                        })
                        .collect(),
                    injected_modules: prepared.injected,
                    reserved_declaration_modules: Vec::new(),
                };
                let specification = Arc::new(WorkbenchCompilationSpec {
                    _authority: authority.clone(),
                    source,
                    cell,
                    templates,
                    include: prepared.include,
                    evidence,
                    declaration_imports: snapshot.view.workbench_imports(),
                });
                Ok(specification)
            })
            .await?;
        let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));
        let parser_specification = specification.clone();
        let parser_cancellation = cancellation.clone();
        let plan = crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
            tidepool_runtime::with_compiler_transaction_cancellable(parser_cancellation, || {
                tidepool_toolchain::artifacts::parse_cell_plan(
                    Arc::new(parser_specification.cell.clone()),
                    &parser_specification.include,
                )
                .map_err(ResidentActorWorkbenchError::Compile)
            })
        }))
        .await
        .map_err(ResidentActorWorkbenchError::Join)??;
        let reservation_specification = specification.clone();
        let admission = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let admitted = match admission_execution {
                    Some(execution) => session.admit_planned_cell_for_execution(
                        execution.admission.clone(),
                        plan,
                        reservation_specification.clone(),
                        reservation_specification.cell.specification_digest(),
                        reservation_specification._authority.authority_digest(),
                        reservation_specification.include.clone(),
                    ),
                    None => session.admit_native_setup_cell_in(
                        context.placement.lexical_scope,
                        plan,
                        reservation_specification.clone(),
                        reservation_specification.cell.specification_digest(),
                        reservation_specification._authority.authority_digest(),
                        reservation_specification.include.clone(),
                    ),
                };
                admitted.map_err(|error| {
                    ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                })
            })
            .await?;
        let check_specification = specification.clone();
        let check_admission = admission.clone();
        let (checked, program) =
            crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                tidepool_runtime::with_compiler_transaction_cancellable(cancellation, || {
                    let include = check_specification
                        .include
                        .iter()
                        .map(PathBuf::as_path)
                        .collect::<Vec<_>>();
                    let view = check_admission.view();
                    tidepool_runtime::session::turn::compile_cell_program_admitted(
                        CellCheckRequest {
                            exact_context: view.exact_declaration_context().cloned(),
                            session_id: Some(view.session()),
                            cell_text: &check_specification.cell.cell_source,
                            template: &check_specification.cell.template_source,
                            include: &include,
                            session_root: view.session_root(),
                            inject_modules: &check_specification.cell.injected_modules,
                            compile_generation: view.next_value_generation().0,
                            compile_view_evidence: &check_specification.evidence,
                        },
                        check_admission.clone(),
                        &check_specification.templates,
                    )
                    .map_err(|failure| {
                        cell_check_error(failure, &check_specification.cell.cell_source)
                    })
                })
            }))
            .await
            .map_err(ResidentActorWorkbenchError::Join)??;
        cancel_on_drop.0 = None;
        let item_capabilities = program
            .items()
            .iter()
            .map(|item| item.checked_item().clone())
            .collect::<Vec<_>>();
        let prefix = self
            .access
            .with_machine(context.clone(), move |session, _, _| {
                session
                    .begin_cell_program(admission, program)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
            })
            .await?;
        let items = item_capabilities
            .into_iter()
            .map(|item| PreparedCellItem {
                ready: PreparedCellStep::Checked {
                    specification: specification.clone(),
                    prefix: prefix
                        .as_ref()
                        .expect("nonempty complete cell has a prefix")
                        .clone(),
                    item,
                },
            })
            .collect::<Vec<_>>();
        let bindings = self
            .access
            .with_machine(context, move |session, context, _| {
                let visible = session.visible_binding_ids_in(context.placement.lexical_scope);
                Ok(session.lease_bindings(&visible))
            })
            .await?;
        Ok((
            checked,
            PreparedCell::Ready {
                items,
                dependencies: CellPreparationLease {
                    _bindings: bindings,
                    _input: leased_input,
                },
            },
        ))
    }

    /// The original, unsplit whole-cell preparation: one exclusive machine
    /// checkout for the whole-cell check and every item's compile, so it
    /// cannot observe a stale view. [`Self::prepare_cell`] falls back here
    /// when its one split attempt goes stale.
    async fn prepare_cell_single_checkout(
        &self,
        context: crate::ActorSessionContext,
        cell_source: String,
    ) -> Result<(CellCheck, PreparedCell), ResidentActorWorkbenchError> {
        #[cfg(test)]
        split_probe::single_checkout();
        let json_input = self.json_input.clone();
        let response = self.response.clone();
        let request = self.request;
        let type_modules = Arc::clone(&self.type_modules);
        let mut source = self.access.source.clone();
        let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));
        let result = self
            .access
            .with_machine(context, move |session, context, _| {
                let mounted_input = json_input
                    .as_ref()
                    .map(|input| {
                        mount_json_input(session, context, &source, &type_modules, input, None)
                    })
                    .transpose()?;
                if let Some(input) = &mounted_input {
                    source
                        .workbench_imports
                        .extend_text(&format!("qualified {} as TidepoolHostInput", input.module));
                    source.preamble = format!(
                        "{}\ninput = TidepoolHostInput.{}\n",
                        source.preamble, input.name
                    )
                    .into();
                }
                let prepared =
                    tidepool_runtime::with_compiler_transaction_cancellable(cancellation, || {
                        if session.machine_disposition()
                            == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
                        {
                            return Err(ResidentActorWorkbenchError::MachineLost);
                        }
                        let candidate_module =
                            session.next_declaration_module().ok_or_else(|| {
                                ResidentActorWorkbenchError::CompileInfrastructure(
                                    "resident cell session has no declaration plane".into(),
                                )
                            })?;
                        source.preamble = match (response.as_ref(), request) {
                            (Some(response), Some(request)) => response.request_preamble(
                                &source.preamble,
                                request,
                                &context.haskell_effects_alias,
                            ),
                            (None, None) => source.preamble.to_string(),
                            _ => unreachable!("request workbench scope is constructed atomically"),
                        }
                        .into();
                        source.preamble = actor_preamble(&source.preamble, context).into();
                        let compile_view =
                            actor_compile_view(session, context, &source, &type_modules)?;
                        let prepared = source
                            .prepare_effectful(&compile_view, &context.haskell_effects_alias)?;
                        let check_preamble = cell_module_preamble(
                            &prepared.preamble,
                            &candidate_module.module_name(),
                        )?;
                        let template = resident_cell_check_template(
                            &check_preamble,
                            &context.haskell_effects_alias,
                            &prepared.imports,
                        );
                        let compile_view_evidence =
                            cell_check_evidence(&compile_view, &template, &prepared);
                        let include = prepared
                            .include
                            .iter()
                            .map(PathBuf::as_path)
                            .collect::<Vec<_>>();
                        let cell_check_request = || CellCheckRequest {
                            exact_context: compile_view.exact_declaration_context().cloned(),
                            session_id: Some(compile_view.session_id()),
                            cell_text: &cell_source,
                            template: &template,
                            include: &include,
                            session_root: compile_view.session_root(),
                            inject_modules: &prepared.injected,
                            compile_generation: compile_view.next_value_generation().0,
                            compile_view_evidence: &compile_view_evidence,
                        };
                        let checked = match check_cell(cell_check_request()) {
                            Ok(checked) => checked,
                            Err(failure) => {
                                // The same-cell shape: this cell both RE-DECLARES a
                                // name and USES it from a bind statement in the SAME
                                // cell. The check module above is already named for
                                // the CANDIDATE next generation (`candidate_module`,
                                // holding the cell's own fresh declaration) while
                                // `prepared.imports` still names the CURRENT
                                // generation unqualified (built before this cell's
                                // own redeclarations were known) — both visible at
                                // once. Retry exactly once with that collision
                                // hidden, the same shadowing every other generation
                                // boundary already gets via `render_module`.
                                let mut patched_imports = None;
                                if classify_compile(&failure.error).class
                                    == FailureClass::UserHaskell
                                {
                                    if let Some(previous_module) = compile_view.library() {
                                        let previous_module = previous_module.module_name();
                                        let message =
                                            tidepool_runtime::session::render_cell_compile_error(
                                                &failure.error,
                                                &cell_source,
                                            );
                                        let names =
                                        tidepool_runtime::session::turn::same_cell_value_collisions(
                                            &message,
                                            &previous_module,
                                            &candidate_module.module_name(),
                                        );
                                        patched_imports = hide_same_cell_collisions(
                                            &prepared.imports,
                                            &previous_module,
                                            &names,
                                        );
                                    }
                                }
                                match patched_imports {
                                    Some(patched_imports) => {
                                        let retried_template = resident_cell_check_template(
                                            &check_preamble,
                                            &context.haskell_effects_alias,
                                            &patched_imports,
                                        );
                                        let retried_evidence = cell_check_evidence(
                                            &compile_view,
                                            &retried_template,
                                            &prepared,
                                        );
                                        match check_cell(CellCheckRequest {
                                            template: &retried_template,
                                            compile_view_evidence: &retried_evidence,
                                            ..cell_check_request()
                                        }) {
                                            Ok(checked) => checked,
                                            Err(failure) => {
                                                return Err(cell_check_error(failure, &cell_source))
                                            }
                                        }
                                    }
                                    None => return Err(cell_check_error(failure, &cell_source)),
                                }
                            }
                        };
                        let prepared = prepare_cell_in_session(
                            session,
                            context,
                            &source,
                            &context.haskell_effects_alias,
                            &type_modules,
                            &checked,
                            &cell_source,
                            &checked.compile_view_evidence,
                            compile_view,
                        )?;
                        Ok((checked, prepared))
                    });
                if let Some(input) = mounted_input {
                    let session_root =
                        carrier_mount_session_root(session, context.placement.lexical_scope)?;
                    session.retire_host_binding_owner(&session_root, &input.binder);
                }
                prepared
            })
            .await;
        cancel_on_drop.0 = None;
        result
    }

    pub(crate) async fn status_discovery(
        &self,
        context: crate::ActorSessionContext,
        discovery: crate::status_tool::StatusDiscovery,
    ) -> Result<String, ResidentActorWorkbenchError> {
        let response = self.response.clone();
        let request = self.request;
        let type_modules = Arc::clone(&self.type_modules);
        let mut source = self.access.source.clone();
        self.access
            .with_machine(context, move |session, context, _| {
                source.preamble = match (response.as_ref(), request) {
                    (Some(response), Some(request)) => response.request_preamble(
                        &source.preamble,
                        request,
                        &context.haskell_effects_alias,
                    ),
                    (None, None) => source.preamble.to_string(),
                    _ => unreachable!("request workbench scope is constructed atomically"),
                }
                .into();
                source.preamble = actor_preamble(&source.preamble, context).into();
                run_status_discovery(session, context, &source, &type_modules, discovery)?
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)
            })
            .await
    }

    /// Every workbench binding visible at `context`'s scope, generation
    /// included — the binding-store half of the status tool's what-is-live
    /// view. A structured read of [`ResidentSession::workbench_bindings_in`];
    /// it retains nothing new of its own.
    pub(crate) async fn live_bindings(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<Vec<tidepool_runtime::session::WorkbenchBinding>, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, |session, context, _| {
                Ok(session.workbench_bindings_in(context.placement.lexical_scope))
            })
            .await
    }

    /// Execute lookup queries and candidate discovery against one immutable compile view.
    pub(crate) async fn resume_lookup(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        request: crate::lookup::LookupRequest,
        usage: crate::UsagePointerTable,
        execution_control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let source = RequestWorkbenchScope {
            response: self.response.as_ref(),
            request: self.request,
            type_modules: &self.type_modules,
        }
        .source(&self.access.source, &context);
        let type_modules = Arc::clone(&self.type_modules);
        // Whether this workbench is currently presenting a typed request:
        // `Tidepool.RequestWorkbenchScope`'s preamble binds `respond`/
        // `sessionReply`/`sessionInput`/`reportProgress` only then, and a
        // miss on one of those names outside that window should say so.
        let request_pending = self.request.is_some();
        // Documentation and rejected-only requests need the current view for
        // their response, but no compiler request. Keep those small paths in
        // one checkout and avoid a blocking-pool hop.
        if !crate::lookup::has_inspection_work(&request, "") {
            return self
                .access
                .with_machine(context, move |session, context, _| {
                    let view = actor_compile_view(session, context, &source, &type_modules)?;
                    let provenance = structured_provenance(
                        &view,
                        &source,
                        tidepool_runtime::session::NameScope::Current,
                    );
                    let prepared =
                        source.prepare_effectful(&view, &context.haskell_effects_alias)?;
                    let mut answer = crate::lookup::execute(
                        request,
                        provenance.fingerprint,
                        &prepared.imports,
                        &prepared.injected,
                        &source.workspace_modules,
                        usage,
                        |queries| {
                            inspect_lookup_queries(
                                &view,
                                &prepared.preamble,
                                &prepared.imports,
                                &prepared.include,
                                &prepared.injected,
                                &context.haskell_effects_alias,
                                queries,
                                None,
                            )
                        },
                    );
                    if !request_pending {
                        crate::lookup::note_request_only_bindings(&mut answer);
                    }
                    session
                        .resume_classified(hole, answer)
                        .map_err(classify_resumption)
                })
                .await;
        }

        let snapshot_source = source.clone();
        let snapshot_modules = Arc::clone(&type_modules);
        let (view, provenance) = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let view =
                    actor_compile_view(session, context, &snapshot_source, &snapshot_modules)?;
                let provenance = structured_provenance(
                    &view,
                    &snapshot_source,
                    tidepool_runtime::session::NameScope::Current,
                );
                Ok((view, provenance))
            })
            .await?;

        let effects_alias = context.haskell_effects_alias.clone();
        let prepared = source.prepare_effectful(&view, &effects_alias)?;
        let source_layer_revision = crate::agent_spec::layer_revision(&context.source_layer);
        let mut answer = if crate::lookup::requires_inspection(
            &request,
            &prepared.imports,
            &provenance.fingerprint,
        ) {
            let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
            let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));
            let timing = crate::call_timing::current_registration();
            let compile_span = tracing::info_span!(
                "compile_blocking",
                actor = %context.actor,
                session = %context.placement.session,
                operation = "lookup",
            );
            let request_view = provenance.fingerprint.clone();
            let imports = prepared.imports.clone();
            let preamble = prepared.preamble.clone();
            let injected = prepared.injected.clone();
            let include = prepared.include.clone();
            let workspace_modules = source.workspace_modules.clone();
            let compile_cancellation = cancellation.clone();
            let inspection_view = view.clone();
            let effects = effects_alias.clone();
            let spawn = {
                let _entered = compile_span.enter();
                spawn_blocking_in_span(move || {
                    let answer = tidepool_runtime::with_compiler_transaction_cancellable(
                        compile_cancellation,
                        || {
                            crate::lookup::execute(
                                request,
                                request_view,
                                &imports,
                                &injected,
                                &workspace_modules,
                                usage,
                                |queries| {
                                    inspect_lookup_queries(
                                        &inspection_view,
                                        &preamble,
                                        &imports,
                                        &include,
                                        &injected,
                                        &effects,
                                        queries,
                                        timing.as_ref(),
                                    )
                                },
                            )
                        },
                    );
                    #[cfg(test)]
                    lookup_inspection_probe::after_request();
                    answer
                })
            };
            let mut spawn = spawn;
            let answer = if let Some(control) = execution_control {
                // Keep cancellation bound to the exact compiler request, then
                // wait for that request to settle before abandoning its hole.
                tokio::select! {
                    result = &mut spawn => result.map_err(ResidentActorWorkbenchError::Join)?,
                    () = control.wait_for_cancellation() => {
                        cancellation.cancel();
                        let _ = spawn.await.map_err(ResidentActorWorkbenchError::Join)?;
                        cancel_on_drop.0 = None;
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "lookup interrupted by invocation cancellation".into(),
                        ));
                    }
                }
            } else {
                spawn.await.map_err(ResidentActorWorkbenchError::Join)?
            };
            cancel_on_drop.0 = None;
            answer
        } else {
            crate::lookup::execute(
                request,
                provenance.fingerprint.clone(),
                &prepared.imports,
                &prepared.injected,
                &source.workspace_modules,
                usage,
                |queries| {
                    inspect_lookup_queries(
                        &view,
                        &prepared.preamble,
                        &prepared.imports,
                        &prepared.include,
                        &prepared.injected,
                        &effects_alias,
                        queries,
                        None,
                    )
                },
            )
        };

        if !request_pending {
            crate::lookup::note_request_only_bindings(&mut answer);
        }

        let revalidate_source = source.clone();
        let revalidate_modules = type_modules;
        self.access
            .with_machine(context, move |session, context, _| {
                let fresh_view =
                    actor_compile_view(session, context, &revalidate_source, &revalidate_modules)?;
                let fresh_prepared = revalidate_source
                    .prepare_effectful(&fresh_view, &context.haskell_effects_alias)?;
                let source_revision_unchanged =
                    crate::agent_spec::layer_revision(&context.source_layer)
                        == source_layer_revision;
                let effect_alias_unchanged = context.haskell_effects_alias == effects_alias;
                if !fresh_view.is_current_for(&view)
                    || fresh_prepared.preamble != prepared.preamble
                    || fresh_prepared.imports != prepared.imports
                    || fresh_prepared.include != prepared.include
                    || !source_revision_unchanged
                    || !effect_alias_unchanged
                {
                    answer.results.clear();
                    answer.candidates.clear();
                    answer.issue = Some("lookup compile view changed".into());
                }
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    #[tracing::instrument(target = "exomonad_actor::workbench_phase", skip_all, fields(actor = %context.actor, item = block.ordinal))]
    pub(crate) async fn begin_prepared_cell_item(
        &self,
        context: crate::ActorSessionContext,
        block: ParsedBlock,
        prepared: PreparedCellItem,
        display_budget: usize,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        if let PreparedCellStep::Checked {
            specification,
            prefix,
            item,
        } = &prepared.ready
        {
            let item = item.clone();
            let prefix = prefix.clone();
            tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_item_admit_started", "workbench phase");
            let reservation = self
                .access
                .with_machine(context.clone(), move |session, _, _| {
                    session.admit_checked_item(prefix, item).map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
                })
                .await?;
            tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_item_admitted", "workbench phase");
            if reservation.item().kind()
                == tidepool_toolchain::checked_cell::CheckedItemKind::Declaration
            {
                let binders = reservation.item().binders().to_vec();
                let prologue_only = reservation.item().source().is_empty();
                let commit = self
                    .access
                    .with_machine(context.clone(), move |session, _, _| {
                        session
                            .adopt_checked_declaration(reservation)
                            .map_err(|error| {
                                ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                            })
                    })
                    .await?;
                return Ok(ResidentWorkbenchStep::Committed {
                    output: declaration_receipt(&binders, prologue_only, commit.generation.0),
                    warnings: Vec::new(),
                    installed_bindings: binders,
                });
            }
            let specification = specification.clone();
            let ready = consume_admitted_cell_item(&specification, reservation, &block)?;
            let ready = match ready {
                CompiledBlock::Ready(ready) => *ready,
                CompiledBlock::Rejected(diagnostic) => {
                    return Ok(ResidentWorkbenchStep::Rejected(diagnostic))
                }
            };
            return Box::pin(begin_ready_block_split(
                &self.access,
                context,
                specification.source.clone(),
                self.type_modules.to_vec(),
                block,
                ready,
                display_budget,
            ))
            .await;
        }
        let ready = match prepared.ready {
            PreparedCellStep::Checked { .. } => unreachable!("checked item handled above"),
            PreparedCellStep::Executable(ready) => ready,
            PreparedCellStep::Declaration {
                generation,
                binders,
                prologue_only,
            } => {
                return Ok(ResidentWorkbenchStep::Committed {
                    output: declaration_receipt(&binders, prologue_only, generation.0),
                    warnings: Vec::new(),
                    installed_bindings: binders,
                });
            }
        };
        let response = self.response.clone();
        let request = self.request;
        let type_modules = Arc::clone(&self.type_modules);
        let mut turn_source = self.access.source.clone();
        // Only the preamble mutation needs a checkout (it reads
        // `context.haskell_effects_alias`, not the machine); the actual
        // install-and-run is off-checkout split below.
        let turn_source = self
            .access
            .with_machine(context.clone(), move |_, context, _| {
                turn_source.preamble = match (response.as_ref(), request) {
                    (Some(response), Some(request)) => response.request_preamble(
                        &turn_source.preamble,
                        request,
                        &context.haskell_effects_alias,
                    ),
                    (None, None) => turn_source.preamble.to_string(),
                    _ => unreachable!("request workbench scope is constructed atomically"),
                }
                .into();
                turn_source.preamble = actor_preamble(&turn_source.preamble, context).into();
                Ok(turn_source)
            })
            .await?;
        Box::pin(begin_ready_block_split(
            &self.access,
            context,
            turn_source,
            type_modules.to_vec(),
            block,
            *ready,
            display_budget,
        ))
        .await
    }

    /// The carrier this workbench built for `kind`, from the cache if one is
    /// already there and still current for the actor's full source graph,
    /// otherwise built once, off checkout, and cached for every
    /// later call of any kind for the life of this workbench's lineage (see
    /// [`ResidentMachineAccess::sharing`]).
    ///
    /// Building takes one short checkout — to reserve a generation for the
    /// throwaway compile below — releases it, then compiles off checkout
    /// ([`compile_host_binding_off_checkout`]) with no re-checkout: the
    /// compiled `(BoundBinder, CompiledTurn)` is generation-independent (see
    /// [`HostCarrier::from_compiled`]), so there is nothing left to install
    /// or revalidate against a later view.
    async fn carrier_for(
        &self,
        context: &crate::ActorSessionContext,
        kind: HostCarrierKind,
    ) -> Result<Arc<HostCarrier>, ResidentActorWorkbenchError> {
        let revision: Vec<Option<String>> = context
            .source_layer
            .iter()
            .chain(self.access.source.base_include.iter())
            .map(|root| crate::agent_spec::layer_revision(std::slice::from_ref(root)))
            .collect();
        if let Some(cached) = self
            .access
            .carriers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&kind)
        {
            if cached.revision == revision {
                return Ok(Arc::clone(&cached.carrier));
            }
        }

        let (view, generation, retained) = self
            .access
            .with_machine(context.clone(), move |session, context, source| {
                let view = actor_compile_view(session, context, source, &[])?;
                let generation = view.next_value_generation();
                // Reserve the generation now, under this checkout, so no
                // concurrent compile can ever mint the same one — the
                // throwaway compile below discards this generation's own
                // name and module, but a collision with a real mount would
                // still corrupt that mount's source stub.
                session.reserve_value_generations_through(generation);
                let retained = session.prepared_retained();
                Ok((view, generation, retained))
            })
            .await?;

        // No checkout held here: the GHC compile runs concurrently with
        // every other actor's turn against this session.
        let effects = context.haskell_effects_alias.clone();
        let source = self.access.source.clone();
        let (binder, compiled) =
            crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                compile_host_binding_off_checkout(
                    &view,
                    &source,
                    &effects,
                    generation,
                    kind.carrier_binding_name(),
                    kind.type_name(),
                    kind.anchor(),
                    kind.imports(),
                    kind.retain_text_constructor(),
                    &retained,
                )
            }))
            .await
            .map_err(ResidentActorWorkbenchError::Join)??;

        let carrier = Arc::new(HostCarrier::from_compiled(
            &binder,
            compiled.into_code(),
            kind.host_binding_type(),
        ));
        self.access
            .carriers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                kind,
                CachedHostCarrier {
                    revision,
                    carrier: Arc::clone(&carrier),
                },
            );
        Ok(carrier)
    }

    /// Retain a command job in the actor's lexical environment for named-tool
    /// output navigation. Reuse an authored or host-mounted binder for the same
    /// job when present; otherwise mount a fresh binder through the lineage's
    /// cached Job carrier under one short checkout. A failed mount leaves the
    /// command owned by its existing command owner.
    pub(crate) async fn bind_command_job(
        &self,
        context: crate::ActorSessionContext,
        job: String,
    ) -> Result<String, ResidentActorWorkbenchError> {
        let carrier = self.carrier_for(&context, HostCarrierKind::Job).await?;
        self.access
            .with_machine(context, move |session, context, _source| {
                let scope = context.placement.lexical_scope;
                if let Some(binding) = session.host_text_binding_in(scope, &job) {
                    return Ok(binding);
                }
                let binding = fresh_job_binding_name(session, scope);
                let session_root = carrier_mount_session_root(session, scope)?;
                let generation = session.val_gen().next();
                session.reserve_value_generations_through(generation);
                let payload = HostCommandJob::Job(job.clone());
                let bound_binder = session
                    .mount_carrier_in(
                        &session_root,
                        scope,
                        &binding,
                        generation,
                        &carrier,
                        HostPayload::Job(&payload),
                    )
                    .map_err(|error| {
                        ResidentActorWorkbenchError::InputMount(format!(
                            "command {job} remains owned, but its retained output binding failed: \
                             {error}"
                        ))
                    })?;
                if let Err(error) =
                    session.tag_host_text_binding_in(scope, &bound_binder, job.clone())
                {
                    session.retire_host_binding_owner(&session_root, &bound_binder);
                    return Err(ResidentActorWorkbenchError::Resident(error));
                }
                Ok(binding)
            })
            .await
    }

    /// Compile-and-run one scope-free Bind/Expr fragment — the tool
    /// installer's own shape, with no request/response context — with the
    /// resident machine checked out only for the snapshot and the
    /// install-and-run step, released for the GHC compile in between. The
    /// split uses [`split_staleness`] to detect a stale snapshot. It is
    /// attempted once; a stale snapshot falls straight through to the original
    /// single-checkout [`begin_fragment`], for the reason
    /// [`Self::prepare_cell`] gives.
    pub(crate) async fn begin_fragment_split(
        &self,
        context: crate::ActorSessionContext,
        source: ActorWorkbenchSource,
        type_modules: Vec<String>,
        block: ParsedBlock,
        verdict: Option<TurnClassification>,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        // Carries one cancellation edge across every off-checkout GHC call
        // this split makes, the same shape `prepare_cell_single_checkout`
        // arms for its own (single-checkout) compile: dropping this future
        // before it settles interrupts whichever compile is in flight,
        // rather than leaving it to finish unobserved.
        let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));
        let (stage, changed) = 'split: {
            let snapshot_source = source.clone();
            let snapshot_type_modules = type_modules.clone();
            let snapshot_block = block.clone();
            let snapshot_verdict = verdict.clone();
            let snapshot = self
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    snapshot_fragment_compile(
                        session,
                        context,
                        &snapshot_source,
                        &snapshot_type_modules,
                        &snapshot_block,
                        snapshot_verdict.as_ref(),
                    )
                })
                .await?;

            // No checkout held here: the GHC compile runs concurrently with
            // every other actor's turn against this session.
            let compile_source = source.clone();
            let compile_block_text = block.clone();
            let effects = context.haskell_effects_alias.clone();
            let compile_cancellation = cancellation.clone();
            let (snapshot, compiled) =
                crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                    tidepool_runtime::with_compiler_transaction_cancellable(
                        compile_cancellation,
                        || {
                            let compiled = compile_fragment_off_checkout(
                                &snapshot,
                                &compile_source,
                                &effects,
                                &compile_block_text,
                            );
                            compiled.map(|compiled| (snapshot, compiled))
                        },
                    )
                }))
                .await
                .map_err(ResidentActorWorkbenchError::Join)??;
            let ready = match compiled {
                CompiledBlock::Rejected(diagnostic) => {
                    // The rejection was derived from `snapshot.view`, taken
                    // before the machine was released for this compile.
                    // Re-derive the view once more before trusting it: if
                    // something else wrote to a scope this compile actually
                    // read from in the meantime, the rejection is stale and
                    // the fragment recompiles under one checkout, exactly as
                    // an install-time mismatch does.
                    let revalidate_source = source.clone();
                    let revalidate_type_modules = type_modules.clone();
                    let revalidate_against = snapshot.view.clone();
                    let stale = self
                        .access
                        .with_machine(context.clone(), move |session, context, _| {
                            if session.machine_disposition()
                                == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
                            {
                                return Err(ResidentActorWorkbenchError::MachineLost);
                            }
                            let fresh_view = actor_compile_view(
                                session,
                                context,
                                &revalidate_source,
                                &revalidate_type_modules,
                            )?;
                            Ok(split_staleness(
                                session,
                                &fresh_view,
                                &revalidate_against,
                                None,
                            ))
                        })
                        .await?;
                    match stale {
                        None => {
                            cancel_on_drop.0 = None;
                            return Ok(ResidentWorkbenchStep::Rejected(diagnostic));
                        }
                        Some(changed) => break 'split ("rejection revalidation", changed),
                    }
                }
                CompiledBlock::Ready(ready) => *ready,
            };

            // Revalidate the GHC-compiled `ready` against the current
            // source-side view in one short checkout, then hand off to
            // `begin_ready_block_split` for the JIT
            // install: that keeps this split's own Cranelift compile off
            // any checkout too, exactly as `begin_prepared_cell_item`
            // already does for a split cell's item install, instead of
            // running it under the checkout this closure used to hold for
            // `begin_ready_block`'s whole install-and-run.
            let install_source = source.clone();
            let install_type_modules = type_modules.clone();
            let stale = self
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    if session.machine_disposition()
                        == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
                    {
                        return Err(ResidentActorWorkbenchError::MachineLost);
                    }
                    let fresh_view = actor_compile_view(
                        session,
                        context,
                        &install_source,
                        &install_type_modules,
                    )?;
                    Ok(split_staleness(session, &fresh_view, &snapshot.view, None))
                })
                .await?;
            if let Some(changed) = stale {
                break 'split ("install", changed);
            }
            let install_source = source.clone();
            let install_type_modules = type_modules.clone();
            let install_block = block.clone();
            let step = begin_ready_block_split(
                &self.access,
                context.clone(),
                install_source,
                install_type_modules,
                install_block,
                ready,
                8192,
            );
            let step = step.await?;
            cancel_on_drop.0 = None;
            return Ok(step);
        };
        log_split_stale("fragment", &context, stage, changed, false);

        cancel_on_drop.0 = None;
        let step = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                begin_fragment(
                    session,
                    context,
                    &source,
                    RequestWorkbenchScope {
                        response: None,
                        request: None,
                        type_modules: &type_modules,
                    },
                    block,
                    None,
                    verdict.as_ref(),
                )
            })
            .await?;
        settle_deferred_display(&self.access, context, step).await
    }

    /// Service structured inspection without holding the resident machine
    /// while the compiler worker runs. The suspended continuation remains the
    /// actor's exclusive turn, and the immutable compile-view fingerprint is
    /// checked again before resumption.
    pub(crate) async fn resume_structured_introspection(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        query: tidepool_runtime::session::NameQuery,
        kind: StructuredInspectionKind,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let source = self.access.source.clone();
        let snapshot_source = source.clone();
        let query_scope = query.scope.clone();
        let snapshot = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let view = actor_compile_view(session, context, &snapshot_source, &[])?;
                let provenance = structured_provenance(&view, &snapshot_source, query_scope);
                Ok((view, provenance))
            })
            .await?;
        let (compile_view, provenance) = snapshot;
        let request_query = match kind {
            StructuredInspectionKind::Info => InspectionQuery::StructuredInfo {
                query: query.clone(),
                provenance: provenance.clone(),
            },
            StructuredInspectionKind::Type => InspectionQuery::StructuredType {
                query: query.clone(),
                provenance: provenance.clone(),
            },
        };
        let compiler_source = source.clone();
        let effects = context.haskell_effects_alias.clone();
        self.access
            .log_include_roots_if_changed(context.placement.session, &context.source_layer);
        let inspection_span = tracing::info_span!(
            "compile_blocking",
            actor = %context.actor,
            session = %context.placement.session,
            operation = "structured_introspection",
        );
        // Enter the span only for the synchronous call that captures it —
        // an `Entered` guard is not `Send` and must not live across the
        // `.await` below.
        let spawn = {
            let _entered = inspection_span.enter();
            spawn_blocking_in_span(move || {
                let prepared = compiler_source
                    .prepare_effectful(&compile_view, &effects)
                    .map_err(|error| error.to_string())?;
                let include = prepared
                    .include
                    .iter()
                    .map(PathBuf::as_path)
                    .collect::<Vec<_>>();
                run_inspections(InspectionRequest {
                    exact_context: compile_view.exact_declaration_context().cloned(),
                    preamble: &prepared.preamble,
                    imports: &prepared.imports,
                    include: &include,
                    session_root: compile_view.session_root(),
                    inject_modules: &prepared.injected,
                    queries: &[request_query],
                    effects: Some(&effects),
                })
                .map_err(|error| error.to_string())
                .and_then(|mut results| {
                    if results.len() == 1 {
                        Ok(results.remove(0))
                    } else {
                        Err(format!(
                            "inspection returned {} results for one query",
                            results.len()
                        ))
                    }
                })
            })
        };
        let inspected = crate::call_timing::timed_compile(spawn)
            .await
            .map_err(ResidentActorWorkbenchError::Join)?;

        self.access
            .with_machine(context, move |session, context, _| {
                let current_view = actor_compile_view(session, context, &source, &[])?;
                let current = structured_provenance(&current_view, &source, query.scope.clone());
                let answer = structured_introspection_answer(kind, inspected, provenance, current);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    /// Settle a resumed fragment outcome. Nominal suspensions retain
    /// the same runtime resource scope and return to the host for nominal actor dispatch.
    ///
    /// A completed fragment whose display is an
    /// [`WorkbenchDisplay::Observation`] takes the off-checkout split
    /// ([`settle_observation_render_split`]): `settle_fragment`'s
    /// render otherwise runs a full GHC-then-Cranelift round trip
    /// (`render_cell_observation`, measured at 5.0-6.5s) inside the one
    /// checkout this method would otherwise hold for the whole call. Every
    /// other outcome/display finishes in the original single checkout —
    /// none of the rest does any GHC work.
    pub(crate) async fn settle_item(
        &self,
        context: crate::ActorSessionContext,
        fragment: ResidentWorkbenchFragment,
        outcome: ResidentOutcome,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        if let (ResidentOutcome::Completed { .. }, WorkbenchDisplay::Observation { .. }) =
            (&outcome, &fragment.display)
        {
            return settle_observation_render_split(&self.access, context, fragment, outcome).await;
        }
        self.access
            .with_machine(context, move |session, context, _| {
                settle_fragment(session, context, fragment, outcome)
            })
            .await
    }
}

/// Settle a completed fragment whose display is an
/// [`WorkbenchDisplay::Observation`], rendering the display page with the
/// machine released for both compiles: the GHC compile of the display
/// bundle and its Cranelift install compile. See
/// [`render_observation_off_checkout`] for the checkouts one attempt takes.
/// A stale attempt renders again from a fresh snapshot, up to
/// [`CHEAP_RETRY_ATTEMPTS`] attempts, before the single-checkout
/// [`render_cell_observation`]. Any render failure, including a lost machine
/// or checkout, becomes a "Display failed" receipt: the value is already
/// bound and committed, as the in-checkout render always reported it. A render failure never loses the value: the
/// receipt says the display failed and names the still-bound observation.
async fn settle_observation_render_split<H, O>(
    access: &ResidentMachineAccess<H, O>,
    context: crate::ActorSessionContext,
    mut fragment: ResidentWorkbenchFragment,
    outcome: ResidentOutcome,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let ResidentOutcome::Completed { output, result: _ } = outcome else {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "observation render settled an outcome that did not complete".into(),
        ));
    };
    fragment.output.extend(output);
    let WorkbenchDisplay::Observation {
        name,
        budget,
        presentation,
        source,
        type_modules,
        protected,
    } = fragment.display
    else {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "observation render settled a fragment with no observation display".into(),
        ));
    };
    let mut installed_bindings = vec![name.clone()];
    installed_bindings.append(&mut fragment.recovered_jobs);
    let request = ObservationRender {
        source,
        type_modules,
        budget: budget.saturating_sub(
            fragment
                .output
                .iter()
                .map(|text| text.chars().count())
                .sum::<usize>(),
        ),
        presented: fragment.presented,
        presentation,
        name,
        protected,
    };
    let committed = |receipt: String| {
        let mut output = fragment.output.join("\n");
        if !output.is_empty() && !receipt.is_empty() {
            output.push('\n');
        }
        output.push_str(&receipt);
        ResidentWorkbenchStep::Committed {
            output,
            warnings: fragment.warnings,
            installed_bindings,
        }
    };

    let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
    let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));
    if request.protected.is_some() {
        let receipt = match render_checked_observation_off_checkout(
            access,
            &context,
            &request,
            &cancellation,
        )
        .await
        {
            Ok(receipt) => receipt,
            Err(error) => request.failed(&error),
        };
        cancel_on_drop.0 = None;
        return Ok(committed(receipt));
    }
    for attempt in 1..=CHEAP_RETRY_ATTEMPTS {
        let (stage, changed) = match render_observation_off_checkout(
            access,
            &context,
            &request,
            &cancellation,
        )
        .await
        {
            Ok(DisplayRenderAttempt::Rendered(receipt)) => {
                cancel_on_drop.0 = None;
                return Ok(committed(receipt));
            }
            Ok(DisplayRenderAttempt::Stale { stage, changed }) => (stage, changed),
            Err(error) => {
                cancel_on_drop.0 = None;
                return Ok(committed(request.failed(&error)));
            }
        };
        log_split_stale(
            "observation render",
            &context,
            stage,
            changed,
            attempt < CHEAP_RETRY_ATTEMPTS,
        );
    }

    cancel_on_drop.0 = None;
    let fallback = request.clone();
    let receipt = access
        .with_machine(context, move |session, context, _| {
            let request = fallback;
            Ok(
                match render_cell_observation(
                    session,
                    context,
                    &request.source,
                    &request.type_modules,
                    &request.name,
                    request.budget,
                    &request.presented,
                    request.presentation,
                ) {
                    Ok(text) => text,
                    Err(error) => request.failed(&error),
                },
            )
        })
        .await
        .unwrap_or_else(|error| request.failed(&error));
    Ok(committed(receipt))
}

/// What one display render needs: the retained observation, the source it
/// compiles against, and the remaining character budget.
#[derive(Clone)]
struct ObservationRender {
    name: String,
    source: ActorWorkbenchSource,
    type_modules: Vec<String>,
    budget: usize,
    presented: Vec<String>,
    presentation: ExpressionPresentation,
    protected: Option<ProtectedObservation>,
}

impl ObservationRender {
    /// The receipt for a display that failed after its value was bound.
    fn failed(&self, error: &ResidentActorWorkbenchError) -> String {
        format!(
            "Display failed: {error}\nValue remains bound as {}. Inspect a smaller field or \
             projection; execution was not repeated.",
            self.name
        )
    }
}

enum DisplayRenderAttempt {
    /// The display receipt: a rendered page, or a failed-display receipt
    /// for a GHC rejection of the display bundle.
    Rendered(String),
    /// A view the compiles read changed before the install; `stage` names
    /// the checkout that found it.
    Stale {
        stage: &'static str,
        changed: SplitStaleView,
    },
}

/// One off-checkout display render. Three short checkouts, two compiles
/// with the machine released:
///
/// 1. snapshot the compile view and reserve the bundle's value generation
///    ([`snapshot_display_compile`]);
/// 2. GHC-compile the display bundle ([`compile_block_off_checkout`]);
/// 3. revalidate the view ([`split_staleness`]) and snapshot the install
///    ([`ResidentSession::snapshot_display_bundle`]);
/// 4. Cranelift-compile the linked program
///    ([`tidepool_runtime::session::PendingDisplayInstall::compile_off_checkout`]);
/// 5. revalidate the view again and the program's imports, install and run
///    ([`ResidentSession::revalidate_and_run_display_bundle`]).
///
/// A session with no machine yet (never the case after a cell item ran)
/// runs the bundle in step 3's checkout.
async fn render_observation_off_checkout<H, O>(
    access: &ResidentMachineAccess<H, O>,
    context: &crate::ActorSessionContext,
    request: &ObservationRender,
    cancellation: &tidepool_runtime::CompilerTransactionCancellation,
) -> Result<DisplayRenderAttempt, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let snapshot_request = request.clone();
    let snapshot = access
        .with_machine(context.clone(), move |session, context, _| {
            snapshot_display_compile(
                session,
                context,
                &snapshot_request.source,
                &snapshot_request.type_modules,
            )
        })
        .await?;
    let generation = snapshot.view.next_value_generation().0;
    #[cfg(test)]
    split_probe::record_display_generation(generation);
    let page_name = format!("__tidepoolPage{generation}");
    let metadata_name = format!("__tidepoolDisplayMetadata{generation}");
    let block = observation_display_block(
        &page_name,
        &metadata_name,
        &context.haskell_effects_alias,
        &request.name,
        request.budget,
        &request.presented,
        request.presentation,
    );

    // GHC, no checkout held.
    let compile_context = context.clone();
    let compile_source = request.source.clone();
    let compile_view = snapshot.view.clone();
    let compile_retained = snapshot.retained.clone();
    let compile_visible_names = snapshot.visible_names.clone();
    let verdict = generated_binds_verdict(&[
        page_name.clone(),
        metadata_name.clone(),
        "cellDisplay".into(),
    ]);
    let compile_cancellation = cancellation.clone();
    let compiled = crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
        tidepool_runtime::with_compiler_transaction_cancellable(compile_cancellation, || {
            compile_block_off_checkout(
                &compile_context,
                &compile_source,
                &compile_context.haskell_effects_alias,
                &block,
                None,
                compile_view,
                &[],
                Some(&verdict),
                None,
                None,
                &compile_retained,
                &compile_visible_names,
                None,
            )
        })
    }))
    .await
    .map_err(ResidentActorWorkbenchError::Join)??;
    let ready = match compiled {
        CompiledBlock::Ready(ready) => *ready,
        // A rejection of the generated display bundle is a failed display,
        // not a staleness to retry: `render_cell_observation` reports it the
        // same way.
        CompiledBlock::Rejected(diagnostic) => {
            return Ok(DisplayRenderAttempt::Rendered(request.failed(
                &ResidentActorWorkbenchError::Inspection(diagnostic.output),
            )));
        }
    };
    let TurnResult::Bind {
        bound, compiled, ..
    } = ready.result
    else {
        return Err(ResidentActorWorkbenchError::Inspection(
            "display bundle did not produce bindings".into(),
        ));
    };
    display_bundle_binders(&bound, &page_name, &metadata_name)?;
    let bound = Arc::new(bound);
    let bundle_generation = ready.generation;

    // Revalidate the view the GHC compile read, and snapshot the install.
    let install_source = request.clone();
    let install_view = snapshot.view.clone();
    let install_bound = Arc::clone(&bound);
    let code = cloned_turn_code(&compiled);
    let pending = access
        .with_machine(context.clone(), move |session, context, _| {
            if session.machine_disposition()
                == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
            {
                return Err(ResidentActorWorkbenchError::MachineLost);
            }
            let fresh_view = actor_compile_view(
                session,
                context,
                &install_source.source,
                &install_source.type_modules,
            )?;
            if let Some(changed) = split_staleness(session, &fresh_view, &install_view, None) {
                return Ok(DisplayInstallSnapshot::Stale(changed));
            }

            let [page, metadata, cell_display] = install_bound.as_slice() else {
                unreachable!("display_bundle_binders checked three binders");
            };
            if !session.prepared_machine_ready() {
                let bundle = session
                    .run_display_bundle_with_sites(
                        code,
                        page,
                        metadata,
                        cell_display,
                        bundle_generation,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                return decode_display_bundle(&bundle, &install_source.name)
                    .map(DisplayInstallSnapshot::Rendered);
            }
            session
                .snapshot_display_bundle(code, page, metadata, cell_display, bundle_generation)
                .map(|pending| DisplayInstallSnapshot::Ready(Box::new(pending)))
                .map_err(ResidentActorWorkbenchError::Resident)
        })
        .await?;
    let pending = match pending {
        DisplayInstallSnapshot::Stale(changed) => {
            return Ok(DisplayRenderAttempt::Stale {
                stage: "view revalidation",
                changed,
            })
        }
        DisplayInstallSnapshot::Rendered(receipt) => {
            return Ok(DisplayRenderAttempt::Rendered(receipt))
        }
        DisplayInstallSnapshot::Ready(pending) => pending,
    };

    // Cranelift, no checkout held.
    let program = crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
        pending.compile_off_checkout()
    }))
    .await
    .map_err(ResidentActorWorkbenchError::Join)?;
    let program = program
        .map_err(|error| ResidentActorWorkbenchError::Resident(ResidentError::Prepared(error)))?;

    #[cfg(test)]
    split_probe::before_display_install().await;
    let run_request = request.clone();
    let run_view = snapshot.view;
    access
        .with_machine(context.clone(), move |session, context, _| {
            if session.machine_disposition()
                == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
            {
                return Err(ResidentActorWorkbenchError::MachineLost);
            }
            let fresh_view = actor_compile_view(
                session,
                context,
                &run_request.source,
                &run_request.type_modules,
            )?;
            if let Some(changed) = split_staleness(session, &fresh_view, &run_view, None) {
                return Ok(DisplayRenderAttempt::Stale {
                    stage: "install",
                    changed,
                });
            }
            match session
                .revalidate_and_run_display_bundle(program)
                .map_err(ResidentActorWorkbenchError::Resident)?
            {
                Some(bundle) => decode_display_bundle(&bundle, &run_request.name)
                    .map(DisplayRenderAttempt::Rendered),
                None => Ok(DisplayRenderAttempt::Stale {
                    stage: "install imports",
                    changed: SplitStaleView::PreparedImports,
                }),
            }
        })
        .await
}

#[tracing::instrument(target = "exomonad_actor::workbench_phase", skip_all, fields(actor = %context.actor))]
async fn render_checked_observation_off_checkout<H, O>(
    access: &ResidentMachineAccess<H, O>,
    context: &crate::ActorSessionContext,
    request: &ObservationRender,
    _cancellation: &tidepool_runtime::CompilerTransactionCancellation,
) -> Result<String, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let protected = request.protected.clone().ok_or_else(|| {
        ResidentActorWorkbenchError::ActorProtocol("protected display has no capture".into())
    })?;
    let budget = request.budget;
    let presented = request.presented.clone();
    tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_display_admit_started", "workbench phase");
    let admission = access
        .with_machine(context.clone(), move |session, _, _| {
            session
                .admit_checked_display(
                    protected.prefix,
                    protected.execution,
                    &protected.binder,
                    budget,
                    presented,
                )
                .map_err(|error| {
                    ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                })
        })
        .await?;
    tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_display_admit_completed", "workbench phase");
    let result = tidepool_runtime::session::turn::consume_cell_program_display(admission.clone())
        .map_err(|failure| {
        ResidentActorWorkbenchError::CompileInfrastructure(classify_compile(&failure.error))
    })?;
    let TurnResult::Bind {
        bound, compiled, ..
    } = result
    else {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "checked display did not compile its bundle".into(),
        ));
    };
    let [page, metadata, alias] = bound.as_slice() else {
        return Err(ResidentActorWorkbenchError::ActorProtocol(
            "checked display has no exact three-binder bundle".into(),
        ));
    };
    let bound = Arc::new([page.clone(), metadata.clone(), alias.clone()]);
    let install_bound = bound.clone();
    let install_admission = admission.clone();
    let code = compiled.into_code();
    let pending = access
        .with_machine(context.clone(), move |session, _, _| {
            let [page, metadata, alias] = install_bound.as_ref();
            session
                .snapshot_checked_display_bundle(
                    code,
                    page,
                    metadata,
                    alias,
                    install_admission.generation(),
                    install_admission,
                )
                .map_err(ResidentActorWorkbenchError::Resident)
        })
        .await?;
    tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_display_native_snapshot_completed", "workbench phase");
    let compiled = crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
        pending.compile_off_checkout()
    }))
    .await
    .map_err(ResidentActorWorkbenchError::Join)?;
    let compiled = compiled
        .map_err(|error| ResidentActorWorkbenchError::Resident(ResidentError::Prepared(error)))?;
    tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_display_jit_completed", "workbench phase");
    let name = request.name.clone();
    access
        .with_machine(context.clone(), move |session, _, _| {
            tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_display_run_started", "workbench phase");
            let bundle = session
                .revalidate_and_run_display_bundle(compiled)
                .map_err(ResidentActorWorkbenchError::Resident)?
                .ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "checked display native install became stale".into(),
                    )
                })?;
            tracing::info!(target: "exomonad_actor::workbench_phase", phase = "checked_display_run_completed", "workbench phase");
            decode_display_bundle(&bundle, &name)
        })
        .await
}

enum DisplayInstallSnapshot {
    Ready(Box<tidepool_runtime::session::PendingDisplayInstall>),
    Rendered(String),
    Stale(SplitStaleView),
}

/// A step whose observation display was deferred out of the checkout that
/// ran it ([`settle_fragment`] never renders): render it now, off-checkout.
/// Every other step passes through unchanged.
async fn settle_deferred_display<H, O>(
    access: &ResidentMachineAccess<H, O>,
    context: crate::ActorSessionContext,
    step: ResidentWorkbenchStep,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    match step {
        ResidentWorkbenchStep::Running { fragment, outcome }
            if matches!(*outcome, ResidentOutcome::Completed { .. }) =>
        {
            settle_observation_render_split(access, context, *fragment, *outcome).await
        }
        step => Ok(step),
    }
}

/// A fresh command output binding name among the scope's workbench bindings.
fn fresh_job_binding_name<H, O>(
    session: &ResidentSession<H, O>,
    scope: tidepool_codegen::scope::ScopeId,
) -> String
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let names: Vec<_> = session
        .workbench_bindings_in(scope)
        .into_iter()
        .map(|binding| binding.name)
        .collect();
    let mut index = session.val_gen().0;
    loop {
        let name = format!("job{index}");
        if !names.contains(&name) {
            return name;
        }
        index += 1;
    }
}

/// A short-checkout snapshot for `begin_fragment_split`'s split compile:
/// the exact source-side view this compile targets, its reserved value
/// generation, the prepared programs still linked against every live
/// prepared binding, the turn classification a bare-expression item resolves
/// to (GHC-sourced when not already known), and — for an expression item —
/// a fresh observation name unique among this scope's visible bindings at
/// snapshot time.
struct FragmentCompileSnapshot {
    view: crate::ActorCompileView,
    generation: tidepool_repr::Generation,
    retained: Vec<(SymbolIdentity, u64)>,
    verdict: Option<TurnClassification>,
    observation_name: Option<String>,
}

/// Take the checkout-scoped snapshot a split fragment compile needs, then
/// release the checkout. Read-only against the session except for the
/// atomic generation reservation.
fn snapshot_fragment_compile<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    block: &ParsedBlock,
    checked_verdict: Option<&TurnClassification>,
) -> Result<FragmentCompileSnapshot, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let view = actor_compile_view(session, context, source, type_modules)?;
    let generation = view.next_value_generation();
    let verdict = match checked_verdict {
        Some(checked) => Some(checked.clone()),
        None => tidepool_runtime::session::classify_block(&[&block.source])
            .map_err(|error| {
                let mut diagnostic = classify_compile(&error);
                diagnostic.message = error.to_string();
                ResidentActorWorkbenchError::CompileInfrastructure(diagnostic)
            })?
            .into_iter()
            .next(),
    };
    if verdict
        .as_ref()
        .is_none_or(|verdict| verdict.kind != TurnKind::Decl)
    {
        session.reserve_value_generations_through(generation);
    }
    let retained = session.prepared_retained();
    let observation_name = if verdict
        .as_ref()
        .is_some_and(|verdict| verdict.kind == TurnKind::Expr)
    {
        let visible = session.workbench_bindings_in(context.placement.lexical_scope);
        let mut name = format!("observation{}", generation.0);
        while visible.iter().any(|binding| binding.name == name) {
            name.push('_');
        }
        Some(name)
    } else {
        None
    };
    Ok(FragmentCompileSnapshot {
        view,
        generation,
        retained,
        verdict,
        observation_name,
    })
}

/// The GHC-compile half of a split fragment compile: everything
/// [`snapshot_fragment_compile`]'s checkout-only prerequisites make
/// possible once they are already in hand. No session or checkout touched
/// here; `begin_fragment_split` runs this with the machine
/// released. Restricted to `begin_fragment`'s own calling convention (no
/// binder pins, no staged cell prefix, no prologue/expression-plan
/// override) — the shape the tool installer and every other
/// `begin_fragment_split` caller compiles.
fn compile_fragment_off_checkout(
    snapshot: &FragmentCompileSnapshot,
    source: &ActorWorkbenchSource,
    effect_stack: &str,
    block: &ParsedBlock,
) -> Result<CompiledBlock, ResidentActorWorkbenchError> {
    let prepared = source.prepare_effectful(&snapshot.view, effect_stack)?;
    let mut templates =
        resident_workbench_templates(&prepared.preamble, effect_stack, &prepared.imports);
    let include_refs: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
    let mut verdict = snapshot.verdict.clone();
    let observation = if let Some(name) = &snapshot.observation_name {
        let preamble = insert_preamble_imports(&prepared.preamble, &prepared.imports);
        let lifts = vec![
            tidepool_runtime::session::ExpressionLift::Effectful,
            tidepool_runtime::session::ExpressionLift::Pure,
        ];
        templates = lifts
            .into_iter()
            .map(|lift| tidepool_runtime::session::TurnTemplate {
                kind: tidepool_runtime::session::TemplateSelector::Bind,
                source: tidepool_runtime::session::turn::assemble_observation_module(
                    &preamble,
                    "__result",
                    effect_stack,
                    "{{TURN}}",
                    lift,
                    None,
                ),
            })
            .collect();
        verdict = Some(TurnClassification {
            kind: TurnKind::Bind,
            binders: vec![name.clone()],
            items: Vec::new(),
        });
        Some((name.clone(), ExpressionPresentation::Rendered, None))
    } else {
        None
    };
    let request = TurnRequest {
        exact_context: snapshot.view.exact_declaration_context().cloned(),
        session_id: Some(snapshot.view.session_id()),
        turn_text: &block.source,
        templates: &templates,
        include: &include_refs,
        session_root: snapshot.view.session_root(),
        inject_modules: &prepared.injected,
        gen: snapshot.generation.0,
        verdict,
        target: None,
        retained_imports: &snapshot.retained,
    };
    match run_turn(request) {
        Ok(result) => Ok(CompiledBlock::Ready(Box::new(ReadyBlock {
            result,
            generation: snapshot.generation,
            declaration_source: block.source.clone(),
            declaration_imports: snapshot.view.workbench_imports(),
            observation,
        }))),
        Err(failure) if classify_compile(&failure.error).class == FailureClass::UserHaskell => {
            let label = format!("<cell item {}>", block.ordinal);
            Ok(CompiledBlock::Rejected(render_turn_compile_rejection(
                &failure.error,
                failure.attempted_source.as_deref(),
                &block.source,
                &label,
            )))
        }
        Err(failure) => Err(ResidentActorWorkbenchError::CompileInfrastructure(
            classify_compile(&failure.error),
        )),
    }
}

fn begin_fragment<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    scope: RequestWorkbenchScope<'_>,
    block: ParsedBlock,
    pins: Option<&[CheckedBinderPin]>,
    verdict: Option<&TurnClassification>,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let source = scope.source(source, context);
    let compiled = match compile_block(
        session,
        context,
        &source,
        &context.haskell_effects_alias,
        scope.type_modules,
        &block,
        pins,
        verdict,
    )? {
        CompiledBlock::Ready(compiled) => compiled,
        CompiledBlock::Rejected(diagnostic) => {
            return Ok(ResidentWorkbenchStep::Rejected(diagnostic));
        }
    };
    begin_ready_block(session, context, &source, scope, block, *compiled, 8192)
}

fn begin_ready_block<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    scope: RequestWorkbenchScope<'_>,
    block: ParsedBlock,
    compiled: ReadyBlock,
    display_budget: usize,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let ReadyBlock {
        result,
        generation,
        declaration_source,
        declaration_imports,
        observation,
    } = compiled;
    match result {
        TurnResult::Decl(receipt) => {
            let result = match session.define_scoped_with_imports_in(
                context.placement.lexical_scope,
                &[&declaration_source],
                &declaration_imports,
            ) {
                Ok(generation) => ResidentWorkbenchStep::Committed {
                    output: declaration_receipt(&receipt.binders, false, generation.0),
                    warnings: Vec::new(),
                    installed_bindings: receipt.binders.clone(),
                },
                Err(tidepool_runtime::session::SessionError::ValidationFailed(failure)) => {
                    ResidentWorkbenchStep::Rejected(failure.rejection_for_input(
                        &format!("<cell item {}>", block.ordinal),
                        &block.source,
                    ))
                }
                Err(error) if classify_session(&error).class == FailureClass::UserHaskell => {
                    ResidentWorkbenchStep::Rejected(classify_session(&error).message.into())
                }
                Err(error) => {
                    return Err(ResidentActorWorkbenchError::Resident(
                        ResidentError::Session(error),
                    ))
                }
            };
            Ok(result)
        }
        TurnResult::Bind {
            bound,
            compiled,
            variant,
            ..
        } => {
            let warnings = compiled.warnings.warnings.clone();
            let protected = protected_observation(&compiled, &bound)?;
            let outcome = match bound.as_slice() {
                [] => {
                    session.run_with_sites("actor_interactive_discard_bind", compiled.into_code())
                }
                [binder] if observation.is_some() => session.run_observation_with_sites(
                    compiled.into_code(),
                    binder,
                    generation,
                    observation
                        .as_ref()
                        .and_then(|(_, _, effectful)| *effectful)
                        .unwrap_or(variant == 0),
                ),
                [binder] => session.run_bind_with_sites(
                    "actor_interactive_bind",
                    compiled.into_code(),
                    binder,
                    generation,
                ),
                binders => session.run_projected_bind_with_sites(
                    "actor_interactive_pattern_bind",
                    compiled.into_code(),
                    binders,
                    generation,
                ),
            };
            finish_bind_step(
                session,
                context,
                source,
                scope.type_modules,
                &block,
                bound,
                warnings,
                observation,
                protected,
                display_budget,
                outcome,
            )
        }
        TurnResult::Expr { .. } => Err(ResidentActorWorkbenchError::ActorProtocol(
            "workbench expression compiled without its observation binding".into(),
        )),
    }
}

/// The shared tail of a Bind turn, whichever entry ran it: the
/// single-checkout match in [`begin_ready_block`] or the off-checkout split
/// in [`begin_ready_block_split`]. Builds the turn's [`WorkbenchDisplay`]
/// from `bound`/`observation` and settles the fragment.
#[allow(clippy::too_many_arguments)]
fn finish_bind_step<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    block: &ParsedBlock,
    bound: Vec<BoundBinder>,
    warnings: Vec<String>,
    observation: Option<(String, ExpressionPresentation, Option<bool>)>,
    protected: Option<ProtectedObservation>,
    display_budget: usize,
    outcome: Result<ResidentOutcome, ResidentError>,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let names = bound
        .iter()
        .map(|binder| binder.name.clone())
        .collect::<Vec<_>>();
    let display = if let Some((name, presentation, _)) = observation {
        WorkbenchDisplay::Observation {
            name,
            budget: display_budget,
            presentation,
            source: source.clone(),
            type_modules: type_modules.to_vec(),
            protected,
        }
    } else if names.is_empty() {
        WorkbenchDisplay::Opaque
    } else {
        WorkbenchDisplay::Binding(bound)
    };
    start_fragment_settlement(
        session,
        context,
        block.ordinal,
        block.source.clone(),
        display,
        warnings,
        outcome,
    )
}

/// [`PendingPreparedMode`] a [`TurnResult::Bind`]'s shape resolves to,
/// exactly as [`begin_ready_block`]'s own `match bound.as_slice()` chooses
/// which `run_*_with_sites` to call -- kept in sync with that match by
/// [`begin_ready_block_split`]'s fallback still calling the originals.
fn pending_mode_for(
    bound: &[BoundBinder],
    generation: tidepool_repr::Generation,
    observation: &Option<(String, ExpressionPresentation, Option<bool>)>,
) -> PendingPreparedMode {
    match bound {
        [] => PendingPreparedMode::Value,
        [binder] if observation.is_some() => PendingPreparedMode::Binding {
            binder: binder.clone(),
            generation,
            observation: Some(Vec::new()),
        },
        [binder] => PendingPreparedMode::Binding {
            binder: binder.clone(),
            generation,
            observation: None,
        },
        binders => PendingPreparedMode::Projected {
            binders: binders.to_vec(),
            generation,
        },
    }
}

/// An owned, `'static` [`TurnCode`] cloned from `turn` without consuming it,
/// so the single-checkout fallback after a stale split install can still run
/// the same compiled turn instead of needing a second GHC compile.
fn cloned_turn_code(turn: &CompiledTurn) -> TurnCode<'static> {
    TurnCode {
        table: std::borrow::Cow::Owned(turn.table.clone()),
        sites: std::borrow::Cow::Owned(turn.asks.clone()),
        prepared: std::borrow::Cow::Owned(turn.prepared.as_ref().clone()),
        certification: std::borrow::Cow::Owned(turn.certification.clone()),
    }
}

/// One split-install attempt's outcome: either the session had no machine
/// yet to snapshot against (see [`ResidentSession::prepared_machine_ready`]),
/// in which case the caller has nothing to compile off-checkout and must use
/// the single-checkout bootstrap install, or a snapshot ready to compile.
enum PreparedSnapshotAttempt {
    Bootstrap,
    Ready(Box<PendingPreparedInstall>),
}

/// The off-checkout split counterpart of [`begin_ready_block`]: a `Decl`
/// commits with no compile at all, in one checkout, exactly as before. A
/// `Bind` turn's JIT install -- [`PreparedEngine::compile_for_install`]'s
/// Cranelift compile, which the wave-3 finding measured dominating resident
/// machine checkout hold (median 79ms, up to 14.9s) -- instead runs
/// off-checkout: snapshot under a short checkout
/// ([`ResidentSession::snapshot_run_prepared`]), compile with no checkout
/// held ([`PendingPreparedInstall::compile_off_checkout`], timed into the
/// call's `compile_ms` bucket rather than `checkout_hold_ms`), then
/// revalidate and install under a fresh checkout
/// ([`ResidentSession::revalidate_and_run_prepared`]). An import that
/// changed between snapshot and revalidation (`Ok(None)`) relinks and
/// recompiles the same GHC output off-checkout from a fresh snapshot, up to
/// [`CHEAP_RETRY_ATTEMPTS`] attempts; after that, or for a session with no
/// machine yet, the turn installs and runs under one checkout, unchanged
/// from [`begin_ready_block`]. A completed observation display renders
/// off-checkout too ([`settle_deferred_display`]).
async fn begin_ready_block_split<H, O>(
    access: &ResidentMachineAccess<H, O>,
    context: crate::ActorSessionContext,
    turn_source: ActorWorkbenchSource,
    type_modules: Vec<String>,
    block: ParsedBlock,
    compiled: ReadyBlock,
    display_budget: usize,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let step = Box::pin(run_ready_block_split(
        access,
        context.clone(),
        turn_source,
        type_modules,
        block,
        compiled,
        display_budget,
    ));
    let step = step.await?;
    settle_deferred_display(access, context, step).await
}

/// [`begin_ready_block_split`] up to the step its run produced, with an
/// observation display still unrendered.
#[tracing::instrument(target = "exomonad_actor::workbench_phase", skip_all, fields(actor = %context.actor, item = block.ordinal))]
async fn run_ready_block_split<H, O>(
    access: &ResidentMachineAccess<H, O>,
    context: crate::ActorSessionContext,
    turn_source: ActorWorkbenchSource,
    type_modules: Vec<String>,
    block: ParsedBlock,
    compiled: ReadyBlock,
    display_budget: usize,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let ReadyBlock {
        result,
        generation,
        declaration_source,
        declaration_imports,
        observation,
    } = compiled;
    let (bound, compiled_turn) = match result {
        TurnResult::Decl(receipt) => {
            let block = block.clone();
            return access
                .with_machine(context, move |session, context, _| {
                    match session.define_scoped_with_imports_in(
                        context.placement.lexical_scope,
                        &[&declaration_source],
                        &declaration_imports,
                    ) {
                        Ok(generation) => Ok(ResidentWorkbenchStep::Committed {
                            output: declaration_receipt(&receipt.binders, false, generation.0),
                            warnings: Vec::new(),
                            installed_bindings: receipt.binders.clone(),
                        }),
                        Err(tidepool_runtime::session::SessionError::ValidationFailed(failure)) => {
                            Ok(ResidentWorkbenchStep::Rejected(
                                failure.rejection_for_input(
                                    &format!("<cell item {}>", block.ordinal),
                                    &block.source,
                                ),
                            ))
                        }
                        Err(error)
                            if classify_session(&error).class == FailureClass::UserHaskell =>
                        {
                            Ok(ResidentWorkbenchStep::Rejected(
                                classify_session(&error).message.into(),
                            ))
                        }
                        Err(error) => Err(ResidentActorWorkbenchError::Resident(
                            ResidentError::Session(error),
                        )),
                    }
                })
                .await;
        }
        TurnResult::Bind {
            bound, compiled, ..
        } => (bound, compiled),
        TurnResult::Expr { .. } => {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "workbench expression compiled without its observation binding".into(),
            ));
        }
    };
    let warnings = compiled_turn.warnings.warnings.clone();
    let protected = protected_observation(&compiled_turn, &bound)?;

    'split: for install_attempt in 1..=CHEAP_RETRY_ATTEMPTS {
        tracing::info!(target: "exomonad_actor::workbench_phase", install_attempt, phase = "native_item_snapshot_started", "workbench phase");
        let code = cloned_turn_code(&compiled_turn);
        let mode = pending_mode_for(&bound, generation, &observation);
        let attempt_future = access.with_machine(context.clone(), move |session, _, _| {
            if !session.prepared_machine_ready() {
                return Ok(PreparedSnapshotAttempt::Bootstrap);
            }
            session
                .snapshot_run_prepared(code, mode, None)
                .map(|pending| PreparedSnapshotAttempt::Ready(Box::new(pending)))
                .map_err(ResidentActorWorkbenchError::Resident)
        });
        let attempt = attempt_future.await?;
        let pending = match attempt {
            PreparedSnapshotAttempt::Bootstrap => break 'split,
            PreparedSnapshotAttempt::Ready(pending) => pending,
        };
        tracing::info!(target: "exomonad_actor::workbench_phase", install_attempt, phase = "native_item_snapshot_completed", "workbench phase");

        // No checkout held here: the Cranelift compile runs concurrently
        // with every other actor's turn against this session.
        let compiled_program =
            crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                pending.compile_off_checkout()
            }))
            .await
            .map_err(ResidentActorWorkbenchError::Join)?;
        let compiled_program = compiled_program.map_err(|error| {
            ResidentActorWorkbenchError::Resident(ResidentError::Prepared(error))
        })?;
        tracing::info!(target: "exomonad_actor::workbench_phase", install_attempt, phase = "native_item_jit_completed", "workbench phase");

        let finish_bound = bound.clone();
        let finish_warnings = warnings.clone();
        let finish_observation = observation.clone();
        let finish_protected = protected.clone();
        let finish_type_modules = type_modules.clone();
        let finish_block = block.clone();
        let finish_source = turn_source.clone();
        let install_future = access.with_machine(context.clone(), move |session, context, _| {
                tracing::info!(target: "exomonad_actor::workbench_phase", install_attempt, phase = "native_item_run_started", "workbench phase");
                match session.revalidate_and_run_prepared(compiled_program) {
                    Ok(Some(outcome)) => finish_bind_step(
                        session,
                        context,
                        &finish_source,
                        &finish_type_modules,
                        &finish_block,
                        finish_bound,
                        finish_warnings,
                        finish_observation,
                        finish_protected,
                        display_budget,
                        Ok(outcome),
                    )
                    .map(Some),
                    Ok(None) => Ok(None),
                    Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
                }
            });
        let step = install_future.await?;
        tracing::info!(target: "exomonad_actor::workbench_phase", install_attempt, phase = "native_item_run_completed", "workbench phase");
        if let Some(step) = step {
            return Ok(step);
        }
        // Revalidation found a stale import: another actor's turn changed
        // a shared binding between the snapshot and this checkout. The
        // GHC-compiled turn is still current; relink and recompile it.
        log_split_stale(
            "prepared install",
            &context,
            "install",
            SplitStaleView::PreparedImports,
            install_attempt < CHEAP_RETRY_ATTEMPTS,
        );
    }

    // Fallback: either this session had no machine yet to snapshot against
    // (the bootstrap case), or the split attempt hit a stale import.
    // Single checkout, exactly as `begin_ready_block`'s Bind arm.
    access
        .with_machine(context, move |session, context, _| {
            tracing::info!(target: "exomonad_actor::workbench_phase", phase = "native_item_bootstrap_started", "workbench phase");
            let outcome = match bound.as_slice() {
                [] => session
                    .run_with_sites("actor_interactive_discard_bind", compiled_turn.into_code()),
                [binder] if observation.is_some() => session.run_observation_with_sites(
                    compiled_turn.into_code(),
                    binder,
                    generation,
                    observation
                        .as_ref()
                        .and_then(|(_, _, effectful)| *effectful)
                        .unwrap_or(false),
                ),
                [binder] => session.run_bind_with_sites(
                    "actor_interactive_bind",
                    compiled_turn.into_code(),
                    binder,
                    generation,
                ),
                binders => session.run_projected_bind_with_sites(
                    "actor_interactive_pattern_bind",
                    compiled_turn.into_code(),
                    binders,
                    generation,
                ),
            };
            tracing::info!(target: "exomonad_actor::workbench_phase", phase = "native_item_bootstrap_completed", "workbench phase");
            finish_bind_step(
                session,
                context,
                &turn_source,
                &type_modules,
                &block,
                bound,
                warnings,
                observation,
                protected,
                display_budget,
                outcome,
            )
        })
        .await
}

fn declaration_receipt(binders: &[String], prologue_only: bool, generation: u64) -> String {
    let description = if !binders.is_empty() {
        format!("defined {}", binders.join(", "))
    } else if prologue_only {
        "accepted cell prologue".to_owned()
    } else {
        "committed declaration".to_owned()
    };
    format!("{description} at generation {generation}")
}

#[cfg(test)]
mod declaration_receipt_tests {
    use super::declaration_receipt;

    #[test]
    fn import_and_pragma_only_cells_have_truthful_receipts() {
        assert_eq!(
            declaration_receipt(&[], true, 7),
            "accepted cell prologue at generation 7"
        );
        assert_eq!(
            declaration_receipt(&[], false, 7),
            "committed declaration at generation 7"
        );
        assert_eq!(
            declaration_receipt(&["watchdogProbe".to_owned()], false, 7),
            "defined watchdogProbe at generation 7"
        );
    }
}

fn start_fragment_settlement<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    input_ordinal: usize,
    cell_text: String,
    display: WorkbenchDisplay,
    warnings: Vec<String>,
    outcome: Result<ResidentOutcome, ResidentError>,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    match outcome {
        Ok(outcome) => settle_fragment(
            session,
            context,
            ResidentWorkbenchFragment {
                display,
                output: Vec::new(),
                presented: Vec::new(),
                recovered_jobs: Vec::new(),
                warnings,
                started_jobs: Vec::new(),
            },
            outcome,
        ),
        Err(ResidentError::Run(error)) => Ok(ResidentWorkbenchStep::Rejected(
            render_runtime_rejection(input_ordinal, &error, &cell_text).into(),
        )),
        // Haskell language failures are user-level rejections; integrity and
        // infrastructure failures stay infrastructure errors.
        Err(ResidentError::Prepared(error))
            if error.kind() == tidepool_runtime::session::PreparedFailureKind::Language =>
        {
            Ok(ResidentWorkbenchStep::Rejected(
                render_cell_runtime_failure(input_ordinal, &error.to_string(), &cell_text).into(),
            ))
        }
        Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
    }
}

fn render_runtime_rejection(
    input_ordinal: usize,
    error: &tidepool_runtime::RuntimeError,
    cell_text: &str,
) -> String {
    let detail = error.to_string();
    render_cell_runtime_failure(input_ordinal, &detail, cell_text)
}

/// One rendering for every route's user-level runtime failure, so a mistake
/// a cell made reads the same whichever engine reported it.
fn render_cell_runtime_failure(input_ordinal: usize, detail: &str, cell_text: &str) -> String {
    let detail = tidepool_runtime::session::runtime_failure_advice(detail, cell_text)
        .unwrap_or_else(|| detail.to_owned());
    format!("<cell item {input_ordinal}>: runtime error: {detail}")
}

fn settle_fragment<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    mut fragment: ResidentWorkbenchFragment,
    outcome: ResidentOutcome,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    // Rendering an observation compiles a display bundle (GHC, then
    // Cranelift). It never runs inside the checkout that ran the value: the
    // completed step is handed back unrendered, and the async caller renders
    // it off-checkout (`settle_deferred_display`, `settle_item`).
    if let (WorkbenchDisplay::Observation { .. }, ResidentOutcome::Completed { .. }) =
        (&fragment.display, &outcome)
    {
        return Ok(ResidentWorkbenchStep::Running {
            fragment: Box::new(fragment),
            outcome: Box::new(outcome),
        });
    }
    match outcome {
        ResidentOutcome::Completed { output, result } => {
            fragment.output.extend(output);
            let mut installed_bindings: Vec<String> = match &fragment.display {
                WorkbenchDisplay::Binding(binders) => {
                    binders.iter().map(|binder| binder.name.clone()).collect()
                }
                WorkbenchDisplay::Observation { name, .. } => vec![name.clone()],
                WorkbenchDisplay::Opaque
                | WorkbenchDisplay::Tool
                | WorkbenchDisplay::ToolDispatch => Vec::new(),
            };
            installed_bindings.append(&mut fragment.recovered_jobs);
            // One command-job-typed binder resolved against the one
            // `Cmd.start` effect this item ran identifies the authored job
            // binding for retained named-tool output navigation. Tag only an
            // entry that remains live at settlement.
            if let (WorkbenchDisplay::Binding(binders), [job]) =
                (&fragment.display, fragment.started_jobs.as_slice())
            {
                if let [binder] = binders.as_slice() {
                    if binder.host_authority == Some(HostBindingAuthority::CommandJob) {
                        // best-effort: see the comment above — a lost race
                        // over the exact live entry is tolerated.
                        session
                            .tag_host_text_binding_in(
                                context.placement.lexical_scope,
                                binder,
                                job.clone(),
                            )
                            .ok();
                    }
                }
            }
            let receipt = match &fragment.display {
                WorkbenchDisplay::Binding(binders) => format!(
                    "[bound {}]",
                    binders
                        .iter()
                        .map(|binder| binder.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                WorkbenchDisplay::Opaque => "<opaque value>".into(),
                WorkbenchDisplay::Tool | WorkbenchDisplay::ToolDispatch => {
                    // A bounded observation marks what it could not afford to
                    // materialize. That is a size answer, so say so instead of
                    // letting the decoder call it a type mismatch.
                    if tidepool_codegen::observation::contains_oversize_sentinel(result.value()) {
                        return Err(ResidentActorWorkbenchError::Inspection(
                            "the tool's answer exceeded the observation budget; return a \
                             smaller Text, or bind the whole value in a cell and select from it"
                                .into(),
                        ));
                    }
                    let text = String::from_value(result.value(), result.table())?;
                    if matches!(&fragment.display, WorkbenchDisplay::ToolDispatch) {
                        let reply: ToolDispatchReply =
                            serde_json::from_str(&text).map_err(|error| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "invalid tool dispatch reply: {error}"
                                ))
                            })?;
                        let output = reply
                            .into_output()
                            .map_err(ResidentActorWorkbenchError::ToolDispatch)?;
                        serde_json::from_value::<String>(output).map_err(|error| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "installed tool output must be rendered Text: {error}"
                            ))
                        })?
                    } else {
                        text
                    }
                }
                WorkbenchDisplay::Observation { .. } => {
                    unreachable!("a completed observation is deferred above")
                }
            };
            let mut transcript = fragment.output.join("\n");
            if !transcript.is_empty() && !receipt.is_empty() {
                transcript.push('\n');
            }
            transcript.push_str(&receipt);
            Ok(ResidentWorkbenchStep::Committed {
                output: transcript,
                warnings: fragment.warnings,
                installed_bindings,
            })
        }
        ResidentOutcome::BindingsCommitted { output } => {
            let bound_name = match &fragment.display {
                WorkbenchDisplay::Binding(binders) => Some(
                    binders
                        .iter()
                        .map(|binder| binder.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                WorkbenchDisplay::Opaque
                | WorkbenchDisplay::Tool
                | WorkbenchDisplay::ToolDispatch
                | WorkbenchDisplay::Observation { .. } => None,
            };
            let receipt = projected_binding_receipt(bound_name.as_deref(), &output)?;
            let installed_bindings = match fragment.display {
                WorkbenchDisplay::Binding(binders) => {
                    binders.into_iter().map(|binder| binder.name).collect()
                }
                WorkbenchDisplay::Opaque
                | WorkbenchDisplay::Tool
                | WorkbenchDisplay::ToolDispatch
                | WorkbenchDisplay::Observation { .. } => Vec::new(),
            };
            fragment.output.push(receipt);
            Ok(ResidentWorkbenchStep::Committed {
                output: fragment.output.join("\n"),
                warnings: fragment.warnings,
                installed_bindings,
            })
        }
        ResidentOutcome::Deferred {
            output,
            hole,
            request,
            work,
        } => {
            fragment.output.extend(output);
            Ok(ResidentWorkbenchStep::Running {
                fragment: Box::new(fragment),
                outcome: Box::new(ResidentOutcome::Deferred {
                    output: Vec::new(),
                    hole,
                    request,
                    work,
                }),
            })
        }
        ResidentOutcome::Suspended {
            output,
            hole,
            request,
        } => {
            fragment.output.extend(output);
            let _ = ResidentRequest::decode(&request, session.data_con_table())?;
            Ok(ResidentWorkbenchStep::Running {
                fragment: Box::new(fragment),
                outcome: Box::new(ResidentOutcome::Suspended {
                    output: Vec::new(),
                    hole,
                    request,
                }),
            })
        }
    }
}

#[cfg(test)]
mod activation_preview_tests {
    use super::{bounded_activation_text, declaration_worth_showing, ACTIVATION_INPUT_LIMIT};

    #[test]
    fn library_reply_type_declaration_is_not_worth_showing() {
        // Text's own module (data.text) is neither a configured workspace
        // module nor a session-declared one: the model already knows the
        // type from "reply type Text" and gains nothing from its internal
        // Array/Int representation.
        assert!(!declaration_worth_showing(
            "Data.Text",
            &["Project.Shell".to_owned()],
        ));
    }

    #[test]
    fn workspace_reply_type_declaration_is_worth_showing() {
        assert!(declaration_worth_showing(
            "Project.Shell",
            &["Project.Shell".to_owned()],
        ));
    }

    #[test]
    fn session_declared_reply_type_declaration_is_worth_showing() {
        // Every session declaration lives under generation-versioned
        // `Tidepool.Session.Lib.G<n>`/`Tidepool.Session.Val.G<n>` modules,
        // never listed in `workspace_modules`.
        assert!(declaration_worth_showing("Tidepool.Session.Lib.G3", &[]));
    }

    #[test]
    fn preview_bounds_preserve_unicode_and_mark_omission() {
        assert_eq!(
            bounded_activation_text("".into(), 4, false, "inspectFull sessionInput"),
            ""
        );
        assert_eq!(
            bounded_activation_text("a\nλ".into(), 4, false, "inspectFull sessionInput"),
            "a\nλ"
        );
        assert_eq!(
            bounded_activation_text("aλz".into(), 2, true, "inspectFull sessionInput"),
            "a\n<additional detail omitted past the 2 bytes cap; expand with `inspectFull sessionInput`>"
        );
        assert_eq!(
            bounded_activation_text("data R".into(), 4, false, "lookup R"),
            "data\n<additional detail omitted past the 4 bytes cap; expand with `lookup R`>"
        );
    }

    #[test]
    fn activation_renders_a_whole_task_and_names_the_cap_past_it() {
        let task = format!(
            "Task {{taskGroup = ForkGroupPath False \"wave/lane\", planPath = \"plans/x.md\", \
             taskSource = GitOid \"3454b78\", obligation = \"{}\", rationale = \"r\", \
             ownedPaths = [\"src/a.rs\"], acceptance = [\"tests pass\"], decisions = []}}",
            "o".repeat(4 * 1024)
        );
        assert_eq!(
            bounded_activation_text(
                task.clone(),
                ACTIVATION_INPUT_LIMIT,
                false,
                "inspectFull sessionInput"
            ),
            task
        );
        let oversized = "x".repeat(ACTIVATION_INPUT_LIMIT + 1);
        let rendered = bounded_activation_text(
            oversized,
            ACTIVATION_INPUT_LIMIT,
            false,
            "inspectFull sessionInput",
        );
        assert!(rendered.starts_with(&"x".repeat(ACTIVATION_INPUT_LIMIT)));
        assert!(rendered.ends_with(
            "\n<additional detail omitted past the 32 KiB cap; expand with `inspectFull sessionInput`>"
        ));
    }
}

/// A reply type's declaration is worth showing to the model only when it is
/// defined by the workspace or the current session: a library, stdlib, or
/// `Tidepool.*` boot-package type is already named by "reply type X", and its
/// declaration would add representation detail with no construction value.
fn declaration_worth_showing(module: &str, workspace_modules: &[String]) -> bool {
    module.starts_with("Tidepool.Session.") || workspace_modules.iter().any(|known| known == module)
}

// An assignment renders whole up to this cap. It bounds the demanded
// character prefix as well as the rendered UTF-8 bytes; the final byte
// clipping can shorten a multibyte prefix further.
const ACTIVATION_INPUT_LIMIT: usize = 32 * 1024;

fn bounded_activation_text(
    mut text: String,
    maximum: usize,
    mut omitted: bool,
    expansion: &str,
) -> String {
    if text.len() > maximum {
        let mut end = maximum;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        omitted = true;
    }
    if omitted {
        let cap = if maximum.is_multiple_of(1024) {
            format!("{} KiB", maximum / 1024)
        } else {
            format!("{maximum} bytes")
        };
        text.push_str(&format!(
            "\n<additional detail omitted past the {cap} cap; expand with `{expansion}`>"
        ));
    }
    text
}

fn decode_activation_observation(
    outcome: ResidentOutcome,
) -> Result<(String, bool), ResidentActorWorkbenchError> {
    match outcome {
        ResidentOutcome::Completed { result, .. } => {
            if tidepool_codegen::observation::contains_oversize_sentinel(result.value()) {
                return Err(ResidentActorWorkbenchError::Inspection(
                    "input preview exceeded the observation budget".into(),
                ));
            }
            <(String, bool)>::from_value(result.value(), result.table())
                .map_err(|error| ResidentActorWorkbenchError::Inspection(error.to_string()))
        }
        _ => Err(ResidentActorWorkbenchError::Inspection(
            "pure input preview unexpectedly suspended".into(),
        )),
    }
}

/// Build and present a retained page from an already captured result.
///
/// Page construction, the strict metadata tuple, and the fresh `cellDisplay`
/// identity compile as one generated three-binder turn.  The resident session
/// binds the page before it forces metadata, then publishes the alias through
/// its existing captured-alias path.  Thus a renderer failure still leaves the
/// observation available without running the expression again.
#[allow(clippy::too_many_arguments)]
/// The exact generated Haskell block a display bundle compiles: a captured
/// page, its `(Text, hasMore, unavailable)` metadata, and `cellDisplay`, all
/// bound as one three-binder turn (`page_name`/`metadata_name` are unique
/// per generation, minted by the caller from a compile view's next value
/// generation). Shared by the single-checkout [`render_cell_observation`]
/// and its off-checkout split counterpart
/// ([`render_observation_off_checkout`]), so both
/// request byte-identical source text against identical binder names.
///
/// `T.copy` is load-bearing. `renderTree` may return a slice into a large
/// Text backing array, while the host should retain only the bounded page
/// it presents.
fn observation_display_block(
    page_name: &str,
    metadata_name: &str,
    effects_alias: &str,
    observation: &str,
    budget: usize,
    presented: &[String],
    presentation: ExpressionPresentation,
) -> ParsedBlock {
    let keys = presented
        .iter()
        .map(|key| {
            format!(
                "T.pack \"{}\"",
                tidepool_runtime::session::escape_workbench_haskell_string(key)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let rendered =
        format!("TidepoolInspection.displayPageWithout [{keys}] {budget} ({observation} ())");
    let opaque = format!("TidepoolInspection.pageWithContinuation {budget} (TidepoolInspection.TextLeaf (T.pack \"<opaque value>\")) Nothing");
    let rendering = match presentation {
        ExpressionPresentation::Rendered => rendered,
        ExpressionPresentation::Opaque => opaque,
    };
    ParsedBlock {
        ordinal: 1,
        total: 1,
        source: format!(
            "({page_name}, {metadata_name}, cellDisplay) <- do {{\n\
             {page_name} <- pure (({rendering}) :: TidepoolInspection.DisplayPage {});\n\
             {metadata_name} <- pure (T.copy (TidepoolInspection.text {page_name}), \
             TidepoolInspection.pageHasMore {page_name}, \
             TidepoolInspection.pageUnavailable {page_name});\n\
             cellDisplay <- pure {page_name};\n\
             pure ({page_name}, {metadata_name}, cellDisplay)\n\
             }}",
            effects_alias,
        ),
    }
}

/// Decode a run display bundle's `(Text, hasMore, unavailable)` metadata
/// into the receipt text a workbench observation presents, appending the
/// continuation/omission footers. Shared by the single-checkout
/// [`render_cell_observation`] and its off-checkout split counterpart.
fn decode_display_bundle(
    bundle: &tidepool_runtime::session::ResidentDisplayBundle,
    observation: &str,
) -> Result<String, ResidentActorWorkbenchError> {
    let (text, more, unavailable): (String, bool, bool) =
        FromHaskell::from_value(bundle.result().value(), bundle.result().table())
            .map_err(|error| ResidentActorWorkbenchError::Inspection(error.to_string()))?;
    let mut output = text;
    if more {
        // A partial view must never read as the whole one. Say that it is a
        // selection, name the binding that holds the rest, and give the one
        // call that continues it. The binding is the retained observation
        // applied to `()` — the same expression this page was rendered from —
        // so what is named here is what a later cell can paste.
        output.push_str(&format!(
            "\n[selection of {observation} (); display continues: cellDisplay.more]"
        ));
    }
    if unavailable {
        output.push_str("\n[custom renderer omitted detail without a continuation]");
    }
    Ok(output)
}

/// The three compiler binders a display bundle's turn must bind, in order,
/// checked against the names [`observation_display_block`] requested.
/// Shared by the single-checkout install and the off-checkout split's own
/// install, so both reject the same "compiler reordered our binders" shape
/// the same way.
fn display_bundle_binders<'a>(
    bound: &'a [BoundBinder],
    page_name: &str,
    metadata_name: &str,
) -> Result<(&'a BoundBinder, &'a BoundBinder, &'a BoundBinder), ResidentActorWorkbenchError> {
    let [page, metadata, cell_display] = bound else {
        return Err(ResidentActorWorkbenchError::Inspection(
            "display bundle must bind page, metadata, and alias".into(),
        ));
    };
    if page.name != page_name
        || metadata.name != metadata_name
        || cell_display.name != "cellDisplay"
    {
        return Err(ResidentActorWorkbenchError::CompileInfrastructure(
            "display bundle returned compiler binders in an unexpected order".into(),
        ));
    }
    Ok((page, metadata, cell_display))
}

#[allow(clippy::too_many_arguments)]
fn render_cell_observation<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    observation: &str,
    budget: usize,
    presented: &[String],
    presentation: ExpressionPresentation,
) -> Result<String, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let generation = actor_compile_view(session, context, source, type_modules)?
        .next_value_generation()
        .0;
    let page_name = format!("__tidepoolPage{generation}");
    let metadata_name = format!("__tidepoolDisplayMetadata{generation}");
    let block = observation_display_block(
        &page_name,
        &metadata_name,
        &context.haskell_effects_alias,
        observation,
        budget,
        presented,
        presentation,
    );
    let ready = match compile_block(
        session,
        context,
        source,
        &context.haskell_effects_alias,
        type_modules,
        &block,
        None,
        Some(&generated_binds_verdict(&[
            page_name.clone(),
            metadata_name.clone(),
            "cellDisplay".into(),
        ])),
    )? {
        CompiledBlock::Ready(ready) => ready,
        CompiledBlock::Rejected(diagnostic) => {
            return Err(ResidentActorWorkbenchError::Inspection(diagnostic.output));
        }
    };
    let TurnResult::Bind {
        bound, compiled, ..
    } = ready.result
    else {
        return Err(ResidentActorWorkbenchError::Inspection(
            "display bundle did not produce bindings".into(),
        ));
    };
    let (page, metadata, cell_display) =
        display_bundle_binders(&bound, &page_name, &metadata_name)?;
    let bundle = session
        .run_display_bundle_with_sites(
            compiled.into_code(),
            page,
            metadata,
            cell_display,
            ready.generation,
        )
        .map_err(ResidentActorWorkbenchError::Resident)?;
    decode_display_bundle(&bundle, observation)
}

/// A short-checkout snapshot for a display bundle's off-checkout compile:
/// the exact source-side view this compile targets, its reserved value
/// generation already claimed, and the retained/visible bindings the
/// compile links against — the same three facts [`compile_block_in_view`]
/// gathers under checkout for a single-checkout compile, taken here so a
/// display render's caller can release the checkout before calling
/// [`compile_block_off_checkout`], mirroring [`snapshot_cell_split`] and
/// [`snapshot_fragment_compile`].
struct DisplayCompileSnapshot {
    view: crate::ActorCompileView,
    retained: Vec<(SymbolIdentity, u64)>,
    visible_names: Vec<String>,
}

fn snapshot_display_compile<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
) -> Result<DisplayCompileSnapshot, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let view = actor_compile_view(session, context, source, type_modules)?;
    // Reserve this display bundle's single value generation — the same
    // reservation `compile_block_in_view` performs for an ordinary bind
    // turn — before releasing the checkout, so a concurrent turn cannot
    // reuse the generation this off-checkout compile is about to target.
    session.reserve_value_generations_through(view.next_value_generation());
    let retained = session.prepared_retained();
    let visible_names = session
        .workbench_bindings_in(context.placement.lexical_scope)
        .into_iter()
        .map(|binding| binding.name)
        .collect();
    Ok(DisplayCompileSnapshot {
        view,
        retained,
        visible_names,
    })
}

impl<H, O> ResidentActorRunner<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Move only the returned actor-program successor out of a hosted cell's
    /// cleanup set. The target guard is established before the source releases
    /// its exact id; all other cell continuations remain owned by the cell.
    pub(crate) fn handoff_actor_continuation(
        &self,
        context: crate::ActorSessionContext,
        outcome: &ResidentOutcome,
    ) -> Result<Option<ParkedHoleAbortGuard>, ResidentActorWorkbenchError> {
        let Some(cont_id) = outcome_continuation_id(outcome) else {
            return Ok(None);
        };
        let reason = "actor program abandoned before continuation settlement".to_owned();
        let source = SLOT_CONTINUATION_OWNER.try_with(Clone::clone).ok();
        let Some(source) = source else {
            return Ok(Some(ParkedHoleAbortGuard::with_latest(
                &self.access,
                context,
                Some(cont_id),
                reason,
            )));
        };
        let refusal = |reason| ResidentActorWorkbenchError::ContinuationHandoff {
            actor: context.actor,
            placement: context.placement,
            continuation: cont_id.clone(),
            reason,
        };
        let exact_owner = source.0.owner == Some((context.actor, context.placement));
        let private_owner = source
            .0
            .retained_authority
            .as_ref()
            .and_then(|authority| {
                authority.downcast_ref::<crate::resident_actor::ExecutionResourceOwners>()
            })
            .is_some_and(|resources| resources.authorizes_cleanup_context(&context));
        if !exact_owner && !private_owner {
            return Err(refusal(ContinuationHandoffFailure::WrongOwner));
        }
        let mut state = source.0.state.lock();
        let current = match &mut *state {
            ParkedHoleState::Owned(current) => current,
            ParkedHoleState::Abandoned(_) => {
                return Err(refusal(ContinuationHandoffFailure::Abandoned))
            }
            ParkedHoleState::Settled => return Err(refusal(ContinuationHandoffFailure::Settled)),
        };
        if !current.contains(&cont_id) {
            return Err(refusal(ContinuationHandoffFailure::NotOwned));
        }
        let target = ParkedHoleAbortGuard::with_retained_latest(
            &self.access,
            context.clone(),
            Some(cont_id.clone()),
            reason,
            source.0.retained_authority.clone(),
        );
        current.remove(&cont_id);
        tracing::info!(actor = ?context.actor, continuation = %cont_id,
            remaining_cell_holes = current.len(), "actor continuation custody transferred");
        Ok(Some(target))
    }

    /// Establish the exact durable public surface before root readiness.
    pub(crate) async fn bind_durable_root_public_owner(
        &self,
        context: crate::ActorSessionContext,
        owner: tidepool_runtime::session::RecoveryPublicOwner,
    ) -> Result<tidepool_runtime::session::PublicManifestCommit, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .initialize_durable_public_scope(owner, context.placement.lexical_scope)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
            })
            .await
    }

    /// Consume the child's original retained placement under the same
    /// checkout that initializes its canonical durable public surface.
    pub(crate) async fn initialize_fork_child_public_owner(
        &self,
        context: crate::ActorSessionContext,
        owner: tidepool_runtime::session::RecoveryPublicOwner,
        lease: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
        retirement: crate::RetainedActorExit,
    ) -> Result<tidepool_runtime::session::PublicManifestCommit, ResidentActorWorkbenchError> {
        self.access
            .with_machine_wait(
                context,
                MachineCheckoutAdmission::UntilRetirement(retirement),
                move |session, context, _| {
                    let scope = context.placement.lexical_scope;
                    session.validate_lexical_scope_lease(scope, &lease)?;
                    session
                        .initialize_durable_public_scope(owner, scope)
                        .map_err(|error| {
                            ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                        })
                },
            )
            .await
    }

    pub(crate) async fn begin_public_bootstrap(
        &self,
        context: crate::ActorSessionContext,
        owner: Arc<crate::resident_actor::WorkbenchPublicOwner>,
    ) -> Result<
        Option<tidepool_runtime::session::DurablePublicBootstrap>,
        ResidentActorWorkbenchError,
    > {
        if !owner.matches_context(&context) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "bootstrap requires its original public owner".into(),
            ));
        }
        let Some(durable) = owner.durable().cloned() else {
            return Ok(None);
        };
        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .begin_durable_public_bootstrap(durable, context.placement.lexical_scope)
                    .map(Some)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
            })
            .await
    }

    pub(crate) async fn publish_public_bootstrap(
        &self,
        context: crate::ActorSessionContext,
        owner: Arc<crate::resident_actor::WorkbenchPublicOwner>,
        bootstrap: Option<tidepool_runtime::session::DurablePublicBootstrap>,
    ) -> Result<tidepool_runtime::session::PublicManifestCommit, ResidentActorWorkbenchError> {
        if !owner.matches_context(&context) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "bootstrap publication requires its original public owner".into(),
            ));
        }
        let Some(bootstrap) = bootstrap else {
            return Ok(tidepool_runtime::session::PublicManifestCommit::Durable);
        };
        let durable = owner.durable().cloned().ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "durable bootstrap lost its original public owner".into(),
            )
        })?;
        self.access.with_machine(context, move |session, context, _| {
            let commit = session.publish_durable_public_bootstrap(bootstrap)
                .map_err(ResidentError::Session)?;
            match commit {
                tidepool_runtime::session::PublicManifestCommit::PublishedDurabilityUnconfirmed { detail } => {
                    match session.confirm_durable_public_scope(&durable, context.placement.lexical_scope) {
                        Ok(()) => Ok(tidepool_runtime::session::PublicManifestCommit::Durable),
                        Err(error) => Ok(tidepool_runtime::session::PublicManifestCommit::PublishedDurabilityUnconfirmed {
                            detail: format!("{detail}; confirmation failed: {error}"),
                        }),
                    }
                }
                other => Ok(other),
            }
        }).await
    }

    pub(crate) async fn validate_fork_child_scope(
        &self,
        context: crate::ActorSessionContext,
        lease: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .validate_lexical_scope_lease(context.placement.lexical_scope, &lease)
                    .map_err(Into::into)
            })
            .await
    }

    /// Transfer only the journal-certified durable predecessor to this exact
    /// newly admitted placement. Runtime fences all pre-transfer offers.
    pub(crate) async fn transfer_recovered_root_public_owner(
        &self,
        context: crate::ActorSessionContext,
        predecessor: &tidepool_runtime::session::RecoveryPublicOwner,
        successor: tidepool_runtime::session::RecoveryPublicOwner,
        authority: Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>,
    ) -> Result<tidepool_runtime::session::PublicManifestCommit, ResidentActorWorkbenchError> {
        let predecessor = predecessor.clone();
        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .transfer_recovered_public_owner(
                        &predecessor,
                        successor,
                        context.placement.lexical_scope,
                        authority,
                    )
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
            })
            .await
    }

    /// Admit one cell's independent lexical write domain and its exact public
    /// baseline in the same machine checkout. Every resumed item keeps this
    /// private scope; publication later compares the captured public view.
    pub(crate) async fn begin_private_execution(
        &self,
        context: crate::ActorSessionContext,
        owner: Arc<crate::resident_actor::WorkbenchPublicOwner>,
        decision: Arc<tidepool_runtime::session::PublicationDecision>,
    ) -> Result<ExecutionPrivateScope, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let public_scope = context.placement.lexical_scope;
                if !owner.matches_context(&context) {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "private admission requires its original public owner".into(),
                    ));
                }
                let admission = Arc::new(
                    match owner.durable() {
                        Some(durable) => {
                            session.begin_durable_private_execution(durable, public_scope)
                        }
                        None => session.begin_ephemeral_private_execution(public_scope),
                    }
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })?,
                );
                let admitted_public = admission.admitted_public().clone();
                let private_scope = admission.private_scope();
                Ok(ExecutionPrivateScope {
                    owner,
                    public_scope,
                    private_scope,
                    admitted_public,
                    decision,
                    admission,
                })
            })
            .await
    }

    /// Complete only the directory durability confirmation for an already
    /// visible initialized owner; the Pending actor keeps its placement alive.
    pub(crate) async fn confirm_durable_public_owner(
        &self,
        context: crate::ActorSessionContext,
        owner: tidepool_runtime::session::RecoveryPublicOwner,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .confirm_durable_public_scope(&owner, context.placement.lexical_scope)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
            })
            .await
    }

    /// Publish a fixed native execution. Every retry captures only the latest
    /// public merge baseline; accepted and rejected compiler graphs both
    /// revalidate under the owning checkout before becoming a terminal result.
    pub(crate) async fn publish_private_execution(
        &self,
        context: crate::ActorSessionContext,
        execution: Arc<ExecutionPrivateScope>,
        publication: tidepool_runtime::session::ExecutionPublicationIntent,
    ) -> Result<PrivateExecutionPublication, ResidentActorWorkbenchError> {
        if context.placement.lexical_scope != execution.private_scope {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "execution publication requires its admitted private context".into(),
            ));
        }
        let mut public_context = context.clone();
        public_context.placement.lexical_scope = execution.public_scope;
        if !execution.owner.matches_context(&public_context) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "publication requires its original public owner".into(),
            ));
        }
        let actor = context.actor;
        let scope = execution.public_scope;
        let publication_error =
            move |phase, source| ResidentActorWorkbenchError::PrivatePublication {
                actor,
                scope,
                phase,
                source: Box::new(source),
            };
        let admission = execution.admission.clone();
        let intent = self
            .access
            .with_machine(context.clone(), move |session, _, _| {
                session
                    .freeze_private_execution(&admission, publication)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
            })
            .await
            .map_err(|error| publication_error(PrivatePublicationPhase::Freeze, error))?;
        let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));
        loop {
            if matches!(
                execution.decision.phase(),
                tidepool_runtime::session::PublicationPhase::CancellationRequested
                    | tidepool_runtime::session::PublicationPhase::Terminated
            ) {
                return Ok(PrivateExecutionPublication::Manifest(
                    tidepool_runtime::session::PublicManifestCommit::Cancelled,
                ));
            }
            let owner = execution.owner.clone();
            let intent = intent.clone();
            let baseline = self
                .access
                .with_machine(context.clone(), move |session, _, _| {
                    match owner.durable() {
                        Some(durable) => {
                            session.restage_execution_publication(durable.clone(), intent)
                        }
                        None => session.restage_ephemeral_execution_publication(intent),
                    }
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })
                })
                .await
                .map_err(|error| publication_error(PrivatePublicationPhase::Restage, error))?;
            let compile_cancellation = cancellation.clone();
            let prepared = crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                tidepool_runtime::with_compiler_transaction_cancellable(
                    compile_cancellation,
                    || match baseline {
                        tidepool_runtime::session::ExecutionPublication::Bindings(base) => {
                            base.stage().map(PreparedExecutionPublication::Manifest)
                        }
                        tidepool_runtime::session::ExecutionPublication::Declarations(base) => {
                            match base.certify()? {
                                tidepool_runtime::session::CertifiedDeclarationPublication::Accepted(accepted) => {
                                    accepted.stage().map(PreparedExecutionPublication::Manifest)
                                }
                                tidepool_runtime::session::CertifiedDeclarationPublication::Rejected(rejected) => {
                                    Ok(PreparedExecutionPublication::Rejected(rejected))
                                }
                            }
                        }
                    },
                )
            }))
            .await
            .map_err(|error| publication_error(PrivatePublicationPhase::CertifyAndStage, ResidentActorWorkbenchError::Join(error)))?
            .map_err(|error| publication_error(PrivatePublicationPhase::CertifyAndStage, ResidentActorWorkbenchError::Resident(ResidentError::Session(error))))?;
            match prepared {
                PreparedExecutionPublication::Manifest(ticket) => {
                    let decision = execution.decision.clone();
                    let outcome = self
                        .access
                        .with_machine(context.clone(), move |session, _, _| {
                            session
                                .publish_staged_public_manifest(ticket, &decision)
                                .map_err(|error| {
                                    ResidentActorWorkbenchError::Resident(ResidentError::Session(
                                        error,
                                    ))
                                })
                        })
                        .await
                        .map_err(|error| {
                            publication_error(PrivatePublicationPhase::Publish, error)
                        })?;
                    if outcome == tidepool_runtime::session::PublicManifestCommit::Stale {
                        continue;
                    }
                    cancel_on_drop.0 = None;
                    return Ok(PrivateExecutionPublication::Manifest(outcome));
                }
                PreparedExecutionPublication::Rejected(rejected) => {
                    let outcome = self
                        .access
                        .with_machine(context.clone(), move |session, _, _| {
                            session
                                .revalidate_declaration_rejection(&rejected)
                                .map_err(|error| {
                                    ResidentActorWorkbenchError::Resident(ResidentError::Session(
                                        error,
                                    ))
                                })
                        })
                        .await
                        .map_err(|error| {
                            publication_error(PrivatePublicationPhase::RevalidateRejection, error)
                        })?;
                    match outcome {
                        tidepool_runtime::session::DeclarationPublicationRejection::Stale => {
                            continue
                        }
                        tidepool_runtime::session::DeclarationPublicationRejection::Rejected {
                            reason,
                            diagnostic,
                        } => {
                            cancel_on_drop.0 = None;
                            return Ok(PrivateExecutionPublication::Rejected {
                                reason,
                                diagnostic,
                            });
                        }
                    }
                }
            }
        }
    }

    pub(crate) async fn confirm_publication_durability(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session.confirm_publication_durability().map_err(|error| {
                    ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                })
            })
            .await
    }

    pub(crate) async fn public_visibility_snapshot(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<tidepool_runtime::session::PublicVisibilitySnapshot, ResidentActorWorkbenchError>
    {
        self.access
            .with_machine(context, |session, context, _| {
                session
                    .public_visibility_snapshot_in(context.placement.lexical_scope)
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "workbench public lexical scope is unavailable".into(),
                        )
                    })
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn capture_context_scope(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<tidepool_codegen::scope::ScopeId, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .mint_detached_scope(context.placement.lexical_scope)
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "checkpoint source scope was retired".into(),
                        )
                    })
            })
            .await
    }

    /// Capture the token root and an independently retained runtime capsule
    /// under one checkout. Admission may keep the capsule after token release.
    pub(crate) async fn capture_retained_context_scope(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<
        (
            ScopeId,
            Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
        ),
        ResidentActorWorkbenchError,
    > {
        self.access
            .with_machine(context, |session, context, _| {
                let scope = session
                    .mint_detached_scope(context.placement.lexical_scope)
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "checkpoint source scope was retired".into(),
                        )
                    })?;
                match session.retain_lexical_scope(scope) {
                    Ok(retained) => Ok((scope, retained)),
                    Err(error) => {
                        session.retire_scope(scope);
                        Err(error.into())
                    }
                }
            })
            .await
    }

    pub(crate) async fn remint_checkpoint_child_scope_from_lease(
        &self,
        context: crate::ActorSessionContext,
        checkpoint: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
        provisional_scope: ScopeId,
    ) -> Result<ScopeId, ResidentActorWorkbenchError> {
        let source_scope = context.placement.lexical_scope;
        self.access
            .with_machine(context, move |session, _, _| {
                // The runtime validates the capsule's owner before creating any
                // child scope. Opaque scope IDs alone carry no ownership.
                let scope = session.mint_scope_from_lease(&checkpoint)?;
                if !session.retain_scope_dependencies(source_scope, scope) {
                    session.retire_scope(scope);
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "authored entry dependencies were unavailable".into(),
                    ));
                }
                session.retire_scope(provisional_scope);
                Ok(scope)
            })
            .await
    }

    pub(crate) async fn retire_fork_scopes(
        &self,
        context: crate::ActorSessionContext,
        scopes: Vec<tidepool_codegen::scope::ScopeId>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                for scope in scopes {
                    session.retire_scope(scope);
                }
                Ok(())
            })
            .await
    }

    pub(crate) async fn retire_checkpoint_scopes(
        &self,
        session_id: tidepool_repr::SessionId,
        scopes: Vec<tidepool_codegen::scope::ScopeId>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_host_machine("checkpoint", session_id, None, move |session, _| {
                for scope in scopes {
                    session.retire_scope(scope);
                }
                Ok(())
            })
            .await
    }

    pub(crate) async fn finalize_fork_scopes(
        &self,
        context: crate::ActorSessionContext,
        previous: Vec<(tidepool_codegen::scope::ScopeId, crate::ForkContext)>,
    ) -> Result<Vec<tidepool_codegen::scope::ScopeId>, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let mut scopes = Vec::with_capacity(previous.len());
                for (old, ancestry) in previous {
                    if ancestry == crate::ForkContext::SelectedContext {
                        scopes.push(old);
                        continue;
                    }
                    let scope = session
                        .mint_scope(context.placement.lexical_scope)
                        .ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "fork parent scope was retired".into(),
                            )
                        })?;
                    session.retire_scope(old);
                    scopes.push(scope);
                }
                Ok(scopes)
            })
            .await
    }

    pub(crate) async fn capture_boundary(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError> {
        self.capture_boundary_mode(context, outcome, actor_realm, BoundaryCapture::Execution)
            .await
    }

    pub(crate) async fn capture_replacement_boundary(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError> {
        self.capture_boundary_mode(context, outcome, actor_realm, BoundaryCapture::Replacement)
            .await
    }

    async fn capture_boundary_mode(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
        mode: BoundaryCapture,
    ) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError> {
        let (hole, request) = match outcome {
            ResidentOutcome::Completed { .. } | ResidentOutcome::BindingsCommitted { .. } => {
                return Ok(ResidentActorBoundary::Completed);
            }
            ResidentOutcome::Deferred { hole, work, .. } => {
                if matches!(mode, BoundaryCapture::Replacement) {
                    let reason = "replacement staging cannot execute external effects".to_owned();
                    let (_, consumed) = self.abort_live(context, hole, reason.clone()).await;
                    if !consumed {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "replacement external effect could not be retired".into(),
                        ));
                    }
                    return Err(ResidentActorWorkbenchError::ActorProtocol(reason));
                }
                return Ok(ResidentActorBoundary::External {
                    continuation: hole,
                    work,
                });
            }
            ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        };

        self.access
            .with_machine(context, move |session, context, _| {
                let decoded = ResidentRequest::decode(&request, session.data_con_table())?;
                if matches!(mode, BoundaryCapture::Replacement)
                    && !matches!(&decoded, ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorCheckpointWith(..)
                        | crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(..)
                    ))
                {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "replacement staging cannot execute `{}`", decoded.operation()
                    )));
                }
                if let ResidentRequest::ActorLocal(local) = &decoded {
                    if let Some((owner, target)) = attached_source_target(local)? {
                        let entry = session.live_payload_handle_owned_by(
                            hole.cont_id(), context.placement.resource_scope,
                        )?.ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol(
                            "attached source has no live mapping closure".into(),
                        ))?;
                        return Ok(ResidentActorBoundary::AttachSource {
                            continuation: hole,
                            owner,
                            source: crate::request::sources::SourceBinding {
                                target,
                                entry: Arc::new(entry),
                            },
                        });
                    }
                }
                match decoded {
                    ResidentRequest::Sleep(crate::generated::sleep::SleepReq::SleepWith(
                        duration,
                    )) => Ok(ResidentActorBoundary::Sleep {
                        continuation: hole,
                        duration: Duration::from_millis(
                            duration
                                .checked_milliseconds()
                                .map_err(ResidentActorWorkbenchError::ActorProtocol)?,
                        ),
                    }),
                    ResidentRequest::Jev(crate::generated::jev::JevReq::JevAskWith(request)) => {
                        Ok(ResidentActorBoundary::Jev { continuation: hole, request })
                    }
                    ResidentRequest::Model(request) => Ok(ResidentActorBoundary::Model {
                        continuation: hole,
                        request,
                        table: session.data_con_table().clone(),
                    }),
                    ResidentRequest::Context(request) => Ok(ResidentActorBoundary::Context {
                        continuation: hole,
                        request,
                        table: session.data_con_table().clone(),
                    }),
                    ResidentRequest::ActorContext(
                        crate::generated::actor_context::ActorContextReq::ActorContextWith,
                    ) => Ok(ResidentActorBoundary::ActorContext(hole)),
                    ResidentRequest::AgentLaunch(
                        crate::generated::agent_launch::AgentLaunchReq::AgentLaunchWith(
                            label,
                            _,
                            unbound_label,
                            role,
                            profile,
                            worktrees,
                            lifetime,
                        ),
                    ) => crate::ResidentActorStart::capture_decoded(
                        session,
                        hole,
                        crate::start::ActorStartRequest {
                            label, role, profile, launch_worktrees: worktrees,
                            fork_group: None, fork_workspace: None, effect_keys: None,
                            fork_effort: None, fork_budget: None, model: None, instructions: None, context: crate::ForkContext::SelectedContext,
                            checkpoint: None,
                            lifetime,
                            session_id: context.placement.session, parent_actor: context.actor,
                            unbound_label,
                        },
                    )
                    .map(ResidentActorBoundary::Start)
                    .map_err(ResidentActorWorkbenchError::StartCapture),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksStartWith(
                        label,
                        _,
                        unbound_label,
                        group,
                        role,
                        profile,
                        worktrees,
                        worktree_spec,
                        bound_dirty_policy,
                        effect_keys,
                        effort,
                        budget,
                        model, fork_context, checkpoint, instructions, lifetime,
                    )) => {
                        let group = u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid fork group id {group}"
                            ))
                        })?;
                        crate::ResidentActorStart::capture_decoded(
                            session,
                            hole,
                            crate::start::ActorStartRequest {
                                label, role, profile, launch_worktrees: worktrees,
                                fork_group: Some(crate::ForkGroupId(group)),
                                fork_workspace: Some(match worktree_spec {
                                    Some(spec) => crate::ForkWorkspaceSeed::Explicit(spec),
                                    None => crate::ForkWorkspaceSeed::CurrentCheckout(bound_dirty_policy),
                                }),
                                effect_keys: Some(effect_keys), fork_effort: effort, fork_budget: budget, model, instructions, context: fork_context, checkpoint, lifetime,
                                session_id: context.placement.session, parent_actor: context.actor,
                                unbound_label,
                            },
                        )
                        .map(ResidentActorBoundary::Start)
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksPreviewWith(role, effect_keys, budget, model, effort, context, instructions, lifetime)) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Preview { continuation: hole, role, effect_keys, budget, model, effort, context, instructions, lifetime })),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksCheckpointWith(name)) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Checkpoint { continuation: hole, name })),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksCheckCheckpointWith(token)) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::CheckCheckpoint { continuation: hole, token })),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksReleaseCheckpointWith(token)) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::ReleaseCheckpoint { continuation: hole, token })),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksBeginWith(
                        relative,
                        group,
                        branches,
                    )) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Begin {
                        continuation: hole,
                        relative,
                        group,
                        branches,
                    })),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksCommitWith(
                        group,
                    )) => Ok(ResidentActorBoundary::ForkGroup(
                        ForkGroupBoundary::Commit {
                            continuation: hole,
                            group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "invalid fork group id {group}"
                                ))
                            })?),
                        },
                    )),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksCommitCapturedWith(group)) => Ok(ResidentActorBoundary::ForkGroup(
                        ForkGroupBoundary::CommitCaptured {
                            continuation: hole,
                            group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(format!("invalid fork group id {group}"))
                            })?),
                        },
                    )),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksAbortWith(
                        group,
                    )) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Abort {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid fork group id {group}"
                            ))
                        })?),
                    })),
                    ResidentRequest::Forks(
                        crate::generated::forks::ForksReq::ForksCleanupWith(group),
                    ) => Ok(ResidentActorBoundary::ForkGroup(
                        ForkGroupBoundary::Cleanup {
                            continuation: hole,
                            group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "invalid fork group id {group}"
                                ))
                            })?),
                        },
                    )),
                    ResidentRequest::Notifications(crate::generated::notifications::NotificationsReq::NotifyWith(target, message)) => {
                        Ok(ResidentActorBoundary::NotificationSend { continuation: hole, target: crate::wait::decode_address(target.0, target.1)?, message })
                    }
                    ResidentRequest::Notifications(crate::generated::notifications::NotificationsReq::PollNotificationWith(receipt)) => {
                        Ok(ResidentActorBoundary::NotificationPoll { continuation: hole, receipt })
                    }
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentInspectWith(
                            target,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentInspect(
                        AgentInspectionBoundary {
                            target: crate::wait::decode_address(target.0, target.1)?,
                            continuation: hole,
                        },
                    )),
                    ResidentRequest::Lookup(crate::generated::lookup::LookupReq::LookupRaw(request)) => Ok(ResidentActorBoundary::Lookup { continuation: hole, request: crate::lookup::LookupRequest::from_value(&request, session.data_con_table())? }),
                    ResidentRequest::Introspection(
                        crate::generated::introspection::IntrospectionReq::IntrospectionInfoWith(
                            query,
                        ),
                    ) => Ok(ResidentActorBoundary::Introspection {
                        continuation: hole,
                        query: IntrospectionNameQuery::from_value(
                            &query,
                            session.data_con_table(),
                        )?
                        .into(),
                        kind: StructuredInspectionKind::Info,
                    }),
                    ResidentRequest::Introspection(
                        crate::generated::introspection::IntrospectionReq::IntrospectionTypeOfWith(
                            query,
                        ),
                    ) => Ok(ResidentActorBoundary::Introspection {
                        continuation: hole,
                        query: IntrospectionNameQuery::from_value(
                            &query,
                            session.data_con_table(),
                        )?
                        .into(),
                        kind: StructuredInspectionKind::Type,
                    }),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentListWith,
                    ) => Ok(ResidentActorBoundary::AgentList(hole)),
                    ResidentRequest::Reflect(
                        crate::generated::reflect::ReflectReq::ReflectWith(count),
                    ) => Ok(ResidentActorBoundary::ReflectConversation {
                        continuation: hole,
                        count,
                    }),
                    ResidentRequest::AgentInspection(crate::generated::agent_inspection::AgentInspectionReq::AgentShareObservationWith(recipient, scope)) => Ok(ResidentActorBoundary::AgentShareObservation {
                        continuation: hole,
                        recipient: crate::wait::decode_address(recipient.0, recipient.1)?,
                        scope: crate::wait::decode_address(scope.0, scope.1)?,
                    }),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentGroupListWith(group),
                    ) => Ok(ResidentActorBoundary::AgentGroupList {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!("invalid fork group id {group}"))
                        })?),
                    }),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentForgetWith(
                            target,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentForget(
                        AgentInspectionBoundary {
                            target: crate::wait::decode_address(target.0, target.1)?,
                            continuation: hole,
                        },
                    )),
                    ResidentRequest::Console(crate::generated::console::ConsoleReq::Print(text)) => Ok(ResidentActorBoundary::Console { continuation: hole, text }),
                    ResidentRequest::Console(crate::generated::console::ConsoleReq::DisplayWith((identity, text, expansions, unavailable), _)) => {
                        let callback = session.live_payload_handle_owned_by(hole.cont_id(), actor_realm)?
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("display has no retained expansion callback".into()))?;
                        Ok(ResidentActorBoundary::DisplayPublish {
                            continuation: hole,
                            output: tidepool_runtime::session::WorkbenchDisplayPage { identity, text, expansions, unavailable },
                            callback,
                        })
                    }
                    ResidentRequest::Console(crate::generated::console::ConsoleReq::DisplayExpandWith((identity, key))) => Ok(ResidentActorBoundary::DisplayExpand { continuation: hole, identity, key }),
                    ResidentRequest::Console(crate::generated::console::ConsoleReq::DisplayAllowanceWith) => Ok(ResidentActorBoundary::DisplayAllowance { continuation: hole }),
                    ResidentRequest::Console(crate::generated::console::ConsoleReq::DisplayExpansionInputWith) => Err(ResidentActorWorkbenchError::ActorProtocol("display expansion input requires a retained callback invocation".into())),
                    ResidentRequest::Commands(request) => Ok(ResidentActorBoundary::Command { continuation: hole, request }),
                    ResidentRequest::AgentControl(
                        crate::generated::agent_control::AgentControlReq::AgentControlStopWith(
                            target,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentStop(AgentInspectionBoundary {
                        target: crate::wait::decode_address(target.0, target.1)?,
                        continuation: hole,
                    })),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentInspectCleanupWith(
                            group,
                        ),
                    ) => Ok(ResidentActorBoundary::CleanupPlan {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid cleanup group id {group}"
                            ))
                        })?),
                    }),
                    ResidentRequest::AgentControl(
                        crate::generated::agent_control::AgentControlReq::AgentControlExecuteCleanupWith(
                            group, inspected,
                        ),
                    ) => Ok(ResidentActorBoundary::CleanupExecute {
                        continuation: hole,
                        inspected: inspected.into_iter().map(|(id, incarnation, revision)| {
                            Ok((crate::wait::decode_address(id, incarnation)?, u64::try_from(revision).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol("invalid cleanup revision".into())
                            })?))
                        }).collect::<Result<_, ResidentActorWorkbenchError>>()?,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid cleanup group id {group}"
                            ))
                        })?),
                    }),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorStartWith(..)
                        | crate::generated::actor::ActorReq::ActorForkWith(..),
                    ) => {
                        let table = session.data_con_table().clone();
                        crate::ResidentActorStart::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            context.placement.session,
                            context.actor,
                        )
                        .map(ResidentActorBoundary::Start)
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorBeginForkGroupWith(
                            relative,
                            group,
                            branches,
                        ),
                    ) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Begin {
                        continuation: hole,
                        relative,
                        group,
                        branches,
                    })),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorCommitForkGroupWith(group),
                    ) => Ok(ResidentActorBoundary::ForkGroup(
                        ForkGroupBoundary::Commit {
                            continuation: hole,
                            group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "invalid fork group id {group}"
                                ))
                            })?),
                        },
                    )),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorAbortForkGroupWith(group),
                    ) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Abort {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid fork group id {group}"
                            ))
                        })?),
                    })),
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorCallWith(
                        target,
                        _,
                    )) => capture_outbound_boundary(
                        session,
                        context,
                        hole,
                        target,
                        OutboundKind::Call,
                        actor_realm,
                    ),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorTryCallWith(target, _),
                    ) => capture_outbound_boundary(
                        session,
                        context,
                        hole,
                        target,
                        OutboundKind::TryCall,
                        actor_realm,
                    ),
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorCastWith(
                        target,
                        _,
                    )) => capture_outbound_boundary(
                        session,
                        context,
                        hole,
                        target,
                        OutboundKind::Cast,
                        actor_realm,
                    ),
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorTryCastWith(
                        target,
                        _,
                    )) => capture_outbound_boundary(
                        session,
                        context,
                        hole,
                        target,
                        OutboundKind::TryCast,
                        actor_realm,
                    ),
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorReplaceWith(target, ..)) => {
                        let target = crate::wait::decode_address(target.0, target.1)?;
                        let table = session.data_con_table().clone();
                        crate::ResidentActorStart::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            context.placement.session,
                            context.actor,
                        )
                        .map(|candidate| ResidentActorBoundary::Replace { target, candidate })
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorDrainWith(target)) => {
                        Ok(ResidentActorBoundary::Drain {
                            continuation: hole,
                            target: crate::wait::decode_address(target.0, target.1)?,
                        })
                    }
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorWaitWith(
                        ..,
                    )) => {
                        let target =
                            crate::wait::decode_wait_target(&request, session.data_con_table())?;
                        Ok(ResidentActorBoundary::Wait(ResidentWaitRequest {
                            target,
                            continuation: hole,
                        }))
                    }
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorPollWith(
                        ..,
                    )) => {
                        let target =
                            crate::wait::decode_poll_target(&request, session.data_con_table())?;
                        Ok(ResidentActorBoundary::Poll(
                            crate::wait::ResidentPollRequest {
                                target,
                                continuation: hole,
                            },
                        ))
                    }
                    ResidentRequest::ActorLocal(crate::generated::actor_local::ActorLocalReq::ActorLocalContextWith) =>
                        Ok(ResidentActorBoundary::ActorLocalContext(hole)),
                    ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(site, _),
                    ) => capture_receiver_boundary(session, hole, site, actor_realm),
                    ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorCheckpointWith(site, _),
                    ) => {
                        let site = u64::try_from(site).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol("negative checkpoint site".into())
                        })?;
                        if session.parked_realm(&hole) != Some(actor_realm) {
                            return Err(ResidentActorWorkbenchError::ActorProtocol(
                                "checkpoint escaped its actor program".into(),
                            ));
                        }
                        let value = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            ?
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol(
                                "checkpoint carried no state".into(),
                            ))?;
                        Ok(ResidentActorBoundary::Checkpoint {
                            continuation: hole,
                            site,
                            value,
                        })
                    }
                    ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorLocalAttachProgressSourceWith(..)
                        | crate::generated::actor_local::ActorLocalReq::ActorLocalAttachSettlementSourceWith(..)
                        | crate::generated::actor_local::ActorLocalReq::ActorLocalAttachCommandSourceWith(..)
                        | crate::generated::actor_local::ActorLocalReq::ActorLocalAttachLifecycleSourceWith(..)
                    ) => unreachable!("attachment captured above"),
                    ResidentRequest::AgentTools(
                        crate::generated::agent_tools::AgentToolsReq::AgentToolsAwaitWith(
                            declarations,
                            synopsis,
                            initial_user_message,
                        ),
                    ) => {
                        let declarations = tidepool_runtime::value_to_json(
                            &declarations,
                            session.data_con_table(),
                            0,
                        );
                        let declarations = serde_json::from_value(declarations)
                            .map_err(ResidentActorWorkbenchError::ToolDeclarations)?;
                        Ok(ResidentActorBoundary::ToolAwait(
                            crate::resident_tools::ResidentToolAwait {
                                continuation: hole,
                                declarations,
                                synopsis,
                                initial_user_message,
                            },
                        ))
                    }
                    ResidentRequest::AgentTools(
                        crate::generated::agent_tools::AgentToolsReq::AgentToolsReplyWith(result),
                    ) => Ok(ResidentActorBoundary::ToolReply(
                        crate::resident_tools::ResidentToolReply {
                            continuation: hole,
                            result: serde_json::from_value(tidepool_runtime::value_to_json(
                                &result,
                                session.data_con_table(),
                                0,
                            )).map_err(|error| ResidentActorWorkbenchError::ActorProtocol(
                                format!("invalid actor tool dispatch reply: {error}")
                            ))?,
                        },
                    )),
                    ResidentRequest::AgentSession(
                        crate::generated::agent_session::AgentSessionReq::AgentSessionWith(..),
                    ) => {
                        let table = session.data_con_table().clone();
                        crate::ResidentInteractiveSession::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            actor_realm,
                        )
                        .map(ResidentActorBoundary::AgentSession)
                        .map_err(ResidentActorWorkbenchError::InteractiveSessionCapture)
                    }
                    ResidentRequest::Replies(RepliesReq::ReserveRequestWith(label, address, notify_owner)) => Ok(
                        ResidentActorBoundary::RequestReservation(RequestReservation {
                            continuation: hole,
                            target: crate::wait::decode_address(address.0, address.1)?,
                            label,
                            notify_owner,
                        }),
                    ),
                    ResidentRequest::Replies(RepliesReq::CurrentRequestWith(site)) => Ok(
                        ResidentActorBoundary::CurrentRequest {
                            continuation: hole,
                            site: u64::try_from(site).ok(),
                        }
                    ),
                    ResidentRequest::Replies(RepliesReq::SubmitRequestWith(
                        request_id,
                        _,
                        address,
                        deadline,
                    )) => {
                        let custody = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            ?
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "request submission suspended without its live payload".into(),
                                )
                            })?;
                        Ok(ResidentActorBoundary::RequestSubmission(
                            RequestSubmission {
                                continuation: hole,
                                request: crate::request_effect::request_id(request_id)?,
                                target: crate::wait::decode_address(address.0, address.1)?,
                                message: crate::MailboxValue::new(
                                    context.placement.session,
                                    custody,
                                ),
                                deadline: deadline
                                    .map(crate::request_effect::RequestDuration::checked)
                                    .transpose()
                                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?,
                            },
                        ))
                    }
                    ResidentRequest::Replies(RepliesReq::AttemptReplyWith(request_id, _, preview)) => {
                        let result = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            ?
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "reply suspended without its live result".into(),
                                )
                            })?;
                        Ok(ResidentActorBoundary::ReplyAttempt(ReplyAttempt {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            result,
                            recoverable: true,
                            preview: if preview.is_empty() { None } else { Some(preview) },
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::ReplyWith(request_id, _, preview)) => {
                        let result = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            ?
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "reply suspended without its live result".into(),
                                )
                            })?;
                        Ok(ResidentActorBoundary::ReplyAttempt(ReplyAttempt {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            result,
                            recoverable: false,
                            preview: if preview.is_empty() { None } else { Some(preview) },
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveResponseWith(request_id)) => {
                        Ok(ResidentActorBoundary::ResponsePoll(ResponsePoll {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::PublishProgressWith(request_id, _)) => {
                        // Request/watch custody can outlive the publishing actor.
                        // Arc<RootCustody> releases this shared-machine root when
                        // the last registry or watch snapshot stops retaining it.
                        let value = session.live_payload_handle_owned_by(hole.cont_id(), RealmId::ROOT)?
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("progress publication has no live payload".into()))?;
                        Ok(ResidentActorBoundary::ProgressPublication {
                            continuation: hole, request: crate::request_effect::request_id(request_id)?, value,
                        })
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveProgressWith(request_id)) => {
                        Ok(ResidentActorBoundary::ProgressPoll(ResponsePoll {
                            continuation: hole, request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::UpdateRequestWith(request, message)) => {
                        Ok(ResidentActorBoundary::RequestUpdate { continuation: hole,
                            request: crate::request_effect::request_id(request)?, message })
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveRequestUpdateWith(request, sequence)) => {
                        Ok(ResidentActorBoundary::RequestUpdatePoll { continuation: hole,
                            update: crate::RequestUpdateId { request: crate::request_effect::request_id(request)?,
                                sequence: u64::try_from(sequence).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid update sequence".into()))? } })
                    }
                    ResidentRequest::Replies(RepliesReq::DetachRequestWith(request_id)) => Ok(
                        ResidentActorBoundary::RequestDetachment {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        },
                    ),
                    ResidentRequest::Replies(RepliesReq::CancelRequestWith(request_id)) => Ok(
                        ResidentActorBoundary::RequestCancellation(RequestCancellation {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }),
                    ),
                    ResidentRequest::Replies(RepliesReq::AbandonResponseWith(request_id)) => Ok(
                        ResidentActorBoundary::ResponseAbandonment(ResponseAbandonment {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }),
                    ),
                    ResidentRequest::Replies(RepliesReq::ForgetResponseWith(request_id)) => {
                        Ok(ResidentActorBoundary::ResponseForget(ResponseForget {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveReplyWith(request_id)) => {
                        Ok(ResidentActorBoundary::ReplyPoll(ReplyPoll {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::AttemptAcknowledgeCancellationWith(
                        request_id,
                    )) => Ok(ResidentActorBoundary::CancellationAcknowledgement(
                        CancellationAcknowledgement {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            recoverable: true,
                        },
                    )),
                    ResidentRequest::Replies(RepliesReq::AcknowledgeCancellationWith(
                        request_id,
                    )) => Ok(ResidentActorBoundary::CancellationAcknowledgement(
                        CancellationAcknowledgement {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            recoverable: false,
                        },
                    )),
                    ResidentRequest::Watches(WatchesReq::RegisterRouteWith(label, callback, dependencies)) => {
                        drop(callback); // Custody is claimed from the suspension, not the decoded value.
                        let entry = session.live_payload_handle_owned_by(hole.cont_id(), context.placement.resource_scope)?
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("route has no retained callback".into()))?;
                        let dependencies = dependencies.into_iter().map(|dependency| {
                            Ok(vec![crate::request_effect::AwaitDependency::checked(dependency)?])
                        }).collect::<Result<Vec<Vec<_>>, tidepool_bridge::BridgeError>>()?;
                        Ok(ResidentActorBoundary::RouteRegistration {
                            registration: WatchRegistration { transient: false, continuation: hole, label, dependencies }, entry,
                        })
                    }
                    ResidentRequest::Watches(WatchesReq::RegisterRouteGroupsWith(label, callback, groups)) => {
                        drop(callback); // Custody is claimed from the suspension, not the decoded value.
                        let entry = session.live_payload_handle_owned_by(hole.cont_id(), context.placement.resource_scope)?
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("route has no retained callback".into()))?;
                        let dependencies = groups.into_iter().map(|dependencies| dependencies.into_iter().map(crate::request_effect::AwaitDependency::checked).collect()).collect::<Result<Vec<Vec<_>>, _>>()?;
                        Ok(ResidentActorBoundary::RouteRegistration {
                            registration: WatchRegistration { transient: false, continuation: hole, label, dependencies }, entry,
                        })
                    }
                    ResidentRequest::Watches(WatchesReq::ListRoutesWith) => Ok(ResidentActorBoundary::RouteList(hole)),
                    ResidentRequest::Watches(WatchesReq::ObserveRouteWith(id)) => Ok(ResidentActorBoundary::RoutePoll(WatchPoll {
                        continuation: hole, watch: crate::request_effect::watch_id(id)?,
                    })),
                    ResidentRequest::Watches(WatchesReq::RegisterWatchWith(
                        label,
                        dependencies,
                    )) => {
                        let dependencies = dependencies
                            .into_iter()
                            .map(|dependency| {
                                Ok(vec![crate::request_effect::AwaitDependency::checked(dependency)?])
                            })
                            .collect::<Result<Vec<Vec<_>>, tidepool_bridge::BridgeError>>()?;
                        Ok(ResidentActorBoundary::WatchRegistration(
                            WatchRegistration {
                                transient: false,
                                continuation: hole,
                                dependencies,
                                label,
                            },
                        ))
                    }
                    ResidentRequest::Watches(WatchesReq::RegisterWatchGroupsWith(label, groups)) => {
                        let dependencies = groups
                            .into_iter()
                            .map(|dependencies| dependencies.into_iter().map(crate::request_effect::AwaitDependency::checked).collect())
                            .collect::<Result<Vec<Vec<_>>, _>>()?;
                        Ok(ResidentActorBoundary::WatchRegistration(
                            WatchRegistration { transient: false, continuation: hole, dependencies, label },
                        ))
                    }
                    ResidentRequest::Watches(WatchesReq::RegisterAwaitWith(groups)) => {
                        let dependencies = groups
                            .into_iter()
                            .map(|dependencies| dependencies.into_iter().map(crate::request_effect::AwaitDependency::checked).collect())
                            .collect::<Result<Vec<Vec<_>>, _>>()?;
                        Ok(ResidentActorBoundary::WatchRegistration(
                            WatchRegistration { transient: true, continuation: hole, dependencies, label: "wait-for".into() },
                        ))
                    }
                    ResidentRequest::Watches(WatchesReq::ObserveWatchWith(watch_id)) => {
                        Ok(ResidentActorBoundary::WatchPoll(WatchPoll {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch_id)?,
                        }))
                    }
                    ResidentRequest::Watches(WatchesReq::AwaitWatchWith(watch_id)) => {
                        Ok(ResidentActorBoundary::WatchAwait(WatchPoll {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch_id)?,
                        }))
                    }
                    ResidentRequest::Watches(WatchesReq::ObserveWatchProgressWith(watch, request, after)) => {
                        Ok(ResidentActorBoundary::WatchProgressPoll {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch)?,
                            request: crate::request_effect::request_id(request)?,
                            after: u64::try_from(after).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("negative progress cursor".into()))?,
                        })
                    }
                    ResidentRequest::Watches(WatchesReq::ObserveCommandWith(job)) => {
                        Ok(ResidentActorBoundary::CommandReportPoll { continuation: hole, job })
                    }
                    ResidentRequest::Watches(WatchesReq::ForgetWatchWith(watch_id)) => {
                        Ok(ResidentActorBoundary::WatchForget(WatchForget {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch_id)?,
                        }))
                    }
                    ResidentRequest::AgentSession(
                        crate::generated::agent_session::AgentSessionReq::AgentAttachWith(
                            initial_user_message,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentAttachment(
                        ResidentAgentAttachment {
                            continuation: hole,
                            initial_user_message,
                        },
                    )),
                    invalid => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "installed actor program suspended on phase-invalid `{}`",
                        invalid.operation()
                    ))),
                }
            })
            .await
    }

    /// Claim and seal the live child entry carried by one public `startActor`
    /// suspension. The same checked-out operation derives its exact source
    /// facade, mints an isolated lexical scope, and rehomes the entry into the
    /// unpublished child's fresh runtime resource scope.
    pub async fn capture_start(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
    ) -> Result<crate::ResidentActorStart, ResidentActorWorkbenchError> {
        let actor_realm = context.placement.resource_scope;
        let boundary = self.capture_boundary(context, outcome, actor_realm).await?;
        match boundary {
            ResidentActorBoundary::Start(start) => Ok(start),
            other => Err(unexpected_boundary("startActor", &other)),
        }
    }

    /// Consume the original installed root only after its actor's durable release.
    pub(crate) async fn run_startup_entry(
        &self,
        context: crate::ActorSessionContext,
        entry: tidepool_runtime::session::PreparedStartupEntry,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .run_startup_entry(entry)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub async fn run_rooted_entry(
        &self,
        context: crate::ActorSessionContext,
        entry: RootCustody,
        realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _context, _| {
                session
                    .run_rooted_entry("actor_program", entry, 0, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    /// Admit the original child entry only while its retained lexical grant
    /// still belongs to this runtime epoch and exact installed scope.
    pub(crate) async fn run_fork_child_rooted_entry(
        &self,
        context: crate::ActorSessionContext,
        entry: RootCustody,
        realm: RealmId,
        lease: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                session.validate_lexical_scope_lease(context.placement.lexical_scope, &lease)?;
                session
                    .run_rooted_entry("actor_program", entry, 0, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn capture_startup_step(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<ResidentActorStartupStep, ResidentActorWorkbenchError> {
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "actor startup completed without reaching readiness".into(),
            ));
        };
        self.access
            .with_machine(context, move |session, _, _| {
                let request_kind = ResidentRequest::decode(&request, session.data_con_table())?;
                use crate::request::sources::{SourceTarget, RequestSourceKind};
                let source = match &request_kind {
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(request, _)) => Some(SourceTarget::Request(source_request_id(*request)?, RequestSourceKind::Progress)),
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(request, _)) => Some(SourceTarget::Request(source_request_id(*request)?, RequestSourceKind::Settlement)),
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(job, _)) => Some(SourceTarget::Command(uuid::Uuid::parse_str(job).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid command job handle".into()))?.as_u128())),
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith((id, incarnation), _)) => Some(SourceTarget::Lifecycle(crate::ActorRef {
                        id: crate::ActorId(u64::try_from(*id).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid lifecycle actor id".into()))?),
                        incarnation: crate::Incarnation(u64::try_from(*incarnation).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid lifecycle incarnation".into()))?),
                    })),
                    _ => None,
                };
                if let Some(target) = source {
                    if session.parked_realm(&hole) != Some(actor_realm) {
                        return Err(ResidentActorWorkbenchError::ActorProtocol("source installation crossed actor realm".into()));
                    }
                    let entry = session.live_payload_handle_owned_by(hole.cont_id(), actor_realm)?.ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("source has no live mapping closure".into()))?;
                    return Ok(ResidentActorStartupStep::InstallSource {
                        continuation: hole,
                        source: crate::request::sources::SourceBinding { target, entry: Arc::new(entry) },
                    });
                }
                match request_kind {
                    ResidentRequest::ActorKernel(
                        crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(
                            ..,
                        ),
                    ) if session.parked_realm(&hole) == Some(actor_realm) => {
                        let hook = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            ?
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "shutdown registration carried no live hook".into(),
                                )
                            })?;
                        Ok(ResidentActorStartupStep::InstallShutdown(
                            ResidentActorShutdown {
                                continuation: hole,
                                hook,
                            },
                        ))
                    }
                    ResidentRequest::ActorKernel(
                        crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith,
                    ) if session.parked_realm(&hole) == Some(actor_realm) => {
                        Ok(ResidentActorStartupStep::Ready(ResidentActorReadiness {
                            hole,
                        }))
                    }
                    ResidentRequest::AgentSession(
                        crate::generated::agent_session::AgentSessionReq::AgentAttachWith(
                            initial_user_message,
                        ),
                    ) if session.parked_realm(&hole) == Some(actor_realm) => {
                        Ok(ResidentActorStartupStep::Attach(ResidentAgentAttachment {
                            continuation: hole,
                            initial_user_message,
                        }))
                    }
                    unsupported => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "actor initialization suspended on unsupported `{}` in {actor_realm:?}",
                        unsupported.operation()
                    ))),
                }
            })
            .await
    }

    pub(crate) async fn run_shutdown(
        &self,
        context: crate::ActorSessionContext,
        hook: RootCustody,
        realm: RealmId,
        reason: crate::ActorExitKind,
        admission_timeout: Duration,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let argument = match reason {
            crate::ActorExitKind::Completed => 0,
            crate::ActorExitKind::Failed => 1,
            crate::ActorExitKind::Cancelled => 2,
        };
        let mut outcome = self
            .access
            .with_machine_wait(
                context.clone(),
                Some(admission_timeout),
                move |session, _context, _| {
                    session
                        .run_rooted_entry("actor_shutdown", hook, argument, realm, None)
                        .map_err(ResidentActorWorkbenchError::Resident)
                },
            )
            .await?;
        loop {
            match outcome {
                ResidentOutcome::Completed { .. } => return Ok(()),
                ResidentOutcome::Deferred { hole, work, .. } => {
                    outcome = self.run_external(context.clone(), hole, work).await?;
                }
                ResidentOutcome::BindingsCommitted { .. } => {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "shutdown completed as an impossible projected binding".into(),
                    ));
                }
                ResidentOutcome::Suspended { hole, request, .. } => {
                    return self
                        .access
                        .with_machine(context, move |session, _, _| {
                            let operation =
                                ResidentRequest::decode(&request, session.data_con_table())?
                                    .operation()
                                    .to_owned();
                            let reason = format!("shutdown suspended on disallowed `{operation}`");
                            let _ = session.abort(hole.cont_id(), reason.clone());
                            Err(ResidentActorWorkbenchError::ActorProtocol(reason))
                        })
                        .await;
                }
            }
        }
    }

    pub(crate) async fn resume_readiness(
        &self,
        context: crate::ActorSessionContext,
        readiness: ResidentActorReadiness,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(readiness.hole, ())
                    .map_err(classify_resumption)
            })
            .await
    }

    /// A bounded, non-forcing text preview of `value`'s shape --
    /// `ResidentSession::render_retained_preview` -- returned alongside
    /// `value` itself, unconsumed, so the caller can still deliver it
    /// wherever it was headed (a settlement notice's `Reply:` preview must
    /// never cost the reply it is describing). `None` when the value cannot
    /// be read this way rather than failing the call.
    pub(crate) async fn preview_retained(
        &self,
        context: crate::ActorSessionContext,
        value: RootCustody,
        char_budget: usize,
    ) -> Result<(RootCustody, Option<String>), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let preview = session.render_retained_preview(&value, char_budget);
                Ok((value, preview))
            })
            .await
    }

    pub(crate) async fn run_rooted_application(
        &self,
        context: crate::ActorSessionContext,
        handler: RootCustody,
        request: Arc<RootCustody>,
        handler_realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _context, _| {
                session
                    .run_rooted_application(
                        "actor_application",
                        &handler,
                        &request,
                        handler_realm,
                        None,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn map_source(
        &self,
        context: crate::ActorSessionContext,
        entry: Arc<RootCustody>,
        event: crate::request::sources::SourceEvent,
    ) -> Result<crate::MailboxValue, ResidentActorWorkbenchError> {
        use crate::generated::actor_kernel::ActorKernelReq;
        use crate::request::sources::SourceEvent;
        let realm = RealmId::fresh();
        let (hole, input) = self
            .access
            .with_machine(context.clone(), move |session, _, _| {
                let outcome = session
                    .run_rooted_entry_borrowed("actor_source", &entry, 0, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "source mapper did not request its input".into(),
                    ));
                };
                if session.parked_realm(&hole) != Some(realm) {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "source mapper crossed an unexpected boundary".into(),
                    ));
                }
                let input = ResidentRequest::decode(&request, session.data_con_table())?;
                Ok((hole, input))
            })
            .await?;
        // Each source kind's mapper asks for its input with the request whose
        // declared reply type is the event (`Tidepool.Actor.Source.sourceEntry`):
        // the host-built answer below is validated against that type, so a
        // mapper of another kind cannot be fed this event.
        let input_matches_event = match &event {
            SourceEvent::Command(_) => matches!(
                input,
                ResidentRequest::ActorKernel(ActorKernelReq::ActorCommandInputWith)
            ),
            SourceEvent::Lifecycle(_) => matches!(
                input,
                ResidentRequest::ActorKernel(ActorKernelReq::ActorLifecycleInputWith)
            ),
            SourceEvent::Progress(_)
            | SourceEvent::ProgressClosed
            | SourceEvent::ProgressRejected(_) => matches!(
                input,
                ResidentRequest::Replies(RepliesReq::ObserveProgressWith(_))
            ),
            SourceEvent::Settled(_) => matches!(
                input,
                ResidentRequest::Replies(RepliesReq::ObserveResponseWith(_))
            ),
        };
        if !input_matches_event {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "source mapper requested the input of another source kind".into(),
            ));
        }
        let outcome = match event {
            SourceEvent::Command(event) => self.resume_value(context.clone(), hole, event).await?,
            SourceEvent::Lifecycle(event) => {
                self.access
                    .with_machine(context.clone(), move |session, _, _| {
                        let value = match event {
                            crate::ActorLifecycle::Live => LifecycleAnswer::Live,
                            crate::ActorLifecycle::Paused(detail) => {
                                LifecycleAnswer::Paused(detail)
                            }
                            crate::ActorLifecycle::Exited(terminal) => {
                                LifecycleAnswer::Exited(terminal)
                            }
                        };
                        session
                            .resume_classified(hole, value)
                            .map_err(classify_resumption)
                    })
                    .await?
            }
            SourceEvent::Progress(snapshot) => {
                self.resume_progress_observation(context.clone(), hole, Ok((Some(snapshot), false)))
                    .await?
            }
            SourceEvent::ProgressClosed => {
                self.resume_progress_observation(context.clone(), hole, Ok((None, true)))
                    .await?
            }
            SourceEvent::ProgressRejected(error) => {
                self.resume_progress_observation(context.clone(), hole, Err(error))
                    .await?
            }
            SourceEvent::Settled(result) => {
                self.resume_response_observation(
                    context.clone(),
                    hole,
                    Ok(match result {
                        Ok(()) => crate::ResponseObservation::Ready,
                        Err(failure) => crate::ResponseObservation::Unavailable(failure),
                    }),
                )
                .await?
            }
        };
        let message = self
            .capture_kernel_value(
                context.clone(),
                outcome,
                ResidentKernelBoundary::Reply,
                0,
                realm,
                context.placement.resource_scope,
            )
            .await?;
        let finished = self
            .resume_unit(context.clone(), message.continuation)
            .await?;
        if !matches!(finished, ResidentOutcome::Completed { .. }) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "source mapper continued after returning its message".into(),
            ));
        }
        self.close_realm(context.clone(), realm).await?;
        Ok(crate::MailboxValue::new(
            context.placement.session,
            message.value,
        ))
    }

    pub(crate) async fn capture_kernel_value(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        expected: ResidentKernelBoundary,
        expected_site: u64,
        handler_realm: RealmId,
        actor_realm: RealmId,
    ) -> Result<KernelValue, ResidentActorWorkbenchError> {
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "mailbox handler completed before `{}`",
                expected.operation()
            )));
        };
        self.access
            .with_machine(context, move |session, _, _| {
                if session.parked_realm(&hole) != Some(handler_realm) {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "`{}` escaped mailbox handler realm {handler_realm:?}",
                        expected.operation()
                    )));
                }
                let decoded = crate::generated::actor_kernel::ActorKernelReq::from_value(
                    &request,
                    session.data_con_table(),
                )?;
                let (actual, site) = match decoded {
                    crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(
                        ..,
                    ) => {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "expected `{}`, got `installShutdown`",
                            expected.operation()
                        )));
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorReplyWith(site, _) => {
                        (ResidentKernelBoundary::Reply, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorContinueWith(site, _) => {
                        (ResidentKernelBoundary::Continue, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith => {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "expected `{}`, got `ready`",
                            expected.operation()
                        )));
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorLifecycleInputWith
                    | crate::generated::actor_kernel::ActorKernelReq::ActorCommandInputWith => {
                        return Err(ResidentActorWorkbenchError::ActorProtocol("source boundary escaped its mapping or installation".into()));
                    }
                };
                let site = u64::try_from(site).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "`{}` carried invalid site id {site}",
                        actual.operation()
                    ))
                })?;
                if actual != expected || site != expected_site {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "expected `{}` at site {expected_site}, got `{}` at site {site}",
                        expected.operation(),
                        actual.operation()
                    )));
                }
                let value = session
                    .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                    ?
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "`{}` suspended without its live value",
                            actual.operation()
                        ))
                    })?;
                Ok(KernelValue {
                    continuation: hole,
                    value,
                })
            })
            .await
    }

    pub(crate) async fn kernel_boundary(
        &self,
        context: crate::ActorSessionContext,
        outcome: &ResidentOutcome,
    ) -> Result<Option<(ResidentKernelBoundary, u64)>, ResidentActorWorkbenchError> {
        let ResidentOutcome::Suspended { request, .. } = outcome else {
            return Ok(None);
        };
        let request = request.clone();
        self.access
            .with_machine(context, move |session, _, _| {
                let decoded = match crate::generated::actor_kernel::ActorKernelReq::from_value(
                    &request,
                    session.data_con_table(),
                ) {
                    Ok(decoded) => decoded,
                    Err(BridgeError::UnknownDataCon(_)) => return Ok(None),
                    Err(source) => return Err(source.into()),
                };
                let (kind, site) = match decoded {
                    crate::generated::actor_kernel::ActorKernelReq::ActorReplyWith(site, _) => {
                        (ResidentKernelBoundary::Reply, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorContinueWith(site, _) => {
                        (ResidentKernelBoundary::Continue, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(
                        ..,
                    )
                    | crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorLifecycleInputWith
                    | crate::generated::actor_kernel::ActorKernelReq::ActorCommandInputWith => {
                        return Ok(None)
                    }
                };
                let site = u64::try_from(site).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "`{}` carried invalid site id {site}",
                        kind.operation()
                    ))
                })?;
                Ok(Some((kind, site)))
            })
            .await
    }

    pub(crate) async fn resume_unit(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, ())
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn abort_live(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        reason: String,
    ) -> (Result<ResidentOutcome, ResidentActorWorkbenchError>, bool) {
        let result = self
            .access
            .with_machine(context, move |session, _, _| {
                let result = session.abort(hole.cont_id(), reason);
                let consumed = !session.parked_holes().contains(&hole.cont_id());
                Ok((result, consumed))
            })
            .await;
        match result {
            Ok((result, consumed)) => (
                result.map_err(ResidentActorWorkbenchError::Resident),
                consumed,
            ),
            Err(error) => (Err(error), false),
        }
    }

    pub(crate) async fn resume_int(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        value: u64,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let value = i64::try_from(value).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "runtime identity exceeds Haskell Int".into(),
                    )
                })?;
                session
                    .resume_classified(hole, value)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_actor_context(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        descriptor: crate::ActorDescriptor,
        bound_worktree: Option<String>,
        runtime: crate::ActorRuntimeObservation,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let answer = ActorContextProjection {
            context: context.clone(),
            descriptor,
            bound_worktree,
            runtime,
        };
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_agent_roster(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        roster: Vec<AgentRosterProjection>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, roster)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_group_roster(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        roster: Option<Vec<AgentRosterProjection>>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, roster)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_agent_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Option<AgentRosterProjection>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, observation)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_agent_forget(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: AgentForgetProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, outcome)
                    .map_err(classify_resumption)
            })
            .await
    }

    /// Run owned external work with no machine checkout held. The caller owns
    /// this operation until settlement; abandoning its reply does not cancel it.
    pub(crate) async fn run_external(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        work: tidepool_effect::DeferredEffect,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let guard = ParkedHoleAbortGuard::new(
            &self.access,
            context.clone(),
            hole.cont_id().to_string(),
            "external operation lost its continuation owner".into(),
        );
        let registration = guard.registration();
        let result = match work {
            tidepool_effect::DeferredEffect::Blocking(work) => {
                let work = work
                    .into_inner()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match tidepool_runtime::spawn_blocking_in_span(work).await {
                    Ok(result) => result,
                    Err(error) => Err(tidepool_effect::EffectError::Handler(format!(
                        "external operation terminated without a confirmed result: {error}"
                    ))),
                }
            }
            tidepool_effect::DeferredEffect::Async(work) => {
                work.into_inner()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .await
            }
        };
        let settled = self
            .access
            .with_machine(context, move |session, _, _| {
                let outcome = match result {
                    Ok(response) => session
                        .resume_response_classified(hole, response)
                        .map_err(classify_resumption),
                    Err(error) => session
                        .abort(hole.cont_id(), error.to_string())
                        .map_err(ResidentActorWorkbenchError::Resident),
                }?;
                registration.replace_in_checkout(session, &outcome);
                Ok(outcome)
            })
            .await;
        if settled.is_ok() {
            // The returned outcome now belongs to the caller, which either
            // settles it or establishes its own continuation owner.
            guard.disarm();
        }
        settled
    }

    pub(crate) async fn resume_value<T: ToHaskell + Send + 'static>(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: T,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, outcome)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_agent_stop(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: AgentStopProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, outcome)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_cleanup_plan(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        plan: CleanupPlanProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, plan)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_cleanup_receipt(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        receipt: CleanupReceiptProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, receipt)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_fork_group(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        group: crate::ForkGroupId,
        group_path: String,
        paths: Vec<String>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let group = i64::try_from(group.0).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "fork group identity exceeds Haskell Int".into(),
                    )
                })?;
                session
                    .resume_classified(hole, Ok::<_, String>((group, group_path, paths)))
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_reply_rejection(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        error: crate::ReplyError,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::ReplyResult::<()>(Err(error));
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_current_request(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        access_site: Option<u64>,
        current: Option<(
            crate::RequestId,
            Arc<tidepool_runtime::session::SiteTypeEvidence>,
            tidepool_codegen::scope::ScopeId,
            tidepool_repr::SessionVarId,
        )>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let refusal = match (access_site, current) {
                    (Some(access_site), Some((request, request_types, scope, input_binding))) => {
                        if !session.request_scope_types_match(&request_types, access_site) {
                            crate::request_effect::RequestScopeRefusal::RequestTypeMismatch
                        } else {
                            let constructor = tidepool_bridge::get_qualified(
                                session.data_con_table(),
                                "Tidepool.Agent.Reply.Internal.RequestActive",
                                2,
                            )
                            .ok_or_else(|| {
                                BridgeError::UnknownDataConName(
                                    "Tidepool.Agent.Reply.Internal.RequestActive".into(),
                                )
                            })?;
                            let request = i64::try_from(request.0).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "current request identity exceeds Haskell Int".into(),
                                )
                            })?;
                            if let Some(outcome) = session
                                .resume_framed_binding_sources_classified(
                                    &hole,
                                    scope,
                                    "sessionInput",
                                    input_binding,
                                    constructor,
                                    vec![request],
                                )
                                .map_err(classify_resumption)?
                            {
                                return Ok(outcome);
                            }
                            crate::request_effect::RequestScopeRefusal::RequestInputShadowed
                        }
                    }
                    (None, Some(_)) => {
                        crate::request_effect::RequestScopeRefusal::RequestTypeMismatch
                    }
                    (_, None) => crate::request_effect::RequestScopeRefusal::NoCurrentRequest,
                };
                session
                    .resume_classified(hole, refusal)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_response_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::ResponseObservation, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::RequestAnswer::Response(observation);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_request_update<T: ToHaskell + Send + 'static>(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<T, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, crate::request_effect::ReplyResult(outcome))
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_progress_publication(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<u64, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(
                        hole,
                        crate::request_effect::ReplyResult(outcome.map(|_| ())),
                    )
                    .map_err(classify_resumption)
            })
            .await
    }

    /// Borrow-export a value from `from`'s machine -- never consumed, so the
    /// source root stays live for a later caller to export again -- and mint
    /// a brand new [`RootCustody`] importing it into `to`'s machine, owned
    /// by `owner` there. For a value more than one destination machine may
    /// need to read independently, such as a request's published progress
    /// snapshot: see [`Self::resume_progress_observation`]. Callers with
    /// `from == to` must not call this -- there is no "import my own root
    /// back into myself" operation, and none is needed.
    pub(crate) async fn import_shared_custody(
        &self,
        custody: Arc<RootCustody>,
        from: tidepool_repr::SessionId,
        to: tidepool_repr::SessionId,
        owner: RealmId,
    ) -> Result<RootCustody, ResidentActorWorkbenchError> {
        let parcel = self
            .access
            .with_host_machine("progress-export-shared", from, None, move |session, _| {
                session
                    .export_shared(&custody)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await?;
        tracing::info!(
            from = ?from,
            to = ?to,
            parcel_bytes = parcel.bytes(),
            "resident shared value exported across a session boundary"
        );
        self.access
            .with_host_machine("progress-import-shared", to, None, move |session, _| {
                session
                    .import_parcel(parcel, owner)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_progress_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<(Option<crate::request::ProgressSnapshot>, bool), crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        // A snapshot rooted on another session must be imported into this
        // observer's own machine BEFORE the resume checkout below -- the
        // borrowed export leaves the publishing session's root untouched,
        // so a later observer (or a retry of this one) can still export it.
        // Same-session (today's only reachable case) skips this entirely.
        let observation = match observation {
            Ok((Some(snapshot), closed)) if snapshot.session != context.placement.session => {
                let imported = self
                    .import_shared_custody(
                        Arc::clone(&snapshot.value),
                        snapshot.session,
                        context.placement.session,
                        context.placement.resource_scope,
                    )
                    .await?;
                Ok((
                    Some(crate::request::ProgressSnapshot {
                        revision: snapshot.revision,
                        value: Arc::new(imported),
                        session: context.placement.session,
                    }),
                    closed,
                ))
            }
            other => other,
        };
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let answer = match observation {
                    Ok((Some(snapshot), _)) => {
                        let constructor = tidepool_bridge::get_qualified(
                            table,
                            "Tidepool.Agent.Reply.Internal.ProgressUpdate",
                            2,
                        )
                        .ok_or_else(|| {
                            BridgeError::UnknownDataConName(
                                "Tidepool.Agent.Reply.Internal.ProgressUpdate".into(),
                            )
                        })?;
                        let revision = i64::try_from(snapshot.revision).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "progress revision exceeds Haskell Int".into(),
                            )
                        })?;
                        let prefix = vec![revision];
                        return session
                            .resume_framed_custody_sources_classified(
                                hole,
                                &snapshot.value,
                                constructor,
                                prefix,
                            )
                            .map_err(classify_resumption);
                    }
                    Ok((None, true)) => ProgressAnswer::Closed,
                    Ok((None, false)) => ProgressAnswer::Pending,
                    Err(error) => ProgressAnswer::Rejected(error),
                };
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_cancel_request(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::CancelRequestOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::RequestAnswer::Cancel(outcome);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_abandonment(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::AbandonResponseOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::RequestAnswer::Abandon(outcome);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_response_forget(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::ForgetResponseOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::RequestAnswer::ForgetResponse(outcome);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_reply_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::ReplyObservation, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::RequestAnswer::Reply(observation);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn abandon_cast_handler(
        &self,
        context: crate::ActorSessionContext,
        receiver_continuation: ResidentHole,
        handler_realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let outcome = session
                    .resume_classified(receiver_continuation, true)
                    .map_err(classify_resumption)?;
                let _ = session.close_realm(handler_realm);
                Ok(outcome)
            })
            .await
    }

    pub(crate) async fn resume_watch_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::WatchObservation, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::RequestAnswer::Watch(observation);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_route_state(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::request::routes::RouteState, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, RouteStateAnswer(observation))
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn run_route_entry(
        &self,
        context: crate::ActorSessionContext,
        entry: RootCustody,
        watch: crate::WatchId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let argument = i64::try_from(watch.0).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "route ID exceeds Haskell Int".into(),
                    )
                })?;
                session
                    .run_rooted_entry(
                        "watch_route",
                        entry,
                        argument,
                        context.placement.resource_scope,
                        None,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_watch_forget(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::ForgetWatchOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::RequestAnswer::ForgetWatch(outcome);
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_tool_invocation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, (name, arguments))
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_live(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        value: RootCustody,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_handle_classified(hole, value)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_terminal(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        terminal: crate::ActorTerminal,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, terminal)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_call_status(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        failure: Option<String>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, CallStatus(failure))
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_optional_terminal(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        terminal: Option<crate::ActorTerminal>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, terminal)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn rehome_mailbox_value(
        &self,
        context: crate::ActorSessionContext,
        value: crate::MailboxValue,
        owner: RealmId,
    ) -> Result<crate::MailboxValue, ResidentActorWorkbenchError> {
        let session_id = value.session();
        let custody = value.into_custody();
        self.access
            .with_machine(context, move |session, _, _| {
                let custody = session
                    .rehome_custody(custody, owner)
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                Ok(crate::MailboxValue::new(session_id, custody))
            })
            .await
    }

    /// Move a rooted value from one resident session's machine to another,
    /// owned by `owner` on the destination. Decomposes through
    /// [`crate::MailboxValue::into_transfer`] first -- a `Runtime` custody
    /// still lives in its own recorded session and is handled exactly as
    /// before (same-session is [`Self::rehome_mailbox_value`], kept for its
    /// `context`-driven actor execution setup so that path is provably
    /// unchanged; cross-session goes through [`Self::transfer_custody`]); a
    /// `Parcel` has no machine affinity of its own (see
    /// `crate::mailbox`'s module doc) and is always imported straight into
    /// `to`, regardless of its nominal origin tag -- never routed through
    /// [`crate::MailboxValue::into_custody`], which panics on that variant.
    pub(crate) async fn transfer_mailbox_value(
        &self,
        context: crate::ActorSessionContext,
        value: crate::MailboxValue,
        to: tidepool_repr::SessionId,
        owner: RealmId,
    ) -> Result<crate::MailboxValue, ResidentActorWorkbenchError> {
        match value.into_transfer() {
            crate::mailbox::MailboxTransfer::Runtime {
                session: from,
                custody,
            } => {
                if from == to {
                    return self
                        .rehome_mailbox_value(
                            context,
                            crate::MailboxValue::new(from, custody),
                            owner,
                        )
                        .await;
                }
                let custody = self.transfer_custody(custody, from, to, owner).await?;
                Ok(crate::MailboxValue::new(to, custody))
            }
            crate::mailbox::MailboxTransfer::Parcel(parcel) => {
                tracing::info!(
                    to = ?to,
                    parcel_bytes = parcel.bytes(),
                    "resident parcel value imported across a session boundary"
                );
                let custody = self
                    .access
                    .with_host_machine("transfer-import-parcel", to, None, move |session, _| {
                        session
                            .import_parcel(parcel, owner)
                            .map_err(ResidentActorWorkbenchError::Resident)
                    })
                    .await?;
                Ok(crate::MailboxValue::new(to, custody))
            }
            #[cfg(test)]
            crate::mailbox::MailboxTransfer::ProbeRuntime { .. }
            | crate::mailbox::MailboxTransfer::ProbeParcel(_) => {
                unreachable!("test probes never reach the resident workbench's own transfer path")
            }
        }
    }

    /// Move a rooted [`RootCustody`] from `from`'s resident session machine
    /// to `to`'s, owned by `owner` on the destination. Same-session moves
    /// straight through [`ResidentSession::rehome_custody`] under one
    /// checkout of that session; cross-session evacuates via
    /// [`ResidentSession::export_custody`]/[`ResidentSession::import_parcel`]
    /// (see `tidepool_runtime::session::resident`), each under its own
    /// checkout -- the two machines never share a checkout, and the source
    /// machine is released before the destination one is touched.
    pub(crate) async fn transfer_custody(
        &self,
        custody: RootCustody,
        from: tidepool_repr::SessionId,
        to: tidepool_repr::SessionId,
        owner: RealmId,
    ) -> Result<RootCustody, ResidentActorWorkbenchError> {
        if from == to {
            return self
                .access
                .with_host_machine("transfer", from, None, move |session, _| {
                    session
                        .rehome_custody(custody, owner)
                        .map_err(ResidentActorWorkbenchError::Resident)
                })
                .await;
        }
        let parcel = self
            .access
            .with_host_machine("transfer-export", from, None, move |session, _| {
                session
                    .export_custody(custody)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await?;
        tracing::info!(
            from = ?from,
            to = ?to,
            parcel_bytes = parcel.bytes(),
            "resident value transferred across a session boundary"
        );
        self.access
            .with_host_machine("transfer-import", to, None, move |session, _| {
                session
                    .import_parcel(parcel, owner)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn prepare_root_program(
        &self,
        session_id: tidepool_repr::SessionId,
        compiled: Arc<tidepool_runtime::session::CompiledTurn>,
    ) -> Result<(crate::ActorPlacement, ResidentOutcome), ResidentActorWorkbenchError> {
        self.access
            .with_host_machine("root", session_id, None, move |session, _| {
                let placement = crate::ActorPlacement {
                    session: session_id,
                    resource_scope: RealmId::fresh(),
                    lexical_scope: session.mint_isolated_scope(),
                };
                let prepared = (|| {
                    session
                        .set_actor_execution(
                            tidepool_runtime::session::SessionRunContext {
                                resource_scope: placement.resource_scope,
                                lexical_scope: placement.lexical_scope,
                                ..tidepool_runtime::session::SessionRunContext::ROOT
                            },
                            tidepool_effect::EffectRunPolicy::HandleOrSuspend,
                            tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE,
                        )
                        .map_err(ResidentActorWorkbenchError::Resident)?;
                    let outcome = session
                        .run_with_sites("forest-root", compiled.code())
                        .map_err(ResidentActorWorkbenchError::Resident)?;
                    Ok::<_, ResidentActorWorkbenchError>(outcome)
                })();
                let outcome = match prepared {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        session.close_realm(placement.resource_scope);
                        session.retire_scope(placement.lexical_scope);
                        return Err(error);
                    }
                };
                Ok((placement, outcome))
            })
            .await
    }

    pub(crate) async fn retire_root_placement(
        &self,
        placement: crate::ActorPlacement,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.retire_root_placement_wait(placement, None).await
    }

    /// Bound realm/placement checkout by a caller-computed deadline instead
    /// of waiting unbounded on a busy machine. `max_wait` uses `checkout_wait`
    /// (a `WaitTimeout` becomes `Unconfirmed` cleanup); `None` keeps the
    /// unbounded `checkout_queued` other callers rely on.
    pub(crate) async fn retire_root_placement_wait(
        &self,
        placement: crate::ActorPlacement,
        max_wait: impl Into<Option<Duration>>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        use crate::ActorRunTarget;
        self.access
            .with_host_machine(
                "root",
                placement.session,
                max_wait.into(),
                move |session, _| {
                    let _ =
                        session.retire_placement(placement.resource_scope, placement.lexical_scope);
                    Ok(())
                },
            )
            .await
    }

    pub(crate) async fn provision_root_scope(
        &self,
        session_id: tidepool_repr::SessionId,
    ) -> Result<crate::ActorPlacement, ResidentActorWorkbenchError> {
        self.access
            .with_host_machine("root", session_id, None, move |session, _| {
                Ok(crate::ActorPlacement {
                    session: session_id,
                    resource_scope: RealmId::fresh(),
                    lexical_scope: session.mint_isolated_scope(),
                })
            })
            .await
    }

    /// Mint a fresh, isolated lexical scope on `session_id`'s own scope
    /// forest, alone — for a launch `child_session_eligibility` marked
    /// eligible (so `capture_decoded` minted it no real scope, only a
    /// placeholder) but whose host offers no dedicated-machine primitive
    /// (`Self::supports_child_sessions` false): it falls back to running on
    /// the launching session, which still needs an actual scope of its own,
    /// same as any other launch there.
    pub(crate) async fn mint_lexical_scope(
        &self,
        session_id: tidepool_repr::SessionId,
    ) -> Result<tidepool_codegen::scope::ScopeId, ResidentActorWorkbenchError> {
        self.access
            .with_host_machine("mint-lexical-scope", session_id, None, move |session, _| {
                Ok(session.mint_isolated_scope())
            })
            .await
    }

    /// Retain the committed fork's exact lexical surface before replacing
    /// its provisional child placement. The lease's detached scope is the
    /// mutable child target; token release cannot retire it prematurely.
    pub(crate) async fn retain_fork_release_scope(
        &self,
        session_id: tidepool_repr::SessionId,
        scope: ScopeId,
    ) -> Result<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>, ResidentActorWorkbenchError>
    {
        self.access
            .with_host_machine(
                "retain-fork-release-scope",
                session_id,
                None,
                move |session, _| session.retain_lexical_scope(scope).map_err(Into::into),
            )
            .await
    }

    pub(crate) async fn close_realm(
        &self,
        context: crate::ActorSessionContext,
        realm: RealmId,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.close_realm_wait(context, realm, None).await
    }

    /// Same bounded-checkout rationale as [`Self::retire_root_placement_wait`].
    pub(crate) async fn close_realm_wait(
        &self,
        context: crate::ActorSessionContext,
        realm: RealmId,
        max_wait: impl Into<Option<Duration>>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine_wait(
                context,
                MachineCheckoutAdmission::Wait(max_wait.into()),
                move |session, _, _| {
                    let _ = session.close_realm(realm);
                    Ok(())
                },
            )
            .await
    }

    pub async fn resume_starting_parent(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        actor: crate::ActorRef,
        allocated_label: String,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(
                        hole,
                        (
                            actor.id.0 as i64,
                            actor.incarnation.0 as i64,
                            allocated_label,
                        ),
                    )
                    .map_err(classify_resumption)
            })
            .await
    }

    pub async fn resume_fork_starting_parent(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        actor: crate::ActorRef,
        allocated_label: String,
        worktree: tidepool_bridge_effects::WtWorktreeHandle,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(
                        hole,
                        Ok::<_, String>((
                            (
                                actor.id.0 as i64,
                                actor.incarnation.0 as i64,
                                allocated_label,
                            ),
                            worktree,
                        )),
                    )
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_fork_failure(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        detail: String,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, Err::<(), _>(detail))
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_fork_unit(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, Ok::<(), String>(()))
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn resume_fork_cleanup(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::ForkGroupCleanupOutcome, crate::ForkGroupError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_classified(hole, ForkCleanupAnswer(outcome))
                    .map_err(classify_resumption)
            })
            .await
    }
}

fn source_request_id(value: i64) -> Result<crate::RequestId, ResidentActorWorkbenchError> {
    u64::try_from(value)
        .map(crate::RequestId)
        .map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid source request".into()))
}

fn attached_source_target(
    request: &crate::generated::actor_local::ActorLocalReq,
) -> Result<
    Option<(crate::ActorRef, crate::request::sources::SourceTarget)>,
    ResidentActorWorkbenchError,
> {
    use crate::generated::actor_local::ActorLocalReq;
    use crate::request::sources::{RequestSourceKind, SourceTarget};
    let (owner, target) = match request {
        ActorLocalReq::ActorLocalAttachProgressSourceWith((owner, request), _) => (
            *owner,
            SourceTarget::Request(source_request_id(*request)?, RequestSourceKind::Progress),
        ),
        ActorLocalReq::ActorLocalAttachSettlementSourceWith((owner, request), _) => (
            *owner,
            SourceTarget::Request(source_request_id(*request)?, RequestSourceKind::Settlement),
        ),
        ActorLocalReq::ActorLocalAttachCommandSourceWith((owner, job), _) => (
            *owner,
            SourceTarget::Command(
                uuid::Uuid::parse_str(job)
                    .map_err(|_| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            "invalid command job handle".into(),
                        )
                    })?
                    .as_u128(),
            ),
        ),
        ActorLocalReq::ActorLocalAttachLifecycleSourceWith((owner, target), _) => (
            *owner,
            SourceTarget::Lifecycle(crate::wait::decode_address(target.0, target.1)?),
        ),
        _ => return Ok(None),
    };
    Ok(Some((
        crate::wait::decode_address(owner.0, owner.1)?,
        target,
    )))
}

#[derive(Clone, Copy)]
enum OutboundKind {
    Call,
    TryCall,
    Cast,
    TryCast,
}

fn capture_outbound_boundary<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    hole: ResidentHole,
    target: (i64, i64),
    kind: OutboundKind,
    actor_realm: RealmId,
) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let (id, incarnation) = target;
    let id = u64::try_from(id).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor request carried invalid actor id {id}"
        ))
    })?;
    let incarnation = u64::try_from(incarnation).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor request carried invalid incarnation {incarnation}"
        ))
    })?;
    let target = crate::ActorRef {
        id: crate::ActorId(id),
        incarnation: crate::Incarnation(incarnation),
    };
    let custody = session
        .live_payload_handle_owned_by(hole.cont_id(), actor_realm)?
        .ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "actor call or cast suspended without its request".into(),
            )
        })?;
    let request = crate::MailboxValue::new(context.placement.session, custody);
    Ok(ResidentActorBoundary::Outbound(match kind {
        OutboundKind::Call => ResidentOutbound::Call {
            target,
            continuation: hole,
            request,
        },
        OutboundKind::TryCall => ResidentOutbound::TryCall {
            target,
            continuation: hole,
            request,
        },
        OutboundKind::Cast => ResidentOutbound::Cast {
            target,
            continuation: hole,
            request,
        },
        OutboundKind::TryCast => ResidentOutbound::TryCast {
            target,
            continuation: hole,
            request,
        },
    }))
}

fn capture_receiver_boundary<H, O>(
    session: &mut ResidentSession<H, O>,
    hole: ResidentHole,
    site: i64,
    actor_realm: RealmId,
) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let site = u64::try_from(site).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor receive carried invalid site id {site}"
        ))
    })?;
    if session.parked_realm(&hole) != Some(actor_realm) {
        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor receive escaped its owning realm {actor_realm:?}"
        )));
    }
    let handler = session
        .live_payload_handle_owned_by(hole.cont_id(), actor_realm)?
        .ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "actor receive suspended without its handler".into(),
            )
        })?;
    Ok(ResidentActorBoundary::Receive(InstalledReceiver {
        site,
        continuation: hole,
        handler,
    }))
}

fn unexpected_boundary(
    expected: &str,
    actual: &ResidentActorBoundary,
) -> ResidentActorWorkbenchError {
    ResidentActorWorkbenchError::ActorProtocol(format!(
        "expected `{expected}`, reached `{}`",
        actual.operation()
    ))
}

struct ReadyBlock {
    result: TurnResult,
    generation: tidepool_repr::Generation,
    declaration_source: String,
    declaration_imports: SourceImports,
    observation: Option<(String, ExpressionPresentation, Option<bool>)>,
}

pub(crate) struct PreparedCellItem {
    ready: PreparedCellStep,
}

/// The cell cursor retains the mounted input owner until its final native
/// item and publication settle. Each compiler capsule retains the exact
/// source and tool owners separately.
pub(crate) struct CellPreparationLease {
    _bindings: tidepool_runtime::session::resident::BindingLease,
    _input: Option<HostInputRetirement>,
}

impl From<tidepool_runtime::session::resident::BindingLease> for CellPreparationLease {
    fn from(bindings: tidepool_runtime::session::resident::BindingLease) -> Self {
        Self {
            _bindings: bindings,
            _input: None,
        }
    }
}

struct WorkbenchCompilationSpec {
    _authority: Arc<crate::resident_actor::WorkbenchCompilationAuthority>,
    source: ActorWorkbenchSource,
    cell: tidepool_toolchain::checked_cell::CheckedCellSpecification,
    templates: Vec<tidepool_runtime::session::TurnTemplate>,
    include: Vec<PathBuf>,
    evidence: String,
    declaration_imports: SourceImports,
}

enum PreparedCellStep {
    Checked {
        specification: Arc<WorkbenchCompilationSpec>,
        prefix: Arc<tidepool_runtime::session::RuntimeCheckedPrefix>,
        item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    },
    Executable(Box<ReadyBlock>),
    Declaration {
        generation: tidepool_repr::Generation,
        binders: Vec<String>,
        prologue_only: bool,
    },
}

pub(crate) enum PreparedCell {
    Ready {
        items: Vec<PreparedCellItem>,
        dependencies: CellPreparationLease,
    },
    Rejected {
        index: usize,
        diagnostic: tidepool_runtime::session::CompileRejection,
    },
}

enum CompiledBlock {
    Ready(Box<ReadyBlock>),
    Rejected(tidepool_runtime::session::CompileRejection),
}

/// A short-checkout snapshot for [`ResidentActorWorkbench::prepare_cell`]'s
/// split compile: the exact source-side view the whole-cell check and every
/// item's compile target, and the declaration module this view currently
/// imports (`None` before the first declaration ever lands).
struct CellSplitSnapshot {
    view: crate::ActorCompileView,
    candidate_module: tidepool_repr::SessionModule,
    /// Retained imports for the speculative single-item compile. Legacy fold
    /// snapshots reserve a value generation; protected setup delegates its
    /// ordered reservations to runtime admission. Installation rechecks source.
    retained: Vec<(SymbolIdentity, u64)>,
}

/// Take the checkout-scoped snapshot a split cell preparation needs, then
/// release the checkout. The request's JSON input carrier, when present, is
/// mounted once by the caller before the split begins (`prepare_cell`)
/// and stays leased and un-retired across every checkout this split releases
/// and re-acquires — this only extends `source`'s imports/preamble to name
/// the already-mounted binding, so a later checkout's fresh view still
/// resolves it. See `prepare_cell`'s `HostInputRetirement` for the matching
/// retire-on-every-exit-path half of that contract.
#[allow(clippy::too_many_arguments)]
fn snapshot_cell_split<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: ActorWorkbenchSource,
    type_modules: &[String],
    response: Option<&ResponseExpectation>,
    request: Option<crate::RequestId>,
    mounted_input: Option<&MountedHostInput>,
) -> Result<(ActorWorkbenchSource, CellSplitSnapshot), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    snapshot_cell_split_owned(
        session,
        context,
        source,
        type_modules,
        response,
        request,
        mounted_input,
        CellSnapshotAdmission::LegacyFold,
    )
}

#[derive(Clone, Copy)]
enum CellSnapshotAdmission<'a> {
    LegacyFold,
    ProtectedSetup,
    PrivateExecution(&'a tidepool_runtime::session::PrivateExecutionAdmission),
}

#[allow(clippy::too_many_arguments)]
fn snapshot_cell_split_owned<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    mut source: ActorWorkbenchSource,
    type_modules: &[String],
    response: Option<&ResponseExpectation>,
    request: Option<crate::RequestId>,
    mounted_input: Option<&MountedHostInput>,
    admission: CellSnapshotAdmission<'_>,
) -> Result<(ActorWorkbenchSource, CellSplitSnapshot), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if let Some(input) = mounted_input {
        source
            .workbench_imports
            .extend_text(&format!("qualified {} as TidepoolHostInput", input.module));
        source.preamble = format!(
            "{}\ninput = TidepoolHostInput.{}\n",
            source.preamble, input.name
        )
        .into();
    }
    if session.machine_disposition()
        == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
    {
        return Err(ResidentActorWorkbenchError::MachineLost);
    }
    let candidate_module = session.next_declaration_module().ok_or_else(|| {
        ResidentActorWorkbenchError::CompileInfrastructure(
            "resident cell session has no declaration plane".into(),
        )
    })?;
    source.preamble = match (response, request) {
        (Some(response), Some(request)) => {
            response.request_preamble(&source.preamble, request, &context.haskell_effects_alias)
        }
        (None, None) => source.preamble.to_string(),
        _ => unreachable!("request workbench scope is constructed atomically"),
    }
    .into();
    source.preamble = actor_preamble(&source.preamble, context).into();
    let view = if let CellSnapshotAdmission::PrivateExecution(execution) = admission {
        if execution.private_scope() != context.placement.lexical_scope {
            return Err(ResidentActorWorkbenchError::Resident(
                ResidentError::Session(
                    tidepool_runtime::session::SessionError::StaleStagedDeclaration,
                ),
            ));
        }
        let session_view = session
            .compile_view_for_execution(execution)
            .map_err(|error| {
                ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
            })?;
        context
            .compile_view(session_view)?
            .with_workbench_imports(&source.workbench_imports)
            .with_type_modules(type_modules)
    } else if matches!(admission, CellSnapshotAdmission::ProtectedSetup) {
        let session_view = session
            .compile_view_in(context.placement.lexical_scope)
            .ok_or(ResidentActorWorkbenchError::Resident(
                ResidentError::Session(tidepool_runtime::session::SessionError::DeadScope(
                    context.placement.lexical_scope,
                )),
            ))?
            .with_scoped_injection();
        context
            .compile_view(session_view)?
            .with_workbench_imports(&source.workbench_imports)
            .with_type_modules(type_modules)
    } else {
        actor_compile_view(session, context, &source, type_modules)?
    };
    // The fold can write a Val interface during the whole-cell check. Claim
    // its identity before releasing checkout: rejecting a stale result later
    // cannot undo a compiler overwriting another actor's interface.
    if matches!(admission, CellSnapshotAdmission::LegacyFold) {
        session.reserve_value_generations_through(view.next_value_generation());
    }
    let retained = session.prepared_retained();
    Ok((
        source,
        CellSplitSnapshot {
            view,
            candidate_module,
            retained,
        },
    ))
}

fn consume_admitted_cell_item(
    specification: &WorkbenchCompilationSpec,
    reservation: Arc<tidepool_runtime::session::RuntimeCheckedItemAdmission>,
    block: &ParsedBlock,
) -> Result<CompiledBlock, ResidentActorWorkbenchError> {
    let result =
        match tidepool_runtime::session::turn::consume_cell_program_item(reservation.clone()) {
            Ok(result) => result,
            Err(failure) if classify_compile(&failure.error).class == FailureClass::UserHaskell => {
                return Ok(CompiledBlock::Rejected(render_turn_compile_rejection(
                    &failure.error,
                    failure.attempted_source.as_deref(),
                    &block.source,
                    &format!("<cell item {}>", block.ordinal),
                )));
            }
            Err(failure) => {
                let mut diagnostic = classify_compile(&failure.error);
                diagnostic.message = tidepool_runtime::session::render_cell_compile_error(
                    &failure.error,
                    &block.source,
                );
                return Err(ResidentActorWorkbenchError::CompileInfrastructure(
                    diagnostic,
                ));
            }
        };
    let observation = reservation
        .observation_name()
        .map(|name| {
            use tidepool_toolchain::checked_cell::{
                CheckedExpressionLift, CheckedExpressionPresentation,
            };
            let item = reservation.item();
            let presentation = match item
                .expression_presentation()
                .map_err(ResidentActorWorkbenchError::Compile)?
            {
                Some(CheckedExpressionPresentation::Rendered) => ExpressionPresentation::Rendered,
                Some(CheckedExpressionPresentation::Opaque) => ExpressionPresentation::Opaque,
                None => {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "checked observation lacks presentation".into(),
                    ))
                }
            };
            let effectful = match item
                .expression_lift()
                .map_err(ResidentActorWorkbenchError::Compile)?
            {
                Some(CheckedExpressionLift::Pure) => false,
                Some(CheckedExpressionLift::Effectful) => true,
                None => {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "checked observation lacks lift".into(),
                    ))
                }
            };
            Ok((name.to_owned(), presentation, Some(effectful)))
        })
        .transpose()?;
    Ok(CompiledBlock::Ready(Box::new(ReadyBlock {
        result,
        generation: reservation.generation(),
        declaration_source: block.source.clone(),
        declaration_imports: specification.declaration_imports.clone(),
        observation,
    })))
}

/// The whole-cell-check half of a split cell preparation: everything
/// [`snapshot_cell_split`]'s checkout-only prerequisites make possible once
/// they are already in hand, including the same-cell redeclaration-collision
/// retry `prepare_cell_single_checkout` runs. No session or checkout touched
/// here.
///
/// Speculatively attaches [`tidepool_runtime::session::CellFoldTurn`]
/// materials to the check request — the SAME generic bind/binddiscard
/// install templates `compile_block_off_checkout` would otherwise send in
/// its own, later request — so a cell that turns out to be a single bind
/// item compiles in this ONE round trip instead of two. Building them costs
/// nothing when the worker doesn't use them (an N-item, expression-only, or
/// declaration cell): no extra GHC work, only a few more small template
/// files on an already-open request. The returned `Option<TurnResult>` is
/// the documented "used it" signal; the caller decides whether `checked`'s
/// OWN classification actually matches that shape before trusting it (never
/// on the redeclaration-collision retry below, which keeps today's path).
fn check_cell_off_checkout(
    snapshot: &CellSplitSnapshot,
    source: &ActorWorkbenchSource,
    effects: &str,
    cell_source: &str,
) -> Result<(CellCheck, Option<tidepool_runtime::session::TurnResult>), ResidentActorWorkbenchError>
{
    let compile_view = &snapshot.view;
    let prepared = source.prepare_effectful(compile_view, effects)?;
    let check_preamble =
        cell_module_preamble(&prepared.preamble, &snapshot.candidate_module.module_name())?;
    let template = resident_cell_check_template(&check_preamble, effects, &prepared.imports);
    let compile_view_evidence = cell_check_evidence(compile_view, &template, &prepared);
    let include = prepared
        .include
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let cell_check_request = || CellCheckRequest {
        exact_context: compile_view.exact_declaration_context().cloned(),
        session_id: Some(compile_view.session_id()),
        cell_text: cell_source,
        template: &template,
        include: &include,
        session_root: compile_view.session_root(),
        inject_modules: &prepared.injected,
        compile_generation: compile_view.next_value_generation().0,
        compile_view_evidence: &compile_view_evidence,
    };
    let fold_templates =
        resident_workbench_templates(&prepared.preamble, effects, &prepared.imports);
    let fold = tidepool_runtime::session::CellFoldTurn {
        templates: &fold_templates,
        gen: compile_view.next_value_generation().0,
        retained_imports: &snapshot.retained,
    };
    match tidepool_runtime::session::check_cell_with_fold(cell_check_request(), fold) {
        Ok((checked, folded)) => Ok((checked, folded)),
        Err(failure) => {
            // See `prepare_cell_single_checkout`'s same-cell-shape comment:
            // a cell that both re-declares and uses a name in the same
            // statement needs the current generation's collision hidden
            // before one retry. The retry never attempts the fold — a
            // collision retry is rare, and its patched imports are not the
            // ones `fold_templates` above was built against.
            let mut patched_imports = None;
            if classify_compile(&failure.error).class == FailureClass::UserHaskell {
                if let Some(previous_module) = compile_view.library() {
                    let previous_module = previous_module.module_name();
                    let message = tidepool_runtime::session::render_cell_compile_error(
                        &failure.error,
                        cell_source,
                    );
                    let names = tidepool_runtime::session::turn::same_cell_value_collisions(
                        &message,
                        &previous_module,
                        &snapshot.candidate_module.module_name(),
                    );
                    patched_imports =
                        hide_same_cell_collisions(&prepared.imports, &previous_module, &names);
                }
            }
            match patched_imports {
                Some(patched_imports) => {
                    let retried_template =
                        resident_cell_check_template(&check_preamble, effects, &patched_imports);
                    let retried_evidence =
                        cell_check_evidence(compile_view, &retried_template, &prepared);
                    match check_cell(CellCheckRequest {
                        template: &retried_template,
                        compile_view_evidence: &retried_evidence,
                        ..cell_check_request()
                    }) {
                        Ok(checked) => Ok((checked, None)),
                        Err(failure) => Err(cell_check_error(failure, cell_source)),
                    }
                }
                None => Err(cell_check_error(failure, cell_source)),
            }
        }
    }
}

/// Whether a folded [`tidepool_runtime::session::TurnResult`] (compiled
/// against the pre-check snapshot's generation, inside
/// `check_cell_off_checkout`'s speculative fold) still targets exactly the
/// value generation `reserve_cell_generations` just reserved for this
/// attempt. A named bind's every bound binder must live in that exact
/// generation's `Session.Val.G<g>` module; a discarding bind has none to
/// check and always matches.
fn fold_result_matches_generation(
    result: &tidepool_runtime::session::TurnResult,
    generation: tidepool_repr::Generation,
) -> bool {
    match result {
        tidepool_runtime::session::TurnResult::Bind { bound, .. } => {
            let expected = tidepool_repr::SessionModule::val(generation).module_name();
            bound.iter().all(|binder| binder.module == expected)
        }
        _ => false,
    }
}

/// The outcome of [`reserve_cell_generations`]'s re-checkout: either the
/// snapshot is still fresh and every item's value generation is reserved, or
/// something else wrote to a scope this compile actually read from and the
/// caller must discard the split and compile under one checkout.
struct CellReservationReady {
    view: crate::ActorCompileView,
    retained: Vec<(SymbolIdentity, u64)>,
    visible_names: Vec<String>,
    /// The pure render of this cell's declaration item, and the live-value
    /// environment it was rendered against — `None` for a cell with no `Decl`
    /// item. Off-checkout, GHC-validating this against a private candidate
    /// directory produces the [`StagedDeclaration`]
    /// [`compile_cell_items_off_checkout`] and [`finalize_cell_install`] need.
    declaration: Option<(
        DeclarationCandidateRender,
        Vec<(tidepool_repr::SessionVarId, String)>,
    )>,
}

enum CellReservation {
    Ready(Box<CellReservationReady>),
    Stale(SplitStaleView),
}

/// Which part of the session a split-compile re-checkout found changed since
/// the snapshot it compiled against. Named in the INFO line
/// [`log_split_stale`] writes for every stale attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplitStaleView {
    /// [`crate::ActorCompileView::is_current_for`] failed: a scope this
    /// cell reads committed a value, import, or declaration, or a value
    /// module the cell could reach was released.
    CompileView,
    /// The session-wide declaration log's next module advanced, so this
    /// cell's declaration candidate names a generation someone else took.
    DeclarationModule,
    /// Adopting the already-validated declaration candidate lost the race
    /// for its generation.
    StagedDeclaration,
    /// [`ResidentSession::revalidate_and_run_prepared`] found an import of
    /// the compiled program changed since its snapshot.
    PreparedImports,
}

/// Off-checkout attempts for a split whose retry is cheap: the prepared
/// install, whose retry relinks and Cranelift-compiles GHC output that is
/// still current, and the display render, whose GHC part is one small
/// generated bundle. A stale first attempt re-snapshots and compiles again
/// with the machine released; only a second stale attempt runs under one
/// checkout.
///
/// The cell split and the fragment (tool-installer) split make one attempt.
/// Their retry would repeat several GHC round trips, and an inherited-context
/// fork whose parent commits every few seconds makes a second attempt of
/// that length stale too; they fall straight through to their
/// single-checkout path instead.
const CHEAP_RETRY_ATTEMPTS: usize = 2;

/// One INFO line per stale split attempt, naming the path, the stage that
/// found the snapshot stale, what changed, and whether the path recompiles
/// off-checkout (`retrying`) or falls back to its single-checkout compile.
fn log_split_stale(
    path: &'static str,
    context: &crate::ActorSessionContext,
    stage: &'static str,
    changed: SplitStaleView,
    retrying: bool,
) {
    if retrying {
        tracing::info!(
            actor = context.actor.id.0,
            path,
            stage,
            changed = ?changed,
            "split compile went stale; recompiling off-checkout against a fresh snapshot"
        );
    } else {
        tracing::info!(
            actor = context.actor.id.0,
            path,
            stage,
            changed = ?changed,
            "split compile went stale; compiling under one checkout"
        );
    }
}

/// The freshness check every split-compile re-checkout makes: the compile
/// view first, then (when this cell's work depends on it) the declaration
/// log's next module.
fn split_staleness<H, O>(
    session: &ResidentSession<H, O>,
    fresh: &crate::ActorCompileView,
    compiled_against: &crate::ActorCompileView,
    candidate_module: Option<tidepool_repr::SessionModule>,
) -> Option<SplitStaleView>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if !fresh.is_current_for(compiled_against) {
        return Some(SplitStaleView::CompileView);
    }
    match candidate_module {
        Some(candidate) if session.next_declaration_module() != Some(candidate) => {
            Some(SplitStaleView::DeclarationModule)
        }
        _ => None,
    }
}

/// Re-checkout after the whole-cell check to revalidate `snapshot` and
/// reserve identities for every item this cell is about to compile, all at
/// once, before releasing the checkout again for the per-item compiles.
/// Reserving the whole run's generations here (rather than one at a time, as
/// the single-checkout path does per item) keeps the per-item compiles
/// entirely off-checkout. `value_item_count` counts only the items that
/// actually consume a value generation — a cell's `Decl` item never does (its
/// identity lives in the Lib module, not a `Val.G` interface), so a caller
/// with a declaration passes the count of every OTHER item, which may be
/// zero. `declaration_receipt` is the cell's own `Decl` item (if it has one),
/// already reduced to the exact GHC receipt used everywhere else a
/// declaration is staged.
#[allow(clippy::too_many_arguments)]
fn reserve_cell_generations<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    snapshot: &CellSplitSnapshot,
    value_item_count: usize,
    declaration_receipt: Option<&DeclarationReceipt>,
) -> Result<CellReservation, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if session.machine_disposition()
        == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
    {
        return Err(ResidentActorWorkbenchError::MachineLost);
    }
    let fresh_view = actor_compile_view(session, context, source, type_modules)?;
    // `next_declaration_module()` is the session-wide declaration log's
    // NEXT generation counter (`SessionLib::next_module`, one monotonic
    // counter for the whole session, not scoped per lexical scope the way
    // `library`/`is_current_for` already are — see
    // `ActorCompileView::is_current_for`'s doc comment and
    // `SessionCompileView::is_current_for`). It only names the
    // candidate module THIS cell's own `Decl` item would claim; a cell
    // with no `Decl` item never reads or writes that module, so another
    // actor committing a declaration and advancing the counter cannot make
    // this cell's own compile stale. Checking it unconditionally treated
    // every actor's declaration as invalidating every other actor's
    // in-flight split compile, which is what produced the stale-retry
    // storm the split compile exists to avoid.
    let candidate = declaration_receipt.map(|_| snapshot.candidate_module);
    if let Some(changed) = split_staleness(session, &fresh_view, &snapshot.view, candidate) {
        return Ok(CellReservation::Stale(changed));
    }
    let reserved = snapshot.view.next_value_generation();
    let compile_view = if value_item_count > 0
        && (value_item_count == 1
            || fresh_view.next_value_generation().0 == reserved.0.saturating_add(1))
    {
        // Reuse the fold reservation. An uncontended multi-item cell can
        // extend it; otherwise its whole range must be reserved afresh.
        if value_item_count > 1 {
            session.reserve_value_generations_through(tidepool_repr::Generation(
                reserved.0.saturating_add((value_item_count - 1) as u64),
            ));
        }
        snapshot.view.clone()
    } else {
        if value_item_count > 1 {
            let g0 = fresh_view.next_value_generation();
            let through = g0.0.saturating_add((value_item_count - 1) as u64);
            session.reserve_value_generations_through(tidepool_repr::Generation(through));
        }
        fresh_view.clone()
    };
    let declaration = declaration_receipt
        .map(|receipt| {
            session
                .render_declaration_candidate_in(
                    context.placement.lexical_scope,
                    receipt,
                    &fresh_view.workbench_imports(),
                )
                .map(|(candidate, values)| {
                    (candidate.with_source_layer(&context.source_layer), values)
                })
        })
        .transpose()
        .map_err(|error| ResidentActorWorkbenchError::Resident(ResidentError::Session(error)))?;
    let retained = session.prepared_retained();
    let visible_names = session
        .workbench_bindings_in(context.placement.lexical_scope)
        .into_iter()
        .map(|binding| binding.name)
        .collect::<Vec<_>>();
    Ok(CellReservation::Ready(Box::new(CellReservationReady {
        view: compile_view,
        retained,
        visible_names,
        declaration,
    })))
}

/// The outcome of [`compile_cell_items_off_checkout`]: either every item
/// compiled and is ready to install, or one was rejected — a rejection needs
/// no re-checkout, since nothing rejected is ever installed or leased.
enum CellItemsOutcome {
    Ready(Vec<PreparedCellItem>),
    Rejected {
        index: usize,
        diagnostic: tidepool_runtime::session::CompileRejection,
    },
}

/// The per-item compile half of a split cell preparation: everything
/// [`reserve_cell_generations`]'s checkout-only prerequisites make possible
/// once they are already in hand. No session or checkout touched here;
/// mirrors `prepare_cell_in_session`'s item loop, folding `with_staged_values`
/// exactly as that loop does so each later item sees the ones compiled before
/// it in this same cell. `staged` and `candidate_dir` are `Some` together,
/// for a cell with a `Decl` item: `staged` supplies that item's already-GHC-
/// validated receipt directly (no compile needed — its GHC work already ran
/// producing `staged`), and `candidate_dir` is the private directory it was
/// validated against, prepended to every OTHER item's own include path so
/// those items resolve the not-yet-installed declaration module.
#[allow(clippy::too_many_arguments)]
fn compile_cell_items_off_checkout(
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    effect_stack: &str,
    checked: &CellCheck,
    cell_text: &str,
    mut compile_view: crate::ActorCompileView,
    retained: &[(SymbolIdentity, u64)],
    visible_names: &[String],
    staged: Option<&StagedDeclaration>,
    candidate_dir: Option<&std::path::Path>,
) -> Result<CellItemsOutcome, ResidentActorWorkbenchError> {
    let mut result = Vec::with_capacity(checked.items.len());
    let mut staged_names: Vec<String> = Vec::new();
    let declaration_imports = compile_view.workbench_imports();
    for (index, item) in checked.items.iter().enumerate() {
        if item.verdict.kind == TurnKind::Decl {
            let staged = staged.ok_or_else(|| {
                ResidentActorWorkbenchError::CompileInfrastructure(
                    "cell declaration item has no staged declaration module".into(),
                )
            })?;
            result.push(PreparedCellItem {
                ready: PreparedCellStep::Executable(Box::new(ReadyBlock {
                    result: TurnResult::Decl(staged.receipt().clone()),
                    generation: compile_view.next_value_generation(),
                    declaration_source: item.source.clone(),
                    declaration_imports: declaration_imports.clone(),
                    observation: None,
                })),
            });
            continue;
        }
        let pins = (item.verdict.kind == TurnKind::Bind)
            .then(|| checked.pins_for_item(index))
            .transpose()
            .map_err(ResidentActorWorkbenchError::Compile)?;
        let expression_plan = (item.verdict.kind == TurnKind::Expr)
            .then(|| {
                checked.expression_plan_for_item(
                    index,
                    cell_text,
                    checked.compile_generation,
                    &checked.compile_view_evidence,
                )
            })
            .transpose()
            .map_err(ResidentActorWorkbenchError::Compile)?;
        let block = ParsedBlock {
            ordinal: index + 1,
            total: checked.items.len(),
            source: item.source.clone(),
        };
        let compiled = compile_block_off_checkout(
            context,
            source,
            effect_stack,
            &block,
            pins.as_deref(),
            compile_view.clone(),
            &staged_names,
            Some(&item.verdict),
            Some(&checked.prologue),
            expression_plan.as_ref(),
            retained,
            visible_names,
            candidate_dir,
        )?;
        let ready = match compiled {
            CompiledBlock::Ready(ready) => *ready,
            CompiledBlock::Rejected(diagnostic) => {
                return Ok(CellItemsOutcome::Rejected { index, diagnostic });
            }
        };
        if let TurnResult::Bind { bound, .. } = &ready.result {
            if !bound.is_empty() {
                let module =
                    tidepool_repr::SessionModule::val(compile_view.next_value_generation());
                let expected = module.module_name();
                if bound.iter().any(|binder| binder.module != expected) {
                    return Err(ResidentActorWorkbenchError::CompileInfrastructure(
                        format!("staged cell binder module does not match {expected}").into(),
                    ));
                }
                let names = bound
                    .iter()
                    .map(|binder| binder.name.clone())
                    .collect::<Vec<_>>();
                staged_names.extend(names.iter().cloned());
                compile_view = compile_view.with_staged_values(module, names);
            }
        }
        result.push(PreparedCellItem {
            ready: PreparedCellStep::Executable(Box::new(ready)),
        });
    }
    Ok(CellItemsOutcome::Ready(result))
}

/// The outcome of [`revalidate_cell_rejection`]'s re-checkout: either the
/// view [`compile_cell_items_off_checkout`] rejected against is still
/// current, so the rejection stands, or the caller must discard the split
/// and compile under one checkout instead of trusting a stale rejection.
enum CellRejectionRevalidation {
    StillCurrent,
    Stale(SplitStaleView),
}

/// Re-checkout after an off-checkout item compile rejects, to check whether
/// the rejection is trustworthy: `compile_cell_items_off_checkout` ran with
/// the machine released, so a concurrent write to a scope this compile
/// actually read from can make what looks like a real compile error just
/// staleness. Same shape as [`finalize_cell_install`]'s revalidation
/// (nothing to lease or install for a rejection, so no `PreparedCell` is
/// produced here).
fn revalidate_cell_rejection<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    candidate_module: Option<tidepool_repr::SessionModule>,
    compiled_against: &crate::ActorCompileView,
) -> Result<CellRejectionRevalidation, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if session.machine_disposition()
        == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
    {
        return Err(ResidentActorWorkbenchError::MachineLost);
    }
    let fresh_view = actor_compile_view(session, context, source, type_modules)?;
    if let Some(changed) = split_staleness(session, &fresh_view, compiled_against, candidate_module)
    {
        return Ok(CellRejectionRevalidation::Stale(changed));
    }
    Ok(CellRejectionRevalidation::StillCurrent)
}

/// The outcome of [`finalize_cell_install`]'s re-checkout: either the
/// snapshot the items compiled against is still fresh and the cell installs,
/// or the caller must discard the split and compile under one checkout.
enum CellInstall {
    Ready(PreparedCell),
    Stale(SplitStaleView),
}

/// Re-checkout after every item compiled to revalidate against the exact
/// view [`compile_cell_items_off_checkout`] compiled against, lease the
/// visible bindings a later item in this same cell might still depend on,
/// and hand back a ready [`PreparedCell`].
/// `staged` is `Some` exactly when this cell has a `Decl` item: the
/// already-GHC-validated candidate [`compile_cell_items_off_checkout`] built
/// against `candidate_dir`. Installing it here — [`session.
/// adopt_staged_declaration_in`](ResidentSession::adopt_staged_declaration_in)
/// — is the first time it touches the shared session root; a stale adopt
/// (something else took this same generation since) is just another
/// `CellInstall::Stale`, same as every other freshness check this function
/// already makes.
#[allow(clippy::too_many_arguments)]
fn finalize_cell_install<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    candidate_module: tidepool_repr::SessionModule,
    compiled_against: &crate::ActorCompileView,
    checked: &CellCheck,
    mut items: Vec<PreparedCellItem>,
    staged: Option<StagedDeclaration>,
) -> Result<CellInstall, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    if session.machine_disposition()
        == Some(tidepool_codegen::machine::MachineDisposition::Unavailable)
    {
        return Err(ResidentActorWorkbenchError::MachineLost);
    }
    let fresh_view = actor_compile_view(session, context, source, type_modules)?;
    // Only a cell with a `Decl` item claims the next declaration module; the
    // counter moving under any other cell is another actor's declaration.
    let candidate = staged.as_ref().map(|_| candidate_module);
    if let Some(changed) = split_staleness(session, &fresh_view, compiled_against, candidate) {
        return Ok(CellInstall::Stale(changed));
    }
    if let Some(staged) = staged {
        let declaration_index = checked
            .items
            .iter()
            .position(|item| item.verdict.kind == TurnKind::Decl)
            .ok_or_else(|| {
                ResidentActorWorkbenchError::CompileInfrastructure(
                    "staged declaration but no Decl item in the checked cell".into(),
                )
            })?;
        let expected_generation = staged.generation();
        let generation = match session.adopt_staged_declaration_in(staged) {
            Ok(commit) => commit.generation,
            Err(tidepool_runtime::session::SessionError::StaleStagedDeclaration) => {
                return Ok(CellInstall::Stale(SplitStaleView::StagedDeclaration))
            }
            Err(error) => {
                return Err(ResidentActorWorkbenchError::Resident(
                    ResidentError::Session(error),
                ))
            }
        };
        if generation != expected_generation {
            return Err(ResidentActorWorkbenchError::CompileInfrastructure(
                "cell declaration generation changed during split installation".into(),
            ));
        }
        let declaration = &checked.items[declaration_index].verdict;
        items[declaration_index].ready = PreparedCellStep::Declaration {
            generation,
            binders: declaration.binders.clone(),
            prologue_only: checked.items[declaration_index].prologue_only,
        };
    }
    let visible = session.visible_binding_ids_in(context.placement.lexical_scope);
    let dependencies = session.lease_bindings(&visible);
    Ok(CellInstall::Ready(PreparedCell::Ready {
        items,
        dependencies: dependencies.into(),
    }))
}

#[allow(clippy::too_many_arguments)]
fn prepare_cell_in_session<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    effect_stack: &str,
    type_modules: &[String],
    checked: &CellCheck,
    cell_text: &str,
    compile_view_evidence: &str,
    base_view: crate::ActorCompileView,
) -> Result<PreparedCell, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let declaration = checked
        .items
        .iter()
        .enumerate()
        .find(|(_, item)| item.verdict.kind == TurnKind::Decl);
    let declaration_imports = base_view.workbench_imports();
    let staged = if let Some((_, item)) = declaration {
        let receipt = DeclarationReceipt {
            binders: item.verdict.binders.clone(),
            items: item.verdict.items.clone(),
            source: tidepool_runtime::session::DeclarationSource {
                prologue: checked.prologue.clone(),
                body: item.source.clone(),
            },
        };
        match session.stage_declarations_in(
            context.placement.lexical_scope,
            &receipt,
            &declaration_imports,
            &context.source_layer,
        ) {
            Ok(staged) => Some(staged),
            Err(error) if classify_session(&error).class == FailureClass::UserHaskell => {
                return Ok(PreparedCell::Rejected {
                    index: 0,
                    diagnostic: classify_session(&error).message.into(),
                });
            }
            Err(error) => {
                return Err(ResidentActorWorkbenchError::Resident(
                    ResidentError::Session(error),
                ));
            }
        }
    } else {
        None
    };
    let checked_generation = base_view.next_value_generation().0;
    let mut compile_view = match &staged {
        Some(staged) => base_view.with_staged_library(staged.module(), staged.items()),
        None => base_view,
    };
    let prepared = (|| {
        let mut result = Vec::with_capacity(checked.items.len());
        let mut staged_names = Vec::new();
        for (index, item) in checked.items.iter().enumerate() {
            if item.verdict.kind == TurnKind::Decl {
                let staged = staged.as_ref().ok_or_else(|| {
                    ResidentActorWorkbenchError::CompileInfrastructure(
                        "cell declaration item has no staged declaration module".into(),
                    )
                })?;
                result.push(PreparedCellItem {
                    ready: PreparedCellStep::Executable(Box::new(ReadyBlock {
                        result: TurnResult::Decl(staged.receipt().clone()),
                        generation: compile_view.next_value_generation(),
                        declaration_source: item.source.clone(),
                        declaration_imports: declaration_imports.clone(),
                        observation: None,
                    })),
                });
                continue;
            }
            let pins = (item.verdict.kind == TurnKind::Bind)
                .then(|| checked.pins_for_item(index))
                .transpose()
                .map_err(ResidentActorWorkbenchError::Compile)?;
            let expression_plan = (item.verdict.kind == TurnKind::Expr)
                .then(|| {
                    checked.expression_plan_for_item(
                        index,
                        cell_text,
                        checked_generation,
                        compile_view_evidence,
                    )
                })
                .transpose()
                .map_err(ResidentActorWorkbenchError::Compile)?;
            let block = ParsedBlock {
                ordinal: index + 1,
                total: checked.items.len(),
                source: item.source.clone(),
            };
            let compiled = compile_block_in_view(
                session,
                context,
                source,
                effect_stack,
                type_modules,
                &block,
                pins.as_deref(),
                compile_view.clone(),
                &staged_names,
                Some(&item.verdict),
                Some(&checked.prologue),
                expression_plan.as_ref(),
            )?;
            let ready = match compiled {
                CompiledBlock::Ready(ready) => *ready,
                CompiledBlock::Rejected(diagnostic) => {
                    return Ok(PreparedCell::Rejected { index, diagnostic });
                }
            };
            if let TurnResult::Bind { bound, .. } = &ready.result {
                if !bound.is_empty() {
                    let module =
                        tidepool_repr::SessionModule::val(compile_view.next_value_generation());
                    let expected = module.module_name();
                    if bound.iter().any(|binder| binder.module != expected) {
                        return Err(ResidentActorWorkbenchError::CompileInfrastructure(
                            format!("staged cell binder module does not match {expected}").into(),
                        ));
                    }
                    let names = bound
                        .iter()
                        .map(|binder| binder.name.clone())
                        .collect::<Vec<_>>();
                    staged_names.extend(names.iter().cloned());
                    compile_view = compile_view.with_staged_values(module, names);
                }
            }
            result.push(PreparedCellItem {
                ready: PreparedCellStep::Executable(Box::new(ready)),
            });
        }
        // Later items were compiled against this cell's starting value
        // environment. Keep those exact identities alive until the prepared
        // prefix finishes, even when an earlier item's display publication
        // shadows one of their public names.
        let visible = session.visible_binding_ids_in(context.placement.lexical_scope);
        let dependencies = session.lease_bindings(&visible);
        Ok(PreparedCell::Ready {
            items: result,
            dependencies: dependencies.into(),
        })
    })();
    let prepared = prepared.and_then(|mut prepared| {
        if let PreparedCell::Ready { items, .. } = &mut prepared {
            if let (Some((_, declaration)), Some(staged)) = (declaration, staged.as_ref()) {
                let generation = session
                    .adopt_staged_declaration_in(staged.clone())
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })?
                    .generation;
                if generation != staged.generation() {
                    return Err(ResidentActorWorkbenchError::CompileInfrastructure(
                        "cell declaration generation changed during exclusive preparation".into(),
                    ));
                }
                items[0].ready = PreparedCellStep::Declaration {
                    generation,
                    binders: declaration.verdict.binders.clone(),
                    prologue_only: declaration.prologue_only,
                };
            }
        }
        Ok(prepared)
    });
    if let Some(staged) = &staged {
        session.discard_staged_declaration(staged);
    }
    prepared
}

fn actor_compile_view<H, O>(
    session: &ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
) -> Result<crate::ActorCompileView, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let session_view = session
        .compile_view_in(context.placement.lexical_scope)
        .ok_or_else(|| {
            ResidentActorWorkbenchError::Resident(ResidentError::Session(
                tidepool_runtime::session::SessionError::DeadScope(context.placement.lexical_scope),
            ))
        })?;
    Ok(context
        .compile_view(session_view)?
        .with_workbench_imports(&source.workbench_imports)
        .with_type_modules(type_modules))
}

/// Compile one fresh, payload-independent value interface. Its binder is
/// unique to this mount, so a later request cannot replace the global slot a
/// previously compiled closure captured.
#[allow(clippy::too_many_arguments)]
fn compile_host_binding<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    binding: &str,
    type_name: &str,
    anchor: &str,
    imports: SourceImports,
    retain_text_constructor: bool,
) -> Result<(BoundBinder, CompiledTurn, tidepool_repr::Generation), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let view = actor_compile_view(session, context, source, type_modules)?;
    let generation = view.next_value_generation();
    let retained = session.prepared_retained();
    let (binder, compiled) = compile_host_binding_off_checkout(
        &view,
        source,
        &context.haskell_effects_alias,
        generation,
        binding,
        type_name,
        anchor,
        imports,
        retain_text_constructor,
        &retained,
    )?;
    Ok((binder, compiled, generation))
}

/// The GHC-compile half of [`compile_host_binding`], run against one
/// already-taken `view` with no session or checkout held. Carrier construction
/// snapshots `view` and reserves `generation` under a short checkout, then calls
/// this off-checkout. The cached carrier retains the compiled shape without
/// its throwaway binding identity.
#[allow(clippy::too_many_arguments)]
fn compile_host_binding_off_checkout(
    view: &crate::ActorCompileView,
    source: &ActorWorkbenchSource,
    effects: &str,
    generation: tidepool_repr::Generation,
    binding: &str,
    type_name: &str,
    anchor: &str,
    imports: SourceImports,
    retain_text_constructor: bool,
    retained_imports: &[(SymbolIdentity, u64)],
) -> Result<(BoundBinder, CompiledTurn), ResidentActorWorkbenchError> {
    let view = view.clone().with_workbench_imports(&imports);
    let prepared = source.prepare_effectful(&view, effects)?;
    let templates = resident_workbench_templates(&prepared.preamble, effects, &prepared.imports);
    let include: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
    let retained_anchor = format!("__tidepoolCarrierAnchor{generation}");
    let (turn, expected_binders) = if retain_text_constructor {
        (
            format!(
                "({binding}, {retained_anchor}) <- pure ((({anchor}) :: {type_name}), \
                 (TidepoolHostJson.String (TidepoolHostText.pack \"\") :: TidepoolHostJson.Value))"
            ),
            vec![binding.to_owned(), retained_anchor],
        )
    } else {
        (
            format!("{binding} <- pure (({anchor}) :: {type_name})"),
            vec![binding.to_owned()],
        )
    };
    let result = run_turn(TurnRequest {
        exact_context: view.exact_declaration_context().cloned(),
        session_id: Some(view.session_id()),
        turn_text: &turn,
        templates: &templates,
        include: &include,
        session_root: view.session_root(),
        inject_modules: &prepared.injected,
        gen: generation.0,
        verdict: Some(generated_binds_verdict(&expected_binders)),
        target: None,
        retained_imports,
    })
    .map_err(|failure| {
        ResidentActorWorkbenchError::InputMount(
            tidepool_runtime::session::render_turn_compile_error(
                &failure.error,
                failure.attempted_source.as_deref(),
                &turn,
                "<host carrier>",
            ),
        )
    })?;
    let TurnResult::Bind {
        mut bound,
        compiled,
        ..
    } = result
    else {
        return Err(ResidentActorWorkbenchError::InputMount(
            "host interface did not compile as a bind".into(),
        ));
    };
    if bound.len() != expected_binders.len()
        || !bound
            .iter()
            .zip(&expected_binders)
            .all(|(binder, expected)| binder.name == *expected)
    {
        return Err(ResidentActorWorkbenchError::InputMount(format!(
            "host interface returned an unexpected binder shape for `{binding}`"
        )));
    }
    Ok((bound.remove(0), compiled))
}

#[derive(Clone)]
struct MountedHostInput {
    binder: BoundBinder,
    module: String,
    name: String,
}

/// Session root for a synchronous carrier mount, cheaply re-derived under
/// the checkout already in hand — no compile, so nothing here needs the full
/// [`actor_compile_view`] snapshot, only the source-side facts a
/// [`HostCarrier`] mount actually reads.
fn carrier_mount_session_root<H, O>(
    session: &ResidentSession<H, O>,
    scope: tidepool_codegen::scope::ScopeId,
) -> Result<PathBuf, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    session
        .compile_view_in(scope)
        .map(|view| view.session_root().to_path_buf())
        .ok_or_else(|| {
            ResidentActorWorkbenchError::Resident(ResidentError::Session(
                tidepool_runtime::session::SessionError::DeadScope(scope),
            ))
        })
}

/// Mount a request's JSON input. With `carrier` cached, this is one
/// `mount_carrier_in` call and no GHC compile; with none yet built for this
/// workbench, it falls back to [`compile_host_binding`]'s per-mount compile.
fn mount_json_input<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    input: &serde_json::Value,
    carrier: Option<&HostCarrier>,
) -> Result<MountedHostInput, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let scope = context.placement.lexical_scope;
    let binding = fresh_host_binding_name(session, scope, "Input");
    let binder = if let Some(carrier) = carrier {
        let session_root = carrier_mount_session_root(session, scope)?;
        let generation = session.val_gen().next();
        session.reserve_value_generations_through(generation);
        session
            .mount_carrier_in(
                &session_root,
                scope,
                &binding,
                generation,
                carrier,
                HostPayload::Json(input),
            )
            .map_err(ResidentActorWorkbenchError::Resident)?
    } else {
        let (binder, compiled, generation) = compile_host_binding(
            session,
            context,
            source,
            type_modules,
            &binding,
            JSON_INPUT_TYPE_NAME,
            JSON_INPUT_ANCHOR,
            json_input_carrier_imports(),
            false,
        )?;
        session
            .mount_json_binding_in(scope, &binder, generation, compiled.into_code(), input)
            .map_err(ResidentActorWorkbenchError::Resident)?;
        binder
    };
    if let Err(error) = session.hide_host_binding_in(scope, &binder) {
        let session_root = carrier_mount_session_root(session, scope)?;
        session.retire_host_binding_owner(&session_root, &binder);
        return Err(ResidentActorWorkbenchError::Resident(error));
    }
    Ok(MountedHostInput {
        module: binder.module.clone(),
        name: binder.name.clone(),
        binder,
    })
}

/// Mount one plain text binding. With `carrier` cached, this is one
/// `mount_carrier_in` call and no GHC compile; with none yet built for this
/// workbench, it falls back to [`compile_host_binding`]'s per-mount compile.
fn mount_text_binding<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    binding: &str,
    text: &str,
    carrier: Option<&HostCarrier>,
) -> Result<(), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let scope = context.placement.lexical_scope;
    if let Some(carrier) = carrier {
        let session_root = carrier_mount_session_root(session, scope)?;
        let generation = session.val_gen().next();
        session.reserve_value_generations_through(generation);
        session
            .mount_carrier_in(
                &session_root,
                scope,
                binding,
                generation,
                carrier,
                HostPayload::Text(text),
            )
            .map_err(ResidentActorWorkbenchError::Resident)?;
        return Ok(());
    }
    let (binder, compiled, generation) = compile_host_binding(
        session,
        context,
        source,
        type_modules,
        binding,
        TEXT_BINDING_TYPE_NAME,
        TEXT_BINDING_ANCHOR,
        text_binding_carrier_imports(),
        true,
    )?;
    session
        .mount_text_binding_in(scope, &binder, generation, compiled.into_code(), text)
        .map_err(ResidentActorWorkbenchError::Resident)
}

fn fresh_host_binding_name<H, O>(
    session: &ResidentSession<H, O>,
    scope: tidepool_codegen::scope::ScopeId,
    category: &str,
) -> String
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let used = session
        .workbench_bindings_in(scope)
        .into_iter()
        .map(|binding| binding.name)
        .chain(
            session
                .current_decl_heads_in(scope)
                .into_iter()
                .map(|(name, _)| name),
        )
        .collect::<std::collections::BTreeSet<_>>();
    let generation = session.val_gen().0;
    #[allow(
        clippy::expect_used,
        reason = "the ordinal range is infinite and `used` is a finite set, so some ordinal is \
                  always free"
    )]
    (0_u64..)
        .map(|ordinal| format!("__tidepool{category}{generation}_{ordinal}"))
        .find(|candidate| !used.contains(candidate))
        .expect("unbounded internal host binding namespace")
}

/// The Job carrier's fixed imports, nominal type and authenticated anchor.
fn command_job_carrier_imports() -> SourceImports {
    SourceImports::from_specs([
        "qualified Tidepool.Command.Types as TidepoolHostJob",
        "qualified Data.Text as TidepoolHostText",
        "qualified Data.Text.Internal as TidepoolHostTextInternal",
        "qualified GHC.Exts as TidepoolHostExts",
        "qualified Tidepool.Aeson as TidepoolHostJson",
    ])
}

const COMMAND_JOB_TYPE_NAME: &str = "TidepoolHostJob.Job";
const COMMAND_JOB_ANCHOR: &str = "TidepoolHostJob.Job (case TidepoolHostExts.noinline (TidepoolHostText.pack \"\") of TidepoolHostTextInternal.Text bytes offset length -> TidepoolHostTextInternal.Text bytes offset length)";

/// [`mount_json_input`]'s own import set and type/anchor pair, shared by its
/// per-mount compile path and [`HostCarrierKind::Json`]'s carrier build so
/// the two can never drift.
fn json_input_carrier_imports() -> SourceImports {
    SourceImports::from_specs([
        "qualified Tidepool.Aeson as TidepoolHostJson",
        "Tidepool.Aeson (object, (.=), toJSON)",
    ])
}

const JSON_INPUT_TYPE_NAME: &str = "TidepoolHostJson.Value";
const JSON_INPUT_ANCHOR: &str = "object [\"anchor\" .= toJSON [TidepoolHostJson.String \"\", TidepoolHostJson.Number (TidepoolHostJson.scientific 0 0), TidepoolHostJson.Bool True, TidepoolHostJson.Null]]";

/// [`mount_text_binding`]'s own import set and type/anchor pair, shared by
/// its per-mount compile path and [`HostCarrierKind::Text`]'s carrier build
/// so the two can never drift.
fn text_binding_carrier_imports() -> SourceImports {
    SourceImports::from_specs([
        "qualified Data.Text as TidepoolHostText",
        "qualified Data.Text.Internal as TidepoolHostTextInternal",
        "qualified GHC.Exts as TidepoolHostExts",
        "qualified Tidepool.Aeson as TidepoolHostJson",
    ])
}

const TEXT_BINDING_TYPE_NAME: &str = "TidepoolHostText.Text";
const TEXT_BINDING_ANCHOR: &str = "case TidepoolHostExts.noinline (TidepoolHostText.pack \"\") of TidepoolHostTextInternal.Text bytes offset length -> TidepoolHostTextInternal.Text bytes offset length";

fn cell_check_evidence(
    view: &crate::ActorCompileView,
    template: &str,
    prepared: &WorkbenchCompilation,
) -> String {
    fn field(hasher: &mut blake3::Hasher, value: &[u8]) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    let mut evidence = blake3::Hasher::new();
    field(&mut evidence, view.evidence_key().as_bytes());
    field(&mut evidence, template.as_bytes());
    for path in prepared.include.iter() {
        field(&mut evidence, path.as_os_str().as_encoded_bytes());
    }
    // Only the injected modules this scope can reach: the rest are injected
    // for findability, and another actor's binds must not invalidate the
    // evidence (`SessionCompileView::is_current_for`).
    for module in view.reachable_module_names() {
        field(&mut evidence, module.as_bytes());
    }
    evidence.finalize().to_hex().to_string()
}

/// The verdict for a block this runtime wrote itself: `<binder> <- pure …`,
/// one bound name, no declaration exports. Classification is its own compiler
/// round trip, and for a generated block it can only answer what the
/// generator already knows — so a generated block states its shape instead of
/// asking. Authored source still classifies; only these fixed shapes skip it.
fn generated_bind_verdict(binder: &str) -> TurnClassification {
    generated_binds_verdict(&[binder.to_string()])
}

/// The verdict for a generated pattern bind.  The compiler owns each name's
/// stable identity and type; Rust supplies only the fixed tuple shape that it
/// just generated.
fn generated_binds_verdict(binders: &[String]) -> TurnClassification {
    TurnClassification {
        kind: TurnKind::Bind,
        binders: binders.to_vec(),
        items: Vec::new(),
    }
}

/// `verdict` is `None` for authored source, whose shape only GHC can answer,
/// and `Some` for a block this runtime generated (see
/// [`generated_bind_verdict`]).
#[allow(clippy::too_many_arguments)]
fn compile_block<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    effect_stack: &str,
    type_modules: &[String],
    block: &ParsedBlock,
    pins: Option<&[CheckedBinderPin]>,
    verdict: Option<&TurnClassification>,
) -> Result<CompiledBlock, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let compile_view = actor_compile_view(session, context, source, type_modules)?;
    compile_block_in_view(
        session,
        context,
        source,
        effect_stack,
        type_modules,
        block,
        pins,
        compile_view,
        &[],
        verdict,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_block_in_view<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    effect_stack: &str,
    _type_modules: &[String],
    block: &ParsedBlock,
    pins: Option<&[CheckedBinderPin]>,
    compile_view: crate::ActorCompileView,
    staged_names: &[String],
    checked_verdict: Option<&TurnClassification>,
    prologue: Option<&tidepool_runtime::session::SourcePrologue>,
    expression_plan: Option<&CheckedExpressionPlan>,
) -> Result<CompiledBlock, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let verdict = match checked_verdict {
        Some(checked) => Some(checked.clone()),
        None => tidepool_runtime::session::classify_block(&[&block.source])
            .map_err(|error| {
                let mut diagnostic = classify_compile(&error);
                diagnostic.message = error.to_string();
                ResidentActorWorkbenchError::CompileInfrastructure(diagnostic)
            })?
            .into_iter()
            .next(),
    };
    // A failed worker may already have published a thin value interface.
    // Its identity is never reused, whether compilation or execution succeeds.
    if verdict
        .as_ref()
        .is_none_or(|verdict| verdict.kind != TurnKind::Decl)
    {
        session.reserve_value_generations_through(compile_view.next_value_generation());
    }
    // On the prepared route the same compile also projects the turn's program,
    // linked against every live prepared binding; on Core this is `None`.
    let retained = session.prepared_retained();
    let visible_names = session
        .workbench_bindings_in(context.placement.lexical_scope)
        .into_iter()
        .map(|binding| binding.name)
        .collect::<Vec<_>>();
    compile_block_off_checkout(
        context,
        source,
        effect_stack,
        block,
        pins,
        compile_view,
        staged_names,
        verdict.as_ref(),
        prologue,
        expression_plan,
        &retained,
        &visible_names,
        None,
    )
}

/// The GHC-compile half of [`compile_block_in_view`], run against an
/// already-resolved `verdict` and one already-taken `compile_view` with no
/// session or checkout held. `retained` and `visible_names` are the exact
/// snapshots [`compile_block_in_view`] (or a split cell's
/// [`reserve_cell_generations`]) took before releasing its checkout; the
/// value generation this compile targets must already be reserved by the
/// caller. Shared by the single-checkout item loop
/// (`prepare_cell_in_session` via `compile_block_in_view`) and the split
/// cell's off-checkout item loop (`compile_cell_items_off_checkout`).
#[allow(clippy::too_many_arguments)]
fn compile_block_off_checkout(
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    effect_stack: &str,
    block: &ParsedBlock,
    pins: Option<&[CheckedBinderPin]>,
    compile_view: crate::ActorCompileView,
    staged_names: &[String],
    verdict: Option<&TurnClassification>,
    prologue: Option<&tidepool_runtime::session::SourcePrologue>,
    expression_plan: Option<&CheckedExpressionPlan>,
    retained: &[(SymbolIdentity, u64)],
    visible_names: &[String],
    candidate_dir: Option<&std::path::Path>,
) -> Result<CompiledBlock, ResidentActorWorkbenchError> {
    // Observation templates embed the checked expression type directly.
    // Bind wrappers receive their pin imports in run_turn_pinned.
    let expression_imports = SourceImports::from_specs(
        expression_plan
            .into_iter()
            .flat_map(|plan| &plan.imports)
            .map(|module| format!("qualified {module}")),
    );
    let compile_view = compile_view.with_workbench_imports(&expression_imports);
    let mut prepared = source.prepare_effectful(&compile_view, effect_stack)?;
    if let Some(prologue) = prologue {
        prepared.preamble = prepared.preamble.replacen(
            "\nmodule ",
            &format!("\n{}module ", prologue.pragma_text()),
            1,
        );
        prepared.preamble = insert_preamble_imports(
            &prepared.preamble,
            &prologue.workbench_imports().template_text(),
        );
    }
    let mut templates =
        resident_workbench_templates(&prepared.preamble, effect_stack, &prepared.imports);
    // A declaration staged for this cell but not yet installed lives only in
    // `candidate_dir` — the shared session root does not have it yet (that
    // only happens at `finalize_cell_install`'s `adopt_staged_declaration_in`).
    // Search it ahead of every other root so `import Tidepool.Session.Lib.G<g>`
    // resolves against the exact candidate this cell's `Decl` item validated,
    // without touching `compile_view`'s own `root` — that stays the real
    // session root, so a later `is_current_for` against a freshly
    // re-derived view is unaffected by this private, per-attempt directory.
    if let Some(candidate_dir) = candidate_dir {
        prepared.include.insert(0, candidate_dir.to_path_buf());
    }
    let include_refs: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
    let mut verdict = verdict.cloned();
    let observation = if verdict
        .as_ref()
        .is_some_and(|verdict| verdict.kind == TurnKind::Expr)
    {
        let mut name = format!("observation{}", compile_view.next_value_generation().0);
        while visible_names.iter().any(|existing| existing == &name)
            || staged_names.iter().any(|staged| staged == &name)
        {
            name.push('_');
        }
        let preamble = insert_preamble_imports(&prepared.preamble, &prepared.imports);
        let lifts = expression_plan
            .map(|plan| vec![plan.lift])
            .unwrap_or_else(|| {
                vec![
                    tidepool_runtime::session::ExpressionLift::Effectful,
                    tidepool_runtime::session::ExpressionLift::Pure,
                ]
            });
        templates = lifts
            .into_iter()
            .map(|lift| tidepool_runtime::session::TurnTemplate {
                kind: tidepool_runtime::session::TemplateSelector::Bind,
                source: tidepool_runtime::session::turn::assemble_observation_module(
                    &preamble,
                    "__result",
                    effect_stack,
                    "{{TURN}}",
                    lift,
                    expression_plan.map(|plan| plan.type_display.as_str()),
                ),
            })
            .collect();
        verdict = Some(TurnClassification {
            kind: TurnKind::Bind,
            binders: vec![name.clone()],
            items: Vec::new(),
        });
        Some((
            name,
            expression_plan.map_or(ExpressionPresentation::Rendered, |plan| plan.presentation),
            expression_plan
                .map(|plan| plan.lift == tidepool_runtime::session::ExpressionLift::Effectful),
        ))
    } else {
        None
    };
    tracing::debug!(
        actor_id = context.actor.id.0,
        incarnation = context.actor.incarnation.0,
        lexical_scope = ?context.placement.lexical_scope,
        generation = compile_view.next_value_generation().0,
        imports = %compile_view.turn_imports(),
        injected = ?prepared.injected,
        source = %block.source,
        "compiling resident actor workbench item"
    );
    let request = TurnRequest {
        exact_context: compile_view.exact_declaration_context().cloned(),
        session_id: Some(compile_view.session_id()),
        turn_text: &block.source,
        templates: &templates,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &prepared.injected,
        gen: compile_view.next_value_generation().0,
        verdict,
        target: None,
        retained_imports: retained,
    };
    let compiled = match pins {
        Some(pins) => run_turn_pinned(request, pins),
        None => run_turn(request),
    };
    match compiled {
        Ok(result) => Ok(CompiledBlock::Ready(Box::new(ReadyBlock {
            result,
            generation: compile_view.next_value_generation(),
            declaration_source: block.source.clone(),
            declaration_imports: compile_view.workbench_imports(),
            observation,
        }))),
        Err(failure) if classify_compile(&failure.error).class == FailureClass::UserHaskell => {
            let label = format!("<cell item {}>", block.ordinal);
            Ok(CompiledBlock::Rejected(render_turn_compile_rejection(
                &failure.error,
                failure.attempted_source.as_deref(),
                &block.source,
                &label,
            )))
        }
        Err(failure) => Err(ResidentActorWorkbenchError::CompileInfrastructure(
            classify_compile(&failure.error),
        )),
    }
}

fn run_status_discovery<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    command: crate::status_tool::StatusDiscovery,
) -> Result<Result<String, String>, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    match command {
        crate::status_tool::StatusDiscovery::Bindings => {
            let bindings = session.workbench_bindings_in(context.placement.lexical_scope);
            let queries = bindings
                .iter()
                .filter_map(|binding| binding.type_query())
                .map(|expression| InspectionQuery::TypeOf(expression.to_owned()))
                .collect::<Vec<_>>();
            let inspected = if queries.is_empty() {
                Vec::new()
            } else {
                inspect_actor_batch(session, context, source, type_modules, &queries)?
            };
            let retained = bindings
                .iter()
                .filter(|binding| binding.type_query().is_some())
                .zip(inspected.iter())
                .filter_map(|(binding, result)| {
                    let Ok(tidepool_runtime::session::InspectionResult::Type { display, .. }) =
                        result
                    else {
                        return None;
                    };
                    Some((
                        binding.name.clone(),
                        binding.defining_generation()?,
                        display.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            session.retain_declaration_value_types_in(context.placement.lexical_scope, &retained);
            let mut inspected = inspected.into_iter();
            let lines = bindings
                .into_iter()
                .map(|binding| {
                    let needs_inspection = binding.type_query().is_some();
                    let kind = binding.kind.label();
                    let rendered = match binding.type_display {
                        Some(type_display) => format!("{} :: {type_display}", binding.name),
                        None if needs_inspection => inspected
                            .next()
                            .and_then(Result::ok)
                            .map(|result| result.render())
                            .unwrap_or_else(|| format!("{} :: <type unavailable>", binding.name)),
                        None => format!("{} :: <type unavailable>", binding.name),
                    };
                    format!("{rendered} [{kind}]")
                })
                .collect::<Vec<_>>();
            Ok(Ok(if lines.is_empty() {
                "no persistent bindings".into()
            } else {
                lines.join("\n")
            }))
        }
        crate::status_tool::StatusDiscovery::Recovery => {
            let Some(report) = session.declaration_recovery_report() else {
                return Ok(Ok("declaration recovery is not configured".into()));
            };
            let mut lines = vec![format!(
                "declaration recovery: source_session={}; successor_session={}; restored={}; unavailable_bindings={}; durability_unconfirmed={}",
                report.source_session,
                report.successor_session,
                report.restored.len(),
                report.unavailable_bindings.len(),
                report.durability_unconfirmed,
            )];
            lines.extend(report.restored.iter().map(|item| {
                format!(
                    "restored generation {} module {}",
                    item.generation, item.module
                )
            }));
            lines.extend(report.unavailable_bindings.iter().map(|item| {
                format!(
                    "unavailable binding {} session {} variable {}",
                    item.name, item.session, item.variable
                )
            }));
            Ok(Ok(lines.join("\n")))
        }
    }
}

fn inspect_actor_batch<H, O>(
    session: &ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    queries: &[InspectionQuery],
) -> Result<
    Vec<Result<tidepool_runtime::session::InspectionResult, String>>,
    ResidentActorWorkbenchError,
>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let compile_view = actor_compile_view(session, context, source, type_modules)?;
    inspect_compile_view(
        &compile_view,
        source,
        queries,
        Some(&context.haskell_effects_alias),
    )
}

fn inspect_compile_view(
    compile_view: &crate::ActorCompileView,
    source: &ActorWorkbenchSource,
    queries: &[InspectionQuery],
    effects: Option<&str>,
) -> Result<
    Vec<Result<tidepool_runtime::session::InspectionResult, String>>,
    ResidentActorWorkbenchError,
> {
    let prepared = match effects {
        Some(effects) => source.prepare_effectful(compile_view, effects)?,
        None => source.prepare(compile_view),
    };
    let include_refs = prepared
        .include
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    match run_inspections(InspectionRequest {
        exact_context: compile_view.exact_declaration_context().cloned(),
        preamble: &prepared.preamble,
        imports: &prepared.imports,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &prepared.injected,
        queries,
        effects,
    }) {
        Ok(results) if results.len() == queries.len() => Ok(results
            .into_iter()
            .map(|result| match result {
                tidepool_runtime::session::InspectionResult::NotFound { .. }
                | tidepool_runtime::session::InspectionResult::ModuleNotFound { .. }
                | tidepool_runtime::session::InspectionResult::Rejected { .. } => {
                    Err(result.render())
                }
                _ => Ok(result),
            })
            .collect()),
        Ok(results) => Err(ResidentActorWorkbenchError::CompileInfrastructure(
            format!(
                "inspection returned {} results for {} queries",
                results.len(),
                queries.len()
            )
            .into(),
        )),
        Err(error) if classify_compile(&error).class == FailureClass::UserHaskell => {
            Ok(vec![Err(classify_compile(&error).message)])
        }
        Err(error) => Err(ResidentActorWorkbenchError::Compile(error)),
    }
}

fn structured_introspection_answer(
    kind: StructuredInspectionKind,
    inspected: Result<tidepool_runtime::session::InspectionResult, String>,
    provenance: tidepool_runtime::session::ScopeProvenance,
    current: tidepool_runtime::session::ScopeProvenance,
) -> StructuredIntrospectionAnswer {
    if current != provenance {
        return StructuredIntrospectionAnswer::ScopeChanged {
            before: provenance,
            after: current,
        };
    }
    match (kind, inspected) {
        (
            StructuredInspectionKind::Info,
            Ok(tidepool_runtime::session::InspectionResult::StructuredInfo(Ok(info))),
        ) => StructuredIntrospectionAnswer::Info(info),
        (
            StructuredInspectionKind::Type,
            Ok(tidepool_runtime::session::InspectionResult::StructuredType(Ok(info))),
        ) => StructuredIntrospectionAnswer::Type(info),
        (
            _,
            Ok(
                tidepool_runtime::session::InspectionResult::StructuredInfo(Err(error))
                | tidepool_runtime::session::InspectionResult::StructuredType(Err(error)),
            ),
        ) => StructuredIntrospectionAnswer::QueryError(error),
        (_, Ok(result)) => StructuredIntrospectionAnswer::CompilerUnavailable(format!(
            "unexpected structured inspection result: {}",
            result.render()
        )),
        (_, Err(detail)) => StructuredIntrospectionAnswer::CompilerUnavailable(detail),
    }
}

fn inspect_lookup_queries(
    view: &crate::ActorCompileView,
    preamble: &str,
    imports: &str,
    include: &[PathBuf],
    injected: &[String],
    effects: &str,
    queries: &[InspectionQuery],
    timing: Option<&crate::call_timing::CallTimingRegistration>,
) -> Result<Vec<tidepool_runtime::session::InspectionResult>, crate::lookup::LookupInspectionError>
{
    if queries.is_empty() {
        return Ok(vec![]);
    }
    let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let inspect = || {
        run_inspections(InspectionRequest {
            exact_context: view.exact_declaration_context().cloned(),
            preamble,
            imports,
            include: &include,
            session_root: view.session_root(),
            inject_modules: injected,
            queries,
            effects: Some(effects),
        })
        .map_err(crate::lookup::LookupInspectionError::Compiler)
    };
    let result = match timing {
        Some(timing) => timing.timed_compile_sync(inspect),
        None => inspect(),
    };
    result
}

#[cfg(test)]
mod lookup_inspection_probe {
    use std::sync::{mpsc, Arc, Mutex, OnceLock};

    struct Probe {
        completed: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    static ACTIVE: OnceLock<Mutex<Option<Arc<Probe>>>> = OnceLock::new();

    pub(super) struct Installed {
        probe: Arc<Probe>,
    }

    impl Drop for Installed {
        fn drop(&mut self) {
            let active = ACTIVE.get_or_init(Default::default);
            let mut active = active.lock().unwrap_or_else(|poison| poison.into_inner());
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.probe))
            {
                *active = None;
            }
        }
    }

    pub(super) fn install() -> (Installed, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel();
        let probe = Arc::new(Probe {
            completed: completed_tx,
            release: Mutex::new(release_rx),
        });
        let active = ACTIVE.get_or_init(Default::default);
        *active.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&probe));
        (Installed { probe }, completed_rx, release_tx)
    }

    pub(super) fn after_request() {
        let active = ACTIVE.get_or_init(Default::default);
        let probe = active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(probe) = probe {
            let _ = probe.completed.send(());
            let _ = probe
                .release
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .recv();
        }
    }
}

fn structured_provenance(
    compile_view: &crate::ActorCompileView,
    source: &ActorWorkbenchSource,
    scope: tidepool_runtime::session::NameScope,
) -> tidepool_runtime::session::ScopeProvenance {
    let prepared = source.prepare(compile_view);
    let generation = compile_view.next_value_generation().0;
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(prepared.preamble.as_bytes());
    fingerprint.update(prepared.imports.as_bytes());
    fingerprint.update(&generation.to_le_bytes());
    for path in &prepared.include {
        fingerprint.update(path.as_os_str().as_encoded_bytes());
        fingerprint.update(&[0]);
    }
    for module in &prepared.injected {
        fingerprint.update(module.as_bytes());
        fingerprint.update(&[0]);
    }
    tidepool_runtime::session::ScopeProvenance {
        scope,
        generation,
        fingerprint: fingerprint.finalize().to_hex().to_string(),
    }
}

fn projected_binding_receipt(
    bound_name: Option<&str>,
    output: &[String],
) -> Result<String, ResidentActorWorkbenchError> {
    let name = bound_name.ok_or_else(|| {
        ResidentActorWorkbenchError::ActorProtocol(
            "projected binding completed without binder metadata".into(),
        )
    })?;
    let mut receipt = format!("bound `{name}`");
    if !output.is_empty() {
        receipt.push_str("\n\nOutput:\n");
        receipt.push_str(&output.join("\n"));
    }
    Ok(receipt)
}

/// Test-only observation of [`ResidentActorWorkbench::prepare_cell`]: a
/// task-local probe (absent outside a test's `scope`) that counts split
/// install checkouts and single-checkout compiles, and can hold the first
/// install checkout until the test has mutated the session.
#[cfg(test)]
mod split_probe {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Default)]
    pub(super) struct SplitProbe {
        /// How many install checkouts, from the first, wait for
        /// `resume_install` after signalling `install_reached`.
        pub(super) held_installs: usize,
        pub(super) install_checkouts: AtomicUsize,
        pub(super) single_checkout_compiles: AtomicUsize,
        pub(super) install_reached: tokio::sync::Notify,
        pub(super) resume_install: tokio::sync::Notify,
        /// How many display-render install checkouts, from the first, wait
        /// for `resume_install` after signalling `install_reached`.
        pub(super) held_display_installs: usize,
        pub(super) display_installs: AtomicUsize,
        /// The value generation each display-render attempt reserved.
        pub(super) display_generations: std::sync::Mutex<Vec<u64>>,
    }

    tokio::task_local! {
        pub(super) static PROBE: Arc<SplitProbe>;
    }

    /// Count this install checkout; hold the first `held_installs` until
    /// each is resumed.
    pub(super) async fn before_install() {
        let Ok(probe) = PROBE.try_with(Arc::clone) else {
            return;
        };
        if probe.install_checkouts.fetch_add(1, Ordering::SeqCst) < probe.held_installs {
            probe.install_reached.notify_one();
            probe.resume_install.notified().await;
        }
    }

    /// Count this display-render install checkout; hold the first
    /// `held_display_installs` until each is resumed.
    pub(super) async fn before_display_install() {
        let Ok(probe) = PROBE.try_with(Arc::clone) else {
            return;
        };
        if probe.display_installs.fetch_add(1, Ordering::SeqCst) < probe.held_display_installs {
            probe.install_reached.notify_one();
            probe.resume_install.notified().await;
        }
    }

    pub(super) fn record_display_generation(generation: u64) {
        PROBE
            .try_with(|probe| {
                probe
                    .display_generations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(generation)
            })
            .ok();
    }

    pub(super) fn single_checkout() {
        PROBE
            .try_with(|probe| {
                probe
                    .single_checkout_compiles
                    .fetch_add(1, Ordering::SeqCst)
            })
            .ok();
    }
}

#[cfg(test)]
mod tool_dispatch_tests {
    use super::*;

    #[test]
    fn dispatch_envelope_preserves_authored_output_and_decodes_typed_refusals() {
        let output = serde_json::json!({"status": "refused", "output": [1, true, null]});
        let reply: ToolDispatchReply = serde_json::from_value(serde_json::json!({
            "status": "success", "output": output,
        }))
        .unwrap();
        assert_eq!(reply.into_output().unwrap(), output);
        for (kind, unknown) in [("unknown_tool", true), ("invalid_input", false)] {
            let mut payload = serde_json::json!({
                "status": "refused", "kind": kind, "error": "correct the call", "tool": "echo",
            });
            if !unknown {
                payload["detail"] = serde_json::json!("invalid argument");
            }
            let reply: ToolDispatchReply = serde_json::from_value(payload).unwrap();
            let error = reply.into_output().unwrap_err();
            assert_eq!(
                matches!(error, ToolDispatchError::UnknownTool { .. }),
                unknown
            );
            assert_eq!(error.to_string(), "correct the call");
            assert_eq!(error.tool(), "echo");
            assert_eq!(serde_json::to_value(error).unwrap()["kind"], kind);
        }
    }

    #[test]
    fn dispatch_envelope_rejects_old_payloads_and_unknown_tags() {
        for payload in [
            serde_json::json!("old naked output"),
            serde_json::json!({"output": "old naked output"}),
            serde_json::json!({"status": "success"}),
            serde_json::json!({"status": "refused", "kind": "invented", "error": "bad"}),
            serde_json::json!({"Right": "accidental Either encoding"}),
            serde_json::json!({"status": "refused", "kind": "unknown_tool", "error": "bad"}),
            serde_json::json!({"status": "refused", "kind": "invalid_input", "error": "bad", "tool": "echo"}),
        ] {
            assert!(serde_json::from_value::<ToolDispatchReply>(payload).is_err());
        }
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;

    fn host_lookup_mount_fixture() -> (
        ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        tempfile::TempDir,
    ) {
        use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
        use tidepool_runtime::session::{ModuleEnv, SessionLib};

        tidepool_testing::eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::notifications_decl(),
            tidepool_mcp::sleep_decl(),
            tidepool_mcp::lookup_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
        let mut include = effects.include_paths().to_vec();
        include.push(tidepool_testing::eval_harness::prelude_path());
        include.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/actors"));
        let preamble = insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "qualified Tidepool.Actors.Exomonad as Exomonad\nqualified Tidepool.Lookup as LookupApi",
        );
        let effects_alias = "'[Exomonad.Notifications, Sleep, Lookup]";
        let session_id = tidepool_repr::SessionId((u64::from(std::process::id()) << 16) | 4_245);
        let session_root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            session_id,
            session_root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("declaration plane")
        .with_validation_include(include.clone());
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let lexical_scope = session.mint_isolated_scope();
        let resource_scope = RealmId::fresh();
        session
            .set_actor_execution(
                tidepool_runtime::session::SessionRunContext {
                    lexical_scope,
                    resource_scope,
                    ..tidepool_runtime::session::SessionRunContext::ROOT
                },
                EffectRunPolicy::HandleOrSuspend,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .expect("actor execution context");
        let context = crate::ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(1)),
            placement: crate::ActorPlacement {
                session: session_id,
                resource_scope,
                lexical_scope,
            },
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: crate::ActorSourceImports::default(),
            haskell_effects_alias: effects_alias.into(),
            source_layer: std::sync::Arc::from([]),
        };
        (
            session,
            context,
            ActorWorkbenchSource::new(preamble, include),
            session_root,
        )
    }

    fn actor_lookup_registry_fixture() -> (
        Arc<ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>>,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        tempfile::TempDir,
    ) {
        let (session, context, source, root) = host_lookup_mount_fixture();
        let machines = Arc::new(ActorMachineRegistry::<
            frunk::HNil,
            tidepool_mcp::CapturedOutput,
        >::new());
        machines.insert_idle(context.placement.session, Box::new(session));
        (machines, context, source, root)
    }

    fn lookup_trace_path() -> PathBuf {
        PathBuf::from(
            std::env::var_os("TIDEPOOL_EXTRACT_DAEMON_LOG")
                .expect("focused lookup proof needs the private compiler daemon trace"),
        )
        .parent()
        .expect("daemon log parent")
        .join("compiler.jsonl")
    }

    fn compiler_trace_events(path: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .expect("read private compiler daemon trace")
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn compiler_request_ids(event: &serde_json::Value) -> Vec<String> {
        event["spans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|span| span["name"] == "compile_request")
            .filter_map(|span| span["compile_request"].as_str().map(str::to_owned))
            .collect()
    }

    fn compiler_trace_started_ids(path: &std::path::Path) -> std::collections::HashSet<String> {
        compiler_trace_events(path)
            .into_iter()
            .filter(|event| event["fields"]["message"] == "compiler request started")
            .flat_map(|event| compiler_request_ids(&event))
            .collect()
    }

    fn compiler_request_event(path: &std::path::Path, request: &str, message: &str) -> bool {
        compiler_trace_events(path).iter().any(|event| {
            event["fields"]["message"] == message
                && compiler_request_ids(event).iter().any(|id| id == request)
        })
    }

    fn wait_for_new_compiler_request(
        path: &std::path::Path,
        before: &std::collections::HashSet<String>,
    ) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(request) = compiler_trace_started_ids(path)
                .difference(before)
                .next()
                .cloned()
            {
                return request;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no compiler request-start event appeared in {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_compiler_request_event(path: &std::path::Path, request: &str, message: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            assert!(
                !compiler_request_event(path, request, "compiler request finished"),
                "compiler request {request} finished before {message}"
            );
            if compiler_request_event(path, request, message) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no {message} event for compiler request {request} appeared in {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn varied_lookup_queries() -> Vec<String> {
        [
            "Int -> Int",
            "Integer -> Integer",
            "Bool -> Bool",
            "Char -> Char",
            "Ordering -> Ordering",
            "Maybe Int -> Maybe Int",
            "Either Int Bool -> Either Int Bool",
            "[Int] -> [Int]",
        ]
        .into_iter()
        .cycle()
        .take(32)
        .map(|ty| format!(":: {ty}"))
        .collect()
    }

    fn real_lookup_request(queries: Vec<String>) -> crate::lookup::LookupRequest {
        crate::lookup::LookupRequest {
            queries,
            discover: false,
            expected_view: None,
            candidate_limit: 128,
            references: vec![],
        }
    }

    fn host_mount_fixture() -> (
        ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        tempfile::TempDir,
    ) {
        host_mount_fixture_with_lib(|_| {})
    }

    fn host_mount_fixture_with_lib(
        configure: impl FnOnce(&mut tidepool_runtime::session::SessionLib),
    ) -> (
        ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        tempfile::TempDir,
    ) {
        use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
        use tidepool_runtime::session::{ModuleEnv, SessionLib};

        tidepool_testing::eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::notifications_decl(),
            tidepool_mcp::sleep_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
        let mut include = effects.include_paths().to_vec();
        include.push(tidepool_testing::eval_harness::prelude_path());
        include.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/actors"));
        let preamble = insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "qualified Tidepool.Actors.Exomonad as Exomonad",
        );
        let effects_alias = "'[Exomonad.Notifications, Sleep]";
        let session_id = tidepool_repr::SessionId((u64::from(std::process::id()) << 16) | 4_244);
        let session_root = tempfile::tempdir().expect("session root");
        let mut lib = SessionLib::open(
            session_id,
            session_root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("declaration plane")
        .with_validation_include(include.clone());
        configure(&mut lib);
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let lexical_scope = session.mint_isolated_scope();
        let resource_scope = RealmId::fresh();
        session
            .set_actor_execution(
                tidepool_runtime::session::SessionRunContext {
                    lexical_scope,
                    resource_scope,
                    ..tidepool_runtime::session::SessionRunContext::ROOT
                },
                EffectRunPolicy::HandleOrSuspend,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .expect("actor execution context");
        let context = crate::ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(1)),
            placement: crate::ActorPlacement {
                session: session_id,
                resource_scope,
                lexical_scope,
            },
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: crate::ActorSourceImports::default(),
            haskell_effects_alias: effects_alias.into(),
            source_layer: std::sync::Arc::from([]),
        };
        (
            session,
            context,
            ActorWorkbenchSource::new(preamble, include),
            session_root,
        )
    }

    #[test]
    fn typed_host_mount_carriers_compile_and_bind() {
        let (mut session, context, source, session_root) = host_mount_fixture();
        let input = mount_json_input(
            &mut session,
            &context,
            &source,
            &[],
            &serde_json::json!({
                "nested": [true, null, {"long": "x".repeat(16 * 1024)}]
            }),
            None,
        )
        .expect("nested JSON carrier mounts");
        assert!(session
            .binding_names_in(context.placement.lexical_scope)
            .iter()
            .all(|name| name != &input.name));

        let mut input_source = source.clone();
        input_source
            .workbench_imports
            .extend_text(&format!("qualified {} as TidepoolHostInput", input.module));
        input_source
            .workbench_imports
            .extend_text("qualified Data.Map as TidepoolHostMap");
        input_source
            .workbench_imports
            .extend_text("qualified Tidepool.Aeson as TidepoolHostJson");
        input_source.preamble = format!(
            "{}\ninput = TidepoolHostInput.{}\n",
            input_source.preamble, input.name
        )
        .into();
        let json_step = begin_fragment(
            &mut session,
            &context,
            &input_source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            ParsedBlock {
                ordinal: 1,
                total: 1,
                source: "jsonSeen <- pure (\\() -> case input of { TidepoolHostJson.Object fields -> if TidepoolHostMap.member \"nested\" fields then (17 :: Int) else 0; _ -> 0 })".into(),
            },
            None,
            None,
        )
        .expect("mounted JSON is executable from the request alias");
        assert!(matches!(json_step, ResidentWorkbenchStep::Committed { .. }));
        let json_display = render_cell_observation(
            &mut session,
            &context,
            &input_source,
            &[],
            "jsonSeen",
            1024,
            &[],
            ExpressionPresentation::Rendered,
        )
        .expect("mounted JSON result renders");
        assert!(json_display.contains("17"), "JSON result: {json_display}");

        mount_text_binding(
            &mut session,
            &context,
            &source,
            &[],
            "tool_result",
            "tool output",
            None,
        )
        .expect("Text carrier mounts after the JSON request executes");
        let mut text_source = source.clone();
        text_source
            .workbench_imports
            .extend_text("qualified Data.Text as TidepoolHostText");
        let text_step = begin_fragment(
            &mut session,
            &context,
            &text_source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            ParsedBlock {
                ordinal: 1,
                total: 1,
                source: "textSeen <- pure (\\() -> TidepoolHostText.unpack tool_result)".into(),
            },
            None,
            None,
        )
        .expect("mounted Text is executable");
        assert!(matches!(text_step, ResidentWorkbenchStep::Committed { .. }));
        let text_display = render_cell_observation(
            &mut session,
            &context,
            &text_source,
            &[],
            "textSeen",
            1024,
            &[],
            ExpressionPresentation::Rendered,
        )
        .expect("mounted Text result renders");
        assert!(
            text_display.contains("tool output"),
            "Text result: {text_display}"
        );
        session.retire_host_binding_owner(session_root.path(), &input.binder);
    }

    /// A split compile takes its `ActorCompileView` snapshot, compiles
    /// off-checkout, then re-derives a fresh view before installing. With no
    /// mutation in between, the split
    /// path must publish the compiled Text binding.
    #[test]
    fn text_carrier_split_compile_then_install_publishes_the_binding() {
        let (mut session, context, source, _session_root) = host_mount_fixture();
        let scope = context.placement.lexical_scope;

        let view = actor_compile_view(&session, &context, &source, &[]).expect("compile view");
        let generation = view.next_value_generation();
        session.reserve_value_generations_through(generation);
        let retained = session.prepared_retained();

        let (binder, compiled) = compile_host_binding_off_checkout(
            &view,
            &source,
            &context.haskell_effects_alias,
            generation,
            "text_binding",
            TEXT_BINDING_TYPE_NAME,
            TEXT_BINDING_ANCHOR,
            text_binding_carrier_imports(),
            true,
            &retained,
        )
        .expect("Text carrier compiles off-checkout");

        let fresh_view = actor_compile_view(&session, &context, &source, &[]).expect("fresh view");
        assert!(
            fresh_view.is_current_for(&view),
            "no mutation happened between snapshot and install: views must still match"
        );

        session
            .mount_text_binding_in(
                scope,
                &binder,
                generation,
                compiled.into_code(),
                "text payload",
            )
            .expect("Text carrier installs after revalidation");

        assert!(session
            .binding_names_in(scope)
            .contains(&"text_binding".into()));
    }

    /// A write to the same scope between a split compile's snapshot and its
    /// re-checkout (here, another mount standing in for a concurrent
    /// actor's install) must be detected by `is_current_for` before
    /// installing the stale compile, and a fresh snapshot must still recover.
    #[test]
    fn a_mutation_between_split_checkouts_invalidates_the_snapshot_and_blocks_install() {
        let (mut session, context, source, _session_root) = host_mount_fixture();
        let scope = context.placement.lexical_scope;

        let view = actor_compile_view(&session, &context, &source, &[]).expect("compile view");
        let generation = view.next_value_generation();
        session.reserve_value_generations_through(generation);
        let retained = session.prepared_retained();

        let (binder, compiled) = compile_host_binding_off_checkout(
            &view,
            &source,
            &context.haskell_effects_alias,
            generation,
            "text_binding",
            TEXT_BINDING_TYPE_NAME,
            TEXT_BINDING_ANCHOR,
            text_binding_carrier_imports(),
            true,
            &retained,
        )
        .expect("Text carrier compiles off-checkout");

        // Stand in for another actor writing to this exact scope while this
        // compile ran off-checkout: mount an unrelated Text carrier, which
        // changes `visible_values`/`shadowing`.
        mount_text_binding(
            &mut session,
            &context,
            &source,
            &[],
            "interloper",
            "interloper text",
            None,
        )
        .expect("interloping carrier mounts");

        let fresh_view = actor_compile_view(&session, &context, &source, &[]).expect("fresh view");
        assert!(
            !fresh_view.is_current_for(&view),
            "an interleaved mutation to the same scope must invalidate the snapshot"
        );

        // The split path must refuse to install against a stale view — the
        // binder never reaches the persistent binding store.
        assert!(session
            .workbench_bindings_in(scope)
            .into_iter()
            .all(|binding| binding.name != "text_binding"));
        drop((binder, compiled, generation));

        // A fresh snapshot recompiles and installs cleanly.
        let retry_view = actor_compile_view(&session, &context, &source, &[]).expect("retry view");
        let retry_generation = retry_view.next_value_generation();
        session.reserve_value_generations_through(retry_generation);
        let retry_retained = session.prepared_retained();
        let (retry_binder, retry_compiled) = compile_host_binding_off_checkout(
            &retry_view,
            &source,
            &context.haskell_effects_alias,
            retry_generation,
            "text_binding2",
            TEXT_BINDING_TYPE_NAME,
            TEXT_BINDING_ANCHOR,
            text_binding_carrier_imports(),
            true,
            &retry_retained,
        )
        .expect("recompile against the fresh snapshot succeeds");
        session
            .mount_text_binding_in(
                scope,
                &retry_binder,
                retry_generation,
                retry_compiled.into_code(),
                "retry text payload",
            )
            .expect("recompiled carrier installs");
        assert!(session
            .binding_names_in(scope)
            .contains(&"text_binding2".into()));
    }

    /// Named-tool command output bindings reuse one cached Job carrier.
    #[tokio::test]
    async fn two_bind_command_job_calls_on_one_workbench_compile_the_job_carrier_once() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);

        let before_first = tidepool_extract_cmd::extract_spawn_count();
        let first = workbench
            .bind_command_job(context.clone(), "job one".into())
            .await
            .expect("the first job binds, building the Job carrier");
        let after_first = tidepool_extract_cmd::extract_spawn_count();
        assert!(
            after_first > before_first,
            "the first bind_command_job call must compile the Job carrier: \
             before={before_first} after={after_first}"
        );

        let second = workbench
            .bind_command_job(context.clone(), "job two".into())
            .await
            .expect("the second job binds through the cached carrier");
        let after_second = tidepool_extract_cmd::extract_spawn_count();
        assert_eq!(
            after_second, after_first,
            "the second bind_command_job call must mount through the cached carrier with no \
             further extractor call: first={first} second={second}"
        );
        assert_ne!(first, second);
        let repeated = workbench
            .bind_command_job(context.clone(), "job one".into())
            .await
            .expect("the same command job reuses its existing binder");
        assert_eq!(repeated, first);
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), after_second);
        workbench
            .access
            .with_machine(context, move |session, context, _| {
                let scope = context.placement.lexical_scope;
                assert_eq!(session.host_text_binding_in(scope, "job one"), Some(first));
                assert_eq!(session.host_text_binding_in(scope, "job two"), Some(second));
                Ok(())
            })
            .await
            .expect("both retained command bindings identify their exact jobs");
    }

    /// Repeated tool-result mounts reuse the Text carrier built for this lineage.
    #[tokio::test]
    async fn two_tool_result_mounts_on_one_workbench_compile_the_text_carrier_once() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);

        let before_first = tidepool_extract_cmd::extract_spawn_count();
        workbench
            .bind_tool_result(context.clone(), "tool_one".into(), "output one".into())
            .await
            .expect("the first tool result binds, building the Text carrier");
        let after_first = tidepool_extract_cmd::extract_spawn_count();
        assert!(
            after_first > before_first,
            "the first mount must compile the Text carrier: before={before_first} after={after_first}"
        );

        workbench
            .bind_tool_result(context.clone(), "tool_two".into(), "output two".into())
            .await
            .expect("the second tool result binds through the cached carrier");
        let after_second = tidepool_extract_cmd::extract_spawn_count();
        assert_eq!(
            after_second, after_first,
            "the second mount must reuse the cached carrier without another extractor call"
        );
        workbench
            .access
            .with_machine(context, |session, context, _| {
                let names = session.binding_names_in(context.placement.lexical_scope);
                assert!(names.contains(&"tool_one".into()));
                assert!(names.contains(&"tool_two".into()));
                Ok(())
            })
            .await
            .expect("both tool results remain bound");
    }

    /// For an ordinary workbench fragment (`begin_fragment`/`begin_ready_block`),
    /// snapshotting via `snapshot_fragment_compile`, compiling off-checkout via
    /// `compile_fragment_off_checkout`, revalidating, then installing and
    /// running via `begin_ready_block` must commit the exact same output and
    /// bindings as calling the original single-checkout `begin_fragment`
    /// directly against an identically-constructed session.
    #[test]
    fn ordinary_fragment_split_compile_then_install_matches_single_checkout_begin_fragment() {
        let (mut split_session, split_context, split_source, _split_root) = host_mount_fixture();
        let (mut direct_session, direct_context, direct_source, _direct_root) =
            host_mount_fixture();

        let block = ParsedBlock {
            ordinal: 1,
            total: 1,
            source: "x <- pure (1 :: Int)".into(),
        };
        let verdict = TurnClassification {
            kind: TurnKind::Bind,
            binders: vec!["x".into()],
            items: Vec::new(),
        };

        let snapshot = snapshot_fragment_compile(
            &mut split_session,
            &split_context,
            &split_source,
            &[],
            &block,
            Some(&verdict),
        )
        .expect("fragment compile snapshot");
        let compiled = compile_fragment_off_checkout(
            &snapshot,
            &split_source,
            &split_context.haskell_effects_alias,
            &block,
        )
        .expect("fragment compiles off-checkout");
        let ready = match compiled {
            CompiledBlock::Ready(ready) => *ready,
            CompiledBlock::Rejected(diagnostic) => {
                panic!("fragment unexpectedly rejected: {diagnostic:?}")
            }
        };

        let fresh_view = actor_compile_view(&split_session, &split_context, &split_source, &[])
            .expect("fresh view");
        assert!(
            fresh_view.is_current_for(&snapshot.view),
            "no mutation happened between snapshot and install: views must still match"
        );

        let split_step = begin_ready_block(
            &mut split_session,
            &split_context,
            &split_source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            block.clone(),
            ready,
            8192,
        )
        .expect("split compile installs and runs");

        let direct_step = begin_fragment(
            &mut direct_session,
            &direct_context,
            &direct_source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            block,
            None,
            Some(&verdict),
        )
        .expect("single-checkout begin_fragment installs and runs");

        let ResidentWorkbenchStep::Committed {
            output: split_output,
            installed_bindings: split_bindings,
            ..
        } = split_step
        else {
            panic!("split compile-then-install did not commit");
        };
        let ResidentWorkbenchStep::Committed {
            output: direct_output,
            installed_bindings: direct_bindings,
            ..
        } = direct_step
        else {
            panic!("single-checkout begin_fragment did not commit");
        };
        assert_eq!(split_output, direct_output);
        assert_eq!(split_bindings, direct_bindings);
    }

    /// The same split shape as `ordinary_fragment_split_compile_then_install_
    /// matches_single_checkout_begin_fragment`, but for a display bundle's
    /// own compile. `render_observation_off_checkout` chains, in order,
    /// `snapshot_display_compile` (checkout), then
    /// `observation_display_block` and `compile_block_off_checkout` (no
    /// checkout), then `display_bundle_binders` and
    /// `snapshot_display_bundle` (checkout), the Cranelift compile (no
    /// checkout), then `revalidate_and_run_display_bundle` (checkout, to
    /// install and execute the page), then `decode_display_bundle`. Run directly
    /// against those same building blocks, this must render the exact same
    /// text as the single-checkout `render_cell_observation` against an
    /// identically-constructed session, the invariant
    /// `render_cell_observation`'s own doc comment states: the observation
    /// type is known at check time, so a display bundle's compile needs no
    /// checkout at all, only its install/execute does.
    #[test]
    fn display_bundle_split_compile_then_install_matches_single_checkout_render_cell_observation() {
        let (mut split_session, split_context, split_source, _split_root) = host_mount_fixture();
        let (mut direct_session, direct_context, direct_source, _direct_root) =
            host_mount_fixture();

        for (session, context, source) in [
            (&mut split_session, &split_context, &split_source),
            (&mut direct_session, &direct_context, &direct_source),
        ] {
            let bind_step = begin_fragment(
                session,
                context,
                source,
                RequestWorkbenchScope {
                    response: None,
                    request: None,
                    type_modules: &[],
                },
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "displaySplitSeen <- pure (\\() -> (42 :: Int))".into(),
                },
                None,
                None,
            )
            .expect("observed value binds");
            assert!(matches!(bind_step, ResidentWorkbenchStep::Committed { .. }));
        }

        let snapshot =
            snapshot_display_compile(&mut split_session, &split_context, &split_source, &[])
                .expect("display compile snapshot");
        let generation = snapshot.view.next_value_generation().0;
        let page_name = format!("__tidepoolPage{generation}");
        let metadata_name = format!("__tidepoolDisplayMetadata{generation}");
        let block = observation_display_block(
            &page_name,
            &metadata_name,
            &split_context.haskell_effects_alias,
            "displaySplitSeen",
            1024,
            &[],
            ExpressionPresentation::Rendered,
        );
        let compiled = compile_block_off_checkout(
            &split_context,
            &split_source,
            &split_context.haskell_effects_alias,
            &block,
            None,
            snapshot.view.clone(),
            &[],
            Some(&generated_binds_verdict(&[
                page_name.clone(),
                metadata_name.clone(),
                "cellDisplay".into(),
            ])),
            None,
            None,
            &snapshot.retained,
            &snapshot.visible_names,
            None,
        )
        .expect("display bundle compiles off-checkout");
        let ready = match compiled {
            CompiledBlock::Ready(ready) => *ready,
            CompiledBlock::Rejected(diagnostic) => {
                panic!("display bundle unexpectedly rejected: {diagnostic:?}")
            }
        };

        let fresh_view = actor_compile_view(&split_session, &split_context, &split_source, &[])
            .expect("fresh view");
        assert!(
            fresh_view.is_current_for(&snapshot.view),
            "no mutation happened between snapshot and install: views must still match"
        );

        let TurnResult::Bind {
            bound, compiled, ..
        } = ready.result
        else {
            panic!("display bundle must be a bind turn");
        };
        assert!(
            compiled.certification.is_some(),
            "display bundle must retain its worker-certified native owners"
        );
        let (page, metadata, cell_display) =
            display_bundle_binders(&bound, &page_name, &metadata_name)
                .expect("expected page/metadata/cellDisplay binder shape");
        let mut page = page.clone();
        let metadata = metadata.clone();
        let mut cell_display = cell_display.clone();
        let original_page_id = page.var_id;
        let original_alias_id = cell_display.var_id;
        // The Cranelift half off-checkout too: snapshot the install, compile
        // with no session borrowed, then revalidate, install and run.
        assert!(split_session.prepared_machine_ready());
        let pending = split_session
            .snapshot_display_bundle(
                cloned_turn_code(&compiled),
                &page,
                &metadata,
                &cell_display,
                ready.generation,
            )
            .expect("display install snapshot");
        // Caller-owned rows can change after capture; installation uses the
        // original rows retained inside the capsule.
        page.name = "foreignCapsulePage".into();
        page.var_id += 1;
        cell_display.name = "foreignCapsuleAlias".into();
        cell_display.var_id += 1;
        let program = pending
            .compile_off_checkout()
            .expect("display bundle compiles off-checkout");
        let bundle = split_session
            .revalidate_and_run_display_bundle(program)
            .expect("display bundle runs")
            .expect("nothing changed between snapshot and install");
        let public = split_session
            .public_visibility_snapshot_in(split_context.placement.lexical_scope)
            .expect("installed display scope");
        assert!(public.bindings.contains(&(
            page_name.clone(),
            tidepool_repr::SessionVarId::from_extract(original_page_id),
        )));
        assert!(public.bindings.contains(&(
            "cellDisplay".into(),
            tidepool_repr::SessionVarId::from_extract(original_alias_id),
        )));
        assert!(!public
            .bindings
            .iter()
            .any(|(name, _)| { name == &page.name || name == &cell_display.name }));
        let split_output =
            decode_display_bundle(&bundle, "displaySplitSeen").expect("display bundle decodes");

        let direct_output = render_cell_observation(
            &mut direct_session,
            &direct_context,
            &direct_source,
            &[],
            "displaySplitSeen",
            1024,
            &[],
            ExpressionPresentation::Rendered,
        )
        .expect("single-checkout render succeeds");

        assert_eq!(split_output, direct_output);
        assert!(split_output.contains("42"), "{split_output}");
    }

    /// A write to the same scope between an ordinary fragment's split-compile
    /// snapshot and its re-checkout (here, another mount standing in for a
    /// concurrent actor's install) must be detected by `is_current_for`
    /// before installing the stale compile, and a fresh snapshot must still
    /// recompile and install cleanly — the same invariant
    /// `a_mutation_between_split_checkouts_invalidates_the_snapshot_and_
    /// blocks_install` proves for the Text carrier path.
    #[test]
    fn a_mutation_between_split_fragment_checkouts_invalidates_the_snapshot_and_forces_a_recompile()
    {
        let (mut session, context, source, _session_root) = host_mount_fixture();
        let scope = context.placement.lexical_scope;

        let block = ParsedBlock {
            ordinal: 1,
            total: 1,
            source: "x <- pure (1 :: Int)".into(),
        };
        let verdict = TurnClassification {
            kind: TurnKind::Bind,
            binders: vec!["x".into()],
            items: Vec::new(),
        };

        let snapshot =
            snapshot_fragment_compile(&mut session, &context, &source, &[], &block, Some(&verdict))
                .expect("fragment compile snapshot");
        let compiled = compile_fragment_off_checkout(
            &snapshot,
            &source,
            &context.haskell_effects_alias,
            &block,
        )
        .expect("fragment compiles off-checkout");
        let ready = match compiled {
            CompiledBlock::Ready(ready) => *ready,
            CompiledBlock::Rejected(diagnostic) => {
                panic!("fragment unexpectedly rejected: {diagnostic:?}")
            }
        };

        // Stand in for another actor writing to this exact scope while this
        // compile ran off-checkout: mount an unrelated Text carrier, which
        // changes `visible_values`/`shadowing`.
        mount_text_binding(
            &mut session,
            &context,
            &source,
            &[],
            "interloper",
            "interloper text",
            None,
        )
        .expect("interloping carrier mounts");

        let fresh_view = actor_compile_view(&session, &context, &source, &[]).expect("fresh view");
        assert!(
            !fresh_view.is_current_for(&snapshot.view),
            "an interleaved mutation to the same scope must invalidate the snapshot"
        );

        // The split path must refuse to install against a stale view — no
        // binding named by the compiled fragment ever reaches the workbench.
        assert!(session
            .workbench_bindings_in(scope)
            .into_iter()
            .all(|binding| binding.name != "x"));
        drop(ready);

        // A fresh snapshot recompiles and installs cleanly.
        let retry_snapshot =
            snapshot_fragment_compile(&mut session, &context, &source, &[], &block, Some(&verdict))
                .expect("retry snapshot");
        let retry_compiled = compile_fragment_off_checkout(
            &retry_snapshot,
            &source,
            &context.haskell_effects_alias,
            &block,
        )
        .expect("retry compiles off-checkout");
        let retry_ready = match retry_compiled {
            CompiledBlock::Ready(ready) => *ready,
            CompiledBlock::Rejected(diagnostic) => {
                panic!("retry compile unexpectedly rejected: {diagnostic:?}")
            }
        };
        let retry_fresh_view =
            actor_compile_view(&session, &context, &source, &[]).expect("retry fresh view");
        assert!(retry_fresh_view.is_current_for(&retry_snapshot.view));

        let step = begin_ready_block(
            &mut session,
            &context,
            &source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            block,
            retry_ready,
            8192,
        )
        .expect("retry installs and runs");
        let ResidentWorkbenchStep::Committed {
            installed_bindings, ..
        } = step
        else {
            panic!("retry did not commit");
        };
        assert_eq!(installed_bindings, vec!["x".to_string()]);
    }

    fn introspection_error_table() -> DataConTable {
        use tidepool_repr::{DataCon, DataConId};
        let mut table = tidepool_test_data::standard_datacon_table();
        for (id, name, arity) in [
            (100, "Left", 1),
            (101, "CurrentScope", 0),
            (102, "ScopeProvenance", 3),
            (103, "CompilerUnavailable", 1),
            (104, "ScopeChanged", 2),
        ] {
            table.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: Vec::new(),
                qualified_name: Some(if name == "Left" {
                    "Data.Either.Left".into()
                } else {
                    format!("Tidepool.Effects.Core.{name}")
                }),
                type_name: String::new(),
            });
        }
        table
    }

    fn current_provenance(
        generation: u64,
        fingerprint: &str,
    ) -> tidepool_runtime::session::ScopeProvenance {
        tidepool_runtime::session::ScopeProvenance {
            scope: tidepool_runtime::session::NameScope::Current,
            generation,
            fingerprint: fingerprint.into(),
        }
    }

    #[test]
    fn agent_forget_projection_collects_nested_retained_ids_and_rejects_overflow() {
        use tidepool_repr::{DataCon, DataConId};
        let mut table = tidepool_test_data::standard_datacon_table();
        for (id, name, arity) in [
            (120, "AgentForgetRetained", 2),
            (121, "AgentForgotten", 0),
            (122, "AgentForgetOutputPending", 1),
            (123, "CleanupActorOutputPending", 3),
        ] {
            table.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: Vec::new(),
                qualified_name: Some(format!("Tidepool.Effects.Core.{name}")),
                type_name: String::new(),
            });
        }
        let answer = AgentForgetProjection::Retained {
            requests: vec![crate::RequestId(7), crate::RequestId(9)],
            watches: vec![crate::WatchId(11)],
        }
        .to_value(&table)
        .unwrap();
        assert!(matches!(
            answer,
            HaskellValue::Con(DataConId(120), ref fields) if fields.len() == 2
        ));
        assert!(matches!(
            AgentForgetProjection::Forgotten.to_value(&table).unwrap(),
            HaskellValue::Con(DataConId(121), ref fields) if fields.is_empty()
        ));
        assert!(matches!(
            AgentForgetProjection::OutputPending { displays: 2 }.to_value(&table).unwrap(),
            HaskellValue::Con(DataConId(122), ref fields) if fields.len() == 1
                && <i64 as tidepool_bridge::FromHaskell>::from_value(&fields[0], &table).unwrap() == 2
        ));
        assert!(matches!(
            CleanupStepProjection::ActorOutputPending {
                actor: crate::ActorRef::first(crate::ActorId(37)), displays: 2,
            }.to_value(&table).unwrap(),
            HaskellValue::Con(DataConId(123), ref fields)
                if fields.iter().map(|field| <i64 as tidepool_bridge::FromHaskell>::from_value(field, &table).unwrap())
                    .collect::<Vec<_>>() == vec![37, 1, 2]
        ));

        let error = AgentForgetProjection::Retained {
            requests: vec![crate::RequestId(u64::MAX)],
            watches: Vec::new(),
        }
        .to_value(&table)
        .unwrap_err();
        assert!(matches!(error, BridgeError::UnsupportedType(_)));
    }

    #[test]
    fn usage_projection_rejects_inconsistent_provider_totals_before_publication() {
        struct Sink;
        impl HaskellVisitor for Sink {
            fn begin_constructor(
                &mut self,
                _id: tidepool_repr::DataConId,
                _fields: usize,
            ) -> Result<(), BridgeError> {
                Ok(())
            }
            fn end_constructor(&mut self) -> Result<(), BridgeError> {
                Ok(())
            }
            fn literal(&mut self, _literal: tidepool_repr::Literal) -> Result<(), BridgeError> {
                Ok(())
            }
            fn byte_array(&mut self, _bytes: Vec<u8>) -> Result<(), BridgeError> {
                Ok(())
            }
        }

        let summary = exomonad_model::ProviderUsageSummary {
            scope: exomonad_model::ProviderUsageScope::Thread("thread".into()),
            completeness: exomonad_model::ProviderUsageCompleteness::Complete,
            observations: 1,
            usage: exomonad_model::TokenUsage {
                input_tokens: 2,
                cached_input_tokens: 3,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: 2,
            },
        };
        let error = visit_usage_summary(&DataConTable::new(), &mut Sink, Some(&summary))
            .expect_err("cached tokens cannot exceed total input tokens");
        assert!(matches!(
            error,
            BridgeError::UnsupportedType(ref detail)
                if detail == "cached input tokens exceed input tokens"
        ));
    }

    #[test]
    fn usage_projection_emits_every_summary_field() {
        use tidepool_repr::{DataCon, DataConId};

        struct UsageSummary(Option<exomonad_model::ProviderUsageSummary>);
        impl tidepool_bridge::sealed::ToHaskellSealed for UsageSummary {}
        impl ToHaskell for UsageSummary {
            fn visit(
                &self,
                table: &DataConTable,
                visitor: &mut dyn HaskellVisitor,
            ) -> Result<(), BridgeError> {
                visit_usage_summary(table, visitor, self.0.as_ref())
            }
        }

        let mut table = tidepool_test_data::standard_datacon_table();
        for (id, name, arity) in [
            (130, "UsageThread", 1),
            (131, "UsageComplete", 0),
            (132, "ProviderUsageSummary", 8),
        ] {
            table.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: Vec::new(),
                qualified_name: Some(format!("Tidepool.Effects.Core.{name}")),
                type_name: String::new(),
            });
        }
        let summary = exomonad_model::ProviderUsageSummary {
            scope: exomonad_model::ProviderUsageScope::Thread("thread".into()),
            completeness: exomonad_model::ProviderUsageCompleteness::Complete,
            observations: 2,
            usage: exomonad_model::TokenUsage {
                input_tokens: 13,
                cached_input_tokens: 5,
                output_tokens: 3,
                reasoning_output_tokens: 2,
                total_tokens: 18,
            },
        };
        let value = UsageSummary(Some(summary)).to_value(&table).unwrap();
        assert!(matches!(
            value,
            HaskellValue::Con(_, ref maybe_fields)
                if matches!(maybe_fields.as_slice(), [HaskellValue::Con(DataConId(132), summary_fields)] if summary_fields.len() == 8)
        ));
    }

    #[test]
    fn actor_context_projection_emits_the_schema_field_count() {
        use tidepool_codegen::scope::ScopeId;
        use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
        use tidepool_repr::{DataCon, DataConId, SessionId};

        let mut table = tidepool_test_data::standard_datacon_table();
        for (id, name, arity) in [
            (140, "ActorContextInfo", 23),
            (141, "ContextCoding", 0),
            (142, "NativeCoding", 0),
            (143, "WorkspaceWritableBound", 0),
            (144, "ActivationRootStarted", 0),
        ] {
            table.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: Vec::new(),
                qualified_name: Some(format!("Tidepool.Effects.Core.{name}")),
                type_name: String::new(),
            });
        }
        let placement = crate::ActorPlacement {
            session: SessionId(1),
            resource_scope: RealmId::ROOT,
            lexical_scope: ScopeId(1),
        };
        let context = crate::ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(1)),
            placement,
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: crate::ActorSourceImports::default(),
            haskell_effects_alias: "'[]".into(),
            source_layer: std::sync::Arc::from([]),
        };
        let projection = ActorContextProjection {
            context,
            descriptor: crate::ActorDescriptor::new("root", placement),
            bound_worktree: None,
            runtime: crate::ActorRuntimeObservation::default(),
        };
        let value = projection.to_value(&table).unwrap();
        assert!(matches!(
            value,
            HaskellValue::Con(DataConId(140), ref fields) if fields.len() == 23
        ));
    }

    #[test]
    fn collected_introspection_projection_rejects_missing_nominal_constructor() {
        use tidepool_repr::{DataCon, DataConId};
        let mut table = tidepool_test_data::standard_datacon_table();
        table.insert(DataCon {
            id: DataConId(100),
            name: "Left".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: Vec::new(),
            qualified_name: Some("Data.Either.Left".into()),
            type_name: String::new(),
        });
        let provenance = current_provenance(1, "same");
        let error = structured_introspection_answer(
            StructuredInspectionKind::Info,
            Err("compiler unavailable".into()),
            provenance.clone(),
            provenance,
        )
        .to_value(&table)
        .unwrap_err();
        assert!(matches!(
            error,
            BridgeError::UnknownDataConName(ref name)
                if name == "Tidepool.Effects.Core.CompilerUnavailable"
        ));
    }

    #[test]
    fn structured_introspection_compiler_failure_is_a_typed_left() {
        use tidepool_repr::DataConId;
        let table = introspection_error_table();
        let provenance = current_provenance(7, "same");
        let answer = structured_introspection_answer(
            StructuredInspectionKind::Info,
            Err("ghc unavailable".into()),
            provenance.clone(),
            provenance,
        )
        .to_value(&table)
        .unwrap();
        assert!(matches!(
            answer,
            HaskellValue::Con(DataConId(100), ref fields)
                if matches!(fields.as_slice(), [HaskellValue::Con(DataConId(103), detail)] if detail.len() == 1)
        ));
    }

    #[test]
    fn structured_introspection_generation_change_is_a_typed_left() {
        use tidepool_repr::DataConId;
        let table = introspection_error_table();
        let before = current_provenance(7, "before");
        let after = current_provenance(8, "after");
        let answer = structured_introspection_answer(
            StructuredInspectionKind::Type,
            Err("compiler result must be discarded".into()),
            before,
            after,
        )
        .to_value(&table)
        .unwrap();
        assert!(matches!(
            answer,
            HaskellValue::Con(DataConId(100), ref fields)
                if matches!(fields.as_slice(), [HaskellValue::Con(DataConId(104), changed)] if changed.len() == 2)
        ));
    }
    #[test]
    fn matched_request_with_invalid_deadline_is_not_skipped_by_dispatch() {
        use tidepool_repr::{DataCon, DataConId};
        let mut table = tidepool_test_data::standard_datacon_table();
        let submit = DataConId(100);
        table.insert(DataCon {
            id: submit,
            name: "SubmitRequestWith".into(),
            tag: 1,
            rep_arity: 4,
            field_bangs: Vec::new(),
            qualified_name: Some("Tidepool.Agent.Reply.Internal.SubmitRequestWith".into()),
            type_name: "Replies".into(),
        });
        let request = HaskellValue::Con(
            submit,
            vec![
                1_i64.to_value(&table).unwrap(),
                HaskellValue::Con(DataConId(999), vec![]),
                (2_i64, 1_i64).to_value(&table).unwrap(),
                Some(HaskellValue::Con(DataConId(998), vec![]))
                    .to_value(&table)
                    .unwrap(),
            ],
        );
        assert!(matches!(
            ResidentRequest::decode(&request, &table),
            Err(ResidentActorWorkbenchError::RequestDecode {
                source: BridgeError::FieldDecode { field: 4, .. },
                ..
            })
        ));
    }

    fn describe_step(step: &ResidentWorkbenchStep) -> String {
        match step {
            ResidentWorkbenchStep::Committed { output, .. } => format!("Committed({output})"),
            ResidentWorkbenchStep::Rejected(rejection) => {
                format!("Rejected({})", rejection.output)
            }
            ResidentWorkbenchStep::Running { .. } => "Running".into(),
            ResidentWorkbenchStep::Replied { .. } => "Replied".into(),
            ResidentWorkbenchStep::CancellationAcknowledged { .. } => {
                "CancellationAcknowledged".into()
            }
        }
    }
    /// The same-cell shape from a live Exomonad session (2026-09-17): one cell
    /// that both RE-DECLARES a name and USES it from a bind statement in
    /// that SAME cell. `notebook_cells_on`'s per-statement `run` closure
    /// drives each text through `begin_fragment` as its own independent
    /// "cell" — which is exactly why it cannot catch this: the
    /// redeclaration and its use never share one whole-cell preflight check
    /// there. This test drives the real `prepare_cell` machinery instead —
    /// `actor_compile_view` + `cell_module_preamble` +
    /// `resident_cell_check_template` + `check_cell` —
    /// exactly as `ResidentActorWorkbench::prepare_cell` assembles them,
    /// without the registry/actor-runner scaffolding that method also needs.
    #[test]
    fn same_cell_redeclaration_and_use_needs_hiding_to_resolve() {
        use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
        use tidepool_runtime::session::{ModuleEnv, SessionLib};

        tidepool_testing::eval_harness::require_extract();
        let declarations = [tidepool_mcp::notifications_decl()];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
        let mut include = effects.include_paths().to_vec();
        include.push(tidepool_testing::eval_harness::prelude_path());
        include.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/actors"));
        let preamble = insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "qualified Tidepool.Actors.Exomonad as Exomonad",
        );
        let effects_alias = "'[Exomonad.Notifications]";
        let session_id = tidepool_repr::SessionId((u64::from(std::process::id()) << 16) | 4_243);
        let session_root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            session_id,
            session_root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("declaration plane")
        .with_validation_include(include.clone());
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let lexical_scope = session.mint_isolated_scope();
        let resource_scope = RealmId::fresh();
        session
            .set_actor_execution(
                tidepool_runtime::session::SessionRunContext {
                    lexical_scope,
                    resource_scope,
                    ..tidepool_runtime::session::SessionRunContext::ROOT
                },
                EffectRunPolicy::HandleOrSuspend,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .expect("actor execution context");
        let context = crate::ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(1)),
            placement: crate::ActorPlacement {
                session: session_id,
                resource_scope,
                lexical_scope,
            },
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: crate::ActorSourceImports::default(),
            haskell_effects_alias: effects_alias.into(),
            source_layer: std::sync::Arc::from([]),
        };
        let source = ActorWorkbenchSource::new(preamble, include);

        // Cell 1: declare `sh`, as its own earlier cell — exactly the
        // "generation 7" of the live incident.
        let step = begin_fragment(
            &mut session,
            &context,
            &source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            ParsedBlock {
                ordinal: 1,
                total: 1,
                source: "sh args = length (args :: [Int])".into(),
            },
            None,
            None,
        )
        .unwrap_or_else(|error| panic!("cell 1 (define sh): {error}"));
        assert!(
            matches!(step, ResidentWorkbenchStep::Committed { .. }),
            "cell 1 must commit: {}",
            describe_step(&step)
        );

        // Cell 2: the exact friction shape — RE-DECLARE `sh` AND use it from
        // a bind statement, both in the SAME cell text (the corrected
        // resubmission after a partial failure, in the live incident).
        let cell_2 = "sh args = 2 * length (args :: [Int])\nrecentA <- pure (sh [1, 2, 3])";

        let candidate_module = session
            .next_declaration_module()
            .expect("resident session has a declaration plane");
        let compile_view =
            actor_compile_view(&session, &context, &source, &[]).expect("compile view");
        let prepared = source.prepare(&compile_view);
        let check_preamble =
            cell_module_preamble(&prepared.preamble, &candidate_module.module_name())
                .expect("preamble names a module");
        let template = resident_cell_check_template(
            &check_preamble,
            &context.haskell_effects_alias,
            &prepared.imports,
        );
        let include_refs: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
        fn request<'a>(
            cell_2: &'a str,
            template: &'a str,
            include_refs: &'a [&'a std::path::Path],
            compile_view: &'a crate::ActorCompileView,
            prepared: &'a WorkbenchCompilation,
        ) -> CellCheckRequest<'a> {
            CellCheckRequest {
                exact_context: compile_view.exact_declaration_context().cloned(),
                session_id: None,
                cell_text: cell_2,
                template,
                include: include_refs,
                session_root: compile_view.session_root(),
                inject_modules: &prepared.injected,
                compile_generation: compile_view.next_value_generation().0,
                compile_view_evidence: "",
            }
        }

        // Without hiding, this reproduces exactly today's live-session
        // failure: GHC reports the redeclared `sh` ambiguous between the
        // cell's own fresh declaration and the unqualified import of the
        // current generation built before this cell's redeclaration was
        // known.
        let failure = check_cell(request(
            cell_2,
            &template,
            &include_refs,
            &compile_view,
            &prepared,
        ))
        .expect_err("without hiding, the same-cell redeclaration is still ambiguous today");
        let message = tidepool_runtime::session::render_cell_compile_error(&failure.error, cell_2);
        assert!(message.contains("Ambiguous occurrence"), "{message}");

        let previous_module = compile_view
            .library()
            .expect("a prior generation exists")
            .module_name();
        let names = tidepool_runtime::session::turn::same_cell_value_collisions(
            &message,
            &previous_module,
            &candidate_module.module_name(),
        );
        assert_eq!(names, vec!["sh".to_string()]);

        // The fix: patch the previous-generation import with the SAME
        // shadowing `render_module` already applies across ordinary
        // generation boundaries, and retry — exactly what `prepare_cell`
        // now does.
        let patched_imports =
            hide_same_cell_collisions(&prepared.imports, &previous_module, &names)
                .expect("the bare library import line is present to patch");
        let retried_template = resident_cell_check_template(
            &check_preamble,
            &context.haskell_effects_alias,
            &patched_imports,
        );
        let checked = check_cell(request(
            cell_2,
            &retried_template,
            &include_refs,
            &compile_view,
            &prepared,
        ))
        .unwrap_or_else(|failure| {
            panic!(
                "retry with hiding must resolve `sh` unambiguously: {}",
                tidepool_runtime::session::render_cell_compile_error(&failure.error, cell_2)
            )
        });
        assert_eq!(
            checked.items.len(),
            2,
            "a decl item and a bind item: {checked:?}"
        );

        // Drive an expression through the production prepare join. The
        // checked plan supplies one lift and one presentation, so preparation
        // issues one executable compilation for this item.
        let expression = "{-# LANGUAGE PolyKinds #-}\nimport Data.Proxy (Proxy(..))\npure Proxy";
        let expression_view =
            actor_compile_view(&session, &context, &source, &[]).expect("expression view");
        let expression_prepared = source.prepare(&expression_view);
        let expression_module = session
            .next_declaration_module()
            .expect("declaration plane");
        let expression_preamble = cell_module_preamble(
            &expression_prepared.preamble,
            &expression_module.module_name(),
        )
        .expect("expression preamble");
        let expression_template = resident_cell_check_template(
            &expression_preamble,
            &context.haskell_effects_alias,
            &expression_prepared.imports,
        );
        let expression_evidence =
            cell_check_evidence(&expression_view, &expression_template, &expression_prepared);
        let expression_include = expression_prepared
            .include
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let expression_checked = check_cell(CellCheckRequest {
            exact_context: expression_view.exact_declaration_context().cloned(),
            session_id: None,
            cell_text: expression,
            template: &expression_template,
            include: &expression_include,
            session_root: expression_view.session_root(),
            inject_modules: &expression_prepared.injected,
            compile_generation: expression_view.next_value_generation().0,
            compile_view_evidence: &expression_evidence,
        })
        .expect("expression whole-cell check");
        assert_eq!(expression_checked.expression_plans.len(), 1);
        let expression_type = &expression_checked.expression_plans[0].type_display;
        assert!(
            expression_type.contains("forall cell0") && expression_type.contains("cell1 :: cell0"),
            "{expression_type}"
        );
        assert!(!expression_type.contains("ZonkAny"), "{expression_type}");
        assert!(!expression_type.contains("() ->"), "{expression_type}");
        let prepared_cell = tidepool_runtime::with_compiler_transaction(|| {
            prepare_cell_in_session(
                &mut session,
                &context,
                &source,
                &context.haskell_effects_alias,
                &[],
                &expression_checked,
                expression,
                &expression_evidence,
                expression_view,
            )
        })
        .expect("typed expression preparation");
        let PreparedCell::Ready { items, .. } = prepared_cell else {
            let PreparedCell::Rejected { diagnostic, .. } = prepared_cell else {
                unreachable!()
            };
            panic!(
                "typed expression preparation was rejected: {diagnostic:?}; plans={:?}",
                expression_checked.expression_plans
            )
        };
        let observations = items
            .iter()
            .filter_map(|item| match &item.ready {
                PreparedCellStep::Executable(ready) => ready.observation.as_ref(),
                PreparedCellStep::Declaration { .. } | PreparedCellStep::Checked { .. } => None,
            })
            .count();
        assert_eq!(observations, 1, "one selected expression wrapper");

        for cell_text in ["import Data.List\n", "{-# LANGUAGE NoLambdaCase #-}\n"] {
            let compile_view =
                actor_compile_view(&session, &context, &source, &[]).expect("compile view");
            let prepared = source.prepare(&compile_view);
            let module = session
                .next_declaration_module()
                .expect("declaration plane");
            let check_preamble = cell_module_preamble(&prepared.preamble, &module.module_name())
                .expect("preamble names a module");
            let template = resident_cell_check_template(
                &check_preamble,
                &context.haskell_effects_alias,
                &prepared.imports,
            );
            let evidence = cell_check_evidence(&compile_view, &template, &prepared);
            let include_refs = prepared
                .include
                .iter()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>();
            let checked = check_cell(CellCheckRequest {
                exact_context: compile_view.exact_declaration_context().cloned(),
                session_id: None,
                cell_text,
                template: &template,
                include: &include_refs,
                session_root: compile_view.session_root(),
                inject_modules: &prepared.injected,
                compile_generation: compile_view.next_value_generation().0,
                compile_view_evidence: &evidence,
            })
            .unwrap_or_else(|failure| {
                panic!(
                    "prologue-only cell should classify: {}",
                    tidepool_runtime::session::render_cell_compile_error(&failure.error, cell_text)
                )
            });
            let item = checked.items.first().expect("one grouped prologue item");
            assert!(item.prologue_only, "{cell_text:?}: {item:?}");
            assert_eq!(item.verdict.kind, tidepool_runtime::session::TurnKind::Decl);
            let prepared_cell = tidepool_runtime::with_compiler_transaction(|| {
                prepare_cell_in_session(
                    &mut session,
                    &context,
                    &source,
                    &context.haskell_effects_alias,
                    &[],
                    &checked,
                    cell_text,
                    &evidence,
                    compile_view,
                )
            })
            .expect("prepare prologue-only cell");
            let PreparedCell::Ready { mut items, .. } = prepared_cell else {
                panic!("prologue-only cell should prepare");
            };
            let PreparedCellStep::Declaration {
                generation,
                binders,
                prologue_only,
            } = items.remove(0).ready
            else {
                panic!("prologue-only cell should use declaration plane");
            };
            assert!(prologue_only);
            assert!(binders.is_empty());
            assert_eq!(
                declaration_receipt(&binders, prologue_only, generation.0),
                format!("accepted cell prologue at generation {}", generation.0)
            );
        }
    }

    /// One prepared item's generation and, for a Bind item, its binder
    /// names — the exact facts `prepare_cell`'s split path and its
    /// single-checkout fallback must agree on.
    fn item_signature(item: &PreparedCellItem) -> (u64, Vec<String>) {
        match &item.ready {
            PreparedCellStep::Executable(ready) => {
                let binders = match &ready.result {
                    TurnResult::Bind { bound, .. } => {
                        bound.iter().map(|binder| binder.name.clone()).collect()
                    }
                    _ => Vec::new(),
                };
                (ready.generation.0, binders)
            }
            PreparedCellStep::Declaration { generation, .. } => (generation.0, Vec::new()),
            PreparedCellStep::Checked { .. } => {
                panic!("legacy signature fixture has no checked lazy item")
            }
        }
    }

    /// A two-item bind cell prepared through the split
    /// (`snapshot_cell_split` → `check_cell_off_checkout` →
    /// `reserve_cell_generations` → `compile_cell_items_off_checkout` →
    /// `finalize_cell_install`) must yield the same generations and binders
    /// as `prepare_cell_in_session`'s single-checkout path, against an
    /// identically-constructed session.
    #[test]
    fn two_item_bind_cell_split_matches_single_checkout_prepare_cell_in_session() {
        let (mut split_session, split_context, split_source, _split_root) = host_mount_fixture();
        let (mut direct_session, direct_context, direct_source, _direct_root) =
            host_mount_fixture();
        let cell = "cellA <- pure (1 :: Int)\ncellB <- pure (cellA + 1)";

        let (source, snapshot) = snapshot_cell_split(
            &mut split_session,
            &split_context,
            split_source,
            &[],
            None,
            None,
            None,
        )
        .expect("split snapshot");
        let (checked, _folded) = check_cell_off_checkout(
            &snapshot,
            &source,
            &split_context.haskell_effects_alias,
            cell,
        )
        .expect("split whole-cell check");
        assert!(
            checked
                .items
                .iter()
                .all(|item| item.verdict.kind != TurnKind::Decl),
            "a two-item bind cell has no declaration item: {checked:?}"
        );
        let reservation = reserve_cell_generations(
            &mut split_session,
            &split_context,
            &source,
            &[],
            &snapshot,
            checked.items.len(),
            None,
        )
        .expect("reserve generations");
        let CellReservation::Ready(ready) = reservation else {
            panic!("no interleaved mutation: the reservation must be fresh");
        };
        let CellReservationReady {
            view,
            retained,
            visible_names,
            declaration: _,
        } = *ready;
        let outcome = compile_cell_items_off_checkout(
            &split_context,
            &source,
            &split_context.haskell_effects_alias,
            &checked,
            cell,
            view.clone(),
            &retained,
            &visible_names,
            None,
            None,
        )
        .expect("split item compile");
        let CellItemsOutcome::Ready(items) = outcome else {
            panic!("both items must compile");
        };
        let install = finalize_cell_install(
            &mut split_session,
            &split_context,
            &source,
            &[],
            snapshot.candidate_module,
            &view,
            &checked,
            items,
            None,
        )
        .expect("finalize install");
        let CellInstall::Ready(PreparedCell::Ready {
            items: split_items, ..
        }) = install
        else {
            panic!("no interleaved mutation: the install must be ready");
        };

        let direct_view = actor_compile_view(&direct_session, &direct_context, &direct_source, &[])
            .expect("direct compile view");
        let direct_prepared = direct_source.prepare(&direct_view);
        let direct_module = direct_session
            .next_declaration_module()
            .expect("declaration plane");
        let direct_check_preamble =
            cell_module_preamble(&direct_prepared.preamble, &direct_module.module_name())
                .expect("preamble names a module");
        let direct_template = resident_cell_check_template(
            &direct_check_preamble,
            &direct_context.haskell_effects_alias,
            &direct_prepared.imports,
        );
        let direct_evidence = cell_check_evidence(&direct_view, &direct_template, &direct_prepared);
        let direct_include = direct_prepared
            .include
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let direct_checked = check_cell(CellCheckRequest {
            exact_context: direct_view.exact_declaration_context().cloned(),
            session_id: None,
            cell_text: cell,
            template: &direct_template,
            include: &direct_include,
            session_root: direct_view.session_root(),
            inject_modules: &direct_prepared.injected,
            compile_generation: direct_view.next_value_generation().0,
            compile_view_evidence: &direct_evidence,
        })
        .expect("direct whole-cell check");
        let direct_prepared_cell = tidepool_runtime::with_compiler_transaction(|| {
            prepare_cell_in_session(
                &mut direct_session,
                &direct_context,
                &direct_source,
                &direct_context.haskell_effects_alias,
                &[],
                &direct_checked,
                cell,
                &direct_evidence,
                direct_view,
            )
        })
        .expect("direct single-checkout prepare");
        let PreparedCell::Ready {
            items: direct_items,
            ..
        } = direct_prepared_cell
        else {
            panic!("direct single-checkout prepare must be ready");
        };

        assert_eq!(split_items.len(), 2);
        assert_eq!(
            split_items.iter().map(item_signature).collect::<Vec<_>>(),
            direct_items.iter().map(item_signature).collect::<Vec<_>>(),
        );
    }

    /// A write to the same scope between a split cell's per-item compile
    /// (`compile_cell_items_off_checkout`) and its final re-checkout
    /// (`finalize_cell_install`) — here, another mount standing in for a
    /// concurrent actor's install — must be detected by
    /// `is_current_for` before installing the stale compile, and a
    /// fresh snapshot must still recover, the same invariant
    /// `a_mutation_between_split_fragment_checkouts_invalidates_the_snapshot_and_forces_a_recompile`
    /// proves for an ordinary fragment.
    #[test]
    fn cell_split_scope_mutation_before_final_checkout_forces_a_recompile() {
        let (mut session, context, base_source, _root) = host_mount_fixture();
        let cell = "onlyItem <- pure (1 :: Int)";

        // Each attempt snapshots from the pristine base source, exactly as
        // `prepare_cell` clones `self.access.source` for its snapshot — never from a previous attempt's already-mutated
        // source, which would double up its per-transaction preamble.
        let (source, snapshot) = snapshot_cell_split(
            &mut session,
            &context,
            base_source.clone(),
            &[],
            None,
            None,
            None,
        )
        .expect("snapshot");
        let (checked, _folded) =
            check_cell_off_checkout(&snapshot, &source, &context.haskell_effects_alias, cell)
                .expect("whole-cell check");
        let reservation = reserve_cell_generations(
            &mut session,
            &context,
            &source,
            &[],
            &snapshot,
            checked.items.len(),
            None,
        )
        .expect("reserve generations");
        let CellReservation::Ready(ready) = reservation else {
            panic!("no interleaved mutation yet: the reservation must be fresh");
        };
        let CellReservationReady {
            view,
            retained,
            visible_names,
            declaration: _,
        } = *ready;
        let outcome = compile_cell_items_off_checkout(
            &context,
            &source,
            &context.haskell_effects_alias,
            &checked,
            cell,
            view.clone(),
            &retained,
            &visible_names,
            None,
            None,
        )
        .expect("item compile");
        let CellItemsOutcome::Ready(items) = outcome else {
            panic!("the item must compile");
        };

        // Stand in for another actor writing to this exact scope between the
        // off-checkout compile and the final re-checkout.
        mount_text_binding(
            &mut session,
            &context,
            &source,
            &[],
            "interloper",
            "interloper text",
            None,
        )
        .expect("interloping carrier mounts");

        let install = finalize_cell_install(
            &mut session,
            &context,
            &source,
            &[],
            snapshot.candidate_module,
            &view,
            &checked,
            items,
            None,
        )
        .expect("finalize install");
        assert!(
            matches!(install, CellInstall::Stale(SplitStaleView::CompileView)),
            "an interleaved mutation must invalidate the snapshot"
        );

        // A fresh snapshot recompiles and installs cleanly. Snapshots again
        // from the pristine base source, not the mutated one above.
        let (source, retry_snapshot) =
            snapshot_cell_split(&mut session, &context, base_source, &[], None, None, None)
                .expect("retry snapshot");
        let (retry_checked, _retry_folded) = check_cell_off_checkout(
            &retry_snapshot,
            &source,
            &context.haskell_effects_alias,
            cell,
        )
        .expect("retry whole-cell check");
        let retry_reservation = reserve_cell_generations(
            &mut session,
            &context,
            &source,
            &[],
            &retry_snapshot,
            retry_checked.items.len(),
            None,
        )
        .expect("retry reserve generations");
        let CellReservation::Ready(retry_ready) = retry_reservation else {
            panic!("retry reservation must be fresh");
        };
        let CellReservationReady {
            view: retry_view,
            retained: retry_retained,
            visible_names: retry_visible_names,
            declaration: _,
        } = *retry_ready;
        let retry_outcome = compile_cell_items_off_checkout(
            &context,
            &source,
            &context.haskell_effects_alias,
            &retry_checked,
            cell,
            retry_view.clone(),
            &retry_retained,
            &retry_visible_names,
            None,
            None,
        )
        .expect("retry item compile");
        let CellItemsOutcome::Ready(retry_items) = retry_outcome else {
            panic!("retry item must compile");
        };
        let retry_install = finalize_cell_install(
            &mut session,
            &context,
            &source,
            &[],
            retry_snapshot.candidate_module,
            &retry_view,
            &retry_checked,
            retry_items,
            None,
        )
        .expect("retry finalize install");
        assert!(matches!(
            retry_install,
            CellInstall::Ready(PreparedCell::Ready { .. })
        ));
    }

    /// Actors co-resident on one machine share its live value set, but a
    /// split compile reads only its own scope's frames and inherited tip.
    /// While the cell's install checkout is held, a sibling actor binds and
    /// retires and a forked child of this actor binds. The cell, which reads
    /// this actor's own earlier binding, must still install off-checkout
    /// with no single-checkout fallback, and settle to the right value.
    #[tokio::test]
    async fn cell_split_other_actors_commits_between_snapshot_and_install_stay_current() {
        use std::sync::atomic::Ordering;
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench = ResidentActorWorkbench::new(machines, source.clone(), None, None, vec![]);
        let (sibling_scope, child_scope) = workbench
            .access
            .with_machine(context.clone(), |session, context, _| {
                let sibling = session.mint_isolated_scope();
                let child = session
                    .mint_scope(context.placement.lexical_scope)
                    .expect("fork scope");
                Ok((sibling, child))
            })
            .await
            .expect("scopes mint");
        let actor_at = |id: u64, lexical_scope| crate::ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(id)),
            placement: crate::ActorPlacement {
                lexical_scope,
                ..context.placement
            },
            ..context.clone()
        };
        let sibling = actor_at(2, sibling_scope);
        let child = actor_at(3, child_scope);

        let own = workbench
            .begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "ownValue <- pure (41 :: Int)".into(),
                },
                None,
            )
            .await
            .expect("the actor's own binding installs");
        if let ResidentWorkbenchStep::Running { fragment, outcome } = own {
            workbench
                .settle_item(context.clone(), *fragment, *outcome)
                .await
                .expect("the actor's own binding settles");
        }

        let probe = Arc::new(split_probe::SplitProbe {
            held_installs: 1,
            ..Default::default()
        });
        let cell = "derived <- pure (ownValue + 1)".to_string();
        let prepare = split_probe::PROBE.scope(
            Arc::clone(&probe),
            workbench.prepare_cell(context.clone(), cell.clone()),
        );
        let interlope = async {
            probe.install_reached.notified().await;
            for (actor, name) in [(&sibling, "siblingValue"), (&child, "childValue")] {
                let mount_source = workbench.access.source.clone();
                workbench
                    .access
                    .with_machine(actor.clone(), move |session, context, _| {
                        mount_text_binding(
                            session,
                            context,
                            &mount_source,
                            &[],
                            name,
                            "another actor's value",
                            None,
                        )
                    })
                    .await
                    .expect("another actor's binding mounts");
            }
            workbench
                .access
                .with_machine(sibling.clone(), move |session, _, _| {
                    session.retire_scope(sibling_scope);
                    Ok(())
                })
                .await
                .expect("the sibling retires");
            probe.resume_install.notify_one();
        };
        let (prepared, ()) = tokio::time::timeout(std::time::Duration::from_secs(600), async {
            tokio::join!(prepare, interlope)
        })
        .await
        .expect("the split settles");
        let (_checked, prepared) = prepared.expect("the cell prepares");
        let PreparedCell::Ready { mut items, .. } = prepared else {
            panic!("the cell must prepare as Ready");
        };
        assert_eq!(probe.install_checkouts.load(Ordering::SeqCst), 1);
        assert_eq!(
            probe.single_checkout_compiles.load(Ordering::SeqCst),
            0,
            "other actors' binds and retirement must not force the single-checkout fallback"
        );

        assert_eq!(items.len(), 1);
        let step = workbench
            .begin_prepared_cell_item(
                context.clone(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: cell,
                },
                items.remove(0),
                4096,
            )
            .await
            .expect("the installed item runs");
        if let ResidentWorkbenchStep::Running { fragment, outcome } = step {
            workbench
                .settle_item(context.clone(), *fragment, *outcome)
                .await
                .expect("the installed item settles");
        }
        let shown = workbench
            .begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "derived".into(),
                },
                None,
            )
            .await
            .expect("the bound value displays");
        let shown = match shown {
            ResidentWorkbenchStep::Running { fragment, outcome } => workbench
                .settle_item(context.clone(), *fragment, *outcome)
                .await
                .expect("the display settles"),
            step => step,
        };
        let ResidentWorkbenchStep::Committed { output, .. } = shown else {
            panic!("the display did not commit");
        };
        assert!(output.contains("42"), "{output}");

        // The fork inherited a snapshot, not a live link: its parent binding
        // afterwards leaves the fork's compile view current.
        let parent_source = workbench.access.source.clone();
        let child_current = workbench
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let before = actor_compile_view(session, &child, &parent_source, &[])?;
                mount_text_binding(
                    session,
                    context,
                    &parent_source,
                    &[],
                    "parentValue",
                    "the parent's later value",
                    None,
                )?;
                let after = actor_compile_view(session, &child, &parent_source, &[])?;
                Ok(after.is_current_for(&before))
            })
            .await
            .expect("the parent binds");
        assert!(child_current);
    }

    /// Retiring this actor's own carrier between the split's compile and
    /// its install changes what the cell could import, so the install must
    /// report the view stale.
    #[test]
    fn cell_split_retiring_own_carrier_before_install_is_stale() {
        let (mut session, context, base_source, _root) = host_mount_fixture();
        let cell = "onlyItem <- pure (1 :: Int)";
        let carrier = mount_json_input(
            &mut session,
            &context,
            &base_source,
            &[],
            &serde_json::json!({"greeting": "hi"}),
            None,
        )
        .expect("own carrier mounts");

        let (source, snapshot) = snapshot_cell_split(
            &mut session,
            &context,
            base_source.clone(),
            &[],
            None,
            None,
            None,
        )
        .expect("snapshot");
        let (checked, _folded) =
            check_cell_off_checkout(&snapshot, &source, &context.haskell_effects_alias, cell)
                .expect("whole-cell check");
        let reservation = reserve_cell_generations(
            &mut session,
            &context,
            &source,
            &[],
            &snapshot,
            checked.items.len(),
            None,
        )
        .expect("reserve generations");
        let CellReservation::Ready(ready) = reservation else {
            panic!("no interleaved mutation yet: the reservation must be fresh");
        };
        let CellReservationReady {
            view,
            retained,
            visible_names,
            declaration: _,
        } = *ready;
        let outcome = compile_cell_items_off_checkout(
            &context,
            &source,
            &context.haskell_effects_alias,
            &checked,
            cell,
            view.clone(),
            &retained,
            &visible_names,
            None,
            None,
        )
        .expect("item compile");
        let CellItemsOutcome::Ready(items) = outcome else {
            panic!("the item must compile");
        };

        let session_root = carrier_mount_session_root(&session, context.placement.lexical_scope)
            .expect("session root");
        session.retire_host_binding_owner(&session_root, &carrier.binder);

        let install = finalize_cell_install(
            &mut session,
            &context,
            &source,
            &[],
            snapshot.candidate_module,
            &view,
            &checked,
            items,
            None,
        )
        .expect("finalize install");
        assert!(
            matches!(install, CellInstall::Stale(SplitStaleView::CompileView)),
            "retiring the actor's own carrier must invalidate the snapshot"
        );
    }

    /// A registry-backed [`ResidentActorWorkbench`] sharing one resident
    /// session, for tests that need real checkout contention
    /// (`prepare_cell`'s split loop itself, not its off-checkout pieces
    /// directly).
    fn actor_registry_fixture() -> (
        Arc<ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>>,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        tempfile::TempDir,
    ) {
        let (session, context, source, root) = host_mount_fixture();
        let session_id = context.placement.session;
        let machines = Arc::new(ActorMachineRegistry::<
            frunk::HNil,
            tidepool_mcp::CapturedOutput,
        >::new());
        machines.insert_idle(session_id, Box::new(session));
        (machines, context, source, root)
    }

    #[tokio::test]
    async fn lookup_revalidation_is_actor_scoped_and_checkout_is_free_during_rpc() {
        for sibling_write in [false, true] {
            let (machines, context, source, _root) = actor_lookup_registry_fixture();
            let workbench = Arc::new(ResidentActorWorkbench::new(
                Arc::clone(&machines),
                source.clone(),
                None,
                None,
                vec![],
            ));
            let validation = if sibling_write {
                concat!(
                    "lookupValidation <- LookupApi.lookupRaw (LookupApi.lookupRequest [\"pollResponse\"]) >>= \\result -> ",
                    "if lookupIssue result == Nothing && not (null (lookupResults result)) ",
                    "then pure True else error \"unrelated sibling write invalidated lookup\""
                )
            } else {
                concat!(
                    "lookupValidation <- LookupApi.lookupRaw (LookupApi.lookupRequest [\"pollResponse\"]) >>= \\result -> ",
                    "if lookupIssue result == Just \"lookup compile view changed\" && null (lookupResults result) ",
                    "then pure True else error \"stale lookup metadata escaped\""
                )
            };
            let step = workbench
                .begin_fragment_split(
                    context.clone(),
                    source.clone(),
                    Vec::new(),
                    ParsedBlock {
                        ordinal: 1,
                        total: 1,
                        source: validation.into(),
                    },
                    Some(generated_bind_verdict("lookupValidation")),
                )
                .await
                .expect("lookup fragment compiles and suspends");
            let (fragment, outcome) = match step {
                ResidentWorkbenchStep::Running { fragment, outcome } => (fragment, outcome),
                ResidentWorkbenchStep::Rejected(rejection) => {
                    panic!("lookup fixture rejected: {rejection:#?}")
                }
                ResidentWorkbenchStep::Committed { output, .. } => {
                    panic!("lookup fixture completed before the effect: {output}")
                }
                _ => panic!("lookup fixture returned an unexpected workbench step"),
            };
            let ResidentOutcome::Suspended { hole, .. } = *outcome else {
                panic!("lookup effect should park a continuation")
            };
            let (probe, completed, release) = lookup_inspection_probe::install();
            let trace_path = lookup_trace_path();
            let requests_before = compiler_trace_started_ids(&trace_path);
            let task_workbench = Arc::clone(&workbench);
            let task_context = context.clone();
            let task = tokio::spawn(async move {
                task_workbench
                    .resume_lookup(
                        task_context,
                        hole,
                        real_lookup_request(varied_lookup_queries()),
                        crate::UsagePointerTable::default(),
                        None,
                    )
                    .await
            });

            let trace_for_wait = trace_path.clone();
            let active_request = tokio::task::spawn_blocking(move || {
                wait_for_new_compiler_request(&trace_for_wait, &requests_before)
            })
            .await
            .expect("request-start waiter joins");
            assert!(!active_request.is_empty());
            tokio::time::timeout(
                Duration::from_secs(30),
                workbench
                    .access
                    .with_machine(context.clone(), |_, _, _| Ok(())),
            )
            .await
            .expect("machine checkout is released while GHC inspection is active")
            .expect("machine checkout succeeds during GHC inspection");
            assert!(
                !compiler_request_event(&trace_path, &active_request, "compiler request finished"),
                "exact GHC request {active_request} finished before the competing checkout"
            );

            let gate_waiter = tokio::task::spawn_blocking(move || {
                completed
                    .recv_timeout(Duration::from_secs(90))
                    .expect("real inspection result reached the revalidation gate")
            });
            gate_waiter.await.expect("inspection gate waiter joins");
            assert!(compiler_request_event(
                &trace_path,
                &active_request,
                "compiler request finished"
            ));

            if sibling_write {
                let sibling_parent_context = context.clone();
                let sibling_source = source.clone();
                workbench
                    .access
                    .with_machine(context.clone(), move |session, _, _| {
                        let sibling = session.mint_isolated_scope();
                        let mut sibling_context = sibling_parent_context;
                        sibling_context.placement.lexical_scope = sibling;
                        let (binder, compiled, generation) = compile_host_binding(
                            session,
                            &sibling_context,
                            &sibling_source,
                            &[],
                            "lookup_sibling_value",
                            TEXT_BINDING_TYPE_NAME,
                            TEXT_BINDING_ANCHOR,
                            text_binding_carrier_imports(),
                            true,
                        )?;
                        session
                            .mount_text_binding_in(
                                sibling,
                                &binder,
                                generation,
                                compiled.into_code(),
                                "unrelated sibling value",
                            )
                            .map_err(ResidentActorWorkbenchError::Resident)?;
                        Ok(())
                    })
                    .await
                    .expect("a sibling-only binding commits");
            } else {
                workbench
                    .access
                    .with_machine(context.clone(), move |session, context, _| {
                        session
                            .define_scoped_in(
                                context.placement.lexical_scope,
                                &["data LookupViewChanged = LookupViewChanged"],
                            )
                            .map_err(|error| {
                                ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                            })?;
                        Ok(())
                    })
                    .await
                    .expect("visible declaration changes the actor compile view");
            }

            drop(probe);
            release.send(()).expect("release real lookup result");
            let outcome = task
                .await
                .expect("lookup task joins")
                .expect("lookup resumes after revalidation");
            let settled = workbench
                .settle_item(context.clone(), *fragment, outcome)
                .await
                .expect("lookup continuation settles");
            assert!(
                matches!(settled, ResidentWorkbenchStep::Committed { .. }),
                "Haskell continuation validates the stale or preserved lookup result"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_lookup_interrupts_its_started_compiler_request_and_recovers() {
        let (machines, context, source, _root) = actor_lookup_registry_fixture();
        let workbench = Arc::new(ResidentActorWorkbench::new(
            Arc::clone(&machines),
            source.clone(),
            None,
            None,
            vec![],
        ));
        let trace_path = lookup_trace_path();
        let step = workbench
            .begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "lookupResult <- LookupApi.lookupRaw (LookupApi.lookupRequest [\"pollResponse\"])".into(),
                },
                Some(generated_binds_verdict(&["lookupResult".into()])),
            )
            .await
            .expect("lookup fragment compiles and suspends");
        let ResidentWorkbenchStep::Running { outcome, .. } = step else {
            panic!("lookup effect should suspend the fragment")
        };
        let ResidentOutcome::Suspended { hole, .. } = *outcome else {
            panic!("lookup effect should park a continuation")
        };
        let continuation_id = hole.cont_id().to_owned();
        let requests_before = compiler_trace_started_ids(&trace_path);
        let cancellation = crate::WorkbenchExecutionControl::untracked();
        let task_cancellation = Arc::clone(&cancellation);
        let task_continuation_id = continuation_id.clone();
        let task_workbench = Arc::clone(&workbench);
        let task_context = context.clone();
        let task = tokio::spawn(async move {
            let guard = ParkedHoleAbortGuard::new(
                &task_workbench.access,
                task_context.clone(),
                task_continuation_id,
                "cancelled lookup request".into(),
            );
            SLOT_CONTINUATION_OWNER
                .scope(
                    guard.registration(),
                    task_workbench.resume_lookup(
                        task_context,
                        hole,
                        real_lookup_request(varied_lookup_queries()),
                        crate::UsagePointerTable::default(),
                        Some(task_cancellation),
                    ),
                )
                .await
        });
        let trace_for_wait = trace_path.clone();
        let request = tokio::task::spawn_blocking(move || {
            wait_for_new_compiler_request(&trace_for_wait, &requests_before)
        })
        .await
        .expect("request-start waiter joins");
        assert!(cancellation.request_cancellation());
        assert!(task.await.expect("cancelled lookup task joins").is_err());
        let trace_for_wait = trace_path.clone();
        tokio::task::spawn_blocking(move || {
            wait_for_compiler_request_event(
                &trace_for_wait,
                &request,
                "compiler request abandoned by client",
            )
        })
        .await
        .expect("abandoned request waiter joins");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let parked = workbench
                .access
                .with_machine(context.clone(), |session, _, _| {
                    Ok(session
                        .parked_holes()
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>())
                })
                .await
                .expect("inspect parked lookup continuation");
            if !parked.contains(&continuation_id) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cancelled lookup continuation remained parked"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let recovered = workbench
            .begin_fragment_split(
                context.clone(),
                source,
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "lookupRecovery <- pure (31 :: Int)".into(),
                },
                Some(generated_bind_verdict("lookupRecovery")),
            )
            .await
            .expect("machine and compiler remain usable after cancellation");
        assert!(
            matches!(recovered, ResidentWorkbenchStep::Committed { .. }),
            "fresh compiler work completes after the cancelled lookup settles"
        );
    }

    #[tokio::test]
    async fn tool_installation_preserves_durable_child_private_admission() {
        struct RunOwner {
            root: PathBuf,
            _lock: std::fs::File,
        }
        impl tidepool_runtime::session::RecoveryRunAuthority for RunOwner {
            fn owns_run(&self, root: &std::path::Path) -> std::io::Result<bool> {
                Ok(root.canonicalize()? == self.root)
            }
        }
        let durable = tempfile::tempdir().unwrap();
        let manifest = durable.path().join("declarations.json");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(durable.path().join("run-owner.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let authority = Arc::new(RunOwner {
            root: durable.path().canonicalize().unwrap(),
            _lock: lock,
        });
        let (mut session, context, source, _root) = host_mount_fixture_with_lib(|lib| {
            lib.attach_owned_recovery_graph_v3(&manifest, authority)
                .unwrap();
        });
        let owner = tidepool_runtime::session::RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/installer-child").unwrap(),
            context.actor.incarnation.0,
        )
        .unwrap();
        // Durable children publish their inherited view before native boot
        // installs tools. The installer must preserve that admitted surface.
        session
            .initialize_durable_public_scope(owner.clone(), context.placement.lexical_scope)
            .unwrap();
        let machines = Arc::new(ActorMachineRegistry::new());
        machines.insert_idle(context.placement.session, Box::new(session));
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![])
                .with_compilation_authority(
                    crate::resident_actor::WorkbenchCompilationAuthority::for_test(context.clone()),
                );
        let before = workbench
            .access
            .with_machine(context.clone(), |session, context, _| {
                Ok(session
                    .public_visibility_snapshot_in(context.placement.lexical_scope)
                    .expect("registered child public surface"))
            })
            .await
            .expect("public baseline after machine registry admission");
        assert!(before.machine_incarnation.is_none());
        let manifest_before = std::fs::read(&manifest).unwrap();
        let tools = workbench
            .prepare_tools(context.clone(), 1, vec![])
            .await
            .unwrap();
        assert!(!tools.declarations.is_empty());
        let installation_scope = tools._installation_scope.scope();
        assert_ne!(installation_scope, context.placement.lexical_scope);
        let cleanup_owner = owner.clone();
        let (admission, installed_public) = workbench
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let after = session
                    .public_visibility_snapshot_in(context.placement.lexical_scope)
                    .unwrap();
                // The first prepared install creates the machine identity.
                // Every declaration, binding, source and visibility fence
                // still belongs to the unchanged durable public surface.
                assert!(after.machine_incarnation.is_some());
                let mut expected = before;
                expected.machine_incarnation = after.machine_incarnation;
                assert_eq!(after, expected);
                assert_eq!(std::fs::read(&manifest).unwrap(), manifest_before);
                assert!(session
                    .public_visibility_snapshot_in(installation_scope)
                    .is_some());
                let admission = session
                    .begin_durable_private_execution(&owner, context.placement.lexical_scope)
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })?;
                Ok((admission, after))
            })
            .await
            .expect("ordinary private notebook admission after tool installation");
        let mut private_context = context.clone();
        private_context.placement.lexical_scope = admission.private_scope();
        let step = workbench
            .begin_fragment_split(
                private_context.clone(),
                source,
                vec![],
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "pure (7 :: Int)".into(),
                },
                None,
            )
            .await
            .unwrap();
        match step {
            ResidentWorkbenchStep::Running { fragment, outcome } => {
                workbench
                    .settle_item(private_context, *fragment, *outcome)
                    .await
                    .unwrap();
            }
            ResidentWorkbenchStep::Committed { .. } => {}
            _ => panic!(
                "ordinary private notebook did not execute: {}",
                describe_step(&step)
            ),
        }
        drop(admission);
        drop(tools);
        workbench
            .access
            .with_machine(context, move |session, context, _| {
                assert_eq!(
                    session.public_visibility_snapshot_in(context.placement.lexical_scope),
                    Some(installed_public)
                );
                let _next = session
                    .begin_durable_private_execution(
                        &cleanup_owner,
                        context.placement.lexical_scope,
                    )
                    .map_err(ResidentError::Session)?;
                assert!(session
                    .public_visibility_snapshot_in(installation_scope)
                    .is_none());
                Ok(())
            })
            .await
            .expect("installed implementation releases its lexical scope");
    }

    /// A bare, unbootstrapped-but-idle machine at an arbitrary session id —
    /// everything a [`ChildSessionFactory`] needs to hand back, without the
    /// notifications/preamble setup [`host_mount_fixture`] does for tests
    /// that actually run a turn.
    fn bare_session_at(
        session_id: tidepool_repr::SessionId,
    ) -> (
        ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        tempfile::TempDir,
    ) {
        use tidepool_runtime::session::{ModuleEnv, SessionLib};
        let session_root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            session_id,
            session_root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("declaration plane");
        (
            ResidentSession::unbootstrapped(
                frunk::HNil,
                tidepool_mcp::CapturedOutput::new(),
                tidepool_runtime::DEFAULT_NURSERY_SIZE,
                Some(lib),
            ),
            session_root,
        )
    }

    fn child_admission_fixture() -> (
        Arc<ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>>,
        ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
        crate::ActorSessionContext,
        tempfile::TempDir,
    ) {
        let id = tidepool_repr::SessionId(79);
        let (mut session, root) = bare_session_at(id);
        let scope = session.mint_isolated_scope();
        let context = crate::ActorDescriptor::new(
            "child-admission",
            crate::ActorPlacement {
                session: id,
                resource_scope: RealmId::ROOT,
                lexical_scope: scope,
            },
        )
        .session_context(crate::ActorRef::first(crate::ActorId(79)));
        let machines = Arc::new(ActorMachineRegistry::new());
        machines.insert_idle(id, Box::new(session));
        let runner = ResidentActorRunner::new(
            Arc::clone(&machines),
            ActorWorkbenchSource::new("", Vec::new()),
        );
        (machines, runner, context, root)
    }

    fn child_retirement() -> crate::ActorTerminal {
        crate::ActorTerminal {
            kind: crate::ActorExitKind::Cancelled,
            summary: "child preparation retired".into(),
        }
    }

    #[tokio::test]
    async fn fork_child_admission_prior_retirement_refuses_ready_checkout() {
        let (machines, runner, context, _root) = child_admission_fixture();
        let retirement = crate::RetainedActorExit::new();
        let terminal = retirement.request_shutdown(child_retirement());
        let result = runner
            .access
            .with_machine_wait(
                context.clone(),
                MachineCheckoutAdmission::UntilRetirement(retirement),
                |_, _, _| -> Result<(), ResidentActorWorkbenchError> {
                    panic!("retired child must not enter the native owner")
                },
            )
            .await;
        assert!(
            matches!(result, Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(actual)) if actual == terminal)
        );
        assert_eq!(
            machines.kind(context.placement.session),
            Some(tidepool_runtime::session::registry::SlotKind::Idle)
        );
    }

    #[tokio::test]
    async fn fork_child_admission_retirement_removes_queued_checkout() {
        let (machines, runner, context, _root) = child_admission_fixture();
        let id = context.placement.session;
        let (session, receipt) = machines.checkout_run(id).unwrap().into_parts();
        let retirement = crate::RetainedActorExit::new();
        let task = runner.access.with_machine_wait(
            context,
            MachineCheckoutAdmission::UntilRetirement(retirement.clone()),
            |_, _, _| -> Result<(), ResidentActorWorkbenchError> {
                panic!("queued retired child must not enter the native owner")
            },
        );
        tokio::pin!(task);
        assert!(matches!(
            futures_util::poll!(task.as_mut()),
            std::task::Poll::Pending
        ));
        let terminal = retirement.request_shutdown(child_retirement());
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap();
        assert!(
            matches!(result, Err(ResidentActorWorkbenchError::RetiredBeforeAdmission(actual)) if actual == terminal)
        );
        machines.settle_suspended(receipt, session, Vec::new());
        // Cancellation removed its queue position and did not consume custody.
        let checkout = machines.checkout_run(id).unwrap();
        drop(checkout);
    }

    #[tokio::test]
    async fn fork_child_admission_claimed_operation_keeps_result_during_retirement() {
        let (machines, runner, context, _root) = child_admission_fixture();
        let id = context.placement.session;
        let retirement = crate::RetainedActorExit::new();
        let (entered, observe) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            runner
                .access
                .with_machine_wait(
                    context,
                    MachineCheckoutAdmission::UntilRetirement(retirement.clone()),
                    move |_, _, _| {
                        entered.send(retirement).unwrap();
                        wait.recv_timeout(Duration::from_secs(1)).unwrap();
                        Ok(37)
                    },
                )
                .await
        });
        let retirement = observe.await.unwrap();
        retirement.request_shutdown(child_retirement());
        assert!(
            !task.is_finished(),
            "an admitted operation must keep its real visibility result"
        );
        release.send(()).unwrap();
        assert_eq!(task.await.unwrap().unwrap(), 37);
        assert_eq!(
            machines.kind(id),
            Some(tidepool_runtime::session::registry::SlotKind::Idle)
        );
    }

    fn child_bootstrap_fixture() -> Arc<tidepool_runtime::session::CompiledTurn> {
        use tidepool_repr::execution_schema::{
            testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
            Group, HeapBinding, HeapRhs, ResultContract, RuntimeRep, ValueId, ValueRef,
        };
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        for (index, (name, fields)) in [("Done", 1), ("Suspended", 2), ("Unit", 0)]
            .into_iter()
            .enumerate()
        {
            let module = if index < 2 {
                "Tidepool.Internal.Resume"
            } else {
                "Fixture"
            };
            let mut identity = testing::identity(module, name);
            identity.namespace = "constructor".into();
            let mut family = testing::identity(module, if index < 2 { "Settled" } else { "Unit" });
            family.namespace = "type".into();
            wire.constructors.push(ConstructorDecl {
                identity,
                family,
                host_id: tidepool_repr::DataConId(900 + index as u64),
                result_rep: RuntimeRep::LiftedRef,
                tag: if index == 1 { 2 } else { 1 },
                family_size: if index < 2 { 2 } else { 1 },
                field_reps: vec![RuntimeRep::LiftedRef; fields],
                strict_fields: vec![false; fields],
                layout: CheckedLayout {
                    fields: (0..fields)
                        .map(|field| FieldLayout {
                            rep: RuntimeRep::LiftedRef,
                            offset: field as u32 * 8,
                        })
                        .collect(),
                    alignment: if fields == 0 { 1 } else { 8 },
                    payload_size: fields as u32 * 8,
                    root_mask: vec![true; fields],
                },
            });
        }
        wire.expressions.nodes = vec![
            ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
            },
            ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(2),
                        fields: vec![],
                    },
                }),
                body: 0,
            },
        ];
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Function { body, .. } = &mut top.binding.rhs else {
            unreachable!()
        };
        *body = 1;
        let mut table = DataConTable::new();
        for constructor in &wire.constructors {
            table.insert(tidepool_repr::DataCon {
                id: constructor.host_id,
                name: constructor.identity.occurrence.clone(),
                tag: constructor.tag,
                rep_arity: constructor.field_reps.len() as u32,
                field_bangs: vec![],
                qualified_name: Some(format!(
                    "{}.{}",
                    constructor.identity.module, constructor.identity.occurrence
                )),
                type_name: constructor.family.occurrence.clone(),
            });
        }
        Arc::new(tidepool_runtime::session::CompiledTurn {
            prepared: Arc::new(testing::prepare(wire).unwrap()),
            table,
            asks: Vec::new(),
            warnings: Default::default(),
            certification: None,
        })
    }

    struct ChildOutputLifetime(Arc<tokio::sync::Notify>);

    impl Drop for ChildOutputLifetime {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    #[derive(Clone)]
    struct ChildOutput(Arc<ChildOutputLifetime>);

    impl OutputSink for ChildOutput {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    fn child_preparation_fixture(
        id: tidepool_repr::SessionId,
        root: PathBuf,
        output: ChildOutput,
    ) -> Box<ResidentSession<frunk::HNil, ChildOutput>> {
        use tidepool_runtime::session::{ModuleEnv, SessionLib};
        let library = SessionLib::open(id, &root, ModuleEnv::standalone_default()).unwrap();
        Box::new(ResidentSession::unbootstrapped(
            frunk::HNil,
            output,
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        ))
    }

    #[tokio::test]
    async fn cancelling_child_preparation_cannot_register_a_late_machine() {
        let id = tidepool_repr::SessionId(810);
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let machines = Arc::new(ActorMachineRegistry::<frunk::HNil, ChildOutput>::new());
        let dropped = Arc::new(tokio::sync::Notify::new());
        let worker_dropped = Arc::clone(&dropped);
        let (started, observe) = tokio::sync::oneshot::channel();
        let started = std::sync::Mutex::new(Some(started));
        let (release, wait) = std::sync::mpsc::channel();
        let wait = std::sync::Mutex::new(wait);
        let runner = ResidentActorRunner::new(
            Arc::clone(&machines),
            ActorWorkbenchSource::new("", Vec::new()),
        )
        .with_child_bootstrap_program(child_bootstrap_fixture())
        .with_child_session_factory(Arc::new(move |id, _| {
            let output = ChildOutput(Arc::new(ChildOutputLifetime(Arc::clone(&worker_dropped))));
            let weak = Arc::downgrade(&output.0);
            let machine = child_preparation_fixture(id, root_path.clone(), output);
            started.lock().unwrap().take().unwrap().send(weak).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ok(machine)
        }));
        let task_runner = runner.clone();
        let task = tokio::spawn(async move {
            task_runner
                .provision_child_session(id, RealmId::ROOT, None, &[])
                .await
        });
        let lifetime = observe.await.unwrap();
        // This current-thread runtime reached another task while preparation is
        // synchronously blocked, proving the factory is off the async thread.
        assert!(machines.kind(id).is_none());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), dropped.notified())
            .await
            .unwrap();
        assert!(lifetime.upgrade().is_none());
        assert!(machines.kind(id).is_none());
        assert!(!runner.access.child_sessions.lock().unwrap().contains(&id));
    }

    #[tokio::test]
    async fn prepared_child_registration_remains_owned_by_retirement() {
        let id = tidepool_repr::SessionId(811);
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let machines = Arc::new(ActorMachineRegistry::<frunk::HNil, ChildOutput>::new());
        let dropped = Arc::new(tokio::sync::Notify::new());
        let factory_dropped = Arc::clone(&dropped);
        let runner = ResidentActorRunner::new(
            Arc::clone(&machines),
            ActorWorkbenchSource::new("", Vec::new()),
        )
        .with_child_bootstrap_program(child_bootstrap_fixture())
        .with_child_session_factory(Arc::new(move |id, _| {
            Ok(child_preparation_fixture(
                id,
                root_path.clone(),
                ChildOutput(Arc::new(ChildOutputLifetime(Arc::clone(&factory_dropped)))),
            ))
        }));
        let scope = runner
            .provision_child_session(id, RealmId::ROOT, None, &[])
            .await
            .unwrap();
        assert_ne!(scope, tidepool_codegen::scope::ScopeId::ROOT);
        assert_eq!(
            machines.kind(id),
            Some(tidepool_runtime::session::registry::SlotKind::Idle)
        );
        assert!(runner.access.child_sessions.lock().unwrap().contains(&id));
        runner.retire_child_session(id, false).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), dropped.notified())
            .await
            .unwrap();
        assert!(machines.kind(id).is_none());
        assert!(!runner.access.child_sessions.lock().unwrap().contains(&id));
    }

    #[test]
    fn child_session_factory_builds_a_type_checking_session_and_inserts_it_idle() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let child_id = tidepool_repr::SessionId(context.placement.session.0.wrapping_add(1));
        // Keep the fixture's tempdir alive for the factory's one call — a
        // production factory instead captures the run's real session root.
        let held_root = std::sync::Mutex::new(None);
        let runner = ResidentActorRunner::new(Arc::clone(&machines), source)
            .with_child_session_factory(Arc::new(move |session_id, _source_layer| {
                let (session, root) = bare_session_at(session_id);
                *held_root.lock().unwrap() = Some(root);
                Ok(Box::new(session))
            }));
        assert_eq!(machines.kind(child_id), None, "not present before spawning");
        runner
            .spawn_child_session(child_id)
            .expect("factory-built session installs idle");
        assert_eq!(
            machines.kind(child_id),
            Some(tidepool_runtime::session::SlotKind::Idle)
        );
        // The parent's own session is untouched by spawning a sibling.
        assert!(machines.kind(context.placement.session).is_some());
    }

    /// This crate's own minimal counterpart to
    /// `tidepool/runtime/tests/prepared_residency.rs`'s `Notebook::bind`:
    /// compile and run one `name <- pure (expr)` turn directly against a
    /// bare, unbootstrapped session (no actor context, no declared effects)
    /// -- just enough to mint one live [`RootCustody`] to move around.
    fn bind_bare_value(
        session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        session_id: tidepool_repr::SessionId,
        session_root: &std::path::Path,
        surface: &tidepool_testing::effect_surface::TestEffectSurface,
        generation: u64,
        name: &str,
        expr: &str,
    ) -> RootCustody {
        let templates = resident_workbench_templates(surface.preamble(), surface.row(), "");
        let include: Vec<_> = surface
            .include_paths()
            .iter()
            .map(PathBuf::as_path)
            .collect();
        let text = format!("{name} <- pure ({expr})");
        let result = run_turn(TurnRequest {
            exact_context: None,
            session_id: Some(session_id),
            turn_text: &text,
            templates: &templates,
            include: &include,
            session_root,
            inject_modules: &[],
            gen: generation,
            verdict: None,
            target: None,
            retained_imports: &[],
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}",
                tidepool_runtime::classify_compile(&failure.error).message
            )
        });
        let TurnResult::Bind {
            bound, compiled, ..
        } = result
        else {
            panic!("{text:?} did not classify as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("{text:?} bound {} names", bound.len());
        };
        let outcome = session
            .run_bind_with_sites(
                "transfer_custody_test",
                compiled.code(),
                binder,
                tidepool_repr::Generation(generation),
            )
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{text:?}: {outcome:?}"
        );
        session
            .retain_binding_custody(name)
            .expect("retain live binding")
            .expect("bound value custody")
    }

    /// [`ResidentActorRunner::transfer_custody`]: same-session behaves
    /// exactly as [`ResidentSession::rehome_custody`] (no handle count
    /// change on the one machine involved); cross-session moves the value
    /// out of its origin machine and into the destination's, with an exact
    /// -1/+1 handle delta on each side (no leak, no double-release) and the
    /// content round-tripping byte-for-byte -- modeled on
    /// `parcel_crosses_two_resident_sessions_sharing_one_image_registry` in
    /// `tidepool/runtime/tests/prepared_residency.rs`, adapted to two real
    /// sessions sharing one [`ActorMachineRegistry`] and driven through
    /// [`ResidentActorRunner`] rather than raw [`ResidentSession`] calls.
    #[tokio::test]
    async fn transfer_custody_moves_a_value_between_two_resident_sessions() {
        tidepool_testing::eval_harness::require_extract();
        let surface = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[])
            .expect("materialize minimal effect surface");

        let session_a_id = tidepool_repr::SessionId(0xA0_A0);
        let session_b_id = tidepool_repr::SessionId(0xB0_B0);
        let (mut session_a, root_a) = bare_session_at(session_a_id);
        let (mut session_b, root_b) = bare_session_at(session_b_id);

        // Warm each machine with a throwaway turn before sharing a registry:
        // `set_image_registry` is a no-op before a machine exists.
        let _warm_a = bind_bare_value(
            &mut session_a,
            session_a_id,
            root_a.path(),
            &surface,
            1,
            "warmA",
            "0 :: Int",
        );
        let _warm_b = bind_bare_value(
            &mut session_b,
            session_b_id,
            root_b.path(),
            &surface,
            1,
            "warmB",
            "0 :: Int",
        );

        let registry = Arc::new(tidepool_runtime::session::ImageRegistry::new());
        session_a.set_image_registry(Arc::clone(&registry));
        session_b.set_image_registry(Arc::clone(&registry));

        let same_session_value = bind_bare_value(
            &mut session_a,
            session_a_id,
            root_a.path(),
            &surface,
            2,
            "sameSession",
            "111 :: Int",
        );
        let same_session_before = session_a
            .render_retained_preview(&same_session_value, 64)
            .expect("session A can render its own custody");

        let crossing_value = bind_bare_value(
            &mut session_a,
            session_a_id,
            root_a.path(),
            &surface,
            3,
            "crossing",
            "222 :: Int",
        );
        let crossing_before = session_a
            .render_retained_preview(&crossing_value, 64)
            .expect("session A can render the value before it crosses");
        // A bound value's own top is a static CAF, so import only resolves
        // it when the destination installed the SAME compiled image --
        // exactly `parcel_crosses_two_resident_sessions_sharing_one_image_registry`'s
        // setup in `tidepool/runtime/tests/prepared_residency.rs`. Session B
        // runs the identical turn at the identical generation so the shared
        // `registry` records the hit before the parcel crosses.
        let _crossing_warm_b = bind_bare_value(
            &mut session_b,
            session_b_id,
            root_b.path(),
            &surface,
            3,
            "crossing",
            "222 :: Int",
        );
        assert!(
            registry.hits() > 0,
            "session A and B's identical `crossing` bind should share one compiled image"
        );

        let machines = Arc::new(ActorMachineRegistry::<
            frunk::HNil,
            tidepool_mcp::CapturedOutput,
        >::new());
        machines.insert_idle(session_a_id, Box::new(session_a));
        machines.insert_idle(session_b_id, Box::new(session_b));
        let source = ActorWorkbenchSource::new(
            surface.preamble().to_string(),
            surface.include_paths().to_vec(),
        );
        let runner: ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput> =
            ResidentActorRunner::new(Arc::clone(&machines), source);

        async fn handle_count(
            runner: &ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
            session_id: tidepool_repr::SessionId,
        ) -> usize {
            runner
                .access
                .with_host_machine("count", session_id, None, |session, _| {
                    Ok(session.value_handle_count())
                })
                .await
                .expect("checkout for counting handles")
        }

        let owner = RealmId::fresh();

        // (a) same session: exactly `rehome_custody` -- no handle count
        // change on the one machine involved.
        let a_before_rehome = handle_count(&runner, session_a_id).await;
        let rehomed = runner
            .transfer_custody(same_session_value, session_a_id, session_a_id, owner)
            .await
            .expect("same-session transfer succeeds");
        let a_after_rehome = handle_count(&runner, session_a_id).await;
        assert_eq!(
            a_after_rehome, a_before_rehome,
            "same-session transfer neither adds nor drops a handle"
        );
        let rehomed_render = runner
            .access
            .with_host_machine("render", session_a_id, None, move |session, _| {
                Ok(session.render_retained_preview(&rehomed, 64))
            })
            .await
            .expect("checkout for rendering")
            .expect("session A can still render the rehomed value");
        assert_eq!(
            rehomed_render, same_session_before,
            "same-session transfer preserves content"
        );

        // (b) cross-session: an exact -1/+1 handle delta, content intact.
        let a_before_cross = handle_count(&runner, session_a_id).await;
        let b_before_cross = handle_count(&runner, session_b_id).await;
        let imported = runner
            .transfer_custody(crossing_value, session_a_id, session_b_id, owner)
            .await
            .expect("cross-session transfer succeeds");
        let a_after_cross = handle_count(&runner, session_a_id).await;
        let b_after_cross = handle_count(&runner, session_b_id).await;
        assert_eq!(
            a_after_cross,
            a_before_cross - 1,
            "export releases the handle on the origin machine exactly once"
        );
        assert_eq!(
            b_after_cross,
            b_before_cross + 1,
            "import mints exactly one new handle on the destination machine"
        );
        let imported_render = runner
            .access
            .with_host_machine("render", session_b_id, None, move |session, _| {
                Ok(session.render_retained_preview(&imported, 64))
            })
            .await
            .expect("checkout for rendering")
            .expect("session B can render the imported value");
        assert_eq!(
            imported_render, crossing_before,
            "the value round-trips byte-for-byte across sessions"
        );
    }

    #[tokio::test]
    async fn cancelling_child_startup_during_custody_transfer_discards_only_the_child_session() {
        use std::future::Future;
        use std::task::Poll;

        tidepool_testing::eval_harness::require_extract();
        let surface = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[])
            .expect("materialize minimal effect surface");
        let parent_id = tidepool_repr::SessionId(0xD0_D0);
        let child_id = tidepool_repr::SessionId(0xD1_D1);
        let (mut parent, parent_root) = bare_session_at(parent_id);
        let retained = bind_bare_value(
            &mut parent,
            parent_id,
            parent_root.path(),
            &surface,
            1,
            "parentValue",
            "333 :: Int",
        );
        let entry = parent
            .retain_binding_custody("parentValue")
            .expect("retain independent custody for the startup entry")
            .expect("parent value remains bound before child startup");
        let before = parent
            .render_retained_preview(&retained, 64)
            .expect("parent custody is evaluable before startup");
        assert!(before.contains("333"), "parent value: {before}");

        let templates = resident_workbench_templates(surface.preamble(), surface.row(), "");
        let include: Vec<_> = surface
            .include_paths()
            .iter()
            .map(PathBuf::as_path)
            .collect();
        let bootstrap = run_turn(TurnRequest {
            exact_context: None,
            session_id: Some(parent_id),
            turn_text: "startupBootstrap <- pure (0 :: Int)",
            templates: &templates,
            include: &include,
            session_root: parent_root.path(),
            inject_modules: &[],
            gen: 2,
            verdict: None,
            target: None,
            retained_imports: &[],
        })
        .expect("child bootstrap compiles");
        let TurnResult::Bind { compiled, .. } = bootstrap else {
            panic!("child bootstrap must be a bind turn");
        };
        let machines = Arc::new(ActorMachineRegistry::new());
        machines.insert_idle(parent_id, Box::new(parent));
        let source = ActorWorkbenchSource::new(
            surface.preamble().to_string(),
            surface.include_paths().to_vec(),
        );
        let child_root = std::sync::Mutex::new(None);
        let runner = ResidentActorRunner::new(Arc::clone(&machines), source)
            .with_child_session_factory(Arc::new(move |id, _source_layer| {
                let (session, root) = bare_session_at(id);
                *child_root.lock().unwrap() = Some(root);
                Ok(Box::new(session))
            }))
            .with_child_bootstrap_program(Arc::new(compiled));
        let owner = RealmId::fresh();
        runner
            .provision_child_session(child_id, owner, None, &[])
            .await
            .expect("fresh child session provisions through its real factory");
        assert!(runner
            .access
            .child_sessions
            .lock()
            .unwrap()
            .contains(&child_id));
        assert_eq!(
            machines.kind(child_id),
            Some(tidepool_runtime::session::SlotKind::Idle)
        );

        let parent_checkout = machines
            .checkout_run(parent_id)
            .expect("hold the source checkout so custody transfer must wait");
        let lease = runner.child_session_startup_lease(child_id);
        let mut preparation = Box::pin(async {
            let _lease = lease;
            runner
                .transfer_custody(entry, parent_id, child_id, owner)
                .await
        });
        let progress = std::future::poll_fn(|cx| Poll::Ready(preparation.as_mut().poll(cx))).await;
        assert!(
            matches!(progress, Poll::Pending),
            "preparation must be awaiting the held source checkout"
        );
        drop(preparation);
        assert_eq!(
            machines.kind(child_id),
            None,
            "cancelled startup removes its child machine"
        );
        assert!(!runner
            .access
            .child_sessions
            .lock()
            .unwrap()
            .contains(&child_id));
        assert_eq!(
            machines.kind(parent_id),
            Some(tidepool_runtime::session::SlotKind::Running),
            "child cleanup preserves the parent's independent checkout"
        );
        drop(parent_checkout);
        let after = runner
            .access
            .with_host_machine("render-parent", parent_id, None, move |session, _| {
                Ok(session.render_retained_preview(&retained, 64))
            })
            .await
            .expect("parent remains registered after cancelled child startup")
            .expect("independent parent custody remains evaluable");
        assert_eq!(after, before);
    }

    /// [`ResidentActorRunner::import_shared_custody`]: the non-consuming,
    /// borrowing counterpart `transfer_custody` cannot stand in for, used by
    /// `resume_progress_observation` when an observer's session differs
    /// from a published `ProgressSnapshot`'s owning session. A published
    /// snapshot may be observed by several observers off the SAME retained
    /// `Arc<RootCustody>` -- proves: (a) two independent observers each get
    /// their own readable custody from one shared owning root; (b) the
    /// owning root's own handle count is UNCHANGED by either import (the
    /// export never consumes it, so a later observer -- or a retry of an
    /// earlier one -- can still export it again); (c) both imported
    /// custodies are independently discardable with no double-free.
    #[tokio::test]
    async fn import_shared_custody_lets_two_observers_read_one_published_root() {
        tidepool_testing::eval_harness::require_extract();
        let surface = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[])
            .expect("materialize minimal effect surface");

        let owner_id = tidepool_repr::SessionId(0xC0_C0);
        let observer_one_id = tidepool_repr::SessionId(0xC1_C1);
        let observer_two_id = tidepool_repr::SessionId(0xC2_C2);
        let (mut owner_session, owner_root) = bare_session_at(owner_id);
        let (mut observer_one, observer_one_root) = bare_session_at(observer_one_id);
        let (mut observer_two, observer_two_root) = bare_session_at(observer_two_id);

        let _warm_owner = bind_bare_value(
            &mut owner_session,
            owner_id,
            owner_root.path(),
            &surface,
            1,
            "warmOwner",
            "0 :: Int",
        );
        let _warm_one = bind_bare_value(
            &mut observer_one,
            observer_one_id,
            observer_one_root.path(),
            &surface,
            1,
            "warmOne",
            "0 :: Int",
        );
        let _warm_two = bind_bare_value(
            &mut observer_two,
            observer_two_id,
            observer_two_root.path(),
            &surface,
            1,
            "warmTwo",
            "0 :: Int",
        );

        let registry = Arc::new(tidepool_runtime::session::ImageRegistry::new());
        owner_session.set_image_registry(Arc::clone(&registry));
        observer_one.set_image_registry(Arc::clone(&registry));
        observer_two.set_image_registry(Arc::clone(&registry));

        let progress_value = bind_bare_value(
            &mut owner_session,
            owner_id,
            owner_root.path(),
            &surface,
            2,
            "progress",
            "333 :: Int",
        );
        let progress_before = owner_session
            .render_retained_preview(&progress_value, 64)
            .expect("the owning session can render its own custody");
        // Every observer must independently install the SAME image for its
        // import to resolve the exported static top -- same reasoning as
        // `transfer_custody`'s own cross-session case above.
        let _progress_warm_one = bind_bare_value(
            &mut observer_one,
            observer_one_id,
            observer_one_root.path(),
            &surface,
            2,
            "progress",
            "333 :: Int",
        );
        let _progress_warm_two = bind_bare_value(
            &mut observer_two,
            observer_two_id,
            observer_two_root.path(),
            &surface,
            2,
            "progress",
            "333 :: Int",
        );

        let machines = Arc::new(ActorMachineRegistry::<
            frunk::HNil,
            tidepool_mcp::CapturedOutput,
        >::new());
        machines.insert_idle(owner_id, Box::new(owner_session));
        machines.insert_idle(observer_one_id, Box::new(observer_one));
        machines.insert_idle(observer_two_id, Box::new(observer_two));
        let source = ActorWorkbenchSource::new(
            surface.preamble().to_string(),
            surface.include_paths().to_vec(),
        );
        let runner: ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput> =
            ResidentActorRunner::new(Arc::clone(&machines), source);

        async fn handle_count(
            runner: &ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
            session_id: tidepool_repr::SessionId,
        ) -> usize {
            runner
                .access
                .with_host_machine("count", session_id, None, |session, _| {
                    Ok(session.value_handle_count())
                })
                .await
                .expect("checkout for counting handles")
        }

        let owner = RealmId::fresh();
        let shared = Arc::new(progress_value);

        let owner_before = handle_count(&runner, owner_id).await;

        let one_before = handle_count(&runner, observer_one_id).await;
        let imported_one = runner
            .import_shared_custody(Arc::clone(&shared), owner_id, observer_one_id, owner)
            .await
            .expect("first observer imports its own custody");
        let one_after = handle_count(&runner, observer_one_id).await;
        assert_eq!(
            one_after,
            one_before + 1,
            "the first observer mints exactly one new handle"
        );

        let owner_after_one = handle_count(&runner, owner_id).await;
        assert_eq!(
            owner_after_one, owner_before,
            "a borrowed export never changes the owning session's own handle count"
        );

        let two_before = handle_count(&runner, observer_two_id).await;
        let imported_two = runner
            .import_shared_custody(Arc::clone(&shared), owner_id, observer_two_id, owner)
            .await
            .expect("a second observer can export the SAME still-live root again");
        let two_after = handle_count(&runner, observer_two_id).await;
        assert_eq!(
            two_after,
            two_before + 1,
            "the second observer mints its own, independent new handle"
        );

        let owner_after_two = handle_count(&runner, owner_id).await;
        assert_eq!(
            owner_after_two, owner_before,
            "the second (repeated) export still never touches the owning session's handle count"
        );

        let render_one = runner
            .access
            .with_host_machine("render", observer_one_id, None, move |session, _| {
                Ok(session.render_retained_preview(&imported_one, 64))
            })
            .await
            .expect("checkout for rendering")
            .expect("observer one can render its independent import");
        let render_two = runner
            .access
            .with_host_machine("render", observer_two_id, None, move |session, _| {
                Ok(session.render_retained_preview(&imported_two, 64))
            })
            .await
            .expect("checkout for rendering")
            .expect("observer two can render its independent import");
        assert_eq!(
            render_one, progress_before,
            "observer one sees the published content"
        );
        assert_eq!(
            render_two, progress_before,
            "observer two sees the published content"
        );

        // The owning root is still live and unconsumed after both imports --
        // a later (third) observer, or a retry, could still export it again.
        let owner_final_render = runner
            .access
            .with_host_machine("render", owner_id, None, move |session, _| {
                Ok(session.render_retained_preview(&shared, 64))
            })
            .await
            .expect("checkout for rendering")
            .expect("the owning session can still render its own retained root");
        assert_eq!(
            owner_final_render, progress_before,
            "the owning root outlives both borrowed exports, unconsumed"
        );
    }

    #[test]
    fn spawn_child_session_without_an_installed_factory_is_a_named_error() {
        let (machines, _context, source, _root) = actor_registry_fixture();
        let runner: ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput> =
            ResidentActorRunner::new(machines, source);
        let error = runner
            .spawn_child_session(tidepool_repr::SessionId(999_999))
            .expect_err("no factory installed");
        assert!(error.contains("no child-session factory"));
    }

    fn child_session_with_outstanding_custody() -> (
        ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
        Arc<ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>>,
        tidepool_repr::SessionId,
        RootCustody,
    ) {
        let (machines, context, source, _root) = actor_registry_fixture();
        let child_id = tidepool_repr::SessionId(context.placement.session.0.wrapping_add(11));
        let (mut child_session, _child_root) = bare_session_at(child_id);
        // `ResidentSession::retain_binding_custody` resolves a name only in
        // `ScopeId::ROOT` (`BindingTable::resolve`'s own doc comment: "the
        // ROOT frame"), so the binding this test mounts (and later resolves
        // a custody token for) must live there too, not in a freshly minted
        // scope.
        let lexical_scope = tidepool_codegen::scope::ScopeId::ROOT;
        let resource_scope = RealmId::fresh();
        child_session
            .set_actor_execution(
                tidepool_runtime::session::SessionRunContext {
                    lexical_scope,
                    resource_scope,
                    ..tidepool_runtime::session::SessionRunContext::ROOT
                },
                tidepool_effect::EffectRunPolicy::HandleOrSuspend,
                tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .expect("actor execution context");
        let child_context = crate::ActorSessionContext {
            placement: crate::ActorPlacement {
                session: child_id,
                resource_scope,
                lexical_scope,
            },
            ..context.clone()
        };
        // A real compiled/mounted binding bootstraps the session's engine;
        // `retain_binding_custody` then mints a `RootCustody` over it —
        // the SAME shape a reply or retained-progress value crosses the
        // resident-workbench boundary as — and this local variable is what
        // "outside the session" means: it is not released until this test
        // drops it below.
        mount_text_binding(
            &mut child_session,
            &child_context,
            &source,
            &[],
            "outstanding",
            "still here",
            None,
        )
        .expect("mount a real binding to hold a live handle");
        let custody = child_session
            .retain_binding_custody("outstanding")
            .expect("retain mounted binding")
            .expect("the mounted binding resolves to a live custody token");
        machines.insert_idle(child_id, Box::new(child_session));

        let runner = ResidentActorRunner::new(Arc::clone(&machines), source);
        runner
            .access
            .child_sessions
            .lock()
            .unwrap()
            .insert(child_id);
        (runner, machines, child_id, custody)
    }

    fn child_session_with_binding_lease() -> (
        ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
        Arc<ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>>,
        tidepool_repr::SessionId,
        tidepool_runtime::session::resident::BindingLease,
        tempfile::TempDir,
    ) {
        let (machines, runner, context, root) = child_admission_fixture();
        let id = context.placement.session;
        runner.access.child_sessions.lock().unwrap().insert(id);
        let (mut session, receipt) = machines.checkout_run(id).unwrap().into_parts();
        let lease = session.lease_bindings(&[]);
        machines.settle_suspended(receipt, session, Vec::new());
        (runner, machines, id, lease, root)
    }

    async fn assert_child_cleanup_finished(
        runner: &ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
        id: tidepool_repr::SessionId,
        owner: &Arc<PendingChildTeardown>,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while runner.access.machines.kind(id).is_some()
                || !owner
                    .worker
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal cleanup worker finishes");
        assert!(!runner.access.child_sessions.lock().unwrap().contains(&id));
        assert!(!runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .contains_key(&id));
    }

    #[tokio::test]
    async fn reaper_release_final_binding_lease_wakes_and_finishes_cleanup() {
        let (runner, machines, id, lease, _root) = child_session_with_binding_lease();
        runner.retire_child_session(id, false).await.unwrap();
        assert!(
            machines.kind(id).is_some(),
            "preparation lease retains the machine"
        );
        let owner = runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .clone();
        std::thread::spawn(move || drop(lease)).join().unwrap();
        assert_child_cleanup_finished(&runner, id, &owner).await;
    }

    #[tokio::test]
    async fn reaper_release_terminal_external_checkout_clears_membership_and_finishes_worker() {
        let (runner, machines, id, lease, _root) = child_session_with_binding_lease();
        runner.retire_child_session(id, false).await.unwrap();
        let owner = runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .clone();
        assert!(machines.remove(id, "external terminal owner").is_some());
        let result = runner
            .access
            .with_host_machine(
                "observe-terminal-child",
                id,
                None,
                |_, _| -> Result<(), ResidentActorWorkbenchError> {
                    panic!("terminal child cannot enter")
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(ResidentActorWorkbenchError::Checkout(
                CheckoutError::Retired { .. }
            ))
        ));
        assert_child_cleanup_finished(&runner, id, &owner).await;
        runner.retire_child_session(id, false).await.unwrap();
        assert!(!runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .contains_key(&id));
        drop(lease);
    }

    #[tokio::test]
    async fn reaper_release_panicked_checkout_clears_membership_and_finishes_worker() {
        let (runner, _machines, id, lease, _root) = child_session_with_binding_lease();
        runner.retire_child_session(id, false).await.unwrap();
        let owner = runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .clone();
        let result = runner
            .access
            .with_host_machine(
                "panic-terminal-child",
                id,
                None,
                |_, _| -> Result<(), ResidentActorWorkbenchError> {
                    panic!("injected terminal checkout panic")
                },
            )
            .await;
        assert!(matches!(result, Err(ResidentActorWorkbenchError::Join(_))));
        assert_child_cleanup_finished(&runner, id, &owner).await;
        drop(lease);
    }

    #[tokio::test]
    async fn reaper_release_unknown_initial_checkout_clears_membership_and_finishes_worker() {
        let (_machines, runner, context, _root) = child_admission_fixture();
        let id = tidepool_repr::SessionId(context.placement.session.0 + 1);
        runner.access.child_sessions.lock().unwrap().insert(id);
        let retirement = runner.retire_child_session(id, false);
        tokio::pin!(retirement);
        assert!(matches!(
            futures_util::poll!(retirement.as_mut()),
            std::task::Poll::Pending
        ));
        let owner = runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .clone();
        retirement.await.unwrap();
        assert_child_cleanup_finished(&runner, id, &owner).await;
        runner.retire_child_session(id, false).await.unwrap();
        assert!(!runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .contains_key(&id));
    }

    #[tokio::test]
    async fn reaper_release_terminal_cleanup_preserves_different_worker_owner() {
        let (runner, _machines, id, lease, _root) = child_session_with_binding_lease();
        let owner = Arc::new(PendingChildTeardown::new());
        runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .insert(id, owner.clone());
        let stale = Arc::new(PendingChildTeardown::new());
        assert!(!runner
            .access
            .finish_child_session_teardown(id, Some(&stale)));
        assert!(runner.access.child_sessions.lock().unwrap().contains(&id));
        assert!(runner.access.owns_pending_child_teardown(id, &owner));
        assert!(runner
            .access
            .finish_child_session_teardown(id, Some(&owner)));
        assert!(!runner.access.child_sessions.lock().unwrap().contains(&id));
        assert!(!runner
            .access
            .pending_child_teardown
            .lock()
            .unwrap()
            .contains_key(&id));
        drop(lease);
    }

    /// A dedicated child session whose actor has retired, but for which a
    /// `RootCustody` is still held outside the session is not torn down. The
    /// count must use outstanding custody rather than `value_handle_count`,
    /// which includes the session's own private bindings. Once the held token
    /// drops, its single cleanup owner checks out the session and tears it
    /// down; the test waits for both registry removal and worker completion.
    #[tokio::test]
    async fn retiring_a_child_session_defers_then_completes_once_custody_clears() {
        let (runner, machines, child_id, custody) = child_session_with_outstanding_custody();
        runner
            .retire_child_session(child_id, false)
            .await
            .expect("retirement check itself does not fail");
        assert!(
            machines.kind(child_id).is_some(),
            "outstanding custody must defer the release"
        );
        assert!(
            runner
                .access
                .pending_child_teardown
                .lock()
                .unwrap()
                .contains_key(&child_id),
            "the deferral must be recorded"
        );
        let owner = Arc::clone(
            runner
                .access
                .pending_child_teardown
                .lock()
                .unwrap()
                .get(&child_id)
                .expect("pending cleanup owner"),
        );

        // Release the custody itself — the only thing outstanding. The
        // session owner signals cleanup, so this test makes no explicit
        // checkout after the reader drops.
        drop(custody);
        tokio::time::timeout(Duration::from_secs(2), async {
            while machines.kind(child_id).is_some()
                || !owner
                    .worker
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the cleanup worker checks released custody and completes");
        assert_eq!(
            machines.kind(child_id),
            None,
            "the deferred release completes once custody clears"
        );
    }

    #[tokio::test]
    async fn concurrent_child_retirements_share_one_cleanup_owner() {
        let (runner, machines, child_id, custody, _root) = child_session_with_binding_lease();
        let (first, second) = tokio::join!(
            runner.retire_child_session(child_id, false),
            runner.retire_child_session(child_id, false),
        );
        first.expect("first retirement check");
        second.expect("concurrent retirement shares the existing cleanup owner");
        assert!(machines.kind(child_id).is_some(), "custody defers teardown");

        let owner = Arc::clone(
            runner
                .access
                .pending_child_teardown
                .lock()
                .unwrap()
                .get(&child_id)
                .expect("pending cleanup owner"),
        );
        drop(custody);
        tokio::time::timeout(Duration::from_secs(2), async {
            while machines.kind(child_id).is_some()
                || !owner
                    .worker
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the single cleanup owner finishes after the last reader drops");
    }

    #[tokio::test]
    async fn canceled_child_retirement_keeps_the_cleanup_owner_until_last_reader_drops() {
        let (runner, machines, child_id, custody, _root) = child_session_with_binding_lease();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let blocker_access = runner.access.sharing();
        let blocker_entered = Arc::clone(&entered);
        let blocker_release = Arc::clone(&release);
        let blocker = tokio::spawn(async move {
            blocker_access
                .with_host_machine("hold-child-session", child_id, None, move |_, _| {
                    blocker_entered.notify_one();
                    let (lock, changed) = &*blocker_release;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = changed.wait(released).unwrap();
                    }
                    Ok(())
                })
                .await
        });
        entered.notified().await;

        let inspection_access = runner.access.sharing();
        let retirement =
            tokio::spawn(async move { runner.retire_child_session(child_id, false).await });
        let owner = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(owner) = inspection_access
                    .pending_child_teardown
                    .lock()
                    .unwrap()
                    .get(&child_id)
                    .cloned()
                {
                    break owner;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retirement installed its cleanup owner before waiting for checkout");
        retirement.abort();
        let _ = retirement.await;

        {
            let (lock, changed) = &*release;
            *lock.lock().unwrap() = true;
            changed.notify_all();
        }
        blocker
            .await
            .expect("blocking checkout task")
            .expect("blocking checkout settles");

        tokio::time::timeout(Duration::from_secs(2), async {
            while !owner
                .initial_checkout_complete
                .load(std::sync::atomic::Ordering::Acquire)
                || !inspection_access
                    .pending_child_teardown
                    .lock()
                    .unwrap()
                    .contains_key(&child_id)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached cleanup checked once while custody remained outstanding");
        drop(custody);

        tokio::time::timeout(Duration::from_secs(2), async {
            while machines.kind(child_id).is_some()
                || !owner
                    .worker
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the detached cleanup owner completes after caller cancellation and final drop");
    }

    /// Like [`actor_registry_fixture`], but mints a second, isolated lexical
    /// scope on the SAME resident session and returns a second context
    /// placed there — so two actors contend for the one session's checkout
    /// queue without also racing each other's declaration/value scope, which
    /// would otherwise force `prepare_cell`'s own bounded staleness retries
    /// and make a checkout-timing assertion depend on that unrelated
    /// contention too.
    fn actor_registry_fixture_two_scopes() -> (
        Arc<ActorMachineRegistry<frunk::HNil, tidepool_mcp::CapturedOutput>>,
        crate::ActorSessionContext,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        tempfile::TempDir,
    ) {
        let (mut session, context_a, source, root) = host_mount_fixture();
        let mut context_b = context_a.clone();
        context_b.placement.lexical_scope = session.mint_isolated_scope();
        let session_id = context_a.placement.session;
        let machines = Arc::new(ActorMachineRegistry::<
            frunk::HNil,
            tidepool_mcp::CapturedOutput,
        >::new());
        machines.insert_idle(session_id, Box::new(session));
        (machines, context_a, context_b, source, root)
    }

    #[test]
    fn folded_cell_reserves_identity_before_another_actors_compile() {
        let (mut session, context_a, source, _root) = host_mount_fixture();
        let mut context_b = context_a.clone();
        context_b.placement.lexical_scope = session.mint_isolated_scope();
        let (source_a, snapshot_a) = snapshot_cell_split(
            &mut session,
            &context_a,
            source.clone(),
            &[],
            None,
            None,
            None,
        )
        .expect("first snapshot");
        let (source_b, snapshot_b) =
            snapshot_cell_split(&mut session, &context_b, source, &[], None, None, None)
                .expect("second snapshot");
        let generation_a = snapshot_a.view.next_value_generation();
        let generation_b = snapshot_b.view.next_value_generation();
        assert_ne!(
            generation_a, generation_b,
            "off-checkout writes need exclusive identities"
        );

        // Both compiles finish before either actor returns for reservation.
        // They must produce different interfaces even in this ordering.
        for (snapshot, source, name, generation) in [
            (&snapshot_a, &source_a, "firstActor", generation_a),
            (&snapshot_b, &source_b, "secondActor", generation_b),
        ] {
            let (_, folded) = check_cell_off_checkout(
                snapshot,
                source,
                &context_a.haskell_effects_alias,
                &format!("{name} <- pure (1 :: Int)"),
            )
            .expect("whole-cell fold");
            assert!(fold_result_matches_generation(
                &folded.expect("fold compiled"),
                generation
            ));
        }
        for (context, source, snapshot, generation) in [
            (&context_a, &source_a, &snapshot_a, generation_a),
            (&context_b, &source_b, &snapshot_b, generation_b),
        ] {
            let reservation =
                reserve_cell_generations(&mut session, context, source, &[], snapshot, 1, None)
                    .expect("revalidate");
            let CellReservation::Ready(ready) = reservation else {
                panic!("unrelated actor must not stale imports");
            };
            assert_eq!(ready.view.next_value_generation(), generation);
        }
    }

    /// A non-`Decl` cell in one actor must not go stale merely because a
    /// DIFFERENT actor, in its own isolated scope, commits a declaration
    /// between this cell's split snapshot and its reservation:
    /// `next_declaration_module()` is the session-wide declaration log's
    /// next-generation counter (`SessionLib::next_module`), but a cell with
    /// no `Decl` item of its own never reads or writes that candidate
    /// module (see `reserve_cell_generations`). Before this fix, the
    /// unconditional `next_declaration_module()` comparison treated every
    /// actor's declaration as staling every OTHER actor's in-flight split
    /// compile, even one that could never have read or written the
    /// generation that moved.
    #[test]
    fn cell_reservation_ignores_another_actors_declaration_between_snapshot_and_reserve() {
        let (mut session, context_a, source, _root) = host_mount_fixture();
        let scope_b = session.mint_isolated_scope();

        // Actor A's split snapshot, taken BEFORE actor B declares —
        // captures the CURRENT `next_declaration_module()` as this cell's
        // (unused, since it has no `Decl` item) candidate.
        let (source_a, snapshot_a) =
            snapshot_cell_split(&mut session, &context_a, source, &[], None, None, None)
                .expect("actor A's split snapshot");

        // Actor B commits a declaration in its OWN isolated scope, between
        // actor A's snapshot and its reservation — advancing the session-
        // wide `next_declaration_module()` counter without touching actor
        // A's own declaration chain, imports, or visible values.
        session
            .define_scoped_with_imports_in(
                scope_b,
                &["otherActorDecl x = x + (1 :: Int)"],
                &SourceImports::default(),
            )
            .expect("actor B's declaration commits");

        // Actor A's cell has no `Decl` item of its own, so reserving its
        // generations must succeed even though the global candidate-module
        // counter moved out from under its snapshot.
        let reservation = reserve_cell_generations(
            &mut session,
            &context_a,
            &source_a,
            &[],
            &snapshot_a,
            1,
            None,
        )
        .expect("reservation does not error");
        assert!(
            matches!(reservation, CellReservation::Ready(_)),
            "another actor's declaration must not stale a cell with no Decl \
             item of its own"
        );
    }

    /// `ResidentActorWorkbench::prepare_cell` must detect a `Decl` item from
    /// its off-checkout whole-cell check and stage it through the split
    /// (`reserve_cell_generations`'s `render_declaration_candidate_in` plus
    /// `validate_declaration_candidate` against a private candidate
    /// directory, then `finalize_cell_install`'s `adopt_staged_declaration_in`)
    /// rather than falling back to `prepare_cell_single_checkout` — the exact
    /// path `same_cell_redeclaration_and_use_needs_hiding_to_resolve` (below)
    /// exercises directly against `prepare_cell_in_session`, and still the
    /// one this cell's session-root include tree must end up looking exactly
    /// as if it had run.
    #[tokio::test]
    async fn cell_split_installs_a_declaration_cell_through_the_split() {
        let (machines, context, source, root) = actor_registry_fixture();
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);
        let cell = "declaredFn args = length (args :: [Int])".to_string();

        let (checked, prepared) = workbench
            .prepare_cell(context, cell)
            .await
            .expect("a declaration cell prepares through the split");
        assert!(
            checked
                .items
                .iter()
                .any(|item| item.verdict.kind == TurnKind::Decl),
            "a bare top-level declaration must classify as Decl: {checked:?}"
        );
        let PreparedCell::Ready { mut items, .. } = prepared else {
            panic!("a declaration cell should prepare as Ready");
        };
        assert!(
            matches!(items.remove(0).ready, PreparedCellStep::Declaration { .. }),
            "the declaration item must use the declaration plane"
        );
        // The split never writes a candidate into the shared session root
        // until `finalize_cell_install` adopts it; the first (only)
        // generation installed here must be the one, real `.hs` file on
        // disk, with no leftover candidate artifact from an earlier,
        // privately-validated attempt.
        let lib_dir = root.path().join("Tidepool/Session/Lib");
        let entries = std::fs::read_dir(&lib_dir)
            .expect("the declaration plane wrote its include tree")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            vec!["G1.hs".to_string()],
            "exactly one installed generation, no orphaned candidate: {entries:?}"
        );
    }

    /// A cell with both a `Decl` item and a later item that uses it must
    /// come out identically whether `prepare_cell` takes the split path
    /// (GHC-validating the declaration off-checkout, against a private
    /// candidate directory, then installing it in
    /// `finalize_cell_install`) or `prepare_cell_single_checkout` (the
    /// original, fully-serialized path `same_cell_redeclaration_and_use_
    /// needs_hiding_to_resolve` exercises). Two independent sessions run the
    /// exact same cell through each path and must land the same Lib
    /// generation, the same declared binders, and — since
    /// [`render::render_module_with_vals`](tidepool_runtime::session) is a
    /// pure function of the declaration log/generation/env — byte-identical
    /// generated module source.
    #[tokio::test]
    async fn cell_split_and_single_checkout_declare_the_same_generation_exports_and_binders() {
        let (machines_split, context_split, source_split, root_split) = actor_registry_fixture();
        let workbench_split =
            ResidentActorWorkbench::new(machines_split, source_split, None, None, vec![]);
        let (machines_single, context_single, source_single, root_single) =
            actor_registry_fixture();
        let workbench_single =
            ResidentActorWorkbench::new(machines_single, source_single, None, None, vec![]);
        let cell =
            "equivDeclFn args = length (args :: [Int])\nequivBound <- pure (equivDeclFn [1, 2, 3])"
                .to_string();

        let (checked_split, prepared_split) = workbench_split
            .prepare_cell(context_split, cell.clone())
            .await
            .expect("the split prepares the declaration+bind cell");
        let (checked_single, prepared_single) = workbench_single
            .prepare_cell_single_checkout(context_single, cell)
            .await
            .expect("single-checkout prepares the declaration+bind cell");

        assert_eq!(checked_split.items.len(), 2, "{checked_split:?}");
        assert_eq!(checked_single.items.len(), 2, "{checked_single:?}");

        let PreparedCell::Ready {
            items: mut items_split,
            ..
        } = prepared_split
        else {
            panic!("the split cell should prepare as Ready");
        };
        let PreparedCell::Ready {
            items: mut items_single,
            ..
        } = prepared_single
        else {
            panic!("the single-checkout cell should prepare as Ready");
        };
        assert_eq!(items_split.len(), 2);
        assert_eq!(items_single.len(), 2);

        let declaration_step = |item: PreparedCellItem| match item.ready {
            PreparedCellStep::Declaration {
                generation,
                binders,
                prologue_only,
            } => (generation, binders, prologue_only),
            _ => panic!("the declaration item must use the declaration plane"),
        };
        let (generation_split, binders_split, prologue_only_split) =
            declaration_step(items_split.remove(0));
        let (generation_single, binders_single, prologue_only_single) =
            declaration_step(items_single.remove(0));
        assert_eq!(
            generation_split, generation_single,
            "both paths declare against an empty session, so both must land at generation 1"
        );
        assert_eq!(binders_split, binders_single);
        assert_eq!(prologue_only_split, prologue_only_single);

        // The bind item's value module number must also line up: the `Decl`
        // item must not have consumed a value generation in either path —
        // `reserve_cell_generations`'s `value_item_count` (the split) and
        // `compile_block_in_view`'s own Decl-skips-reservation check (the
        // single-checkout path) must agree.
        let bind_module = |item: PreparedCellItem| {
            let PreparedCellStep::Executable(ready) = item.ready else {
                panic!("the bind item must compile through the ordinary item path");
            };
            let tidepool_runtime::session::TurnResult::Bind { bound, .. } = ready.result else {
                panic!("the second item must be a bind result");
            };
            bound
                .first()
                .map(|binder| binder.module.clone())
                .expect("the bind produced at least one binder")
        };
        assert_eq!(
            bind_module(items_split.remove(0)),
            bind_module(items_single.remove(0)),
            "the bind item after the declaration must land at the same value module in both paths"
        );

        let split_module =
            std::fs::read_to_string(root_split.path().join("Tidepool/Session/Lib/G1.hs"))
                .expect("the split installed generation 1");
        let single_module =
            std::fs::read_to_string(root_single.path().join("Tidepool/Session/Lib/G1.hs"))
                .expect("single-checkout installed generation 1");
        assert_eq!(
            split_module, single_module,
            "the generated module is a pure function of the log/generation/env, so an identical \
             cell against an identical empty session must render identical source"
        );
    }

    /// A request workbench's mounted JSON input must stay resolvable across
    /// every checkout `prepare_cell`'s split releases and re-acquires: the
    /// input is mounted once, before the split, and must not be
    /// retired until every off-checkout step that reads it (the whole-cell
    /// check and each item's compile) has finished. A two-item bind cell
    /// with one item reading `input` reproduces the exact shape that used
    /// to fail — the input's module went unresolved once the split reached
    /// its off-checkout compiles.
    #[tokio::test]
    async fn cell_split_prepares_a_two_item_bind_cell_that_reads_a_request_workbench_json_input() {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        // A request workbench's preamble always declares `respond`, whether
        // or not the cell calls it, and its signature needs `Replies` in
        // the effect stack to typecheck. `Exomonad` (qualified) is already
        // in scope everywhere this fixture compiles — `host_mount_fixture`
        // bakes it into `source.preamble` — and re-exports `Replies`.
        context.haskell_effects_alias = "'[Exomonad.Replies]".into();
        let workbench = ResidentActorWorkbench::new(
            machines,
            source,
            Some(ResponseExpectation::new("()")),
            Some(crate::RequestId(1)),
            vec![],
        )
        .with_json_input(Some(serde_json::json!({"greeting": "hi"})));
        let cell = "seen <- pure input\nechoed <- pure seen".to_string();

        let (checked, prepared) = workbench
            .prepare_cell(context, cell)
            .await
            .expect("a two-item bind cell with a request input prepares through the split");
        assert_eq!(
            checked.items.len(),
            2,
            "both bind statements must classify as items: {checked:?}"
        );
        assert!(
            checked
                .items
                .iter()
                .all(|item| item.verdict.kind == TurnKind::Bind),
            "neither item is a declaration: {checked:?}"
        );
        let PreparedCell::Ready { items, .. } = prepared else {
            panic!("a two-item bind cell should prepare as Ready");
        };
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| matches!(item.ready, PreparedCellStep::Executable(_))),
            "both items compile through the split's item loop, not the declaration plane"
        );
    }

    #[tokio::test]
    async fn current_request_cell_compiles_closed_site_evidence() {
        let (machines, mut context, mut source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Replies]".into();
        source
            .workbench_imports
            .extend_text("qualified Tidepool.Agent.Reply as TidepoolReply");
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);
        let cell = "scope <- (TidepoolReply.currentRequest :: Eff '[Exomonad.Replies] (TidepoolReply.RequestScope () ()))".to_owned();

        let (checked, prepared) = workbench
            .prepare_cell(context, cell)
            .await
            .expect("a closed request scope compiles with site evidence");
        assert_eq!(checked.items.len(), 1);
        let PreparedCell::Ready { items, .. } = prepared else {
            panic!("the request scope is one executable binding")
        };
        let PreparedCellStep::Executable(ready) = &items[0].ready else {
            panic!("the request scope binding must compile")
        };
        let TurnResult::Bind { compiled, .. } = &ready.result else {
            panic!("the request scope binding must carry compiled sites")
        };
        assert!(
            compiled.asks.iter().any(|site| site.inputs.len() == 3),
            "currentRequest carries input, result, and ResponseResult result evidence"
        );
    }

    /// Mount the source binding through the legacy typed host interface used
    /// by these request-scope alias tests. The custody is retained from the
    /// actual source binder and consumed by the ordinary compiled-binding
    /// mount; these tests do not model activation-input admission.
    async fn mount_request_scope_test_input(
        workbench: &ResidentActorWorkbench<frunk::HNil, tidepool_mcp::CapturedOutput>,
        context: crate::ActorSessionContext,
    ) -> Result<tidepool_repr::SessionVarId, ResidentActorWorkbenchError> {
        let scope = context.placement.lexical_scope;
        let source = workbench.access.source.clone();
        let type_modules = workbench.type_modules.clone();
        workbench
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let (source_id, ..) = session
                    .current_binding_in(scope, "sourceValue")
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::InputMount(
                            "sourceValue is not visible in the request scope test".into(),
                        )
                    })?;
                let custody = session
                    .retain_binding_custody_in(scope, "sourceValue", source_id)?
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::InputMount(
                            "sourceValue has no retained custody".into(),
                        )
                    })?;
                let (binder, compiled, generation) = compile_host_binding(
                    session,
                    context,
                    &source,
                    &type_modules,
                    "sessionInput",
                    "()",
                    "sourceValue",
                    SourceImports::default(),
                    false,
                )?;
                session.mount_compiled_binding_in(
                    scope,
                    &binder,
                    generation,
                    &compiled.table,
                    custody,
                )?;
                Ok(tidepool_repr::SessionVarId::from_extract(binder.var_id))
            })
            .await
    }

    /// Compile one authentic producer and receiver, then retain two original
    /// native inputs for independent activation mounts in the same machine.
    fn activation_input_fixture(
        configure: impl FnOnce(&mut tidepool_runtime::session::SessionLib),
    ) -> (
        ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        Vec<tidepool_runtime::session::RuntimeActivationInput>,
        tempfile::TempDir,
    ) {
        use tidepool_runtime::session::{ModuleEnv, SessionLib};
        let (_, mut context, _, _) = host_mount_fixture();
        let declarations = [
            tidepool_mcp::agent_tools_decl(),
            tidepool_mcp::agent_session_decl(),
            tidepool_mcp::actor_decl(),
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
            tidepool_mcp::fs_read_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::notifications_decl(),
            tidepool_mcp::console_decl(),
            tidepool_mcp::sleep_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
        let mut include = effects.include_paths().to_vec();
        include.push(tidepool_testing::eval_harness::prelude_path());
        include.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge/haskell/actors"));
        let mut preamble = tidepool_mcp::build_preamble(&declarations, false);
        for import in [
            "Tidepool.Agent.Reply (Replies)",
            "Tidepool.Agent.Ref (AgentProtocol(..))",
            "qualified Tidepool.Agent.Ref as Ref",
            "qualified Tidepool.Actors.Internal.Agent as Agents",
            "qualified Tidepool.Effects.Core as Core",
        ] {
            preamble = insert_preamble_imports(&preamble, import);
        }
        context.haskell_effects_alias = "'[Replies]".into();
        context.effect_policy = tidepool_effect::EffectRunPolicy::SuspendAll;
        context.placement.lexical_scope = ScopeId::ROOT;
        let root = tempfile::tempdir().unwrap();
        let mut lib = SessionLib::open(
            context.placement.session,
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(include.clone());
        configure(&mut lib);
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        session
            .set_actor_execution(
                context.run_context(),
                context.effect_policy,
                context.live_payload,
            )
            .unwrap();
        let source = ActorWorkbenchSource::new(preamble, include);
        let view = actor_compile_view(&session, &context, &source, &[]).unwrap();
        let prepared = source
            .prepare_effectful(&view, &context.haskell_effects_alias)
            .unwrap();
        let templates = resident_workbench_templates(
            &prepared.preamble,
            &context.haskell_effects_alias,
            &prepared.imports,
        );
        let compile = |text: &str| {
            let include = prepared
                .include
                .iter()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>();
            tidepool_runtime::session::turn::run_turn(TurnRequest {
                exact_context: None,
                session_id: None,
                turn_text: text,
                templates: &templates,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &[],
                gen: view.next_value_generation().0,
                verdict: None,
                target: None,
                retained_imports: &[],
            })
            .unwrap()
        };
        let TurnResult::Bind {
            compiled: producer, ..
        } = compile(include_str!(
            "../../../tidepool/runtime/src/session/fixtures/activation-input-function.hs"
        ))
        else {
            panic!("native input producer must execute a bind");
        };
        let TurnResult::Bind {
            compiled: receiver,
            bound,
            ..
        } = compile(include_str!(
            "../../../tidepool/runtime/src/session/fixtures/activation-input-receiver.hs"
        ))
        else {
            panic!("native receiver must install retained bindings");
        };
        session
            .run_projected_bind_with_sites(
                "activationReceiverFixture",
                receiver.code(),
                &bound,
                view.next_value_generation(),
            )
            .unwrap();
        let suspend = |outcome| match outcome {
            ResidentOutcome::Suspended { hole, .. } => hole,
            other => panic!("native activation fixture must suspend: {other:?}"),
        };
        let mut reservation = suspend(
            session
                .run_with_sites("originalActivationRequests", producer.code())
                .unwrap(),
        );
        let mut inputs = Vec::new();
        for request in 1_i64..=2 {
            let submission = suspend(session.resume(reservation, request).unwrap());
            let payload = session
                .live_payload_handle(submission.cont_id())
                .unwrap()
                .unwrap();
            let receiver = session
                .retain_binding_custody("activationReceiver")
                .unwrap()
                .unwrap();
            let activation = suspend(
                session
                    .run_rooted_application(
                        "originalActivationInput",
                        &receiver,
                        &payload,
                        context.placement.resource_scope,
                        None,
                    )
                    .unwrap(),
            );
            let input = producer
                .asks
                .iter()
                .filter(|site| !site.inputs.is_empty())
                .find_map(|site| {
                    session
                        .capture_activation_input(
                            &activation,
                            context.placement.resource_scope,
                            site.site,
                        )
                        .ok()
                })
                .expect("original authenticated input site");
            inputs.push(input);
            session
                .abort(
                    activation.cont_id(),
                    "fixture retained original input".into(),
                )
                .unwrap();
            let next = session.resume(submission, ()).unwrap();
            if request == 1 {
                reservation = suspend(next);
            } else {
                break;
            }
        }
        (session, context, source, inputs, root)
    }

    #[tokio::test]
    async fn activation_inputs_preserve_durable_private_admission_across_replacement() {
        struct RunOwner {
            root: PathBuf,
            _lock: std::fs::File,
        }
        impl tidepool_runtime::session::RecoveryRunAuthority for RunOwner {
            fn owns_run(&self, root: &std::path::Path) -> std::io::Result<bool> {
                Ok(root.canonicalize()? == self.root)
            }
        }
        let durable = tempfile::tempdir().unwrap();
        let manifest = durable.path().join("declarations.json");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(durable.path().join("run-owner.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let authority = Arc::new(RunOwner {
            root: durable.path().canonicalize().unwrap(),
            _lock: lock,
        });
        let (mut session, context, source, inputs, _root) = activation_input_fixture(|lib| {
            lib.attach_owned_recovery_graph_v3(&manifest, authority)
                .unwrap();
        });
        let path = tidepool_repr::ActorPath::parse("root/activation-child").unwrap();
        let durable_owner =
            tidepool_runtime::session::RecoveryPublicOwner::new(&path, context.actor.incarnation.0)
                .unwrap();
        let public_scope = context.placement.lexical_scope;
        session
            .initialize_durable_public_scope(durable_owner.clone(), public_scope)
            .unwrap();
        let descriptor = crate::ActorDescriptor::new("activation-child", context.placement)
            .with_persistence_policy(crate::ActorPersistencePolicy::Durable)
            .with_actor_path(path);
        let owner = crate::resident_actor::WorkbenchPublicOwner::issue(
            &context,
            &descriptor,
            Some(durable_owner.clone()),
        )
        .unwrap();
        let machines = Arc::new(ActorMachineRegistry::new());
        machines.insert_idle(context.placement.session, Box::new(session));
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![])
            .with_compilation_authority(
                crate::resident_actor::WorkbenchCompilationAuthority::for_test(context.clone()),
            );
        let mut previous = None;
        let mut captured = None;
        for input in inputs {
            let before_manifest = std::fs::read(&manifest).unwrap();
            let bootstrap = workbench_runner_for_test(&workbench)
                .begin_public_bootstrap(context.clone(), owner.clone())
                .await
                .unwrap();
            let (_, _, binding) = workbench
                .mount_activation_input(context.clone(), input, "()".into(), None, vec![])
                .await
                .unwrap();
            assert_eq!(
                workbench_runner_for_test(&workbench)
                    .publish_public_bootstrap(context.clone(), owner.clone(), bootstrap)
                    .await
                    .unwrap(),
                tidepool_runtime::session::PublicManifestCommit::Durable
            );
            assert_ne!(std::fs::read(&manifest).unwrap(), before_manifest);
            assert_ne!(previous, Some(binding));
            let expected = durable_owner.clone();
            let custody = workbench
                .access
                .with_machine(context.clone(), move |session, _, _| {
                    let _admission = session
                        .begin_durable_private_execution(&expected, public_scope)
                        .map_err(ResidentError::Session)?;
                    assert_eq!(
                        session
                            .current_binding_in(public_scope, "sessionInput")
                            .unwrap()
                            .0,
                        binding
                    );
                    let custody = session
                        .retain_binding_custody_in(public_scope, "sessionInput", binding)?
                        .unwrap();
                    Ok(custody)
                })
                .await
                .expect("published activation admits an ordinary durable private cell");
            if captured.is_none() {
                captured = Some(custody);
            }
            previous = Some(binding);
        }
        workbench
            .access
            .with_machine(context, move |session, _, _| {
                let _admission = session
                    .begin_durable_private_execution(&durable_owner, public_scope)
                    .map_err(ResidentError::Session)?;
                assert!(session
                    .render_retained_preview(&captured.unwrap(), 128)
                    .is_some());
                assert!(session
                    .retain_binding_custody_in(public_scope, "sessionInput", previous.unwrap())?
                    .is_some());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn activation_refuses_reserved_declaration_without_public_mutation() {
        let (mut session, context, source, mut inputs, _root) = activation_input_fixture(|_| {});
        let step = begin_fragment(
            &mut session,
            &context,
            &source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            ParsedBlock {
                ordinal: 1,
                total: 1,
                source: "sessionInput = ()".into(),
            },
            None,
            None,
        )
        .unwrap();
        assert!(matches!(step, ResidentWorkbenchStep::Committed { .. }));
        let public_scope = context.placement.lexical_scope;
        let before = session.public_visibility_snapshot_in(public_scope).unwrap();
        let machines = Arc::new(ActorMachineRegistry::new());
        machines.insert_idle(context.placement.session, Box::new(session));
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![])
            .with_compilation_authority(
                crate::resident_actor::WorkbenchCompilationAuthority::for_test(context.clone()),
            );
        let error = workbench
            .mount_activation_input(context.clone(), inputs.remove(0), "()".into(), None, vec![])
            .await
            .unwrap_err();
        assert!(
            matches!(error, ResidentActorWorkbenchError::InputMount(ref detail) if detail.contains("reserved"))
        );
        workbench
            .access
            .with_machine(context, move |session, _, _| {
                assert_eq!(
                    session.public_visibility_snapshot_in(public_scope),
                    Some(before)
                );
                assert!(session
                    .current_decl_heads_in(public_scope)
                    .iter()
                    .any(|(name, _)| name == "sessionInput"));
                assert!(session
                    .current_binding_in(public_scope, "sessionInput")
                    .is_none());
                Ok(())
            })
            .await
            .unwrap();
    }

    fn workbench_runner_for_test(
        workbench: &ResidentActorWorkbench<frunk::HNil, tidepool_mcp::CapturedOutput>,
    ) -> ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput> {
        ResidentActorRunner {
            access: workbench.access.sharing(),
        }
    }

    #[tokio::test]
    async fn request_scope_alias_borrow_refuses_a_shadowed_mount() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench = ResidentActorWorkbench::new(machines, source.clone(), None, None, vec![]);
        let step = workbench
            .begin_fragment_split(
                context.clone(),
                source,
                vec![],
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "sourceValue <- pure ()".into(),
                },
                None,
            )
            .await
            .expect("source value compiles");
        if let ResidentWorkbenchStep::Running { fragment, outcome } = step {
            workbench
                .settle_item(context.clone(), *fragment, *outcome)
                .await
                .expect("source value binds");
        }
        let scope = context.placement.lexical_scope;
        let first = mount_request_scope_test_input(&workbench, context.clone())
            .await
            .expect("first request-scope alias mount");
        let second = mount_request_scope_test_input(&workbench, context.clone())
            .await
            .expect("second alias shadows the first");
        assert_ne!(first, second);
        workbench
            .access
            .with_machine(context, move |session, _, _| {
                assert_ne!(
                    session
                        .current_binding_in(scope, "sessionInput")
                        .map(|binding| binding.0),
                    Some(first)
                );
                assert_eq!(
                    session
                        .current_binding_in(scope, "sessionInput")
                        .map(|binding| binding.0),
                    Some(second)
                );
                Ok(())
            })
            .await
            .expect("shadowed input is refused by identity");
    }

    #[tokio::test]
    async fn current_request_private_cell_borrows_scope_alias_without_extra_root() {
        let (machines, mut context, mut source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Replies]".into();
        source
            .workbench_imports
            .extend_text("qualified Tidepool.Agent.Reply as TidepoolReply");
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![]);
        let runner = ResidentActorRunner::new(machines, source.clone());
        let step = workbench
            .begin_fragment_split(
                context.clone(),
                source,
                vec![],
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "sourceValue <- pure ()".into(),
                },
                None,
            )
            .await
            .unwrap();
        match step {
            ResidentWorkbenchStep::Running { fragment, outcome } => {
                workbench
                    .settle_item(context.clone(), *fragment, *outcome)
                    .await
                    .unwrap();
            }
            ResidentWorkbenchStep::Committed { .. } => {}
            ResidentWorkbenchStep::Rejected(rejection) => {
                panic!("source binding rejected: {}", rejection.output)
            }
            _ => panic!("source binding did not complete"),
        }
        let original_scope = context.placement.lexical_scope;
        let binding = mount_request_scope_test_input(&workbench, context.clone())
            .await
            .unwrap();
        let (private, before) = workbench
            .access
            .with_machine(context.clone(), move |session, _, _| {
                Ok((
                    session
                        .mint_detached_scope(original_scope)
                        .expect("capture activation"),
                    session.value_handle_count(),
                ))
            })
            .await
            .unwrap();
        context.placement.lexical_scope = private;
        let cell = "requestScope <- (TidepoolReply.currentRequest :: Eff '[Exomonad.Replies] (TidepoolReply.RequestScope () ()))".to_owned();
        let (_, prepared) = workbench
            .prepare_cell(context.clone(), cell.clone())
            .await
            .unwrap();
        let PreparedCell::Ready { mut items, .. } = prepared else {
            panic!("ready private cell")
        };
        let step = workbench
            .begin_prepared_cell_item(
                context.clone(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: cell,
                },
                items.remove(0),
                4096,
            )
            .await
            .unwrap();
        let ResidentWorkbenchStep::Running { fragment, outcome } = step else {
            panic!("request parks")
        };
        let boundary = runner
            .capture_boundary(context.clone(), *outcome, context.placement.resource_scope)
            .await
            .unwrap();
        let ResidentActorBoundary::CurrentRequest {
            continuation,
            site: Some(site),
        } = boundary
        else {
            panic!("typed request")
        };
        let (types, custody_before_resume) = workbench
            .access
            .with_machine(context.clone(), move |session, _, _| {
                let types = session
                    .request_site_type_evidence(site)
                    .expect("closed unit input/reply evidence");
                Ok((types, session.outstanding_custody()))
            })
            .await
            .unwrap();
        let outcome = runner
            .resume_current_request(
                context.clone(),
                continuation,
                Some(site),
                Some((
                    crate::RequestId(1),
                    Arc::new(types),
                    original_scope,
                    binding,
                )),
            )
            .await
            .unwrap();
        workbench
            .access
            .with_machine(context.clone(), move |session, _, _| {
                assert_eq!(
                    session.outstanding_custody(),
                    custody_before_resume,
                    "borrowed response adds no external custody or binding lease"
                );
                Ok(())
            })
            .await
            .unwrap();
        workbench
            .settle_item(context.clone(), *fragment, outcome)
            .await
            .unwrap();
        workbench
            .access
            .with_machine(context, move |session, _, _| {
                assert_eq!(
                    session.value_handle_count(),
                    before + 1,
                    "only the completed requestScope binding adds a root"
                );
                assert_eq!(
                    session
                        .current_binding_in(original_scope, "sessionInput")
                        .map(|row| row.0),
                    Some(binding)
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn current_request_without_an_activation_returns_typed_refusal() {
        let (machines, mut context, mut source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Replies]".into();
        source
            .workbench_imports
            .extend_text("qualified Tidepool.Agent.Reply as TidepoolReply");
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![]);
        let runner = ResidentActorRunner::new(machines, source);
        let cell = "scope <- (TidepoolReply.currentRequest :: Eff '[Exomonad.Replies] (TidepoolReply.RequestScope () ()))".to_owned();
        let (_, prepared) = workbench
            .prepare_cell(context.clone(), cell.clone())
            .await
            .expect("request access compiles");
        let PreparedCell::Ready { mut items, .. } = prepared else {
            panic!("request access is an executable cell")
        };
        let step = workbench
            .begin_prepared_cell_item(
                context.clone(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: cell,
                },
                items.remove(0),
                4096,
            )
            .await
            .expect("request access suspends");
        let ResidentWorkbenchStep::Running { fragment, outcome } = step else {
            panic!("request access must produce a suspended effect")
        };
        let boundary = runner
            .capture_boundary(context.clone(), *outcome, context.placement.resource_scope)
            .await
            .expect("capture request access");
        let ResidentActorBoundary::CurrentRequest { continuation, site } = boundary else {
            panic!("request access is decoded as its typed effect")
        };
        assert!(site.is_some(), "extractor assigns a closed site");
        let outcome = runner
            .resume_current_request(context.clone(), continuation, site, None)
            .await
            .expect("no activation returns a typed refusal");
        workbench
            .settle_item(context, *fragment, outcome)
            .await
            .expect("refusal is a valid RequestScope result");
    }

    /// A single bind item (`x <- e`, no declaration) is the fold's target
    /// shape: `check_cell_off_checkout` speculatively compiles it inside the
    /// SAME worker request as the whole-cell check, so `prepare_cell` never
    /// issues a second `timed_compile` round trip for it. This is the
    /// counter test the fold's whole point rests on.
    #[tokio::test]
    async fn single_bind_item_cell_folds_into_one_daemon_round_trip() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let actor_id = context.actor.id.0;
        let actor_incarnation = context.actor.incarnation.0;
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);
        let cell = "answer <- pure (1 :: Int)".to_string();

        let scope = crate::call_timing::CallScope::new("cell", actor_id, actor_incarnation);
        let (checked, prepared) = scope
            .run(workbench.prepare_cell(context, cell))
            .await
            .expect("a single bind item cell prepares through the split");
        assert_eq!(checked.items.len(), 1, "{checked:?}");
        assert_eq!(checked.items[0].verdict.kind, TurnKind::Bind, "{checked:?}");
        let PreparedCell::Ready { items, .. } = prepared else {
            panic!("a single bind item cell should prepare as Ready");
        };
        assert_eq!(items.len(), 1);
        assert!(
            matches!(items[0].ready, PreparedCellStep::Executable(_)),
            "the folded item still installs through the executable plane"
        );
        assert_eq!(
            scope.compile_count(),
            1,
            "a single-bind-item, no-declaration cell must fold the whole-cell \
             check and its item's compile into ONE daemon round trip"
        );
    }

    /// A cell with more than one item cannot fold — a later item's compile
    /// must import the earlier item's freshly generated `Session.Val.G<n>`
    /// interface, a real module boundary the whole-cell check's own compile
    /// does not produce. `prepare_cell` must keep paying the ordinary two
    /// round trips: the whole-cell check, then the item loop.
    #[tokio::test]
    async fn two_item_bind_cell_keeps_two_daemon_round_trips() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let actor_id = context.actor.id.0;
        let actor_incarnation = context.actor.incarnation.0;
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);
        let cell = "cellA <- pure (1 :: Int)\ncellB <- pure (cellA + 1)".to_string();

        let scope = crate::call_timing::CallScope::new("cell", actor_id, actor_incarnation);
        let (checked, prepared) = scope
            .run(workbench.prepare_cell(context, cell))
            .await
            .expect("a two-item bind cell prepares through the split");
        assert_eq!(checked.items.len(), 2, "{checked:?}");
        let PreparedCell::Ready { items, .. } = prepared else {
            panic!("a two-item bind cell should prepare as Ready");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            scope.compile_count(),
            2,
            "an N-item cell is unaffected by the fold: the whole-cell check \
             and the item loop remain two round trips"
        );
    }

    /// A declaration cell never folds — its item stages through a private
    /// candidate directory (`validate_declaration_candidate`), a GHC-compile
    /// shape the fold does not build. `prepare_cell` must keep paying its
    /// existing two round trips (the whole-cell check, then the combined
    /// declaration-validation-and-item-loop compile).
    #[tokio::test]
    async fn declaration_cell_keeps_two_daemon_round_trips() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let actor_id = context.actor.id.0;
        let actor_incarnation = context.actor.incarnation.0;
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);
        let cell = "declaredFn args = length (args :: [Int])".to_string();

        let scope = crate::call_timing::CallScope::new("cell", actor_id, actor_incarnation);
        let (checked, prepared) = scope
            .run(workbench.prepare_cell(context, cell))
            .await
            .expect("a declaration cell prepares through the split");
        assert!(
            checked
                .items
                .iter()
                .any(|item| item.verdict.kind == TurnKind::Decl),
            "{checked:?}"
        );
        let PreparedCell::Ready { .. } = prepared else {
            panic!("a declaration cell should prepare as Ready");
        };
        assert_eq!(
            scope.compile_count(),
            2,
            "a declaration cell is unaffected by the fold"
        );
    }

    /// An inherited-context fork's parent can commit between the split's
    /// off-checkout compile and its install checkout. The first stale
    /// install must fall straight through to the single-checkout compile:
    /// one split compile (the folded check), one install checkout, one
    /// single-checkout compile, and no second split attempt. Bounded by a
    /// timeout so a second attempt waiting on the probe fails instead of
    /// hanging.
    #[tokio::test]
    async fn stale_split_install_falls_back_to_one_single_checkout_compile() {
        use std::sync::atomic::Ordering;
        let (machines, context, source, _root) = actor_registry_fixture();
        let actor_id = context.actor.id.0;
        let actor_incarnation = context.actor.incarnation.0;
        let workbench = ResidentActorWorkbench::new(machines, source, None, None, vec![]);
        let cell = "answer <- pure (1 :: Int)".to_string();

        let probe = Arc::new(split_probe::SplitProbe {
            held_installs: 1,
            ..Default::default()
        });
        let scope = crate::call_timing::CallScope::new("cell", actor_id, actor_incarnation);
        let prepare = split_probe::PROBE.scope(
            Arc::clone(&probe),
            scope.run(workbench.prepare_cell(context.clone(), cell)),
        );
        // Stand in for the parent committing a binding in the scope this
        // cell reads while the split's compile is off-checkout.
        let interlope = async {
            probe.install_reached.notified().await;
            let interloper_source = workbench.access.source.clone();
            workbench
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    mount_text_binding(
                        session,
                        context,
                        &interloper_source,
                        &[],
                        "interloper",
                        "interloper text",
                        None,
                    )
                })
                .await
                .expect("interloping binding mounts");
            probe.resume_install.notify_one();
        };
        let (prepared, ()) = tokio::time::timeout(std::time::Duration::from_secs(600), async {
            tokio::join!(prepare, interlope)
        })
        .await
        .expect("the stale split settles without a second held attempt");
        let (checked, prepared) = prepared.expect("the cell prepares after the stale split");
        assert_eq!(checked.items.len(), 1, "{checked:?}");
        assert!(
            matches!(prepared, PreparedCell::Ready { .. }),
            "the single-checkout fallback prepares the cell"
        );
        assert_eq!(
            probe.install_checkouts.load(Ordering::SeqCst),
            1,
            "exactly one split attempt reached its install checkout"
        );
        assert_eq!(
            scope.compile_count(),
            1,
            "only the first split attempt's folded compile ran off-checkout"
        );
        assert_eq!(
            probe.single_checkout_compiles.load(Ordering::SeqCst),
            1,
            "the stale install falls through to exactly one single-checkout compile"
        );
    }

    /// A display render whose install checkout finds the view changed
    /// renders again off-checkout, and the retry's page takes a value
    /// generation above both the first attempt's reservation and the
    /// interloping binding's: a stale attempt leaves a gap, never a reused
    /// generation.
    #[tokio::test]
    async fn stale_display_render_retries_at_a_fresh_generation() {
        use std::sync::atomic::Ordering;
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench = ResidentActorWorkbench::new(machines, source.clone(), None, None, vec![]);
        // Bootstrap the machine, so the expression below is an ordinary
        // post-bootstrap install.
        workbench
            .begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "displayRetryWarmup <- pure (0 :: Int)".into(),
                },
                None,
            )
            .await
            .expect("warmup fragment installs");

        let probe = Arc::new(split_probe::SplitProbe {
            held_display_installs: 1,
            ..Default::default()
        });
        let render = split_probe::PROBE.scope(
            Arc::clone(&probe),
            workbench.begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "(41 :: Int) + 1".into(),
                },
                None,
            ),
        );
        let interlope = async {
            probe.install_reached.notified().await;
            let interloper_source = workbench.access.source.clone();
            let generation = workbench
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    mount_text_binding(
                        session,
                        context,
                        &interloper_source,
                        &[],
                        "displayInterloper",
                        "interloper text",
                        None,
                    )?;
                    Ok(session
                        .workbench_bindings_in(context.placement.lexical_scope)
                        .into_iter()
                        .find(|binding| binding.name == "displayInterloper")
                        .and_then(|binding| binding.defining_generation()))
                })
                .await
                .expect("interloping binding mounts")
                .expect("the interloper has a defining generation");
            probe.resume_install.notify_one();
            generation
        };
        let (step, interloper) = tokio::time::timeout(std::time::Duration::from_secs(600), async {
            tokio::join!(render, interlope)
        })
        .await
        .expect("the stale render settles");
        let ResidentWorkbenchStep::Committed { output, .. } = step.expect("the expression commits")
        else {
            panic!("the expression did not commit");
        };
        assert!(output.contains("42"), "{output}");
        assert!(!output.contains("Display failed"), "{output}");
        assert_eq!(
            probe.display_installs.load(Ordering::SeqCst),
            2,
            "the stale render retried off-checkout and reached a second install"
        );
        assert_eq!(
            probe.single_checkout_compiles.load(Ordering::SeqCst),
            0,
            "nothing compiled under the checkout"
        );
        let generations = probe
            .display_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let [first, retry] = generations.as_slice() else {
            panic!("expected two display attempts: {generations:?}");
        };
        assert!(
            retry > first && *retry > interloper,
            "retry generation {retry} must exceed the first attempt's {first} and the \
             interloper's {interloper}"
        );
    }

    /// A minimal [`tracing_subscriber::fmt::MakeWriter`] capturing JSON log
    /// lines into a shared buffer, so a test can read back structured field
    /// values (here, `with_host_machine`'s "resident machine checkout
    /// admitted" `waited_ms`) instead of only formatted text.
    #[derive(Clone)]
    struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
        type Writer = CapturedLog;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Two actors preparing cells concurrently, sharing one resident
    /// session: the split keeps the machine released across each actor's
    /// GHC work, so the second actor's checkout admissions
    /// (`with_host_machine`'s "resident machine checkout admitted"
    /// `waited_ms`) must stay short snapshot/install waits — never on the
    /// order of the first actor's own whole-cell compile time, the way an
    /// unsplit single checkout held across the whole compile would force.
    #[tokio::test]
    async fn cell_split_second_actors_checkout_wait_excludes_first_actors_ghc_compile() {
        let (machines, mut context_a, mut context_b, source, _root) =
            actor_registry_fixture_two_scopes();
        let workbench = Arc::new(ResidentActorWorkbench::new(
            machines,
            source,
            None,
            None,
            vec![],
        ));

        context_a.actor = crate::ActorRef::first(crate::ActorId(101));
        context_b.actor = crate::ActorRef::first(crate::ActorId(102));
        let actor_b_label = context_b.actor.to_string();

        let log = CapturedLog(Arc::new(std::sync::Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(log.clone())
            .with_max_level(tracing::Level::INFO)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let wb_a = Arc::clone(&workbench);
        let wb_b = Arc::clone(&workbench);
        // Actor A's cell is deliberately larger (three sequential items, each
        // its own whole-cell-check-plus-compile round trip) so its total GHC
        // time dominates the run; actor B's is the smallest possible cell.
        let cell_a =
            "wA1 <- pure (1 :: Int)\nwA2 <- pure (wA1 + 1)\nwA3 <- pure (wA2 + 1)".to_string();
        let cell_b = "wB1 <- pure (1 :: Int)".to_string();

        let started = std::time::Instant::now();
        let (result_a, result_b) = tokio::join!(
            wb_a.prepare_cell(context_a, cell_a),
            wb_b.prepare_cell(context_b, cell_b)
        );
        let total = started.elapsed();
        let (checked_a, _) = result_a.expect("actor A's cell prepares");
        let (checked_b, _) = result_b.expect("actor B's cell prepares");
        assert!(checked_a
            .items
            .iter()
            .all(|item| item.verdict.kind != TurnKind::Decl));
        assert!(checked_b
            .items
            .iter()
            .all(|item| item.verdict.kind != TurnKind::Decl));

        drop(_guard);
        let log_text = String::from_utf8(
            log.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
        .expect("captured log is UTF-8");
        let waited: Vec<u64> = log_text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value
                    .get("fields")
                    .and_then(|fields| fields.get("message"))
                    .and_then(|message| message.as_str())
                    == Some("resident machine checkout admitted")
                    && value
                        .get("fields")
                        .and_then(|fields| fields.get("actor"))
                        .and_then(|actor| actor.as_str())
                        == Some(actor_b_label.as_str())
            })
            .filter_map(|value| {
                let waited_ms = value.get("fields")?.get("waited_ms")?;
                // `u128` fields render as a JSON string, not a number, in
                // tracing-subscriber's JSON formatter.
                waited_ms
                    .as_u64()
                    .or_else(|| waited_ms.as_str()?.parse().ok())
            })
            .collect();
        assert!(
            !waited.is_empty(),
            "actor B must have logged at least one checkout admission: {log_text}"
        );
        let max_wait = *waited.iter().max().expect("non-empty");
        // With the split, actor B's checkout waits are short
        // snapshot/reserve/install steps, never actor A's whole GHC compile.
        // Without the split, actor B's very first checkout wait alone would
        // be on the order of `total` (actor A holding the machine for its
        // entire compile).
        assert!(
            u128::from(max_wait) < total.as_millis() / 2,
            "actor B's longest checkout wait ({max_wait}ms) should be a small fraction of the \
             run's total time ({}ms) — the split must keep the machine released during actor \
             A's GHC compile",
            total.as_millis()
        );
    }

    /// `begin_fragment_split`'s install step must hand off to
    /// `begin_ready_block_split` for a STEADY-STATE fragment (one compiled
    /// after the resident machine is already bootstrapped) — the same
    /// off-checkout JIT install `begin_prepared_cell_item` already uses for
    /// a split cell's item, instead of running the Cranelift compile under
    /// the checkout the way `begin_ready_block` does. `prepare_tools` (the
    /// child spec installer wave 4 measured holding the machine for 272s of
    /// JIT across 22 installs) is built on `begin_fragment_split`. This
    /// must still commit the exact same output and bindings as the
    /// original single-checkout `begin_fragment`, against an identically
    /// warmed-up session — `begin_ready_block_split`'s own tests (e.g.
    /// `stale_import_between_snapshot_and_revalidation_falls_back` in
    /// tidepool-runtime) already cover that its JIT compile itself runs
    /// off-checkout; this proves the swap did not change what
    /// `begin_fragment_split` commits.
    #[tokio::test]
    async fn begin_fragment_split_after_bootstrap_matches_single_checkout_begin_fragment() {
        let (split_machines, split_context, split_source, _split_root) = actor_registry_fixture();
        let split_workbench =
            ResidentActorWorkbench::new(split_machines, split_source.clone(), None, None, vec![]);
        let (mut direct_session, direct_context, direct_source, _direct_root) =
            host_mount_fixture();

        // A brand-new session's very first turn bootstraps the resident
        // machine and has no split path at all (`ResidentSession::
        // prepared_machine_ready`'s doc comment): warm the split side up
        // with one throwaway fragment first, so the fragment under test
        // exercises the split's STEADY-STATE install
        // (`begin_ready_block_split`), not the one-time bootstrap install
        // this change does not touch. The receipt text and installed
        // binder names carry no generation number, so this warmup does not
        // need a matching one on the direct (single-checkout) side.
        let warmup_block = ParsedBlock {
            ordinal: 1,
            total: 1,
            source: "fragInstallWarmup <- pure (0 :: Int)".into(),
        };
        let warmup_verdict = TurnClassification {
            kind: TurnKind::Bind,
            binders: vec!["fragInstallWarmup".into()],
            items: Vec::new(),
        };
        split_workbench
            .begin_fragment_split(
                split_context.clone(),
                split_source.clone(),
                Vec::new(),
                warmup_block,
                Some(warmup_verdict),
            )
            .await
            .expect("warmup fragment installs");

        let block = ParsedBlock {
            ordinal: 1,
            total: 1,
            source: "fragInstallOffCheckout <- pure (1 :: Int)".into(),
        };
        let verdict = TurnClassification {
            kind: TurnKind::Bind,
            binders: vec!["fragInstallOffCheckout".into()],
            items: Vec::new(),
        };
        let split_step = split_workbench
            .begin_fragment_split(
                split_context,
                split_source,
                Vec::new(),
                block.clone(),
                Some(verdict.clone()),
            )
            .await
            .expect("fragment installs through the post-bootstrap split");
        let direct_step = begin_fragment(
            &mut direct_session,
            &direct_context,
            &direct_source,
            RequestWorkbenchScope {
                response: None,
                request: None,
                type_modules: &[],
            },
            block,
            None,
            Some(&verdict),
        )
        .expect("single-checkout begin_fragment installs and runs");

        let ResidentWorkbenchStep::Committed {
            output: split_output,
            installed_bindings: split_bindings,
            ..
        } = split_step
        else {
            panic!("post-bootstrap split fragment did not commit");
        };
        let ResidentWorkbenchStep::Committed {
            output: direct_output,
            installed_bindings: direct_bindings,
            ..
        } = direct_step
        else {
            panic!("single-checkout begin_fragment did not commit");
        };
        assert_eq!(split_output, direct_output);
        assert_eq!(split_bindings, direct_bindings);
    }

    /// A declaration cell's own GHC validation runs off-checkout, against a
    /// private candidate directory — so two actors declaring concurrently,
    /// in the SAME lexical scope (unlike
    /// [`actor_registry_fixture_two_scopes`], which deliberately isolates
    /// scopes to avoid this exact contention), must race for the one shared
    /// `next_declaration_module()` generation: whichever installs second sees
    /// its candidate go stale at `finalize_cell_install` and falls back to
    /// [`Self::prepare_cell_single_checkout`]. Both must still land, with two DISTINCT
    /// generations, and — the property this split exists to preserve — the
    /// shared session root must end up with exactly those two real, complete
    /// modules: no half-written or overwritten candidate from the loser's
    /// discarded attempt, because a candidate never touches the root until
    /// its own `adopt_staged_declaration_in` succeeds.
    #[tokio::test]
    async fn cell_split_concurrent_declarations_in_one_scope_retry_without_a_torn_lib_module() {
        let (machines, context_a, source, root) = actor_registry_fixture();
        let mut context_b = context_a.clone();
        let mut context_a = context_a;
        context_a.actor = crate::ActorRef::first(crate::ActorId(301));
        context_b.actor = crate::ActorRef::first(crate::ActorId(302));
        let workbench = Arc::new(ResidentActorWorkbench::new(
            machines,
            source,
            None,
            None,
            vec![],
        ));

        let wb_a = Arc::clone(&workbench);
        let wb_b = Arc::clone(&workbench);
        let cell_a = "concurrentDeclA x = x + (1 :: Int)".to_string();
        let cell_b = "concurrentDeclB x = x * (2 :: Int)".to_string();

        let (result_a, result_b) = tokio::join!(
            wb_a.prepare_cell(context_a, cell_a),
            wb_b.prepare_cell(context_b, cell_b)
        );
        let (checked_a, prepared_a) = result_a.expect("actor A's declaration prepares");
        let (checked_b, prepared_b) = result_b.expect("actor B's declaration prepares");
        assert!(checked_a
            .items
            .iter()
            .any(|item| item.verdict.kind == TurnKind::Decl));
        assert!(checked_b
            .items
            .iter()
            .any(|item| item.verdict.kind == TurnKind::Decl));

        let generation_of = |prepared: PreparedCell| {
            let PreparedCell::Ready { mut items, .. } = prepared else {
                panic!("a declaration cell should prepare as Ready");
            };
            match items.remove(0).ready {
                PreparedCellStep::Declaration { generation, .. } => generation,
                _ => panic!("the declaration item must use the declaration plane"),
            }
        };
        let generation_a = generation_of(prepared_a);
        let generation_b = generation_of(prepared_b);
        assert_ne!(
            generation_a, generation_b,
            "two concurrent declarations in one scope must land at distinct generations"
        );

        let lib_dir = root.path().join("Tidepool/Session/Lib");
        let mut entries = std::fs::read_dir(&lib_dir)
            .expect("the declaration plane wrote its include tree")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            vec!["G1.hs".to_string(), "G2.hs".to_string()],
            "exactly the two installed generations, no torn or orphaned candidate: {entries:?}"
        );
        let g1 = std::fs::read_to_string(lib_dir.join("G1.hs")).expect("G1.hs is a real file");
        let g2 = std::fs::read_to_string(lib_dir.join("G2.hs")).expect("G2.hs is a real file");
        assert!(
            g1.contains("concurrentDeclA") || g2.contains("concurrentDeclA"),
            "concurrentDeclA must appear in exactly one installed generation: G1={g1:?} G2={g2:?}"
        );
        assert!(
            g1.contains("concurrentDeclB") || g2.contains("concurrentDeclB"),
            "concurrentDeclB must appear in exactly one installed generation: G1={g1:?} G2={g2:?}"
        );
    }

    /// A rejection `revalidate_cell_rejection` was produced against — the
    /// exact `c_view` `compile_cell_items_off_checkout` compiled against —
    /// must not be trusted once another binding invalidates that view before
    /// the revalidation checkout runs: it must be reported `Stale`, exactly
    /// as `finalize_cell_install`'s own revalidation already is proven by
    /// `cell_split_scope_mutation_before_final_checkout_forces_a_recompile`.
    /// A fresh snapshot recompiles and installs cleanly afterward.
    #[test]
    fn cell_split_item_rejection_before_revalidation_checkout_is_retried_on_a_stale_view() {
        let (mut session, context, base_source, _root) = host_mount_fixture();
        let cell = "onlyItem <- pure (1 :: Int)";

        let (source, snapshot) = snapshot_cell_split(
            &mut session,
            &context,
            base_source.clone(),
            &[],
            None,
            None,
            None,
        )
        .expect("snapshot");
        let (checked, _folded) =
            check_cell_off_checkout(&snapshot, &source, &context.haskell_effects_alias, cell)
                .expect("whole-cell check");
        let reservation = reserve_cell_generations(
            &mut session,
            &context,
            &source,
            &[],
            &snapshot,
            checked.items.len(),
            None,
        )
        .expect("reserve generations");
        let CellReservation::Ready(ready) = reservation else {
            panic!("no interleaved mutation yet: the reservation must be fresh");
        };
        let CellReservationReady { view, .. } = *ready;

        // Nothing has mutated yet: a rejection compiled against this exact
        // view must still be reported current.
        let revalidation = revalidate_cell_rejection(
            &mut session,
            &context,
            &source,
            &[],
            Some(snapshot.candidate_module),
            &view,
        )
        .expect("revalidate against the unmutated view");
        assert!(
            matches!(revalidation, CellRejectionRevalidation::StillCurrent),
            "an untouched view must settle as still current"
        );

        // Stand in for another actor writing to this exact scope between the
        // off-checkout item compile and the revalidation checkout.
        mount_text_binding(
            &mut session,
            &context,
            &source,
            &[],
            "interloper",
            "interloper text",
            None,
        )
        .expect("interloping carrier mounts");

        let revalidation = revalidate_cell_rejection(
            &mut session,
            &context,
            &source,
            &[],
            Some(snapshot.candidate_module),
            &view,
        )
        .expect("revalidate against the mutated view");
        assert!(
            matches!(
                revalidation,
                CellRejectionRevalidation::Stale(SplitStaleView::CompileView)
            ),
            "an interleaved mutation must invalidate the rejected view, not settle it as current"
        );

        // A fresh snapshot recompiles and installs cleanly. Snapshots
        // again from the pristine base source, not
        // the mutated one above.
        let (source, retry_snapshot) =
            snapshot_cell_split(&mut session, &context, base_source, &[], None, None, None)
                .expect("retry snapshot");
        let (retry_checked, _retry_folded) = check_cell_off_checkout(
            &retry_snapshot,
            &source,
            &context.haskell_effects_alias,
            cell,
        )
        .expect("retry whole-cell check");
        let retry_reservation = reserve_cell_generations(
            &mut session,
            &context,
            &source,
            &[],
            &retry_snapshot,
            retry_checked.items.len(),
            None,
        )
        .expect("retry reserve generations");
        let CellReservation::Ready(retry_ready) = retry_reservation else {
            panic!("retry reservation must be fresh");
        };
        let CellReservationReady {
            view: retry_view,
            retained: retry_retained,
            visible_names: retry_visible_names,
            declaration: _,
        } = *retry_ready;
        let retry_outcome = compile_cell_items_off_checkout(
            &context,
            &source,
            &context.haskell_effects_alias,
            &retry_checked,
            cell,
            retry_view.clone(),
            &retry_retained,
            &retry_visible_names,
            None,
            None,
        )
        .expect("retry item compile");
        let CellItemsOutcome::Ready(retry_items) = retry_outcome else {
            panic!("retry item must compile");
        };
        let retry_install = finalize_cell_install(
            &mut session,
            &context,
            &source,
            &[],
            retry_snapshot.candidate_module,
            &retry_view,
            &retry_checked,
            retry_items,
            None,
        )
        .expect("retry finalize install");
        assert!(
            matches!(
                retry_install,
                CellInstall::Ready(PreparedCell::Ready { .. })
            ),
            "the cell must prepare once retried against a fresh view"
        );
    }

    /// A fragment fixture whose evaluation genuinely suspends waiting on a
    /// host answer (`ActorContext`'s query, since this bare test session has
    /// no local actor context handler) — the same shape `prepare_tools` gets
    /// back from `begin_fragment_split` before its second checkout decodes
    /// and resumes it.
    fn suspending_fragment() -> (ParsedBlock, TurnClassification) {
        (
            ParsedBlock {
                ordinal: 1,
                total: 1,
                source: "notified <- maybe (pure (Left NotificationUnauthorized)) \
                          (\\p -> Exomonad.sendMessage p \"hello\") =<< Exomonad.parentAgent"
                    .into(),
            },
            TurnClassification {
                kind: TurnKind::Bind,
                binders: vec!["notified".into()],
                items: Vec::new(),
            },
        )
    }

    #[tokio::test]
    async fn public_visibility_snapshot_pairs_declaration_and_exact_binding_identities() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![]);
        let runner = ResidentActorRunner::new(machines, source.clone());
        let before = runner
            .public_visibility_snapshot(context.clone())
            .await
            .expect("initial public view");
        let step = workbench
            .begin_fragment_split(
                context.clone(),
                source,
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "snapshotValue <- pure (42 :: Int)".into(),
                },
                None,
            )
            .await
            .expect("Haskell binding compiles");
        if let ResidentWorkbenchStep::Running { fragment, outcome } = step {
            workbench
                .settle_item(context.clone(), *fragment, *outcome)
                .await
                .expect("Haskell binding settles");
        }
        let bound = runner
            .public_visibility_snapshot(context.clone())
            .await
            .expect("bound public view");
        assert!(bound.epoch > before.epoch);
        assert_eq!(bound.declaration_tip, before.declaration_tip);
        assert!(before
            .bindings
            .iter()
            .all(|(name, _)| name != "snapshotValue"));
        assert!(bound
            .bindings
            .iter()
            .any(|(name, _)| name == "snapshotValue"));

        runner
            .access
            .with_machine(context.clone(), |session, context, _| {
                session
                    .define_scoped_in(
                        context.placement.lexical_scope,
                        &["data SnapshotTag = SnapshotTag"],
                    )
                    .expect("declaration commits");
                Ok(())
            })
            .await
            .expect("declaration checkout");
        let declared = runner
            .public_visibility_snapshot(context)
            .await
            .expect("declaration public view");
        assert!(declared.epoch > bound.epoch);
        assert!(declared.declaration_tip.0 > bound.declaration_tip.0);
        assert_eq!(declared.bindings, bound.bindings);
    }

    #[tokio::test]
    async fn answered_haskell_effect_reports_later_continuation_failure_as_delivered() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![]);
        let runner = ResidentActorRunner::new(machines, source.clone());
        let step = workbench
            .begin_fragment_split(
                context.clone(),
                source,
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "sleep (minutes 0) >> error \"failure after answer\"".into(),
                },
                None,
            )
            .await
            .expect("Haskell fragment suspends at sleep");
        let ResidentWorkbenchStep::Running { outcome, .. } = step else {
            panic!("expected a running fragment: {}", describe_step(&step));
        };
        let ResidentOutcome::Suspended { hole, .. } = *outcome else {
            panic!("sleep effect must suspend");
        };
        let failure = runner
            .resume_unit(context, hole)
            .await
            .expect_err("resumed Haskell continuation must fail");
        assert!(
            matches!(failure, ResidentActorWorkbenchError::Delivered(_)),
            "{failure}"
        );
    }

    #[tokio::test]
    async fn captured_haskell_binding_outlives_failed_parent_and_released_token_for_two_delayed_children(
    ) {
        let (machines, context, source, _root) = actor_registry_fixture();
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![]);
        let runner = ResidentActorRunner::new(Arc::clone(&machines), source.clone());
        let parent = workbench
            .begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: "capturedValue <- pure (41 :: Int)".into(),
                },
                None,
            )
            .await
            .expect("parent Haskell binding compiles");
        let parent = match parent {
            ResidentWorkbenchStep::Running { fragment, outcome } => workbench
                .settle_item(context.clone(), *fragment, *outcome)
                .await
                .expect("parent binding commits"),
            step => step,
        };
        assert!(matches!(parent, ResidentWorkbenchStep::Committed { .. }));

        let (captured_scope, retained) = runner
            .capture_retained_context_scope(context.clone())
            .await
            .expect("capture independently owned parent Haskell environment");
        let retained_scope = retained.scope();
        let groups = crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default());
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread".into(),
            "unfinished-parent".into(),
            "unfinished-parent".into(),
        );
        let token = groups.capture_checkpoint_with_retained_scope(
            "real Haskell context".into(),
            context.actor,
            crate::EffectiveRole::root(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            context.placement.session,
            captured_scope,
            boundary.clone(),
            None,
            retained,
            crate::ActorPersistencePolicy::Ephemeral,
        );
        groups
            .settle_checkpoint(&token, context.placement.session, true)
            .expect("Haskell checkpoint answer delivered");
        let first = groups
            .admitted_checkpoint(&token, context.placement.session)
            .expect("first child admitted before parent completion")
            .0;
        let second = groups
            .admitted_checkpoint(&token, context.placement.session)
            .expect("second child admitted before parent completion")
            .0;
        let (release_first, first_ready) = tokio::sync::oneshot::channel();
        let (release_second, second_ready) = tokio::sync::oneshot::channel();
        let delayed_child = |admitted: crate::lineage::CheckpointLease,
                             ready: tokio::sync::oneshot::Receiver<()>,
                             actor_id| {
            let runner = &runner;
            let workbench = &workbench;
            let context = context.clone();
            async move {
                // Production start_child retains this exact share before its
                // asynchronous workspace admission. Model that await here;
                // the actor integration fixture exercises the actual adapter.
                let capsule = Arc::clone(admitted.retained_scope().unwrap());
                ready.await.expect("workspace admission resumes");
                let provisional = workbench
                    .access
                    .with_machine(context.clone(), |session, _, _| {
                        Ok(session.mint_isolated_scope())
                    })
                    .await
                    .expect("child provisional scope");
                let mut child = context;
                child.actor = crate::ActorRef::first(crate::ActorId(actor_id));
                child.placement.lexical_scope = provisional;
                child.placement.lexical_scope = runner
                    .remint_checkpoint_child_scope_from_lease(child.clone(), capsule, provisional)
                    .await
                    .expect("released token cannot erase admitted lexical state");
                (child, admitted)
            }
        };
        let retire_original = async {
            let failed = workbench
                .begin_fragment_split(
                    context.clone(),
                    source.clone(),
                    Vec::new(),
                    ParsedBlock {
                        ordinal: 1,
                        total: 1,
                        source: "sleep (minutes 0) >> error \"parent failed\"".into(),
                    },
                    None,
                )
                .await
                .expect("parent failing continuation compiles");
            let ResidentWorkbenchStep::Running { outcome, .. } = failed else {
                panic!("parent failing continuation runs");
            };
            let ResidentOutcome::Suspended { hole, .. } = *outcome else {
                panic!("parent suspends before failure");
            };
            assert!(matches!(
                runner.resume_unit(context.clone(), hole).await,
                Err(ResidentActorWorkbenchError::Delivered(_))
            ));
            assert!(groups
                .settle_checkpoints(context.actor, &boundary, false)
                .is_empty());
            groups.retire_actor(context.actor);
            let retired = groups
                .release_checkpoint(&token, context.placement.session)
                .expect("release token while both children await workspace admission");
            runner
                .retire_checkpoint_scopes(context.placement.session, retired.into_iter().collect())
                .await
                .expect("original captured scope retires before remint");
            groups
                .confirm_checkpoint_release(&token, context.placement.session, captured_scope)
                .expect("original scope retirement acknowledged");
            assert!(groups.retains_session(context.placement.session));
            assert!(matches!(
                groups.admitted_checkpoint(&token, context.placement.session),
                Err(crate::CheckpointRefusal::ReleasedCheckpoint)
            ));
            release_first.send(()).unwrap();
            release_second.send(()).unwrap();
        };
        let ((first, first_lease), (second, second_lease), ()) = tokio::join!(
            delayed_child(first, first_ready, 2),
            delayed_child(second, second_ready, 3),
            retire_original,
        );
        drop(first_lease);
        assert!(groups.retains_session(context.placement.session));
        drop(second_lease);
        assert!(!groups.retains_session(context.placement.session));
        workbench
            .access
            .with_machine(context.clone(), move |session, context, _| {
                // Ordinary owner admission reaps the dropped lexical capsule.
                let temporary = session.retain_lexical_scope(ScopeId::ROOT)?;
                let scope = temporary.scope();
                drop(temporary);
                session.retire_scope(scope);
                assert!(session.compile_view_in(retained_scope).is_none());
                session.retire_scope(context.placement.lexical_scope);
                Ok(())
            })
            .await
            .expect("capsule and failed parent release their roots");

        for child in [first, second] {
            let step = workbench
                .begin_fragment_split(
                    child.clone(),
                    source.clone(),
                    Vec::new(),
                    ParsedBlock {
                        ordinal: 1,
                        total: 1,
                        source: "capturedValue + 1".into(),
                    },
                    None,
                )
                .await
                .expect("child compiles against inherited binding after final capsule release");
            let step = match step {
                ResidentWorkbenchStep::Running { fragment, outcome } => workbench
                    .settle_item(child.clone(), *fragment, *outcome)
                    .await
                    .expect("child uses independently retained binding"),
                step => step,
            };
            let ResidentWorkbenchStep::Committed { output, .. } = step else {
                panic!("child value did not commit");
            };
            assert_eq!(output.trim(), "42", "actual inherited value must render");
            workbench
                .access
                .with_machine(child.clone(), |session, context, _| {
                    let scope = context.placement.lexical_scope;
                    assert!(session
                        .binding_names_in(scope)
                        .contains(&"capturedValue".into()));
                    session.retire_scope(scope);
                    assert!(session.compile_view_in(scope).is_none());
                    assert_eq!(session.scope_binding_count(scope), 0);
                    Ok(())
                })
                .await
                .expect("last child releases inherited binding ownership");
        }
    }

    #[tokio::test]
    async fn checkpoint_release_retains_exact_scope_after_checkout_failure_until_retry() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let runner = ResidentActorRunner::new(Arc::clone(&machines), source);
        let scope = runner
            .capture_context_scope(context.clone())
            .await
            .expect("captured scope");
        let groups = crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default());
        let token = groups.capture_checkpoint(
            "retryable release".into(),
            context.actor,
            crate::EffectiveRole::root(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            context.placement.session,
            scope,
            tidepool_runtime::session::WorkbenchForkBoundary::external(
                "thread".into(),
                "call".into(),
                "call".into(),
            ),
        );
        groups
            .settle_checkpoint(&token, context.placement.session, true)
            .expect("capture published");
        let removed = machines
            .remove(
                context.placement.session,
                "inject checkpoint cleanup checkout failure",
            )
            .expect("machine was registered");
        assert_eq!(
            groups.release_checkpoint(&token, context.placement.session),
            Ok(Some(scope))
        );
        assert!(runner
            .retire_checkpoint_scopes(context.placement.session, vec![scope])
            .await
            .is_err());
        assert_eq!(
            groups.pending_release_scopes(context.placement.session),
            vec![(token.clone(), scope)]
        );
        assert!(groups.retains_session(context.placement.session));
        assert_eq!(
            groups.checkpoint(&token, context.placement.session).err(),
            Some(crate::CheckpointRefusal::ReleasedCheckpoint)
        );
        let tidepool_runtime::session::Slot::Idle(machine) = removed else {
            panic!("fixture machine was idle");
        };
        machines.insert_idle(context.placement.session, machine);
        let retry = groups
            .release_checkpoint(&token, context.placement.session)
            .expect("idempotent retry");
        assert_eq!(retry, Some(scope));
        runner
            .retire_checkpoint_scopes(context.placement.session, vec![scope])
            .await
            .expect("retry retires exact scope");
        groups
            .confirm_checkpoint_release(&token, context.placement.session, scope)
            .expect("cleanup acknowledged");
        assert!(groups
            .pending_release_scopes(context.placement.session)
            .is_empty());
        assert!(!groups.retains_session(context.placement.session));
        assert_eq!(
            groups.release_checkpoint(&token, context.placement.session),
            Ok(None)
        );
    }

    #[tokio::test]
    async fn external_work_releases_machine_until_its_failure_settles() {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![]);
        let runner = Arc::new(ResidentActorRunner::new(
            Arc::clone(&machines),
            source.clone(),
        ));
        let (block, verdict) = suspending_fragment();
        let step = workbench
            .begin_fragment_split(context.clone(), source, Vec::new(), block, Some(verdict))
            .await
            .expect("fragment parks");
        let ResidentWorkbenchStep::Running { outcome, .. } = step else {
            panic!("expected a parked fragment");
        };
        let ResidentOutcome::Suspended { hole, .. } = *outcome else {
            panic!("expected a parked continuation");
        };
        let cont_id = hole.cont_id().to_string();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task = {
            let runner = Arc::clone(&runner);
            let context = context.clone();
            tokio::spawn(async move {
                runner
                    .run_external(
                        context,
                        hole,
                        tidepool_effect::DeferredEffect::blocking(move || {
                            started_tx.send(()).expect("observer waiting");
                            release_rx.recv().expect("release external work");
                            Err(tidepool_effect::EffectError::Handler(
                                "external refusal".into(),
                            ))
                        }),
                    )
                    .await
            })
        };
        started_rx.await.expect("external work started");
        let observed = tokio::time::timeout(
            Duration::from_secs(5),
            runner.access.with_machine(context.clone(), {
                let cont_id = cont_id.clone();
                move |session, _, _| Ok(session.parked_holes().contains(&cont_id.as_str()))
            }),
        )
        .await;
        // Release the worker even if admission failed, so a failing assertion
        // cannot leave a blocking thread behind in the test runtime.
        release_tx.send(()).expect("external work still waiting");
        assert!(observed
            .expect("other machine work must progress")
            .expect("checkout"));
        let failure = task
            .await
            .expect("external driver joins")
            .expect_err("external failure");
        assert!(
            failure.to_string().contains("external refusal"),
            "{failure}"
        );
        let parked = runner
            .access
            .with_machine(context, move |session, _, _| {
                Ok(session.parked_holes().contains(&cont_id.as_str()))
            })
            .await
            .expect("final checkout");
        assert!(!parked, "failed external work must retire its continuation");
    }

    /// `prepare_tools` holds a suspended `ResidentHole` (from
    /// `begin_fragment_split`) across the `await` for a second machine
    /// checkout that decodes and resumes it. If the task driving that await
    /// is dropped before the checkout is granted, nothing else resumes or
    /// aborts the parked continuation — unless `ParkedHoleAbortGuard`, armed
    /// across exactly that gap, is still attached. This reproduces the gap's
    /// shape directly against the guard (not the full tool-installer
    /// pipeline, which needs a real spec file this unit test has no reason
    /// to stand up): park a hole through `begin_fragment_split`, arm a guard
    /// over it, and drop the task carrying the guard before it ever reaches
    /// (and disarms) a checkout — the same failure `prepare_tools` itself
    /// cannot observe without this guard.
    #[tokio::test]
    async fn dropping_the_task_that_holds_an_armed_parked_hole_guard_aborts_the_hole() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let mut suspend_context = context.clone();
        suspend_context.haskell_effects_alias =
            "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench = Arc::new(ResidentActorWorkbench::new(
            Arc::clone(&machines),
            source.clone(),
            None,
            None,
            vec![],
        ));

        let (block, verdict) = suspending_fragment();
        let step = workbench
            .begin_fragment_split(
                suspend_context.clone(),
                source,
                Vec::new(),
                block,
                Some(verdict),
            )
            .await
            .expect("fragment split suspends waiting on a host answer");
        let ResidentWorkbenchStep::Running { outcome, .. } = step else {
            panic!("expected a suspension: {}", describe_step(&step));
        };
        let ResidentOutcome::Suspended { hole, .. } = *outcome else {
            panic!("running fragment has a suspension")
        };
        let cont_id = hole.cont_id().to_string();

        let parked_before = workbench
            .access
            .with_machine(suspend_context.clone(), {
                let cont_id = cont_id.clone();
                move |session, _, _| Ok(session.parked_holes().contains(&cont_id.as_str()))
            })
            .await
            .expect("checkout");
        assert!(
            parked_before,
            "the suspended fragment must have parked {cont_id}"
        );

        // Build the guard exactly as `prepare_tools` does — armed, keyed on
        // the hole's cont_id, standing between the checkout that parked it
        // and the one meant to resume it — inside a task we then cancel
        // before it ever reaches (and disarms) that second checkout.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let cancelled_task = {
            let workbench = Arc::clone(&workbench);
            let context = suspend_context.clone();
            let cont_id = cont_id.clone();
            tokio::spawn(async move {
                let guard = ParkedHoleAbortGuard::new(
                    &workbench.access,
                    context,
                    cont_id,
                    "test: task cancelled before its resuming checkout".into(),
                );
                ready_tx
                    .send(())
                    .expect("test still waiting on the ready signal");
                // Stand in for the second checkout never being granted before
                // this task is dropped.
                std::future::pending::<()>().await;
                guard.disarm(); // unreachable: the task is aborted first
            })
        };
        ready_rx
            .await
            .expect("the guard was constructed before cancellation");
        cancelled_task.abort();
        let join_result = cancelled_task.await;
        assert!(
            join_result.is_err_and(|error| error.is_cancelled()),
            "the task must have been cancelled, not merely finished"
        );

        // The guard's `Drop` spawns its own background checkout to abort the
        // hole, racing this poll — wait for it to land.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let still_parked = workbench
                .access
                .with_machine(suspend_context.clone(), {
                    let cont_id = cont_id.clone();
                    move |session, _, _| Ok(session.parked_holes().contains(&cont_id.as_str()))
                })
                .await
                .expect("checkout");
            if !still_parked {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the parked hole was never aborted after the guard was dropped armed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn actor_continuation_handoff_preserves_successor_and_aborts_other_cell_holes() {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench =
            ResidentActorWorkbench::new(Arc::clone(&machines), source.clone(), None, None, vec![]);
        let runner = ResidentActorRunner::new(machines, source.clone());
        let cell = ParkedHoleAbortGuard::with_latest(
            &workbench.access,
            context.clone(),
            None,
            "test cell abandoned".into(),
        );
        let registration = cell.registration();
        let (successor, other) = registration
            .scope(async {
                let mut outcomes = Vec::new();
                for _ in 0..2 {
                    let (block, verdict) = suspending_fragment();
                    let step = workbench
                        .begin_fragment_split(
                            context.clone(),
                            source.clone(),
                            vec![],
                            block,
                            Some(verdict),
                        )
                        .await
                        .expect("real native fragment parks");
                    let ResidentWorkbenchStep::Running { outcome, .. } = step else {
                        panic!("expected native suspension");
                    };
                    outcomes.push(*outcome);
                }
                let other = outcomes.pop().unwrap();
                (outcomes.pop().unwrap(), other)
            })
            .await;
        let successor_id = outcome_continuation_id(&successor).unwrap();
        let other_id = outcome_continuation_id(&other).unwrap();
        let actor = registration
            .sync_scope(|| runner.handoff_actor_continuation(context.clone(), &successor))
            .expect("actor accepts exact successor")
            .expect("suspended successor has cleanup");
        assert_eq!(
            registration.awaiting_acknowledgement(),
            vec![other_id.clone()]
        );
        let duplicate = registration
            .sync_scope(|| runner.handoff_actor_continuation(context.clone(), &successor));
        assert!(matches!(
            duplicate,
            Err(ResidentActorWorkbenchError::ContinuationHandoff {
                reason: ContinuationHandoffFailure::NotOwned,
                ..
            })
        ));
        assert_eq!(
            registration.awaiting_acknowledgement(),
            vec![other_id.clone()]
        );
        drop(cell);
        let inspect = |expected_actor: bool| {
            let successor_id = successor_id.clone();
            let other_id = other_id.clone();
            let context = context.clone();
            let workbench = &workbench;
            async move {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let settled = workbench
                            .access
                            .with_machine(context.clone(), {
                                let successor_id = successor_id.clone();
                                let other_id = other_id.clone();
                                move |session, _, _| {
                                    let parked = session.parked_holes();
                                    if expected_actor {
                                        assert!(parked.contains(&successor_id.as_str()));
                                    }
                                    Ok(!parked.contains(&other_id.as_str())
                                        && parked.contains(&successor_id.as_str())
                                            == expected_actor)
                                }
                            })
                            .await
                            .expect("native cleanup checkout");
                        if settled {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("exact cleanup settles");
            }
        };
        inspect(true).await;
        drop(actor);
        inspect(false).await;
    }

    #[tokio::test]
    async fn actor_continuation_handoff_refuses_abandoned_settled_and_foreign_owners() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let runner = ResidentActorRunner::new(machines, source);
        let outcome = ResidentOutcome::Suspended {
            output: vec![],
            hole: ResidentHole::plain("successor"),
            request: HaskellValue::Con(tidepool_repr::DataConId(0), vec![]),
        };
        for reason in [
            ContinuationHandoffFailure::Abandoned,
            ContinuationHandoffFailure::Settled,
            ContinuationHandoffFailure::WrongOwner,
        ] {
            let state = match reason {
                ContinuationHandoffFailure::Abandoned => {
                    ParkedHoleState::Abandoned(["successor".into()].into_iter().collect())
                }
                ContinuationHandoffFailure::Settled => ParkedHoleState::Settled,
                _ => ParkedHoleState::Owned(["successor".into()].into_iter().collect()),
            };
            let mut owner = context.placement;
            if reason == ContinuationHandoffFailure::WrongOwner {
                owner.lexical_scope = tidepool_codegen::scope::ScopeId(u64::MAX);
            }
            let guard = ParkedHoleAbortGuard {
                shared: Arc::new(ParkedHoleAbortState {
                    owner: Some((context.actor, owner)),
                    abort: Arc::new(|_| {}),
                    state: Mutex::new(state),
                    reason: "test".into(),
                    retained_authority: None,
                }),
            };
            let registration = guard.registration();
            let before = registration.awaiting_acknowledgement();
            let error = registration
                .sync_scope(|| runner.handoff_actor_continuation(context.clone(), &outcome))
                .err()
                .expect("refused handoff");
            assert!(
                matches!(error, ResidentActorWorkbenchError::ContinuationHandoff {
                reason: actual, ..
            } if actual == reason)
            );
            assert_eq!(registration.awaiting_acknowledgement(), before);
        }
    }

    #[tokio::test]
    async fn late_parked_hole_registration_aborts_only_its_own_continuation() {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench = Arc::new(ResidentActorWorkbench::new(
            Arc::clone(&machines),
            source.clone(),
            None,
            None,
            vec![],
        ));
        let park = |workbench: Arc<ResidentActorWorkbench<_, _>>,
                    context: crate::ActorSessionContext,
                    source: ActorWorkbenchSource| async move {
            let (block, verdict) = suspending_fragment();
            let step = workbench
                .begin_fragment_split(context, source, Vec::new(), block, Some(verdict))
                .await
                .expect("fragment parks");
            let ResidentWorkbenchStep::Running { outcome, .. } = step else {
                panic!("expected a parked fragment")
            };
            *outcome
        };
        let owned = park(Arc::clone(&workbench), context.clone(), source.clone()).await;
        let unrelated = park(Arc::clone(&workbench), context.clone(), source).await;
        let owned_id = outcome_continuation_id(&owned).expect("owned hole");
        let unrelated_id = outcome_continuation_id(&unrelated).expect("unrelated hole");
        assert_ne!(owned_id, unrelated_id);

        let guard = ParkedHoleAbortGuard::with_latest(
            &workbench.access,
            context.clone(),
            None,
            "slot caller left before its blocking checkout returned".into(),
        );
        let registration = guard.registration();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let workbench_for_checkout = Arc::clone(&workbench);
        let checkout_context = context.clone();
        let checkout = tokio::spawn(async move {
            workbench_for_checkout
                .access
                .with_machine(checkout_context, move |session, _, _| {
                    entered_tx.send(()).expect("test awaits checkout");
                    release_rx.recv().expect("test releases checkout");
                    registration.replace_in_checkout(session, &owned);
                    Ok(())
                })
                .await
        });
        tokio::task::spawn_blocking(move || entered_rx.recv().expect("checkout entered"))
            .await
            .expect("waiter joins");
        drop(guard);
        release_tx.send(()).expect("checkout still waiting");
        checkout
            .await
            .expect("checkout joins")
            .expect("checkout succeeds");

        let parked = workbench
            .access
            .with_machine(context, move |session, _, _| {
                Ok(session
                    .parked_holes()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>())
            })
            .await;
        let parked = parked.expect("inspect parked holes");
        assert!(!parked.contains(&owned_id));
        assert!(parked.contains(&unrelated_id));
    }

    #[tokio::test]
    async fn cancelled_slot_aborts_a_hole_created_by_a_late_blocking_checkout() {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench = Arc::new(ResidentActorWorkbench::new(
            Arc::clone(&machines),
            source.clone(),
            None,
            None,
            vec![],
        ));
        let (block, verdict) = suspending_fragment();
        let unrelated = workbench
            .begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                block,
                Some(verdict),
            )
            .await
            .expect("unrelated fragment parks");
        let ResidentWorkbenchStep::Running { outcome, .. } = unrelated else {
            panic!("expected unrelated suspension")
        };
        let unrelated_id = outcome_continuation_id(&outcome).expect("unrelated hole");

        let (parked_tx, parked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task_workbench = Arc::clone(&workbench);
        let checkout_workbench = Arc::clone(&workbench);
        let task_context = context.clone();
        let task = tokio::spawn(async move {
            task_workbench
                .with_exact_continuation_cleanup(
                    task_context.clone(),
                    "late blocking slot".into(),
                    async move {
                        checkout_workbench
                            .access
                            .with_machine(task_context, move |session, context, _| {
                                let (block, verdict) = suspending_fragment();
                                let step = begin_fragment(
                                    session,
                                    context,
                                    &source,
                                    RequestWorkbenchScope {
                                        response: None,
                                        request: None,
                                        type_modules: &[],
                                    },
                                    block,
                                    None,
                                    Some(&verdict),
                                )?;
                                let ResidentWorkbenchStep::Running { outcome, .. } = &step else {
                                    panic!("expected late suspension")
                                };
                                parked_tx
                                    .send(outcome_continuation_id(outcome).expect("late hole"))
                                    .expect("test waits for late hole");
                                release_rx.recv().expect("test releases checkout");
                                Ok(step)
                            })
                            .await
                    },
                )
                .await
        });
        let late_id = tokio::task::spawn_blocking(move || parked_rx.recv().expect("hole parked"))
            .await
            .expect("waiter joins");
        task.abort();
        assert!(task.await.is_err_and(|error| error.is_cancelled()));
        release_tx.send(()).expect("checkout still blocked");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let ids = workbench
                .access
                .with_machine(context.clone(), move |session, _, _| {
                    Ok(session
                        .parked_holes()
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>())
                })
                .await
                .expect("inspect parked holes");
            if !ids.contains(&late_id) {
                assert!(ids.contains(&unrelated_id));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "late hole still parked"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn cancelled_slot_scope_aborts_its_hole_and_keeps_an_unrelated_hole() {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench = Arc::new(ResidentActorWorkbench::new(
            Arc::clone(&machines),
            source.clone(),
            None,
            None,
            vec![],
        ));
        let (block, verdict) = suspending_fragment();
        let unrelated = workbench
            .begin_fragment_split(
                context.clone(),
                source.clone(),
                Vec::new(),
                block,
                Some(verdict),
            )
            .await
            .expect("unrelated fragment parks");
        let ResidentWorkbenchStep::Running { outcome, .. } = unrelated else {
            panic!("expected an unrelated suspension")
        };
        let unrelated_id = outcome_continuation_id(&outcome).expect("unrelated hole");

        let (parked_tx, parked_rx) = tokio::sync::oneshot::channel();
        let task_workbench = Arc::clone(&workbench);
        let slot_workbench = Arc::clone(&workbench);
        let task_context = context.clone();
        let task = tokio::spawn(async move {
            task_workbench
                .with_exact_continuation_cleanup(
                    task_context.clone(),
                    "cancelled slot".into(),
                    async {
                        let (block, verdict) = suspending_fragment();
                        let step = slot_workbench
                            .begin_fragment_split(
                                task_context,
                                source,
                                Vec::new(),
                                block,
                                Some(verdict),
                            )
                            .await
                            .expect("slot fragment parks");
                        let ResidentWorkbenchStep::Running { outcome, .. } = step else {
                            panic!("expected slot suspension")
                        };
                        parked_tx
                            .send(outcome_continuation_id(&outcome).expect("slot hole"))
                            .expect("test waiting for slot hole");
                        std::future::pending::<()>().await;
                    },
                )
                .await
        });
        let slot_id = parked_rx.await.expect("slot parked");
        task.abort();
        assert!(task.await.expect_err("cancelled task").is_cancelled());

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let ids = workbench
                .access
                .with_machine(context.clone(), move |session, _, _| {
                    Ok(session
                        .parked_holes()
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>())
                })
                .await
                .expect("inspect parked holes");
            if !ids.contains(&slot_id) {
                assert!(ids.contains(&unrelated_id));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "slot hole still parked"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn failed_abort_retains_frame_authority_until_owning_realm_retirement() {
        failed_abort_frame_owner(false).await;
    }

    #[tokio::test]
    async fn failed_abort_releases_frame_authority_after_confirmed_native_machine_loss() {
        failed_abort_frame_owner(true).await;
    }

    #[tokio::test]
    async fn exact_cleanup_acknowledgement_allows_a_later_native_fragment() {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench =
            ResidentActorWorkbench::new(machines.clone(), source.clone(), None, None, vec![]);
        let guard = ParkedHoleAbortGuard::with_latest(
            &workbench.access,
            context.clone(),
            None,
            "test exact cleanup".into(),
        );
        let registration = guard.registration();
        for attempt in 0..2 {
            let (block, verdict) = suspending_fragment();
            let step = registration
                .scope(workbench.begin_fragment_split(
                    context.clone(),
                    source.clone(),
                    vec![],
                    block,
                    Some(verdict),
                ))
                .await
                .expect("real native fragment starts");
            let ResidentWorkbenchStep::Running { fragment, outcome } = step else {
                panic!("fixture must retain a native continuation");
            };
            let id = outcome_continuation_id(&outcome).expect("fixture suspends");
            assert!(registration.awaiting_acknowledgement().contains(&id));
            let mut foreign = context.clone();
            foreign.actor.incarnation.0 += 1;
            assert!(workbench
                .abort_owned_continuations(foreign, registration.clone(), "foreign cleanup".into(),)
                .await
                .is_err());
            assert!(registration.awaiting_acknowledgement().contains(&id));
            workbench
                .abort_owned_continuations(
                    context.clone(),
                    registration.clone(),
                    format!("test attempt {attempt}"),
                )
                .await
                .expect("exact checkout confirms native abort");
            assert!(registration.awaiting_acknowledgement().is_empty());
            assert!(
                matches!(&*registration.0.state.lock(), ParkedHoleState::Owned(ids) if ids.is_empty())
            );
            workbench
                .access
                .with_machine(context.clone(), move |session, _, _| {
                    assert!(!session.parked_holes().contains(&id.as_str()));
                    Ok(())
                })
                .await
                .expect("aborted hole is actually gone");
            drop((fragment, outcome));
        }
        drop(guard);
    }

    async fn failed_abort_frame_owner(lose_machine: bool) {
        let (machines, mut context, source, _root) = actor_registry_fixture();
        context.haskell_effects_alias = "'[Exomonad.Notifications, Exomonad.ActorContext]".into();
        let workbench =
            ResidentActorWorkbench::new(machines.clone(), source.clone(), None, None, vec![]);
        let authority = Arc::new(());
        let authority_weak = Arc::downgrade(&authority);
        let mut failed_cleanup_context = context.clone();
        failed_cleanup_context.placement.lexical_scope = tidepool_codegen::scope::ScopeId(u64::MAX);
        let guard = ParkedHoleAbortGuard::with_retained_latest(
            &workbench.access,
            failed_cleanup_context.clone(),
            None,
            "test failed cleanup admission".into(),
            Some(authority),
        );
        let cleanup_weak = Arc::downgrade(&guard.shared);
        let registration = guard.registration();
        let (block, verdict) = suspending_fragment();
        let (step, nested_cleanup_weak) = registration
            .scope(workbench.with_exact_continuation_cleanup(
                failed_cleanup_context,
                "nested slot cleanup admission failed".into(),
                async {
                    let nested_cleanup_weak = SLOT_CONTINUATION_OWNER
                        .try_with(|owner| Arc::downgrade(&owner.0))
                        .expect("nested production cleanup owner is installed");
                    let step = workbench
                        .begin_fragment_split(context.clone(), source, vec![], block, Some(verdict))
                        .await;
                    (step, nested_cleanup_weak)
                },
            ))
            .await;
        let step = step.expect("real nested native frame parks with original authority");
        let ResidentWorkbenchStep::Running { outcome, .. } = step else {
            panic!("expected a native suspension")
        };
        let owned_id = outcome_continuation_id(&outcome).expect("owned hole");
        drop(outcome);
        drop(guard);
        drop(registration);

        // The failed checkout has returned and no abort claim is kept alive
        // by an artificial cycle. Its lease belongs to the native frame.
        tokio::time::timeout(Duration::from_secs(5), async {
            while cleanup_weak.upgrade().is_some() || nested_cleanup_weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed background cleanup releases its task owner");
        assert!(
            authority_weak.upgrade().is_some(),
            "failed abort keeps exact frame resources"
        );
        let inspect_id = owned_id.clone();
        workbench
            .access
            .with_machine(context.clone(), move |session, _, _| {
                assert!(session.parked_holes().contains(&inspect_id.as_str()));
                session.close_realm(RealmId(u64::MAX));
                assert!(session.parked_holes().contains(&inspect_id.as_str()));
                Ok(())
            })
            .await
            .expect("unrelated realm closure leaves the exact frame intact");
        assert!(
            authority_weak.upgrade().is_some(),
            "unrelated cleanup does not release resources"
        );

        if lose_machine {
            let result: Result<(), ResidentActorWorkbenchError> = workbench
                .access
                .with_machine(context.clone(), |_, _, _| {
                    panic!("test native machine loss")
                })
                .await;
            assert!(matches!(result, Err(ResidentActorWorkbenchError::Join(_))));
            assert!(machines.kind(context.placement.session).is_none());
        } else {
            let owned_realm = context.placement.resource_scope;
            workbench
                .access
                .with_machine(context, move |session, _, _| {
                    assert_eq!(session.close_realm(owned_realm).0, 1);
                    assert!(!session.parked_holes().contains(&owned_id.as_str()));
                    Ok(())
                })
                .await
                .expect("existing realm owner confirms native retirement");
        }
        assert!(
            authority_weak.upgrade().is_none(),
            "confirmed native retirement releases resources"
        );
    }

    #[test]
    fn slot_owner_tracks_resuspension_and_does_not_abort_a_completed_turn() {
        let aborted = Arc::new(Mutex::new(Vec::<String>::new()));
        let make_guard = || {
            let aborted = Arc::clone(&aborted);
            ParkedHoleAbortGuard {
                shared: Arc::new(ParkedHoleAbortState {
                    owner: None,
                    abort: Arc::new(move |id| aborted.lock().push(id)),
                    state: Mutex::new(ParkedHoleState::Owned(Default::default())),
                    reason: "test".into(),
                    retained_authority: None,
                }),
            }
        };

        let completed = make_guard();
        let completed_registration = completed.registration();
        completed_registration.observe(ResidentContinuationEvent::Parked("first".into()));
        completed_registration.observe(ResidentContinuationEvent::Retired("first".into()));
        drop(completed);
        assert!(aborted.lock().is_empty(), "completion must not abort twice");

        let suspended = make_guard();
        let suspended_registration = suspended.registration();
        suspended_registration.observe(ResidentContinuationEvent::Parked("old".into()));
        suspended_registration.observe(ResidentContinuationEvent::Retired("old".into()));
        suspended_registration.observe(ResidentContinuationEvent::Parked("latest".into()));
        drop(suspended);
        assert_eq!(&*aborted.lock(), &["latest"]);

        aborted.lock().clear();
        let nested = make_guard();
        let nested_registration = nested.registration();
        nested_registration.observe(ResidentContinuationEvent::Parked("parent".into()));
        nested_registration.observe(ResidentContinuationEvent::Parked("helper".into()));
        nested_registration.observe(ResidentContinuationEvent::Retired("helper".into()));
        drop(nested);
        assert_eq!(&*aborted.lock(), &["parent"]);

        aborted.lock().clear();
        let both = make_guard();
        let both_registration = both.registration();
        both_registration.observe(ResidentContinuationEvent::Parked("parent".into()));
        both_registration.observe(ResidentContinuationEvent::Parked("helper".into()));
        drop(both);
        assert_eq!(&*aborted.lock(), &["helper", "parent"]);
    }
    #[test]
    fn native_setup_admission_refuses_other_parser_shapes_before_reservation() {
        let (mut session, context, source, _root) = host_mount_fixture();
        let mut sibling = context.clone();
        sibling.actor = crate::ActorRef::first(crate::ActorId(2));
        sibling.placement.lexical_scope = session.mint_isolated_scope();
        let _foreign_input = mount_json_input(
            &mut session,
            &sibling,
            &source,
            &[],
            &serde_json::json!(42),
            None,
        )
        .expect("sibling owns a genuine compiled native value");
        let raw_view = actor_compile_view(&session, &context, &source, &[]).unwrap();
        let raw_injection = source.prepare(&raw_view).injected;
        let before_snapshot = raw_view.next_value_generation();
        let (scoped_source, snapshot) = snapshot_cell_split_owned(
            &mut session,
            &context,
            source.clone(),
            &[],
            None,
            None,
            None,
            CellSnapshotAdmission::ProtectedSetup,
        )
        .unwrap();
        let prepared = scoped_source.prepare(&snapshot.view);
        assert!(!raw_injection.is_empty());
        assert!(
            prepared.injected.is_empty(),
            "sibling values are not selected by setup"
        );
        assert_eq!(snapshot.view.next_value_generation(), before_snapshot);
        assert_eq!(
            session
                .compile_view_in(context.placement.lexical_scope)
                .unwrap()
                .next_value_generation(),
            before_snapshot,
            "protected snapshot must leave reservation to ordered admission"
        );
        let template = resident_cell_check_template(
            &prepared.preamble,
            &context.haskell_effects_alias,
            &prepared.imports,
        );
        let templates = resident_workbench_templates(
            &prepared.preamble,
            &context.haskell_effects_alias,
            &prepared.imports,
        );
        for text in [
            "",
            "pure ()",
            "let named = 1",
            "named <- pure ()",
            "data Owned = Owned",
            "_ <- pure ()\n_ <- pure ()",
        ] {
            let before = actor_compile_view(&session, &context, &source, &[])
                .unwrap()
                .next_value_generation();
            let specification =
                Arc::new(tidepool_toolchain::checked_cell::CheckedCellSpecification {
                    admission_digest: [0; 32],
                    cell_source: text.into(),
                    template_source: template.clone(),
                    turn_templates: templates
                        .iter()
                        .map(|template| {
                            let kind = match template.kind {
                                tidepool_runtime::session::TemplateSelector::Decl => "decl",
                                tidepool_runtime::session::TemplateSelector::Bind => "bind",
                                tidepool_runtime::session::TemplateSelector::BindDiscard => {
                                    "binddiscard"
                                }
                                tidepool_runtime::session::TemplateSelector::Expr => "expr",
                            };
                            (kind.to_owned(), template.source.clone())
                        })
                        .collect(),
                    injected_modules: prepared.injected.clone(),
                    reserved_declaration_modules: Vec::new(),
                });
            let plan = tidepool_toolchain::artifacts::parse_cell_plan(
                specification.clone(),
                &prepared.include,
            );
            if let Ok(plan) = plan {
                assert!(
                    session
                        .admit_native_setup_cell_in(
                            context.placement.lexical_scope,
                            plan,
                            specification.clone(),
                            specification.specification_digest(),
                            [9; 32],
                            prepared.include.clone()
                        )
                        .is_err(),
                    "setup accepted {text:?}"
                );
            } else {
                assert!(
                    text.is_empty(),
                    "nonempty negative parser fixture must produce a sealed plan: {text:?}"
                );
            }
            assert_eq!(
                actor_compile_view(&session, &context, &source, &[])
                    .unwrap()
                    .next_value_generation(),
                before,
                "refused setup reserved generation for {text:?}"
            );
        }
        let specification = Arc::new(tidepool_toolchain::checked_cell::CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "_ <- pure ()".into(),
            template_source: template,
            turn_templates: templates
                .iter()
                .map(|template| {
                    let kind = match template.kind {
                        tidepool_runtime::session::TemplateSelector::Decl => "decl",
                        tidepool_runtime::session::TemplateSelector::Bind => "bind",
                        tidepool_runtime::session::TemplateSelector::BindDiscard => "binddiscard",
                        tidepool_runtime::session::TemplateSelector::Expr => "expr",
                    };
                    (kind.to_owned(), template.source.clone())
                })
                .collect(),
            injected_modules: prepared.injected,
            reserved_declaration_modules: Vec::new(),
        });
        let raw = session
            .admit_cell_in(
                context.placement.lexical_scope,
                0,
                specification.clone(),
                specification.specification_digest(),
                [9; 32],
                prepared.include.clone(),
            )
            .unwrap();
        let raw_view = raw.view().clone();
        let include = prepared
            .include
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let request = CellCheckRequest {
            exact_context: raw_view.exact_declaration_context().cloned(),
            session_id: Some(raw_view.session()),
            cell_text: &specification.cell_source,
            template: &specification.template_source,
            include: &include,
            session_root: raw_view.session_root(),
            inject_modules: &specification.injected_modules,
            compile_generation: raw_view.next_value_generation().0,
            compile_view_evidence: "raw no-private refusal control",
        };
        assert!(
            tidepool_runtime::session::turn::compile_cell_program_admitted(
                request, raw, &templates
            )
            .is_err(),
            "bare public cell admission must not acquire native setup authority"
        );
        let mut leaked = specification.as_ref().clone();
        leaked.injected_modules = raw_injection;
        let leaked = Arc::new(leaked);
        let leaked_plan =
            tidepool_toolchain::artifacts::parse_cell_plan(leaked.clone(), &prepared.include)
                .unwrap();
        let before_refusal = session
            .compile_view_in(context.placement.lexical_scope)
            .unwrap()
            .next_value_generation();
        let refusal = match session.admit_native_setup_cell_in(
            context.placement.lexical_scope,
            leaked_plan,
            leaked.clone(),
            leaked.specification_digest(),
            [9; 32],
            prepared.include.clone(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("setup accepted foreign injection"),
        };
        match refusal {
            tidepool_runtime::session::SessionError::InvalidNativeSetupAdmission {
                scope,
                reason:
                    tidepool_runtime::session::NativeSetupAdmissionFailure::InjectionInventory {
                        planned,
                        reachable,
                    },
                ..
            } => {
                assert_eq!(scope, context.placement.lexical_scope);
                assert!(planned.count > 0);
                assert_eq!(reachable.count, 0);
                assert_ne!(planned.digest, reachable.digest);
            }
            other => panic!("expected exact injection refusal, got {other:?}"),
        }
        assert_eq!(
            session
                .compile_view_in(context.placement.lexical_scope)
                .unwrap()
                .next_value_generation(),
            before_refusal
        );
        let plan = tidepool_toolchain::artifacts::parse_cell_plan(
            specification.clone(),
            &prepared.include,
        )
        .unwrap();
        let admitted = session
            .admit_native_setup_cell_in(
                context.placement.lexical_scope,
                plan,
                specification.clone(),
                specification.specification_digest(),
                [9; 32],
                prepared.include,
            )
            .unwrap();
        assert!(admitted.private_execution().is_none());
        assert_eq!(admitted.plan_reservation().unwrap().items().len(), 1);
    }
}
