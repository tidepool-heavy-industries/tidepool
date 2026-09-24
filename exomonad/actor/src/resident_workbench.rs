//! Actor binding for the shared resident Haskell workbench.
//!
//! This adapter owns no machine and holds no checkout between calls. Each
//! fenced block checks out the actor's registered resident session, installs
//! the exact actor context, compiles and runs one segment on the blocking
//! pool, then restores the machine before the actor loop continues.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
    PendingPreparedInstall, PendingPreparedMode, PreparedRuntimeError, ResidentError, ResidentHole,
    ResidentOutcome, ResidentResumeError, ResidentSession, RootCustody, SourceImports,
    StagedDeclaration, TurnClassification, TurnCode, TurnKind, TurnRequest, TurnResult,
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
        ResidentActorWorkbenchError::CompileInfrastructure(
            tidepool_runtime::session::render_cell_compile_error(&failure.error, cell_source),
        )
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

/// Trusted source environment supplied by actor deployment. The canonical
/// `ActorEffects` alias itself lives in the imported Haskell facade; Rust does
/// not reflect or authorize its row entries.
#[derive(Clone)]
pub struct ActorWorkbenchSource {
    preamble: Arc<str>,
    base_include: Arc<[PathBuf]>,
    workbench_imports: SourceImports,
    tools: Option<Arc<str>>,
    /// The workspace's `[haskell] spec` key, when it names one. Rule two of
    /// spec discovery; rule one is a file in the actor's own checkout and
    /// belongs to no shared value.
    spec: Option<Arc<str>>,
    workspace_modules: Arc<[String]>,
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

    #[must_use]
    pub fn new(preamble: impl Into<Arc<str>>, base_include: Vec<PathBuf>) -> Self {
        Self {
            preamble: preamble.into(),
            base_include: base_include.into(),
            workbench_imports: SourceImports::from_specs([
                "qualified Data.Set as Set",
                "qualified Tidepool.Inspection as TidepoolInspection",
                "Tidepool.Inspection (print, cellDisplay)",
            ]),
            tools: None,
            spec: None,
            workspace_modules: Arc::from([]),
        }
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

    /// Select the deployment-frozen, qualified Haskell tool-record value.
    #[must_use]
    pub fn with_tools(mut self, entry: impl Into<Arc<str>>) -> Self {
        let entry = entry.into();
        if let Some((module, _)) = entry.rsplit_once('.') {
            self.workbench_imports
                .extend_text(&format!("qualified {module}"));
        }
        self.workbench_imports
            .extend_text("qualified Tidepool.Agent.Contract");
        self.tools = Some(entry);
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

/// What one `installSpec` publishes: the declared surface, and which slots the
/// same compile filled.
pub(crate) struct SpecInstallation {
    pub(crate) tools: Vec<exomonad_tool::ToolDeclaration>,
    pub(crate) slots: Vec<String>,
}

/// Read an installation out of the suspension's JSON.
///
/// A bare array is a tools-only installation, which is what every spec with no
/// slot filled publishes and what the shape was before slots existed; the
/// object form additionally names the slots.
pub(crate) fn decode_installation(
    installation: serde_json::Value,
) -> Result<SpecInstallation, ResidentActorWorkbenchError> {
    let declarations = |value| {
        serde_json::from_value::<Vec<exomonad_tool::ToolDeclaration>>(value).map_err(|error| {
            ResidentActorWorkbenchError::ActorProtocol(format!("tool declarations: {error}"))
        })
    };
    match installation {
        serde_json::Value::Object(mut fields) => {
            let tools = declarations(
                fields
                    .remove("tools")
                    .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
            )?;
            let slots = fields
                .remove("slots")
                .and_then(|slots| serde_json::from_value::<Vec<String>>(slots).ok())
                .unwrap_or_default();
            Ok(SpecInstallation { tools, slots })
        }
        array => Ok(SpecInstallation {
            tools: declarations(array)?,
            slots: Vec::new(),
        }),
    }
}

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

/// A carrier this workbench already built, and the source-layer revision it
/// was built against.
struct CachedHostCarrier {
    revision: Option<String>,
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
struct ResidentMachineAccess<H, O> {
    machines: Arc<ActorMachineRegistry<H, O>>,
    source: ActorWorkbenchSource,
    /// Builds a fresh, idle machine for a session id this host has decided
    /// deserves its own — installed once by the composition root; `None`
    /// keeps every launch on the shared session, unchanged, which is also
    /// today's behavior everywhere this hasn't been wired up yet. See
    /// [`crate::start::child_session_eligibility`] for the one caller that
    /// consults this.
    child_session_factory: Option<ChildSessionFactory<H, O>>,
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
            child_session_factory: None,
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
            child_session_factory: self.child_session_factory.clone(),
            logged_include_roots: std::sync::Mutex::new(std::collections::HashMap::new()),
            carriers: Arc::clone(&self.carriers),
        }
    }
}

/// Builds a fresh, idle [`ResidentSession`] for a session id the caller has
/// already minted — the composition root's one factory, installed once and
/// called by [`ResidentActorRunner::spawn_child_session`]. `Fn`, not
/// `FnOnce`: called once per eligible child for the life of the host.
pub type ChildSessionFactory<H, O> = Arc<
    dyn Fn(tidepool_repr::SessionId) -> Result<Box<ResidentSession<H, O>>, String> + Send + Sync,
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

/// Guards a [`ResidentHole`] a caller holds between the checkout that parked
/// it and a later checkout that will resume or abort it, so a task dropped
/// while awaiting that later checkout still settles the hole instead of
/// leaking a parked continuation. [`Self::disarm`] once the owning checkout
/// is actually in hand — from that point the hole's fate is decided
/// synchronously inside one held checkout, the same as every other
/// `resume_*`/abort call, and this guard is no longer needed. If instead
/// dropped still armed, [`Drop`] spawns one more checkout in the background
/// to abort the hole there, since `Drop` cannot itself run async code. Same
/// shape as [`HostInputRetirement`]; not generic over `H`/`O`, for the same
/// reason.
struct ParkedHoleAbortGuard {
    background_abort: Option<Box<dyn FnOnce() + Send>>,
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
        let machines = Arc::clone(&access.machines);
        let source = access.source.clone();
        let background_abort: Box<dyn FnOnce() + Send> = Box::new(move || {
            tokio::spawn(async move {
                let access = ResidentMachineAccess::new(machines, source);
                if let Err(error) = access
                    .with_machine(context, move |session, _, _| {
                        if let Err(abort_error) = session.abort(&cont_id, reason) {
                            tracing::warn!(
                                %cont_id,
                                %abort_error,
                                "failed to abort a hole abandoned before its resuming checkout"
                            );
                        }
                        Ok(())
                    })
                    .await
                {
                    tracing::warn!(
                        %error,
                        "failed to check out the machine to abort a hole abandoned before its resuming checkout"
                    );
                }
            });
        });
        Self {
            background_abort: Some(background_abort),
        }
    }

    /// The owning checkout is in hand: the hole's fate is now decided
    /// synchronously within it, so no background cleanup is needed.
    fn disarm(mut self) {
        self.background_abort = None;
    }
}

impl Drop for ParkedHoleAbortGuard {
    fn drop(&mut self) {
        if let Some(background) = self.background_abort.take() {
            background();
        }
    }
}

/// Concrete resident workbench for one typed agent-session obligation.
pub struct ResidentActorWorkbench<H, O> {
    access: ResidentMachineAccess<H, O>,
    response: Option<ResponseExpectation>,
    request: Option<crate::RequestId>,
    type_modules: Arc<[String]>,
    json_input: Option<serde_json::Value>,
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
    /// against a single command-job-typed binder in `display` is the same
    /// name-to-job fact the automatic-binding path already knows — see
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

enum WorkbenchDisplay {
    Binding(Vec<BoundBinder>),
    Opaque,
    Tool,
    Observation {
        name: String,
        budget: usize,
        presentation: ExpressionPresentation,
        source: ActorWorkbenchSource,
        type_modules: Vec<String>,
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
    CommandBackgrounded {
        job: String,
        binding: String,
        reason: CommandObservationStop,
    },
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
    Console {
        continuation: ResidentHole,
        text: String,
    },
    Sleep {
        continuation: ResidentHole,
        duration: Duration,
    },
    Jev {
        continuation: ResidentHole,
        request: String,
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
    WatchForget(WatchForget),
}

pub(crate) enum ForkGroupBoundary {
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
    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::Completed => "program completion",
            Self::Sleep { .. } => "sleep",
            Self::Jev { .. } => "jev",
            Self::Command { .. } => "command job",
            Self::Console { .. } => "print",
            Self::NotificationSend { .. } => "notify",
            Self::NotificationPoll { .. } => "pollNotification",
            Self::ActorContext(_) => "actorContext",
            Self::ActorLocalContext(_) => "actor local context",
            Self::ForkGroup(ForkGroupBoundary::Preview { .. }) => "preview context-fork policy",
            Self::ForkGroup(ForkGroupBoundary::Begin { .. }) => "begin context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Commit { .. }) => "commit context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Abort { .. }) => "abort context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Cleanup { .. }) => "cleanup context-fork group",
            Self::Start(_) => "startActor",
            Self::Replace { .. } => "replaceActor",
            Self::Outbound(ResidentOutbound::Call { .. }) => "call",
            Self::Outbound(ResidentOutbound::TryCall { .. }) => "tryCall",
            Self::Outbound(ResidentOutbound::Cast { .. }) => "cast",
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
            Self::RequestSubmission(_) => "request",
            Self::ReplyAttempt(_) => "reply",
            Self::ResponsePoll(_) => "pollResponse",
            Self::ProgressPublication { .. } => "reportProgress",
            Self::ProgressPoll(_) => "pollProgress",
            Self::RequestUpdate { .. } => "updateRequest",
            Self::RequestUpdatePoll { .. } => "pollRequestUpdate",
            Self::WatchProgressPoll { .. } => "pollWatch progress",
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
enum ResidentRequest {
    Console(crate::generated::console::ConsoleReq),
    Sleep(crate::generated::sleep::SleepReq),
    Commands(crate::generated::commands::CommandsReq),
    Notifications(crate::generated::notifications::NotificationsReq),
    Jev(crate::generated::jev::JevReq),
    Actor(crate::generated::actor::ActorReq),
    ActorContext(crate::generated::actor_context::ActorContextReq),
    AgentControl(crate::generated::agent_control::AgentControlReq),
    AgentInspection(crate::generated::agent_inspection::AgentInspectionReq),
    Lookup(crate::generated::lookup::LookupReq),
    Introspection(crate::generated::introspection::IntrospectionReq),
    AgentLaunch(crate::generated::agent_launch::AgentLaunchReq),
    Forks(crate::generated::forks::ForksReq),
    ActorKernel(crate::generated::actor_kernel::ActorKernelReq),
    ActorLocal(crate::generated::actor_local::ActorLocalReq),
    AgentTools(crate::generated::agent_tools::AgentToolsReq),
    AgentSession(crate::generated::agent_session::AgentSessionReq),
    Reflect(crate::generated::reflect::ReflectReq),
    Replies(RepliesReq),
    Watches(WatchesReq),
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
            Self::Forks(crate::generated::forks::ForksReq::ForksPreviewWith(..)) => {
                "preview context-fork policy"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksCommitWith(..)) => {
                "commit context-fork group"
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
            Self::Replies(RepliesReq::SubmitRequestWith(..)) => "request submission",
            Self::Replies(RepliesReq::AttemptReplyWith(..)) => "attemptReply",
            Self::Replies(RepliesReq::PublishProgressWith(..)) => "reportProgress",
            Self::Replies(RepliesReq::ObserveProgressWith(..)) => "pollProgress",
            Self::Replies(RepliesReq::UpdateRequestWith(..)) => "updateRequest",
            Self::Replies(RepliesReq::ObserveRequestUpdateWith(..)) => "pollRequestUpdate",
            Self::Replies(RepliesReq::ReplyWith(..)) => "reply",
            Self::Replies(RepliesReq::ObserveResponseWith(..)) => "pollResponse",
            Self::Replies(RepliesReq::CancelRequestWith(..)) => "cancelRequest",
            Self::Replies(RepliesReq::AbandonResponseWith(..)) => "abandonResponse",
            Self::Replies(RepliesReq::ForgetResponseWith(..)) => "forgetResponse",
            Self::Replies(RepliesReq::ObserveReplyWith(..)) => "pollReply",
            Self::Replies(RepliesReq::AttemptAcknowledgeCancellationWith(..)) => {
                "attemptAcknowledgeCancellation"
            }
            Self::Replies(RepliesReq::AcknowledgeCancellationWith(..)) => "acknowledgeCancellation",
            Self::Watches(WatchesReq::RegisterWatchWith(..)) => "watch",
            Self::Watches(WatchesReq::RegisterWatchGroupsWith(..)) => "watch",
            Self::Watches(WatchesReq::RegisterRouteWith(..)) => "route",
            Self::Watches(WatchesReq::RegisterRouteGroupsWith(..)) => "route",
            Self::Watches(WatchesReq::ObserveRouteWith(..)) => "pollRoute",
            Self::Watches(WatchesReq::ListRoutesWith) => "listRoutes",
            Self::Watches(WatchesReq::ObserveWatchWith(..)) => "pollWatch",
            Self::Watches(WatchesReq::ObserveWatchProgressWith(..)) => "pollWatch progress",
            Self::Watches(WatchesReq::ForgetWatchWith(..)) => "forgetWatch",
        }
    }
}

impl<H, O> ResidentActorRunner<H, O> {
    #[must_use]
    pub fn new(machines: Arc<ActorMachineRegistry<H, O>>, source: ActorWorkbenchSource) -> Self {
        Self {
            access: ResidentMachineAccess::new(machines, source),
        }
    }

    /// Install the composition root's child-session factory. Omitted, every
    /// launch keeps running on the session that admitted it — today's
    /// behavior, unchanged.
    #[must_use]
    pub fn with_child_session_factory(mut self, factory: ChildSessionFactory<H, O>) -> Self {
        self.access.child_session_factory = Some(factory);
        self
    }

    /// Build a fresh machine for `session_id` and register it idle in the
    /// shared registry, using the installed [`ChildSessionFactory`]. Returns
    /// an error naming why when no factory is installed, the factory itself
    /// fails, or `session_id` somehow already names a live entry (it is
    /// minted fresh by the caller — `tidepool_repr::SessionId` collision is
    /// not expected, but silently overwriting a live session is never safe).
    #[allow(dead_code, reason = "wired by the child-session launch parcel")]
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
        let machine = factory(session_id)?;
        if self
            .access
            .machines
            .insert_idle(session_id, machine)
            .is_some()
        {
            return Err(format!(
                "session {session_id} already had a live entry; refusing to overwrite it"
            ));
        }
        Ok(())
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
        }
    }

    #[must_use]
    pub(crate) fn with_json_input(mut self, input: Option<serde_json::Value>) -> Self {
        self.json_input = input;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommandObservationStop {
    #[error("the 30-second observation expired; the retained job may since have finished")]
    Deadline,
    #[error("command completed, but output observation failed: {0:?}")]
    OutputUnavailable(tidepool_bridge_effects::CommandError),
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorWorkbenchError {
    #[error("command {job} retained: {reason}")]
    CommandObservationStopped {
        job: String,
        reason: CommandObservationStop,
    },
    #[error(transparent)]
    CompileView(#[from] ActorCompileViewError),
    #[error("resident machine checkout failed: {0}")]
    Checkout(CheckoutError<String>),
    #[error("resident workbench compiler failed: {0}")]
    Compile(CompileError),
    #[error("resident cell check failed: {0}")]
    CellCheck(tidepool_runtime::session::CellCheckFailure),
    #[error("resident workbench compiler infrastructure failed:\n{0}")]
    CompileInfrastructure(String),
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
    /// An earlier cell hit an integrity failure; this actor's machine refuses
    /// all further execution.
    #[error(
        "this actor's Haskell machine was lost to an earlier integrity failure (reported by that \
         cell); no further cells can run here. Restart the session: declarations are replayed, \
         live values are lost"
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
    #[error("invalid agent tool declaration set: {0}")]
    ToolDeclarations(serde_json::Error),
}

impl ResidentActorWorkbenchError {
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
        max_wait: Option<Duration>,
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
        self.with_host_machine(
            context.actor.to_string(),
            context.placement.session,
            max_wait,
            move |session, source| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                operation(session, &context, source)
            },
        )
        .instrument(compile_span)
        .await
    }

    async fn with_host_machine<T: Send + 'static>(
        &self,
        actor: impl Into<String>,
        session_id: tidepool_repr::SessionId,
        max_wait: Option<Duration>,
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
        let checkout = match max_wait {
            Some(limit) => {
                self.machines
                    .checkout_wait(session_id, request, limit)
                    .await
            }
            None => self.machines.checkout_queued(session_id, request).await,
        }
        .map_err(ResidentActorWorkbenchError::Checkout)?;
        // Every cell holds the run's resident machine exclusively, so the
        // wait for it is a primary per-cell cost. `info`, not `debug`: this
        // needs to reach the run's INFO log, not just the detailed trace.
        tracing::info!(actor = %actor, session = ?session_id,
            waited_ms = admission_started.elapsed().as_millis(),
            "resident machine checkout admitted");
        crate::call_timing::add_checkout_wait_ms(admission_started.elapsed().as_millis());
        let held_since = std::time::Instant::now();
        let (mut session, receipt) = checkout.into_parts();
        let source = self.source.clone();
        let machines = Arc::clone(&self.machines);

        let task = spawn_blocking_in_span(move || {
            // The blocking task owns the machine and its linear checkout
            // receipt together. Its async caller may be cooperatively
            // cancelled while this closure is running; settlement must not
            // depend on that caller continuing to poll the JoinHandle.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                        tracing::info!(actor = %actor, session = ?session_id,
                            held_ms = held_since.elapsed().as_millis(),
                            "resident machine checkout released (retired)");
                        return outcome;
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
    /// Resolve this actor's spec without compiling anything, for status and
    /// for a reload receipt that must name the rule before it knows whether
    /// the spec compiles.
    pub(crate) fn resolve_spec(
        &self,
        context: &crate::ActorSessionContext,
    ) -> crate::agent_spec::ResolvedSpec {
        // The roots this actor's cells resolve a module in, in GHC's order:
        // its own checkout's layer, then what the run provides. An actor with
        // no checkout of its own, the root among them, finds the run's spec.
        let mut roots = context.source_layer.to_vec();
        roots.extend(self.access.source.base_include.iter().cloned());
        crate::agent_spec::resolve(
            &roots,
            self.access.source.spec.as_deref(),
            self.access.source.tools.as_deref(),
        )
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
    pub(crate) async fn prepare_tools(
        &self,
        context: crate::ActorSessionContext,
        install: u64,
    ) -> Result<Option<ResidentWorkbenchTools>, ResidentActorWorkbenchError> {
        let resolved = self.resolve_spec(&context);
        // The revision of the root the spec was read from; a configured entry
        // names no file, so it answers for the actor's own layer.
        let revision = match resolved.file.as_deref().and_then(std::path::Path::parent) {
            Some(root) => crate::agent_spec::layer_revision(&[root.to_path_buf()]),
            None => crate::agent_spec::layer_revision(&context.source_layer),
        };
        let Some(entry) = resolved.entry.clone() else {
            return Ok(None);
        };
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
        source.preamble = format!(
            "{}\ntype HostedToolEffects = Tidepool.Effects.Core.AgentTools ': {}\n",
            source.preamble, context.haskell_effects_alias
        )
        .into();
        // Rules one and two name a spec value; rule three names the tools
        // record `[haskell] tools` names today. `installTools` is a spec whose
        // only field is set, so both reach one installer and one retained
        // dispatcher shape.
        let installer = match resolved.rule {
            crate::agent_spec::SpecRule::CheckoutModule
            | crate::agent_spec::SpecRule::WorkspaceSpec => "installSpec",
            crate::agent_spec::SpecRule::WorkspaceTools
            | crate::agent_spec::SpecRule::BuiltinDefault => "installTools",
        };
        let authored_effects = context.haskell_effects_alias.clone();
        let publication_resolved = resolved.clone();
        let mut compile_context = context.clone();
        compile_context.haskell_effects_alias = "HostedToolEffects".into();
        let block = ParsedBlock {
            ordinal: 1,
            total: 1,
            source: format!(
                "_ <- Tidepool.Agent.Contract.{installer} @({authored_effects}) {entry}"
            ),
        };
        let verdict = TurnClassification {
            kind: TurnKind::Bind,
            binders: Vec::new(),
            items: Vec::new(),
        };
        // Compile with the resident machine checked out only for the
        // snapshot and the install-and-run step (`begin_fragment_split`),
        // released for the GHC compile in between. The suspension this
        // fragment produces is plain session-held data, so reading it back
        // out below is an ordinary later checkout, the same shape every
        // `resume_*` method already uses against a held hole.
        let step = self
            .begin_fragment_split(
                compile_context.clone(),
                source,
                Vec::new(),
                block,
                Some(verdict),
            )
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
        // background to abort the hole there. Disarmed the moment the
        // checkout below is actually in hand, since everything past that
        // point settles the hole synchronously inside it.
        let abort_guard = ParkedHoleAbortGuard::new(
            &self.access,
            compile_context.clone(),
            hole.cont_id().to_string(),
            "tool installer's parked hole was abandoned before its resuming checkout".into(),
        );
        self.access
            .with_machine(compile_context, move |session, context, _| {
                abort_guard.disarm();
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
                    let installation =
                        tidepool_runtime::value_to_json(&declarations, session.data_con_table(), 0);
                    let SpecInstallation { tools, slots } = decode_installation(installation)?;
                    let declarations =
                        crate::resident_interactive::project_tools(tools).map_err(|error| {
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
                        if let Err(abort_error) =
                            session.abort(hole.cont_id(), "tool publication rejected".into())
                        {
                            tracing::warn!(
                                hole = hole.cont_id(),
                                %abort_error,
                                "failed to abort parked hole after tool publication rejection"
                            );
                        }
                        return Err(error);
                    }
                };
                let settled = session
                    .resume_classified(hole, ())
                    .map_err(classify_resumption)?;
                if !matches!(
                    settled,
                    ResidentOutcome::Completed { .. } | ResidentOutcome::BindingsCommitted { .. }
                ) {
                    if let ResidentOutcome::Suspended { hole, .. } = settled {
                        if let Err(abort_error) = session.abort(
                            hole.cont_id(),
                            "tool installer must finish after publication".into(),
                        ) {
                            tracing::warn!(
                                hole = hole.cont_id(),
                                %abort_error,
                                "failed to abort parked hole after tool installer overrun"
                            );
                        }
                    }
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "tool installer did not finish after publication".into(),
                    ));
                }
                Ok(Some(ResidentWorkbenchTools {
                    declarations,
                    dispatch: Arc::new(dispatch),
                    slots,
                    resolved: publication_resolved,
                    install,
                    revision,
                }))
            })
            .await
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
                    WorkbenchDisplay::Tool,
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

    /// The continuations parked in this actor's machine right now, oldest
    /// first. Taken before a slot runs, so what it leaves behind can be told
    /// from what was already there.
    pub(crate) async fn parked_continuations(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<Vec<String>, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                Ok(session
                    .parked_holes()
                    .into_iter()
                    .map(str::to_owned)
                    .collect())
            })
            .await
    }

    /// Abort every continuation parked since `before` was taken.
    ///
    /// A slot whose wait ran out is no longer being driven, so an effect it
    /// was suspended on will never be answered. Left parked, that turn holds
    /// the machine against the next caller. One handler runs at a time per
    /// actor, so anything parked since the snapshot is the slot's own. An
    /// aborted turn may suspend again while unwinding, hence the bounded loop.
    pub(crate) async fn abort_parked_since(
        &self,
        context: crate::ActorSessionContext,
        before: Vec<String>,
        reason: String,
    ) -> Result<usize, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let mut aborted = 0usize;
                for _ in 0..8 {
                    let Some(abandoned) = session
                        .parked_holes()
                        .into_iter()
                        .find(|hole| !before.iter().any(|known| known == hole))
                        .map(str::to_owned)
                    else {
                        break;
                    };
                    match session.abort(&abandoned, reason.clone()) {
                        Ok(_) => aborted += 1,
                        Err(error) => {
                            // The hole stays parked; don't count it as
                            // aborted or the caller believes cleanup happened
                            // when it did not.
                            tracing::warn!(
                                hole = %abandoned,
                                %error,
                                "failed to abort a stray parked continuation"
                            );
                        }
                    }
                }
                Ok(aborted)
            })
            .await
    }

    /// Bind one tool result under the handle the slot was shown, so a pruned
    /// view keeps the whole of what it selected from addressable.
    ///
    /// This is [`Self::bind_command_job`]'s mechanism, for the same reason: a
    /// value the model may want back belongs in the lexical environment it
    /// already computes in, not in a second handle registry beside it. The
    /// handle is chosen before the slot runs, and defined only when the slot
    /// actually prunes.
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
        input_type: String,
        input: RootCustody,
        reply_type: String,
        reply_declaration: Option<String>,
        reply_declaration_modules: Vec<String>,
    ) -> Result<(String, String), ResidentActorWorkbenchError> {
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
        let preview = self
            .access
            .with_machine(context, move |session, context, _| {
                use tidepool_runtime::session::turn::{
                    assemble_activation_module, run_activation_turn,
                };
                use tidepool_runtime::session::{TemplateSelector, TurnTemplate};
                let view = actor_compile_view(session, context, &source, &type_modules)?;
                let generation = view.next_value_generation();
                let prepared = source.prepare(&view);
                let preamble = insert_preamble_imports(&prepared.preamble, &prepared.imports);
                let templates = [TurnTemplate {
                    kind: TemplateSelector::Bind,
                    source: assemble_activation_module(
                        &preamble,
                        &context.haskell_effects_alias,
                        &input_type,
                        ACTIVATION_INPUT_LIMIT,
                    ),
                }];
                let include: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
                let retained = session.prepared_retained();
                let result = run_activation_turn(TurnRequest {
                session_id: Some(view.session_id()),
                    turn_text: "sessionInput <- pure undefined",
                    templates: &templates,
                    include: &include,
                    session_root: view.session_root(),
                    inject_modules: &prepared.injected,
                    gen: generation.0,
                    verdict: Some(generated_bind_verdict("sessionInput")),
                    target: None,
                    retained_imports: &retained,
                })
                .map_err(|failure| {
                    ResidentActorWorkbenchError::InputMount(failure.error.to_string())
                })?;
                let TurnResult::Bind {
                    bound, compiled, ..
                } = result
                else {
                    return Err(ResidentActorWorkbenchError::InputMount(
                        "activation did not produce a bind".into(),
                    ));
                };
                let [binder] = bound.as_slice() else {
                    return Err(ResidentActorWorkbenchError::InputMount(
                        "activation did not produce one input binder".into(),
                    ));
                };
                session
                    .mount_compiled_binding_in(
                        context.placement.lexical_scope,
                        binder,
                        generation,
                        &compiled.table,
                        input,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let preview = match session.run_mounted_inspection_with_sites(
                    compiled.into_code(),
                    tidepool_repr::SessionVarId::from_extract(binder.var_id),
                ) {
                    Ok(ResidentOutcome::Suspended { hole, .. }) => {
                        if let Err(abort_error) = session
                            .abort(hole.cont_id(), "pure activation preview suspended".into())
                        {
                            tracing::warn!(
                                hole = hole.cont_id(),
                                %abort_error,
                                "failed to abort parked hole after pure activation preview suspended"
                            );
                        }
                        Err(ResidentActorWorkbenchError::Inspection(
                            "pure activation preview suspended".into(),
                        ))
                    }
                    Ok(outcome) => decode_activation_observation(outcome),
                    Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
                };
                Ok(preview)
            })
            .await?;
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
        ))
    }

    /// Prepare one resident cell, splitting `check_cell` (GHC) and every
    /// per-item compile off the machine checkout — mirroring
    /// [`Self::begin_fragment_split`]/[`Self::bind_command_job`] (Problem 1
    /// of the compile-path design note): snapshot under a short checkout
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
    /// [`crate::ActorCompileView::compile_relevant_eq`] and an unchanged
    /// `next_declaration_module()`, up to `MAX_SPLIT_ATTEMPTS` times. Only
    /// once those bounded retries are exhausted does this fall back to
    /// [`Self::prepare_cell_single_checkout`], whose one exclusive checkout
    /// stays held across its own declaration staging and every item compile.
    pub(crate) async fn prepare_cell(
        &self,
        context: crate::ActorSessionContext,
        cell_source: String,
    ) -> Result<(CellCheck, PreparedCell), ResidentActorWorkbenchError> {
        const MAX_SPLIT_ATTEMPTS: u32 = 3;
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

        // Carries one cancellation edge across every off-checkout GHC call
        // this split makes, the same shape `prepare_cell_single_checkout`
        // arms for its own (single-checkout) compile: dropping this future
        // before it settles interrupts whichever compile is in flight,
        // rather than leaving it to finish unobserved.
        let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));

        for _ in 0..MAX_SPLIT_ATTEMPTS {
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
                CellReservation::Stale => continue,
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
                // The whole-cell check already produced this item's compiled
                // artifact (`check_cell_off_checkout`'s fold): no further
                // GHC round trip for this attempt. Still validate its
                // module against THIS reservation's generation — the fold
                // compiled against the pre-check snapshot's generation,
                // which only still matches when nothing else wrote to this
                // scope between the check and `reserve_cell_generations`
                // above; a mismatch here means the two disagree despite
                // `compile_relevant_eq` passing (should not happen, but is
                // cheap to guard) and this attempt falls back to an
                // ordinary compile rather than installing a wrong binder.
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
                                        ResidentActorWorkbenchError::CompileInfrastructure(format!(
                                            "declaration candidate directory: {error}"
                                        ))
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
                    // in the meantime, the rejection is stale and this
                    // attempt must recompile against a fresh snapshot,
                    // exactly as `begin_fragment_split`'s off-checkout
                    // rejection and this same split's install-time mismatch
                    // already do. A rejected declaration item's own candidate
                    // was validated against a private directory dropped when
                    // this compile finished, so there is nothing further to
                    // discard here.
                    let revalidate_source = source.clone();
                    let revalidate_type_modules = Arc::clone(&type_modules);
                    let revalidate_against = c_view.clone();
                    let revalidate_candidate_module = snapshot.candidate_module;
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
                    if matches!(revalidation, CellRejectionRevalidation::StillCurrent) {
                        if let Some(guard) = leased_input.take() {
                            guard.retire(&self.access).await;
                        }
                        cancel_on_drop.0 = None;
                        return Ok((checked, PreparedCell::Rejected { index, diagnostic }));
                    }
                    continue;
                }
                CellItemsOutcome::Ready(items) => items,
            };

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
                CellInstall::Stale => continue,
            }
        }

        // Contention exhausted the bounded split-compile retries — fall
        // back to the original single-checkout path, whose one exclusive
        // borrow cannot itself observe a stale view.
        if let Some(guard) = leased_input.take() {
            guard.retire(&self.access).await;
        }
        cancel_on_drop.0 = None;
        self.prepare_cell_single_checkout(context, cell_source)
            .await
    }

    /// The original, unsplit whole-cell preparation: one exclusive machine
    /// checkout for the whole-cell check and every item's compile. Cells
    /// with a `Decl` item always run this path — declaration staging writes
    /// and validates the next Lib module in the shared session root, which
    /// must stay serialized. [`Self::prepare_cell`]'s bounded split-compile
    /// retries also fall back here when they run out.
    async fn prepare_cell_single_checkout(
        &self,
        context: crate::ActorSessionContext,
        cell_source: String,
    ) -> Result<(CellCheck, PreparedCell), ResidentActorWorkbenchError> {
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
                        let prepared = source.prepare(&compile_view);
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
        self.access
            .with_machine(context, move |session, context, _| {
                let view = actor_compile_view(session, context, &source, &type_modules)?;
                let provenance = structured_provenance(
                    &view,
                    &source,
                    tidepool_runtime::session::NameScope::Current,
                );
                let prepared = source.prepare(&view);
                let include = prepared
                    .include
                    .iter()
                    .map(PathBuf::as_path)
                    .collect::<Vec<_>>();
                let mut answer = crate::lookup::execute(
                    request,
                    provenance.fingerprint,
                    &prepared.imports,
                    &prepared.injected,
                    &source.workspace_modules,
                    usage,
                    |queries| {
                        if queries.is_empty() {
                            return Ok(vec![]);
                        }
                        run_inspections(InspectionRequest {
                            preamble: &prepared.preamble,
                            imports: &prepared.imports,
                            include: &include,
                            session_root: view.session_root(),
                            inject_modules: &prepared.injected,
                            queries,
                            effects: Some(&context.haskell_effects_alias),
                        })
                        .map_err(|error| error.to_string())
                    },
                );
                if !request_pending {
                    crate::lookup::note_request_only_bindings(&mut answer);
                }
                session
                    .resume_classified(hole, answer)
                    .map_err(classify_resumption)
            })
            .await
    }

    pub(crate) async fn begin_prepared_cell_item(
        &self,
        context: crate::ActorSessionContext,
        block: ParsedBlock,
        prepared: PreparedCellItem,
        display_budget: usize,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        let ready = match prepared.ready {
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
        begin_ready_block_split(
            &self.access,
            context,
            turn_source,
            type_modules.to_vec(),
            block,
            *ready,
            display_budget,
        )
        .await
    }

    /// Install a trusted job reference after stopping a foreground computation.
    pub(crate) async fn bind_background_job(
        &self,
        context: crate::ActorSessionContext,
        job: String,
        reason: CommandObservationStop,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        let binding = self.bind_command_job(context, job.clone()).await?;
        Ok(ResidentWorkbenchStep::CommandBackgrounded {
            job,
            binding,
            reason,
        })
    }

    /// The carrier this workbench built for `kind`, from the cache if one is
    /// already there and still current for the actor's own source-layer
    /// revision, otherwise built once, off checkout, and cached for every
    /// later call of any kind for the life of this workbench's lineage (see
    /// [`ResidentMachineAccess::sharing`]).
    ///
    /// Building takes one short checkout — to reserve a generation for the
    /// throwaway compile below, exactly [`bind_command_job`]'s old snapshot
    /// step — releases it, then compiles off checkout
    /// ([`compile_host_binding_off_checkout`]) with no re-checkout: the
    /// compiled `(BoundBinder, CompiledTurn)` is generation-independent (see
    /// [`HostCarrier::from_compiled`]), so there is nothing left to install
    /// or revalidate against a later view.
    async fn carrier_for(
        &self,
        context: &crate::ActorSessionContext,
        kind: HostCarrierKind,
    ) -> Result<Arc<HostCarrier>, ResidentActorWorkbenchError> {
        let revision = crate::agent_spec::layer_revision(&context.source_layer);
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

    /// Bind a Job carrier for `job`. The first call for this workbench's
    /// lineage builds the Job carrier ([`Self::carrier_for`], one short
    /// checkout plus an off-checkout compile); every call after that —
    /// including this one, once the carrier is cached — mounts through it
    /// ([`tidepool_runtime::session::ResidentSession::mount_carrier_in`])
    /// under a single short checkout: already-bound check, fresh name and
    /// reserved generation, mount, then the tag/retire-on-error sequence
    /// [`install_command_job_binder`] also uses for its own single-checkout
    /// compile-and-install.
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
                            "command {job} remains owned, but its automatic binding failed: \
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
    /// same split [`Self::bind_command_job`] applies to the Job carrier
    /// compile (Problem 1 of the compile-path design note), reusing
    /// [`crate::ActorCompileView::compile_relevant_eq`] to detect a stale
    /// snapshot and recompile against a fresh one, up to `MAX_SPLIT_ATTEMPTS`
    /// times; beyond that this falls back to the original single-checkout
    /// [`begin_fragment`].
    pub(crate) async fn begin_fragment_split(
        &self,
        context: crate::ActorSessionContext,
        source: ActorWorkbenchSource,
        type_modules: Vec<String>,
        block: ParsedBlock,
        verdict: Option<TurnClassification>,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        const MAX_SPLIT_ATTEMPTS: u32 = 3;
        // Carries one cancellation edge across every off-checkout GHC call
        // this split makes, the same shape `prepare_cell_single_checkout`
        // arms for its own (single-checkout) compile: dropping this future
        // before it settles interrupts whichever compile is in flight,
        // rather than leaving it to finish unobserved.
        let cancellation = tidepool_runtime::CompilerTransactionCancellation::new();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(cancellation.clone()));
        for _ in 0..MAX_SPLIT_ATTEMPTS {
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
                    // this attempt must recompile against a fresh snapshot,
                    // exactly as an install-time mismatch already does.
                    let revalidate_source = source.clone();
                    let revalidate_type_modules = type_modules.clone();
                    let revalidate_against = snapshot.view.clone();
                    let still_current = self
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
                            Ok(fresh_view.compile_relevant_eq(&revalidate_against))
                        })
                        .await?;
                    if still_current {
                        cancel_on_drop.0 = None;
                        return Ok(ResidentWorkbenchStep::Rejected(diagnostic));
                    }
                    continue;
                }
                CompiledBlock::Ready(ready) => *ready,
            };

            let install_source = source.clone();
            let install_type_modules = type_modules.clone();
            let install_block = block.clone();
            let outcome = self
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
                    if !fresh_view.compile_relevant_eq(&snapshot.view) {
                        return Ok(FragmentInstall::Stale);
                    }
                    let step = begin_ready_block(
                        session,
                        context,
                        &install_source,
                        RequestWorkbenchScope {
                            response: None,
                            request: None,
                            type_modules: &install_type_modules,
                        },
                        install_block,
                        ready,
                        8192,
                    )?;
                    Ok(FragmentInstall::Installed(step))
                })
                .await?;
            match outcome {
                FragmentInstall::Installed(step) => {
                    cancel_on_drop.0 = None;
                    return Ok(step);
                }
                FragmentInstall::Stale => continue,
            }
        }

        // Contention exhausted the bounded split-compile retries — fall back
        // to the original single-checkout path, whose one exclusive borrow
        // cannot itself observe a stale view.
        cancel_on_drop.0 = None;
        self.access
            .with_machine(context, move |session, context, _| {
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
            .await
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
                let prepared = compiler_source.prepare(&compile_view);
                let include = prepared
                    .include
                    .iter()
                    .map(PathBuf::as_path)
                    .collect::<Vec<_>>();
                run_inspections(InspectionRequest {
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
    pub(crate) async fn settle_item(
        &self,
        context: crate::ActorSessionContext,
        fragment: ResidentWorkbenchFragment,
        outcome: ResidentOutcome,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                settle_fragment(session, context, fragment, outcome)
            })
            .await
    }
}

/// A binding name for a fresh Job carrier, not already used by a workbench
/// binding in `scope`. Shared by `bind_command_job`'s split and
/// single-checkout fallback paths so both name bindings the same way.
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

/// The outcome of `begin_fragment_split`'s re-checkout install step.
enum FragmentInstall {
    Installed(ResidentWorkbenchStep),
    /// The re-derived view no longer matches the one this compile ran
    /// against; the caller must recompile against a fresh snapshot.
    Stale,
}

/// A short-checkout snapshot for `begin_fragment_split`'s split compile:
/// the exact source-side view this compile targets, its reserved value
/// generation, the prepared programs still linked against every live
/// prepared binding, the turn classification a bare-expression item resolves
/// to (GHC-sourced when not already known), and — for an expression item —
/// a fresh observation name unique among this scope's visible bindings at
/// snapshot time. Mirrors `bind_command_job`'s `BindJobPrepare`.
struct FragmentCompileSnapshot {
    view: crate::ActorCompileView,
    generation: tidepool_repr::Generation,
    retained: Vec<(SymbolIdentity, u64)>,
    verdict: Option<TurnClassification>,
    observation_name: Option<String>,
}

/// Take the checkout-scoped snapshot a split fragment compile needs, then
/// release the checkout. Read-only against the session except for the
/// atomic generation reservation — mirrors `bind_command_job`'s snapshot
/// step (Problem 1 of the compile-path design note).
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
            .map_err(|error| ResidentActorWorkbenchError::CompileInfrastructure(error.to_string()))?
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
/// here; `begin_fragment_split`'s retry loop runs this with the machine
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
    let prepared = source.prepare(&snapshot.view);
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
            classify_compile(&failure.error).message,
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
/// so a bounded split-install retry can take a fresh snapshot from the same
/// compiled turn instead of needing a second GHC compile.
fn cloned_turn_code(turn: &CompiledTurn) -> TurnCode<'static> {
    TurnCode {
        table: std::borrow::Cow::Owned(turn.table.clone()),
        sites: std::borrow::Cow::Owned(turn.asks.clone()),
        prepared: std::borrow::Cow::Owned(turn.prepared.clone()),
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
/// changed between snapshot and revalidation (`Ok(None)`) retries from a
/// fresh snapshot, bounded by `MAX_SPLIT_ATTEMPTS`; a session with no
/// machine yet, or split attempts exhausted, falls back to the original
/// single-checkout install-and-run, unchanged from [`begin_ready_block`].
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
    const MAX_SPLIT_ATTEMPTS: u32 = 3;
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

    for _ in 0..MAX_SPLIT_ATTEMPTS {
        let code = cloned_turn_code(&compiled_turn);
        let mode = pending_mode_for(&bound, generation, &observation);
        let attempt = access
            .with_machine(context.clone(), move |session, _, _| {
                if !session.prepared_machine_ready() {
                    return Ok(PreparedSnapshotAttempt::Bootstrap);
                }
                session
                    .snapshot_run_prepared(code, mode, None)
                    .map(|pending| PreparedSnapshotAttempt::Ready(Box::new(pending)))
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await?;
        let pending = match attempt {
            PreparedSnapshotAttempt::Bootstrap => break,
            PreparedSnapshotAttempt::Ready(pending) => pending,
        };

        // No checkout held here: the Cranelift compile runs concurrently
        // with every other actor's turn against this session.
        let (pending, compiled_program) =
            crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
                let mut pending = pending;
                let compiled = pending.compile_off_checkout();
                (pending, compiled)
            }))
            .await
            .map_err(ResidentActorWorkbenchError::Join)?;
        let compiled_program = compiled_program.map_err(|error| {
            ResidentActorWorkbenchError::Resident(ResidentError::Prepared(
                PreparedRuntimeError::Compile(error),
            ))
        })?;

        let finish_bound = bound.clone();
        let finish_warnings = warnings.clone();
        let finish_observation = observation.clone();
        let finish_type_modules = type_modules.clone();
        let finish_block = block.clone();
        let finish_source = turn_source.clone();
        let step = access
            .with_machine(context.clone(), move |session, context, _| {
                match session.revalidate_and_run_prepared(*pending, compiled_program) {
                    Ok(Some(outcome)) => finish_bind_step(
                        session,
                        context,
                        &finish_source,
                        &finish_type_modules,
                        &finish_block,
                        finish_bound,
                        finish_warnings,
                        finish_observation,
                        display_budget,
                        Ok(outcome),
                    )
                    .map(Some),
                    Ok(None) => Ok(None),
                    Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
                }
            })
            .await?;
        if let Some(step) = step {
            return Ok(step);
        }
        // Revalidation found a stale import: another actor's turn changed
        // a shared binding between the snapshot and this checkout. Retry
        // from a fresh snapshot.
    }

    // Fallback: either this session had no machine yet to snapshot against
    // (the bootstrap case), or every split attempt hit a stale import.
    // Single checkout, exactly as `begin_ready_block`'s Bind arm.
    access
        .with_machine(context, move |session, context, _| {
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
            finish_bind_step(
                session,
                context,
                &turn_source,
                &type_modules,
                &block,
                bound,
                warnings,
                observation,
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
    match outcome {
        ResidentOutcome::Completed { output, result } => {
            fragment.output.extend(output);
            let mut installed_bindings: Vec<String> = match &fragment.display {
                WorkbenchDisplay::Binding(binders) => {
                    binders.iter().map(|binder| binder.name.clone()).collect()
                }
                WorkbenchDisplay::Observation { name, .. } => vec![name.clone()],
                WorkbenchDisplay::Opaque | WorkbenchDisplay::Tool => Vec::new(),
            };
            installed_bindings.append(&mut fragment.recovered_jobs);
            // The same name-to-job fact `mount_command_job` records for a
            // host-minted binding, but for whatever name the model itself
            // gave the job: one command-job-typed binder, resolved against
            // the one `Cmd.start` effect this item ran, is unambiguous.
            // `bind_command_job` then finds this name instead of minting a
            // fresh alias for a job the model already named. A lost race
            // over the exact live entry is tolerated; the automatic-binding
            // path mints a fresh alias later, same as before this existed.
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
                WorkbenchDisplay::Tool => {
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
                    String::from_value(result.value(), result.table())?
                }
                WorkbenchDisplay::Observation {
                    name,
                    budget,
                    presentation,
                    source,
                    type_modules,
                } => {
                    match render_cell_observation(
                        session, context, source, type_modules,
                        name, budget.saturating_sub(fragment.output.iter().map(|text| text.chars().count()).sum::<usize>()), &fragment.presented,
                        *presentation,
                    ) {
                        Ok(text) => text,
                        Err(error) => format!("Display failed: {error}\nValue remains bound as {name}. Inspect a smaller field or projection; execution was not repeated."),
                    }
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
                | WorkbenchDisplay::Observation { .. } => None,
            };
            let receipt = projected_binding_receipt(bound_name.as_deref(), &output)?;
            let installed_bindings = match fragment.display {
                WorkbenchDisplay::Binding(binders) => {
                    binders.into_iter().map(|binder| binder.name).collect()
                }
                WorkbenchDisplay::Opaque
                | WorkbenchDisplay::Tool
                | WorkbenchDisplay::Observation { .. } => Vec::new(),
            };
            fragment.output.push(receipt);
            Ok(ResidentWorkbenchStep::Committed {
                output: fragment.output.join("\n"),
                warnings: fragment.warnings,
                installed_bindings,
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
    use super::{bounded_activation_text, declaration_worth_showing};

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
            "a\n<additional detail omitted; expand with `inspectFull sessionInput`>"
        );
        assert_eq!(
            bounded_activation_text("data R".into(), 4, false, "lookup R"),
            "data\n<additional detail omitted; expand with `lookup R`>"
        );
    }
}

/// A reply type's declaration is worth showing to the model only when it is
/// defined by the workspace or the current session: a library, stdlib, or
/// `Tidepool.*` boot-package type is already named by "reply type X", and its
/// declaration would add representation detail with no construction value.
fn declaration_worth_showing(module: &str, workspace_modules: &[String]) -> bool {
    module.starts_with("Tidepool.Session.") || workspace_modules.iter().any(|known| known == module)
}

// Bound the demanded character prefix as well as the rendered UTF-8 bytes.
// The final byte clipping can shorten a multibyte prefix further.
const ACTIVATION_INPUT_LIMIT: usize = 16 * 1024;

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
        text.push_str(&format!(
            "\n<additional detail omitted; expand with `{expansion}`>"
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
    let page_name = format!(
        "__tidepoolPage{}",
        actor_compile_view(session, context, source, type_modules)?
            .next_value_generation()
            .0
    );
    let metadata_name = format!("__tidepoolDisplayMetadata{}", session.val_gen().0);
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
    // `T.copy` is load-bearing. `renderTree` may return a slice into a large
    // Text backing array, while the host should retain only the bounded page
    // it presents.
    let block = ParsedBlock {
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
            context.haskell_effects_alias,
        ),
    };
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
    let [page, metadata, cell_display] = bound.as_slice() else {
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
    let bundle = session
        .run_display_bundle_with_sites(
            compiled.into_code(),
            page,
            metadata,
            cell_display,
            ready.generation,
        )
        .map_err(ResidentActorWorkbenchError::Resident)?;
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

impl<H, O> ResidentActorRunner<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
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
                        ),
                    ) => crate::ResidentActorStart::capture_decoded(
                        session,
                        hole,
                        crate::start::ActorStartRequest {
                            label, role, profile, launch_worktrees: worktrees,
                            fork_group: None, fork_workspace: None, effect_keys: None,
                            fork_effort: None, fork_budget: None, model: None, instructions: None, context: crate::ForkContext::SelectedContext,
                            lifetime: crate::start::ActorStartRequest::FRESH_LAUNCH_LIFETIME,
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
                        model, fork_context, instructions, lifetime,
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
                                effect_keys: Some(effect_keys), fork_effort: effort, fork_budget: budget, model, instructions, context: fork_context, lifetime,
                                session_id: context.placement.session, parent_actor: context.actor,
                                unbound_label,
                            },
                        )
                        .map(ResidentActorBoundary::Start)
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksPreviewWith(role, effect_keys, budget, model, effort, context, instructions, lifetime)) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Preview { continuation: hole, role, effect_keys, budget, model, effort, context, instructions, lifetime })),
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
                            result: tidepool_runtime::value_to_json(
                                &result,
                                session.data_con_table(),
                                0,
                            ),
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
                            registration: WatchRegistration { continuation: hole, label, dependencies }, entry,
                        })
                    }
                    ResidentRequest::Watches(WatchesReq::RegisterRouteGroupsWith(label, callback, groups)) => {
                        drop(callback); // Custody is claimed from the suspension, not the decoded value.
                        let entry = session.live_payload_handle_owned_by(hole.cont_id(), context.placement.resource_scope)?
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("route has no retained callback".into()))?;
                        let dependencies = groups.into_iter().map(|dependencies| dependencies.into_iter().map(crate::request_effect::AwaitDependency::checked).collect()).collect::<Result<Vec<Vec<_>>, _>>()?;
                        Ok(ResidentActorBoundary::RouteRegistration {
                            registration: WatchRegistration { continuation: hole, label, dependencies }, entry,
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
                            WatchRegistration { continuation: hole, dependencies, label },
                        ))
                    }
                    ResidentRequest::Watches(WatchesReq::ObserveWatchWith(watch_id)) => {
                        Ok(ResidentActorBoundary::WatchPoll(WatchPoll {
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
        self.access
            .with_machine_wait(
                context,
                Some(admission_timeout),
                move |session, _context, _| match session
                    .run_rooted_entry("actor_shutdown", hook, argument, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)?
                {
                    ResidentOutcome::Completed { .. } => Ok(()),
                    ResidentOutcome::BindingsCommitted { .. } => {
                        Err(ResidentActorWorkbenchError::ActorProtocol(
                            "shutdown completed as an impossible projected binding".into(),
                        ))
                    }
                    ResidentOutcome::Suspended { request, .. } => {
                        let request = ResidentRequest::decode(&request, session.data_con_table())?;
                        Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "shutdown suspended on disallowed `{}`",
                            request.operation()
                        )))
                    }
                },
            )
            .await
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

    /// End only the suspended computation; the independent command job remains owned.
    pub(crate) async fn stop_command_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        job: String,
        reason: CommandObservationStop,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let stopped = session.abort(
                    hole.cont_id(),
                    "foreground command observation ended".into(),
                );
                if session.parked_holes().contains(&hole.cont_id()) {
                    return match stopped {
                        Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
                        Ok(_) => Err(ResidentActorWorkbenchError::ActorProtocol(
                            "command observation continuation remained parked after abort".into(),
                        )),
                    };
                }
                match stopped {
                    Err(ResidentError::Run(tidepool_runtime::RuntimeError::Jit(
                        tidepool_effect::error::EffectError::Handler(_),
                    ))) => {
                        Err(ResidentActorWorkbenchError::CommandObservationStopped { job, reason })
                    }
                    Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
                    Ok(_) => Err(ResidentActorWorkbenchError::ActorProtocol(
                        "aborted command observation unexpectedly completed".into(),
                    )),
                }
            })
            .await
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

    pub(crate) async fn resume_progress_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<(Option<crate::request::ProgressSnapshot>, bool), crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
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
            .with_machine_wait(context, max_wait.into(), move |session, _, _| {
                let _ = session.close_realm(realm);
                Ok(())
            })
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

#[derive(Clone, Copy)]
enum OutboundKind {
    Call,
    TryCall,
    Cast,
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

enum PreparedCellStep {
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
        dependencies: tidepool_runtime::session::resident::BindingLease,
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
/// imports (`None` before the first declaration ever lands). Mirrors
/// `bind_command_job`'s `BindJobPrepare`/`begin_fragment_split`'s
/// `FragmentCompileSnapshot`.
struct CellSplitSnapshot {
    view: crate::ActorCompileView,
    candidate_module: tidepool_repr::SessionModule,
    /// Captured under this SAME checkout, for the single-item fold
    /// (`check_cell_off_checkout`'s [`tidepool_runtime::session::CellFoldTurn`]):
    /// on the prepared route, the fold's speculative compile needs exactly
    /// the retained-imports snapshot an ordinary item compile would take
    /// under `reserve_cell_generations`'s later, fresher checkout. Reusing
    /// this earlier one is safe only because the fold's result is later
    /// installed through the SAME `compile_relevant_eq` staleness check
    /// every other off-checkout compile in this split already goes through;
    /// a stale value here just makes the fold's compile itself fail or the
    /// later re-checkout discard its result, never a wrong install.
    retained: Vec<(SymbolIdentity, u64)>,
}

/// Take the checkout-scoped snapshot a split cell preparation needs, then
/// release the checkout. The request's JSON input carrier, when present, is
/// mounted once by the caller before the retry loop begins (`prepare_cell`)
/// and stays leased and un-retired across every checkout this split releases
/// and re-acquires — this only extends `source`'s imports/preamble to name
/// the already-mounted binding, so a later checkout's fresh view still
/// resolves it. See `prepare_cell`'s `HostInputRetirement` for the matching
/// retire-on-every-exit-path half of that contract.
#[allow(clippy::too_many_arguments)]
fn snapshot_cell_split<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    mut source: ActorWorkbenchSource,
    type_modules: &[String],
    response: Option<&ResponseExpectation>,
    request: Option<crate::RequestId>,
    mounted_input: Option<&MountedHostInput>,
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
    let view = actor_compile_view(session, context, &source, type_modules)?;
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
    let prepared = source.prepare(compile_view);
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
/// caller must retry from a fresh snapshot.
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
    Stale,
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
    // `library`/`compile_relevant_eq` already are — see
    // `ActorCompileView::compile_relevant_eq`'s doc comment and
    // `SessionCompileView::compile_relevant_eq`). It only names the
    // candidate module THIS cell's own `Decl` item would claim; a cell
    // with no `Decl` item never reads or writes that module, so another
    // actor committing a declaration and advancing the counter cannot make
    // this cell's own compile stale. Checking it unconditionally treated
    // every actor's declaration as invalidating every other actor's
    // in-flight split compile, which is what produced the stale-retry
    // storm the split compile exists to avoid.
    let candidate_stale = declaration_receipt.is_some()
        && session.next_declaration_module() != Some(snapshot.candidate_module);
    if !fresh_view.compile_relevant_eq(&snapshot.view) || candidate_stale {
        return Ok(CellReservation::Stale);
    }
    if value_item_count > 0 {
        let g0 = fresh_view.next_value_generation();
        let through = g0.0.saturating_add((value_item_count - 1) as u64);
        session.reserve_value_generations_through(tidepool_repr::Generation(through));
    }
    let declaration = declaration_receipt
        .map(|receipt| {
            session.render_declaration_candidate_in(
                context.placement.lexical_scope,
                receipt,
                &fresh_view.workbench_imports(),
            )
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
        view: fresh_view,
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
                    return Err(ResidentActorWorkbenchError::CompileInfrastructure(format!(
                        "staged cell binder module does not match {expected}"
                    )));
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
/// current, so the rejection stands, or the caller must retry the whole
/// split from a fresh snapshot instead of trusting a stale rejection.
enum CellRejectionRevalidation {
    StillCurrent,
    Stale,
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
    candidate_module: tidepool_repr::SessionModule,
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
    if !fresh_view.compile_relevant_eq(compiled_against)
        || session.next_declaration_module() != Some(candidate_module)
    {
        return Ok(CellRejectionRevalidation::Stale);
    }
    Ok(CellRejectionRevalidation::StillCurrent)
}

/// The outcome of [`finalize_cell_install`]'s re-checkout: either the
/// snapshot the items compiled against is still fresh and the cell installs,
/// or the caller must retry the whole split from a fresh snapshot.
enum CellInstall {
    Ready(PreparedCell),
    Stale,
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
    if !fresh_view.compile_relevant_eq(compiled_against)
        || session.next_declaration_module() != Some(candidate_module)
    {
        return Ok(CellInstall::Stale);
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
                return Ok(CellInstall::Stale)
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
        dependencies,
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
                        return Err(ResidentActorWorkbenchError::CompileInfrastructure(format!(
                            "staged cell binder module does not match {expected}"
                        )));
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
            dependencies,
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
/// already-taken `view` with no session or checkout held. A split compile
/// (see `bind_command_job`) snapshots `view` and reserves `generation` under
/// a short checkout, calls this off-checkout, then re-checks out only to
/// revalidate ([`crate::ActorCompileView::compile_relevant_eq`]) and install.
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
    let prepared = source.prepare(&view);
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

/// The Job carrier's own import set and type/anchor pair, shared by
/// [`mount_command_job`]'s single-checkout compile path and
/// [`HostCarrierKind::Job`]'s carrier build so the two can never drift.
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

/// The single-checkout Job carrier compile-and-install path: one GHC compile
/// per mount, no cache. `bind_command_job` no longer calls this in
/// production (it mounts through the cached [`HostCarrier`] instead — see
/// [`ResidentActorWorkbench::carrier_for`]); it remains as the reference
/// implementation `command_job_split_compile_then_install_binds_the_same_as_mount_command_job`
/// and other tests check the carrier path's compiled artifact against.
#[cfg(test)]
fn mount_command_job<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    binding: &str,
    job: &str,
) -> Result<(), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let (binder, compiled, generation) = compile_host_binding(
        session,
        context,
        source,
        &[],
        binding,
        COMMAND_JOB_TYPE_NAME,
        COMMAND_JOB_ANCHOR,
        command_job_carrier_imports(),
        true,
    )?;
    install_command_job_binder(session, context, binder, compiled, generation, job)
}

/// Install an already-compiled Job carrier. Split out of
/// [`mount_command_job`] so its test-only single-checkout path and the
/// `command_job_split_compile_then_install_binds_the_same_as_mount_command_job`
/// test's manual split path share the exact install/tag/retire-on-error
/// sequence, with no GHC call here.
#[cfg(test)]
fn install_command_job_binder<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    binder: BoundBinder,
    compiled: CompiledTurn,
    generation: tidepool_repr::Generation,
    job: &str,
) -> Result<(), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    session
        .mount_typed_binding_in(
            context.placement.lexical_scope,
            &binder,
            generation,
            compiled.into_code(),
            tidepool_runtime::session::HostBindingType::COMMAND_JOB,
            &HostCommandJob::Job(job.to_owned()),
        )
        .map_err(ResidentActorWorkbenchError::Resident)?;
    if let Err(error) =
        session.tag_host_text_binding_in(context.placement.lexical_scope, &binder, job.to_owned())
    {
        let session_root = carrier_mount_session_root(session, context.placement.lexical_scope)?;
        session.retire_host_binding_owner(&session_root, &binder);
        return Err(ResidentActorWorkbenchError::Resident(error));
    }
    Ok(())
}

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
    for module in &prepared.injected {
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
            .map_err(|error| ResidentActorWorkbenchError::CompileInfrastructure(error.to_string()))?
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
/// cell's off-checkout item loop (`compile_cell_items_off_checkout`), as
/// `compile_host_binding_off_checkout` is shared by `mount_command_job` and
/// `bind_command_job`.
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
    let pin_heads = pins
        .into_iter()
        .flatten()
        .flat_map(|pin| &pin.heads)
        .chain(expression_plan.into_iter().flat_map(|plan| &plan.heads));
    let pin_imports = SourceImports::from_specs(
        pin_heads
            .filter(|head| head.module.starts_with("Tidepool.Session."))
            .map(|head| format!("qualified {}", head.module)),
    );
    let compile_view = compile_view.with_workbench_imports(&pin_imports);
    let mut prepared = source.prepare(&compile_view);
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
    // session root, so a later `compile_relevant_eq` against a freshly
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
            classify_compile(&failure.error).message,
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
            let warning = session
                .recovery_manifest_warning()
                .map(|warning| format!("; manifest_warning={warning}"))
                .unwrap_or_default();
            let mut lines = vec![format!(
                "declaration recovery: source_session={:?}; successor_session={}; replayed={}; lost={}{}",
                report.source_session,
                report.successor_session,
                report.replayed.len(),
                report.lost.len(),
                warning,
            )];
            lines.extend(report.replayed.iter().map(|item| {
                format!(
                    "replayed session {} generation {} -> {} source_hash={}",
                    item.origin_session,
                    item.source_generation,
                    item.successor_generation,
                    item.source_hash
                )
            }));
            lines.extend(report.lost.iter().map(|item| {
                format!(
                    "lost session {} generation {} source_hash={} reason={}",
                    item.origin_session, item.source_generation, item.source_hash, item.reason
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
    let prepared = source.prepare(compile_view);
    let include_refs = prepared
        .include
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    match run_inspections(InspectionRequest {
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
        Ok(results) => Err(ResidentActorWorkbenchError::CompileInfrastructure(format!(
            "inspection returned {} results for {} queries",
            results.len(),
            queries.len()
        ))),
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

#[cfg(test)]
mod request_tests {
    use super::*;

    fn host_mount_fixture() -> (
        ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        crate::ActorSessionContext,
        ActorWorkbenchSource,
        tempfile::TempDir,
    ) {
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
        let session_id = tidepool_repr::SessionId((u64::from(std::process::id()) << 16) | 4_244);
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
        mount_command_job(
            &mut session,
            &context,
            &source,
            "job_binding",
            "command job",
        )
        .expect("strict Job carrier mounts through its authenticated constructor");
        assert_eq!(
            session.host_text_binding_in(context.placement.lexical_scope, "command job"),
            Some("job_binding".into())
        );

        let mut text_source = source.clone();
        text_source
            .workbench_imports
            .extend_text("qualified Data.Text as TidepoolHostText");
        text_source
            .workbench_imports
            .extend_text("qualified Tidepool.Command.Types as TidepoolHostJob");
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
                source: "textSeen <- pure (\\() -> TidepoolHostText.unpack tool_result ++ \":\" ++ case job_binding of TidepoolHostJob.Job text -> TidepoolHostText.unpack text)".into(),
            },
            None,
            None,
        )
        .expect("mounted Text and strict Job are executable");
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
        .expect("mounted Text and Job result renders");
        assert!(
            text_display.contains("tool output:command job"),
            "Text/Job result: {text_display}"
        );
        session.retire_host_binding_owner(session_root.path(), &input.binder);
    }

    /// Problem 1 of the compile-path design note: a split compile takes its
    /// `ActorCompileView` snapshot, compiles off-checkout, then re-derives a
    /// fresh view before installing. With no mutation in between, the split
    /// path must bind exactly what `mount_command_job`'s single-checkout
    /// path binds.
    #[test]
    fn command_job_split_compile_then_install_binds_the_same_as_mount_command_job() {
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
            "job_binding",
            COMMAND_JOB_TYPE_NAME,
            COMMAND_JOB_ANCHOR,
            command_job_carrier_imports(),
            true,
            &retained,
        )
        .expect("job carrier compiles off-checkout");

        let fresh_view = actor_compile_view(&session, &context, &source, &[]).expect("fresh view");
        assert!(
            fresh_view.compile_relevant_eq(&view),
            "no mutation happened between snapshot and install: views must still match"
        );

        install_command_job_binder(
            &mut session,
            &context,
            binder,
            compiled,
            generation,
            "command job",
        )
        .expect("job carrier installs after revalidation");

        assert_eq!(
            session.host_text_binding_in(scope, "command job"),
            Some("job_binding".into())
        );
    }

    /// A write to the same scope between a split compile's snapshot and its
    /// re-checkout (here, another mount standing in for a concurrent
    /// actor's install) must be detected by `compile_relevant_eq` before
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
            "job_binding",
            COMMAND_JOB_TYPE_NAME,
            COMMAND_JOB_ANCHOR,
            command_job_carrier_imports(),
            true,
            &retained,
        )
        .expect("job carrier compiles off-checkout");

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
            !fresh_view.compile_relevant_eq(&view),
            "an interleaved mutation to the same scope must invalidate the snapshot"
        );

        // The split path must refuse to install against a stale view — the
        // binder never reaches the persistent binding store.
        assert!(session
            .workbench_bindings_in(scope)
            .into_iter()
            .all(|binding| binding.name != "job_binding"));
        drop((binder, compiled, generation));

        // A fresh snapshot recompiles and installs cleanly — the same
        // staleness recovery `begin_fragment_split` and `prepare_cell`'s own
        // split paths rely on.
        let retry_view = actor_compile_view(&session, &context, &source, &[]).expect("retry view");
        let retry_generation = retry_view.next_value_generation();
        session.reserve_value_generations_through(retry_generation);
        let retry_retained = session.prepared_retained();
        let (retry_binder, retry_compiled) = compile_host_binding_off_checkout(
            &retry_view,
            &source,
            &context.haskell_effects_alias,
            retry_generation,
            "job_binding2",
            COMMAND_JOB_TYPE_NAME,
            COMMAND_JOB_ANCHOR,
            command_job_carrier_imports(),
            true,
            &retry_retained,
        )
        .expect("recompile against the fresh snapshot succeeds");
        install_command_job_binder(
            &mut session,
            &context,
            retry_binder,
            retry_compiled,
            retry_generation,
            "retry command job",
        )
        .expect("recompiled carrier installs");
        assert_eq!(
            session.host_text_binding_in(scope, "retry command job"),
            Some("job_binding2".into())
        );
    }

    /// The first `bind_command_job` call for a fresh workbench builds the
    /// Job carrier ([`ResidentActorWorkbench::carrier_for`]), which is the
    /// only extractor invocation `bind_command_job` itself makes. A second
    /// call, for a different job, mounts through the now-cached carrier and
    /// makes none: [`tidepool_extract_cmd::extract_spawn_count`] — the same
    /// counter the cell-cost and documentation tests already read from —
    /// must not move between the two.
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
    }

    /// The same split shape as `command_job_split_compile_then_install_
    /// binds_the_same_as_mount_command_job`, but for an ordinary workbench
    /// fragment (`begin_fragment`/`begin_ready_block`): snapshotting via
    /// `snapshot_fragment_compile`, compiling off-checkout via
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
            fresh_view.compile_relevant_eq(&snapshot.view),
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

    /// A write to the same scope between an ordinary fragment's split-compile
    /// snapshot and its re-checkout (here, another mount standing in for a
    /// concurrent actor's install) must be detected by `compile_relevant_eq`
    /// before installing the stale compile, and a fresh snapshot must still
    /// recompile and install cleanly — the same invariant
    /// `a_mutation_between_split_checkouts_invalidates_the_snapshot_and_
    /// blocks_install` proves for the Job carrier path.
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
            !fresh_view.compile_relevant_eq(&snapshot.view),
            "an interleaved mutation to the same scope must invalidate the snapshot"
        );

        // The split path must refuse to install against a stale view — no
        // binding named by the compiled fragment ever reaches the workbench.
        assert!(session
            .workbench_bindings_in(scope)
            .into_iter()
            .all(|binding| binding.name != "x"));
        drop(ready);

        // A fresh snapshot recompiles and installs cleanly — the
        // bounded-retry recovery `begin_fragment_split` relies on.
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
        assert!(retry_fresh_view.compile_relevant_eq(&retry_snapshot.view));

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
        for (id, name, arity) in [(120, "AgentForgetRetained", 2), (121, "AgentForgotten", 0)] {
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
            ResidentWorkbenchStep::CommandBackgrounded { .. } => "CommandBackgrounded".into(),
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
                PreparedCellStep::Declaration { .. } => None,
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
    /// `compile_relevant_eq` before installing the stale compile, and a
    /// fresh snapshot must still recover, the same invariant
    /// `a_mutation_between_split_fragment_checkouts_invalidates_the_snapshot_and_forces_a_recompile`
    /// proves for an ordinary fragment.
    #[test]
    fn cell_split_scope_mutation_before_final_checkout_forces_a_recompile() {
        let (mut session, context, base_source, _root) = host_mount_fixture();
        let cell = "onlyItem <- pure (1 :: Int)";

        // Each attempt snapshots from the pristine base source, exactly as
        // `prepare_cell`'s retry loop re-clones `self.access.source` every
        // iteration — never from a previous attempt's already-mutated
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
            matches!(install, CellInstall::Stale),
            "an interleaved mutation must invalidate the snapshot"
        );

        // A fresh snapshot recompiles and installs cleanly — the
        // bounded-retry recovery `prepare_cell` relies on. Snapshots again
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

    #[test]
    fn child_session_factory_builds_a_type_checking_session_and_inserts_it_idle() {
        let (machines, context, source, _root) = actor_registry_fixture();
        let child_id = tidepool_repr::SessionId(context.placement.session.0.wrapping_add(1));
        // Keep the fixture's tempdir alive for the factory's one call — a
        // production factory instead captures the run's real session root.
        let held_root = std::sync::Mutex::new(None);
        let runner = ResidentActorRunner::new(Arc::clone(&machines), source)
            .with_child_session_factory(Arc::new(move |session_id| {
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
        let (source_a, snapshot_a) = snapshot_cell_split(
            &mut session,
            &context_a,
            source,
            &[],
            None,
            None,
            None,
        )
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
    /// input is mounted once, before the retry loop, and must not be
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

    /// A declaration cell's own GHC validation runs off-checkout, against a
    /// private candidate directory — so two actors declaring concurrently,
    /// in the SAME lexical scope (unlike
    /// [`actor_registry_fixture_two_scopes`], which deliberately isolates
    /// scopes to avoid this exact contention), must race for the one shared
    /// `next_declaration_module()` generation: whichever installs second sees
    /// its candidate go stale at `finalize_cell_install` and retries against
    /// a fresh snapshot ([`Self::prepare_cell`]'s bounded
    /// `MAX_SPLIT_ATTEMPTS` loop). Both must still land, with two DISTINCT
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
    /// A fresh snapshot recompiles and installs cleanly afterward — the
    /// bounded-retry recovery `prepare_cell` relies on when a rejection
    /// turns out stale.
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
            snapshot.candidate_module,
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
            snapshot.candidate_module,
            &view,
        )
        .expect("revalidate against the mutated view");
        assert!(
            matches!(revalidation, CellRejectionRevalidation::Stale),
            "an interleaved mutation must invalidate the rejected view, not settle it as current"
        );

        // A fresh snapshot recompiles and installs cleanly — the
        // bounded-retry recovery `prepare_cell` relies on once a rejection
        // is found stale. Snapshots again from the pristine base source, not
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
}
