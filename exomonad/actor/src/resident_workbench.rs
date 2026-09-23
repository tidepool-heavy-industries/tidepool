//! Actor binding for the shared resident Haskell workbench.
//!
//! This adapter owns no machine and holds no checkout between calls. Each
//! fenced block checks out the actor's registered resident session, installs
//! the exact actor context, compiles and runs one segment on the blocking
//! pool, then restores the machine before the actor loop continues.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, FromHaskell, HaskellVisitor, ToHaskell};
use tidepool_bridge_derive::FromHaskell as DeriveFromHaskell;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{request_constructor, DispatchEffect};
use tidepool_repr::DataConTable;
use tidepool_runtime::session::registry::{CheckoutError, SessionRegistry};
use tidepool_runtime::session::{
    check_cell, hide_preamble_exports, insert_preamble_imports, render_turn_compile_rejection,
    resident_cell_check_template, resident_workbench_templates, run_inspections, run_turn,
    run_turn_pinned, BoundBinder, CellCheck, CellCheckRequest, CheckedBinderPin,
    CheckedExpressionPlan, CompiledTurn, DeclarationReceipt, ExpressionPresentation,
    InspectionQuery, InspectionRequest, OutputSink, ParsedBlock, ResidentError, ResidentHole,
    ResidentOutcome, ResidentResumeError, ResidentSession, RootCustody, SourceImports,
    TurnClassification, TurnKind, TurnRequest, TurnResult,
};
use tidepool_runtime::{classify_compile, classify_session, CompileError, FailureClass};

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

/// Shared checkout boundary for every actor machine entry path. Fenced
/// fragments and installed actor programs differ above this layer, but use
/// exactly the same admission and settlement mechanism. Every checkout
/// installs the supplied actor context before invoking its operation, so a
/// child cannot leak its lexical scope, runtime resource scope, or principal into the
/// next parent or sibling entry.
struct ResidentMachineAccess<H, O> {
    machines: Arc<ActorMachineRegistry<H, O>>,
    source: ActorWorkbenchSource,
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
        Self { machines, source }
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
}

impl ResidentWorkbenchFragment {
    pub(crate) fn summarizes_bound_commands(&self) -> bool {
        matches!(&self.display, WorkbenchDisplay::Binding(_))
    }

    pub(crate) fn retain_job_binding(&mut self, binding: String) {
        self.recovered_jobs.push(binding);
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
    Binding(Vec<String>),
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
        fragment: ResidentWorkbenchFragment,
        outcome: Box<ResidentOutcome>,
    },
    Replied {
        request: crate::RequestId,
        result: RootCustody,
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
            access: ResidentMachineAccess::new(
                Arc::clone(&self.access.machines),
                self.access.source.clone(),
            ),
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

fn visit_named(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    module: &str,
    name: &str,
    fields: impl FnOnce(&mut dyn HaskellVisitor) -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    let qualified = format!("{module}.{name}");
    let constructor = table
        .get_by_qualified_name(&qualified)
        .ok_or_else(|| BridgeError::UnknownDataConName(qualified.clone()))?;
    let arity = table
        .get(constructor)
        .map(|data_con| data_con.rep_arity as usize)
        .ok_or(BridgeError::UnknownDataConName(qualified))?;
    visitor.begin_constructor(constructor, arity)?;
    fields(visitor)?;
    visitor.end_constructor()
}

fn actor_haskell_int(value: u64, label: &str) -> Result<i64, BridgeError> {
    i64::try_from(value)
        .map_err(|_| BridgeError::UnsupportedType(format!("{label} exceeds Haskell Int")))
}

fn visit_core(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    name: &str,
    fields: impl FnOnce(&mut dyn HaskellVisitor) -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    visit_named(table, visitor, "Tidepool.Effects.Core", name, fields)
}

struct RouteStateAnswer(Result<crate::request::routes::RouteState, crate::ReplyError>);

impl tidepool_bridge::sealed::ToHaskellSealed for RouteStateAnswer {}
impl ToHaskell for RouteStateAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        use crate::request::routes::RouteState;
        match &self.0 {
            Ok(RouteState::Waiting) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteWaiting",
                |_| Ok(()),
            ),
            Ok(RouteState::Running) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteRunning",
                |_| Ok(()),
            ),
            Ok(RouteState::Completed) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteCompleted",
                |_| Ok(()),
            ),
            Ok(RouteState::Failed(error)) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteFailed",
                |visitor| error.visit(table, visitor),
            ),
            Err(error) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteRejected",
                |visitor| error.visit(table, visitor),
            ),
        }
    }
}

struct CallStatus(Option<String>);

impl tidepool_bridge::sealed::ToHaskellSealed for CallStatus {}
impl ToHaskell for CallStatus {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match &self.0 {
            Some(summary) => visit_core(table, visitor, "ActorCallFailed", |visitor| {
                summary.visit(table, visitor)
            }),
            None => visit_core(table, visitor, "ActorCallSucceeded", |_| Ok(())),
        }
    }
}

enum ProgressAnswer {
    Pending,
    Closed,
    Rejected(crate::ReplyError),
}

enum LifecycleAnswer {
    Live,
    Paused(String),
    Exited(crate::ActorTerminal),
}

struct ForkCleanupAnswer(Result<crate::ForkGroupCleanupOutcome, crate::ForkGroupError>);

impl tidepool_bridge::sealed::ToHaskellSealed for ForkCleanupAnswer {}
impl ToHaskell for ForkCleanupAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match &self.0 {
            Ok(crate::ForkGroupCleanupOutcome::Cleaned) => {
                visit_core(table, visitor, "ForkGroupCleaned", |_| Ok(()))
            }
            Ok(crate::ForkGroupCleanupOutcome::Active(actors)) => {
                visit_core(table, visitor, "ForkGroupStillActive", |visitor| {
                    actors
                        .iter()
                        .map(|actor| {
                            Ok((
                                actor_haskell_int(actor.id.0, "actor id")?,
                                actor_haskell_int(actor.incarnation.0, "actor incarnation")?,
                            ))
                        })
                        .collect::<Result<Vec<_>, BridgeError>>()?
                        .visit(table, visitor)
                })
            }
            Err(error) => visit_core(table, visitor, "ForkGroupCleanupRejected", |visitor| {
                error.to_string().visit(table, visitor)
            }),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for LifecycleAnswer {}
impl ToHaskell for LifecycleAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::Live => visit_core(table, visitor, "ActorLive", |_| Ok(())),
            Self::Paused(detail) => visit_core(table, visitor, "ActorPaused", |visitor| {
                detail.visit(table, visitor)
            }),
            Self::Exited(terminal) => visit_core(
                table,
                visitor,
                match terminal.kind {
                    crate::ActorExitKind::Completed => "ActorFinished",
                    crate::ActorExitKind::Failed => "ActorFailed",
                    crate::ActorExitKind::Cancelled => "ActorCancelled",
                },
                |visitor| terminal.summary.visit(table, visitor),
            ),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for ProgressAnswer {}
impl ToHaskell for ProgressAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::Pending => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Reply.Internal",
                "ProgressPending",
                |_| Ok(()),
            ),
            Self::Closed => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Reply.Internal",
                "ProgressClosed",
                |_| Ok(()),
            ),
            Self::Rejected(error) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Reply.Internal",
                "ProgressRejected",
                |visitor| error.visit(table, visitor),
            ),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for AgentForgetProjection {}
impl ToHaskell for AgentForgetProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::Forgotten => visit_core(table, visitor, "AgentForgotten", |_| Ok(())),
            Self::Running => visit_core(table, visitor, "AgentForgetRunning", |_| Ok(())),
            Self::Unavailable => visit_core(table, visitor, "AgentForgetUnavailable", |_| Ok(())),
            Self::Retained { requests, watches } => {
                visit_core(table, visitor, "AgentForgetRetained", |visitor| {
                    requests
                        .iter()
                        .map(|request| actor_haskell_int(request.0, "request id"))
                        .collect::<Result<Vec<_>, _>>()?
                        .visit(table, visitor)?;
                    watches
                        .iter()
                        .map(|watch| actor_haskell_int(watch.0, "watch id"))
                        .collect::<Result<Vec<_>, _>>()?
                        .visit(table, visitor)
                })
            }
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for AgentStopProjection {}
impl ToHaskell for AgentStopProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::StoppedNow => visit_core(table, visitor, "AgentStoppedNow", |_| Ok(())),
            Self::StoppedRetaining(detail) => {
                visit_core(table, visitor, "AgentStoppedRetaining", |visitor| {
                    detail.visit(table, visitor)
                })
            }
            Self::StoppedReleasing => {
                visit_core(table, visitor, "AgentStoppedReleasing", |_| Ok(()))
            }
            Self::AlreadyStopped => {
                visit_core(table, visitor, "AgentStopAlreadyStopped", |_| Ok(()))
            }
            Self::Unavailable => visit_core(table, visitor, "AgentStopUnavailable", |_| Ok(())),
            Self::Unauthorized => visit_core(table, visitor, "AgentStopUnauthorized", |_| Ok(())),
            Self::Failed(detail) => visit_core(table, visitor, "AgentStopFailed", |visitor| {
                detail.visit(table, visitor)
            }),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for CleanupActorProjection {}
impl ToHaskell for CleanupActorProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        visit_core(table, visitor, "CleanupActorPlan", |visitor| {
            actor_haskell_int(self.actor.id.0, "actor id")?.visit(table, visitor)?;
            actor_haskell_int(self.actor.incarnation.0, "actor incarnation")?
                .visit(table, visitor)?;
            self.label.visit(table, visitor)?;
            visit_core(
                table,
                visitor,
                if self.terminal {
                    "CleanupActorTerminal"
                } else {
                    "CleanupActorRunning"
                },
                |_| Ok(()),
            )?;
            actor_haskell_int(self.revision, "cleanup revision")?.visit(table, visitor)
        })
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for CleanupPlanProjection {}
impl ToHaskell for CleanupPlanProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        visit_core(table, visitor, "CleanupPlan", |visitor| {
            actor_haskell_int(self.group.0, "fork group id")?.visit(table, visitor)?;
            self.actors.visit(table, visitor)?;
            self.pending_responses
                .iter()
                .map(|request| actor_haskell_int(request.0, "request id"))
                .collect::<Result<Vec<_>, _>>()?
                .visit(table, visitor)?;
            self.pending_watches
                .iter()
                .map(|watch| actor_haskell_int(watch.0, "watch id"))
                .collect::<Result<Vec<_>, _>>()?
                .visit(table, visitor)?;
            self.refusal.visit(table, visitor)
        })
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for CleanupStepProjection {}
impl ToHaskell for CleanupStepProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let ids = |visitor: &mut dyn HaskellVisitor, values: &[u64], label| {
            values
                .iter()
                .copied()
                .map(|value| actor_haskell_int(value, label))
                .collect::<Result<Vec<_>, _>>()?
                .visit(table, visitor)
        };
        match self {
            Self::ForgotResponses(requests) => {
                visit_core(table, visitor, "CleanupForgotResponses", |visitor| {
                    ids(
                        visitor,
                        &requests.iter().map(|request| request.0).collect::<Vec<_>>(),
                        "request id",
                    )
                })
            }
            Self::ForgotWatches(watches) => {
                visit_core(table, visitor, "CleanupForgotWatches", |visitor| {
                    ids(
                        visitor,
                        &watches.iter().map(|watch| watch.0).collect::<Vec<_>>(),
                        "watch id",
                    )
                })
            }
            Self::StoppedActor(actor, outcome) => {
                visit_core(table, visitor, "CleanupStoppedActor", |visitor| {
                    actor_haskell_int(actor.id.0, "actor id")?.visit(table, visitor)?;
                    actor_haskell_int(actor.incarnation.0, "actor incarnation")?
                        .visit(table, visitor)?;
                    outcome.visit(table, visitor)
                })
            }
            Self::ForgotActor(actor) => {
                visit_core(table, visitor, "CleanupForgotActor", |visitor| {
                    actor_haskell_int(actor.id.0, "actor id")?.visit(table, visitor)?;
                    actor_haskell_int(actor.incarnation.0, "actor incarnation")?
                        .visit(table, visitor)
                })
            }
            Self::ActorRetained {
                actor,
                requests,
                watches,
            } => visit_core(table, visitor, "CleanupActorRetained", |visitor| {
                actor_haskell_int(actor.id.0, "actor id")?.visit(table, visitor)?;
                actor_haskell_int(actor.incarnation.0, "actor incarnation")?
                    .visit(table, visitor)?;
                requests
                    .iter()
                    .map(|request| actor_haskell_int(request.0, "request id"))
                    .collect::<Result<Vec<_>, _>>()?
                    .visit(table, visitor)?;
                watches
                    .iter()
                    .map(|watch| actor_haskell_int(watch.0, "watch id"))
                    .collect::<Result<Vec<_>, _>>()?
                    .visit(table, visitor)
            }),
            Self::GroupRetired(group) => {
                visit_core(table, visitor, "CleanupGroupRetired", |visitor| {
                    actor_haskell_int(group.0, "fork group id")?.visit(table, visitor)
                })
            }
            Self::Blocked(detail) => visit_core(table, visitor, "CleanupBlocked", |visitor| {
                detail.visit(table, visitor)
            }),
            Self::StalePlan => visit_core(table, visitor, "CleanupStalePlan", |_| Ok(())),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for CleanupReceiptProjection {}
impl ToHaskell for CleanupReceiptProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        visit_core(table, visitor, "CleanupReceipt", |visitor| {
            self.plan.visit(table, visitor)?;
            self.steps.visit(table, visitor)?;
            self.complete.visit(table, visitor)
        })
    }
}

fn visit_usage_observation(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    sample: Option<&crate::ProviderUsageSample>,
) -> Result<(), BridgeError> {
    match sample {
        None => Option::<i64>::None.visit(table, visitor),
        Some(sample) => visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
            visit_core(table, visitor, "ProviderUsageObservation", |visitor| {
                sample.observation_id.visit(table, visitor)?;
                sample.source_timestamp.visit(table, visitor)?;
                sample.cached_input_tokens.visit(table, visitor)?;
                sample.uncached_input_tokens.visit(table, visitor)
            })
        }),
    }
}

fn visit_usage_summary(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    summary: Option<&exomonad_model::ProviderUsageSummary>,
) -> Result<(), BridgeError> {
    use exomonad_model::{ProviderUsageCompleteness, ProviderUsageScope};
    match summary {
        None => Option::<i64>::None.visit(table, visitor),
        Some(summary) => {
            if summary.usage.cached_input_tokens > summary.usage.input_tokens {
                return Err(BridgeError::UnsupportedType(
                    "cached input tokens exceed input tokens".into(),
                ));
            }
            let uncached = summary
                .usage
                .input_tokens
                .checked_sub(summary.usage.cached_input_tokens)
                .ok_or_else(|| {
                    BridgeError::UnsupportedType("cached input tokens exceed input tokens".into())
                })?;
            visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
                visit_core(table, visitor, "ProviderUsageSummary", |visitor| {
                    match &summary.scope {
                        ProviderUsageScope::Thread(thread) => {
                            visit_core(table, visitor, "UsageThread", |visitor| {
                                thread.visit(table, visitor)
                            })?
                        }
                        ProviderUsageScope::Turn { thread, turn } => {
                            visit_core(table, visitor, "UsageTurn", |visitor| {
                                thread.visit(table, visitor)?;
                                turn.visit(table, visitor)
                            })?
                        }
                    }
                    visit_core(
                        table,
                        visitor,
                        match summary.completeness {
                            ProviderUsageCompleteness::Partial => "UsagePartial",
                            ProviderUsageCompleteness::Complete => "UsageComplete",
                        },
                        |_| Ok(()),
                    )?;
                    summary.observations.visit(table, visitor)?;
                    summary.usage.cached_input_tokens.visit(table, visitor)?;
                    uncached.visit(table, visitor)?;
                    summary.usage.output_tokens.visit(table, visitor)?;
                    summary
                        .usage
                        .reasoning_output_tokens
                        .visit(table, visitor)?;
                    summary.usage.total_tokens.visit(table, visitor)
                })
            })
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for AgentRosterProjection {}
impl ToHaskell for AgentRosterProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let role = match self.descriptor.effective_role().role() {
            crate::ActorRole::Root => "ContextRoot",
            crate::ActorRole::Research => "ContextResearch",
            crate::ActorRole::Coding => "ContextCoding",
            crate::ActorRole::Scaffolding => "ContextScaffolding",
            crate::ActorRole::Integration => "ContextIntegration",
            crate::ActorRole::Inherited => "ContextInherited",
        };
        visit_core(table, visitor, "AgentRosterEntry", |visitor| {
            actor_haskell_int(self.actor.id.0, "actor id")?.visit(table, visitor)?;
            actor_haskell_int(self.actor.incarnation.0, "actor incarnation")?
                .visit(table, visitor)?;
            self.descriptor.label().to_owned().visit(table, visitor)?;
            self.runtime
                .requested_model
                .as_deref()
                .or(self.descriptor.model_name())
                .map(str::to_owned)
                .visit(table, visitor)?;
            self.runtime.confirmed_model.visit(table, visitor)?;
            actor_haskell_int(self.received.0, "received count")?.visit(table, visitor)?;
            actor_haskell_int(self.received.1, "received count")?.visit(table, visitor)?;
            self.runtime
                .compactions
                .map(|x| actor_haskell_int(x, "compaction count"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .creator()
                .map(|x| actor_haskell_int(x.id.0, "creator id"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .creator()
                .map(|x| actor_haskell_int(x.incarnation.0, "creator incarnation"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .supervisor_parent()
                .map(|x| actor_haskell_int(x.id.0, "supervisor id"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .supervisor_parent()
                .map(|x| actor_haskell_int(x.incarnation.0, "supervisor incarnation"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .context_parent()
                .map(|x| actor_haskell_int(x.id.0, "context parent id"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .context_parent()
                .map(|x| actor_haskell_int(x.incarnation.0, "context parent incarnation"))
                .transpose()?
                .visit(table, visitor)?;
            match &self.terminal {
                None => visit_core(table, visitor, "RosterRunning", |_| Ok(()))?,
                Some(t) => match t.kind {
                    crate::ActorExitKind::Completed => {
                        visit_core(table, visitor, "RosterStopped", |_| Ok(()))?
                    }
                    crate::ActorExitKind::Failed => {
                        visit_core(table, visitor, "RosterFailed", |v| {
                            t.summary.visit(table, v)
                        })?
                    }
                    crate::ActorExitKind::Cancelled => {
                        visit_core(table, visitor, "RosterCancelled", |v| {
                            t.summary.visit(table, v)
                        })?
                    }
                },
            }
            match self.runtime.provider_turn.as_ref().map(|t| &t.state) {
                None => visit_core(table, visitor, "ProviderUnknown", |_| Ok(()))?,
                Some(exomonad_model::ProviderTurnState::Active) => {
                    visit_core(table, visitor, "ProviderActive", |_| Ok(()))?
                }
                Some(exomonad_model::ProviderTurnState::Succeeded) => {
                    visit_core(table, visitor, "ProviderSucceeded", |_| Ok(()))?
                }
                Some(exomonad_model::ProviderTurnState::Interrupted) => {
                    visit_core(table, visitor, "ProviderInterrupted", |_| Ok(()))?
                }
                Some(exomonad_model::ProviderTurnState::Failed(f)) => {
                    visit_core(table, visitor, "ProviderFailed", |v| match f {
                        exomonad_model::ProviderFailure::RequestRejected => {
                            visit_core(table, v, "RequestRejected", |_| Ok(()))
                        }
                        exomonad_model::ProviderFailure::TransportFailed => {
                            visit_core(table, v, "TransportFailed", |_| Ok(()))
                        }
                        exomonad_model::ProviderFailure::Other(d) => {
                            visit_core(table, v, "OtherProviderFailure", |v| d.visit(table, v))
                        }
                    })?
                }
            }
            self.runtime
                .provider_turn
                .as_ref()
                .map(|turn| turn.turn.clone())
                .visit(table, visitor)?;
            self.runtime
                .provider_observation_stale
                .visit(table, visitor)?;
            match self.terminal {
                None => visit_named(table, visitor, "GHC.Maybe", "Just", |v| {
                    visit_core(
                        table,
                        v,
                        self.runtime
                            .disposition(!self.requests.0.is_empty() || !self.requests.1.is_empty())
                            .constructor_name(),
                        |_| Ok(()),
                    )
                })?,
                Some(_) => Option::<i64>::None.visit(table, visitor)?,
            }
            self.requests
                .0
                .iter()
                .map(|x| actor_haskell_int(x.0, "request id"))
                .collect::<Result<Vec<_>, _>>()?
                .visit(table, visitor)?;
            self.requests
                .1
                .iter()
                .map(|x| actor_haskell_int(x.0, "request id"))
                .collect::<Result<Vec<_>, _>>()?
                .visit(table, visitor)?;
            visit_core(table, visitor, role, |_| Ok(()))?;
            self.bound_worktree.visit(table, visitor)?;
            self.descriptor
                .fork_group()
                .map(|x| actor_haskell_int(x.0, "fork group id"))
                .transpose()?
                .visit(table, visitor)?;
            actor_haskell_int(self.descriptor.placement().lexical_scope.0, "lexical scope")?
                .visit(table, visitor)?;
            self.runtime.provider_thread.visit(table, visitor)?;
            self.runtime.provider_parent_thread.visit(table, visitor)?;
            visit_usage_observation(table, visitor, self.runtime.first_provider_usage.as_ref())?;
            visit_usage_observation(table, visitor, self.runtime.latest_provider_usage())?;
            visit_usage_summary(table, visitor, self.runtime.provider_usage_summary.as_ref())?;
            visit_usage_summary(
                table,
                visitor,
                self.runtime.latest_turn_usage_summary.as_ref(),
            )?;
            match self.runtime.latest_provider_usage() {
                None => Option::<i64>::None.visit(table, visitor)?,
                Some(s) => visit_named(table, visitor, "GHC.Maybe", "Just", |v| {
                    visit_core(
                        table,
                        v,
                        match s.cache_boundary {
                            crate::CacheBoundaryReason::Fresh => "CacheFresh",
                            crate::CacheBoundaryReason::ForkedPrefix => "CacheForkedPrefix",
                            crate::CacheBoundaryReason::ReattachedThread => "CacheReattachedThread",
                            crate::CacheBoundaryReason::ProviderUnknown => "CacheProviderUnknown",
                        },
                        |_| Ok(()),
                    )
                })?,
            }
            actor_haskell_int(self.runtime.event_watermark, "event watermark")?
                .visit(table, visitor)?;
            visit_workbench_posture(table, visitor, &self.runtime.workbench_posture)?;
            self.runtime.launched_at_unix_ms.visit(table, visitor)
        })
    }
}

fn visit_workbench_posture(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    posture: &crate::ActorWorkbenchPosture,
) -> Result<(), BridgeError> {
    match posture {
        crate::ActorWorkbenchPosture::Idle => {
            visit_core(table, visitor, "WorkbenchIdle", |_| Ok(()))
        }
        crate::ActorWorkbenchPosture::RunningUnit {
            input_unit_index,
            total,
        } => visit_core(table, visitor, "WorkbenchRunningUnit", |v| {
            i64::try_from(*input_unit_index)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)?;
            i64::try_from(*total)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)
        }),
        crate::ActorWorkbenchPosture::AwaitingEffect {
            input_unit_index,
            total,
            effect,
        } => visit_core(table, visitor, "WorkbenchAwaitingEffect", |v| {
            i64::try_from(*input_unit_index)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)?;
            i64::try_from(*total)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)?;
            effect.visit(table, v)
        }),
        crate::ActorWorkbenchPosture::TerminalTransfer { transfer } => {
            visit_core(table, visitor, "WorkbenchTerminalTransfer", |v| {
                visit_core(
                    table,
                    v,
                    match transfer {
                        crate::ActorWorkbenchTransfer::Reply => "WorkbenchReplyTransfer",
                        crate::ActorWorkbenchTransfer::CancellationAcknowledgement => {
                            "WorkbenchCancellationTransfer"
                        }
                    },
                    |_| Ok(()),
                )
            })
        }
        crate::ActorWorkbenchPosture::Failed => {
            visit_core(table, visitor, "WorkbenchFailed", |_| Ok(()))
        }
    }
}

struct ActorContextProjection {
    context: crate::ActorSessionContext,
    descriptor: crate::ActorDescriptor,
    bound_worktree: Option<String>,
    runtime: crate::ActorRuntimeObservation,
}

impl tidepool_bridge::sealed::ToHaskellSealed for ActorContextProjection {}
impl ToHaskell for ActorContextProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let role = match self.descriptor.effective_role().role() {
            crate::ActorRole::Root => "ContextRoot",
            crate::ActorRole::Research => "ContextResearch",
            crate::ActorRole::Coding => "ContextCoding",
            crate::ActorRole::Scaffolding => "ContextScaffolding",
            crate::ActorRole::Integration => "ContextIntegration",
            crate::ActorRole::Inherited => "ContextInherited",
        };
        let tools = match self.descriptor.effective_role().native_tools() {
            crate::NativeToolClass::InspectionOnly => "NativeInspectionOnly",
            crate::NativeToolClass::Coding => "NativeCoding",
            crate::NativeToolClass::Integration => "NativeIntegration",
            crate::NativeToolClass::Inherited => "NativeInherited",
        };
        let workspace = match self.descriptor.effective_role().workspace() {
            crate::WorkspaceAccess::None => "WorkspaceNone",
            crate::WorkspaceAccess::InspectOnly => "WorkspaceInspectOnly",
            crate::WorkspaceAccess::WritableBound => "WorkspaceWritableBound",
        };
        visit_core(table, visitor, "ActorContextInfo", |v| {
            actor_haskell_int(self.context.actor.id.0, "actor id")?.visit(table, v)?;
            actor_haskell_int(self.context.actor.incarnation.0, "actor incarnation")?
                .visit(table, v)?;
            self.descriptor
                .context_parent()
                .map(|x| actor_haskell_int(x.id.0, "context parent id"))
                .transpose()?
                .visit(table, v)?;
            self.descriptor
                .context_parent()
                .map(|x| actor_haskell_int(x.incarnation.0, "context parent incarnation"))
                .transpose()?
                .visit(table, v)?;
            self.descriptor.label().to_owned().visit(table, v)?;
            visit_core(table, v, role, |_| Ok(()))?;
            self.descriptor
                .effective_role()
                .haskell_effects_type()
                .visit(table, v)?;
            visit_core(table, v, tools, |_| Ok(()))?;
            visit_core(table, v, workspace, |_| Ok(()))?;
            self.bound_worktree.visit(table, v)?;
            self.descriptor
                .fork_group()
                .map(|x| actor_haskell_int(x.0, "fork group id"))
                .transpose()?
                .visit(table, v)?;
            actor_haskell_int(self.context.placement.lexical_scope.0, "lexical scope")?
                .visit(table, v)?;
            match &self.runtime.activation_kind {
                crate::ActorActivationKind::RootStarted => {
                    visit_core(table, v, "ActivationRootStarted", |_| Ok(()))?
                }
                crate::ActorActivationKind::RequestActivated {
                    request,
                    activation_sequence,
                } => visit_core(table, v, "ActivationRequest", |v| {
                    actor_haskell_int(request.0, "request id")?.visit(table, v)?;
                    actor_haskell_int(*activation_sequence, "activation sequence")?.visit(table, v)
                })?,
                crate::ActorActivationKind::EventsActivated { inbox_sequences } => {
                    visit_core(table, v, "ActivationEvents", |v| {
                        inbox_sequences
                            .iter()
                            .copied()
                            .map(|x| actor_haskell_int(x, "inbox sequence"))
                            .collect::<Result<Vec<_>, _>>()?
                            .visit(table, v)
                    })?
                }
            }
            actor_haskell_int(self.runtime.event_watermark, "event watermark")?.visit(table, v)?;
            self.runtime.provider_thread.visit(table, v)?;
            self.runtime.provider_parent_thread.visit(table, v)?;
            visit_usage_observation(table, v, self.runtime.first_provider_usage.as_ref())?;
            visit_usage_observation(table, v, self.runtime.latest_provider_usage())?;
            visit_usage_summary(table, v, self.runtime.provider_usage_summary.as_ref())?;
            visit_usage_summary(table, v, self.runtime.latest_turn_usage_summary.as_ref())?;
            i64::from(self.descriptor.effective_role().descendants().maximum_depth)
                .visit(table, v)?;
            self.descriptor
                .effective_role()
                .descendants()
                .maximum_active_children
                .map(i64::from)
                .visit(table, v)?;
            self.descriptor
                .effective_role()
                .prompt_profile()
                .to_owned()
                .visit(table, v)
        })
    }
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
        ResidentActorWorkbench::new(
            Arc::clone(&self.access.machines),
            self.access.source.clone(),
            Some(response),
            Some(request),
            type_modules,
        )
    }

    pub(crate) fn application_workbench(&self) -> ResidentActorWorkbench<H, O> {
        ResidentActorWorkbench::new(
            Arc::clone(&self.access.machines),
            self.access.source.clone(),
            None,
            None,
            Vec::new(),
        )
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
        Self {
            access: ResidentMachineAccess::new(machines, source),
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
        self.with_host_machine(
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
        .await
    }

    async fn with_host_machine<T: Send + 'static>(
        &self,
        session_id: tidepool_repr::SessionId,
        max_wait: Option<Duration>,
        operation: impl FnOnce(
                &mut ResidentSession<H, O>,
                &ActorWorkbenchSource,
            ) -> Result<T, ResidentActorWorkbenchError>
            + Send
            + 'static,
    ) -> Result<T, ResidentActorWorkbenchError> {
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
        tracing::debug!(session = ?session_id,
            waited_ms = admission_started.elapsed().as_millis(),
            "resident machine checkout admitted");
        let (mut session, receipt) = checkout.into_parts();
        let source = self.source.clone();
        let machines = Arc::clone(&self.machines);

        let task = tokio::task::spawn_blocking(move || {
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
                        return outcome;
                    }

                    let holes = session
                        .parked_holes()
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                    machines.settle_suspended(receipt, session, holes);
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
                    std::panic::resume_unwind(payload);
                }
            }
        })
        .await;

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
        self.access
            .with_machine(context, move |session, context, _| {
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
                let step = begin_fragment(
                    session,
                    &compile_context,
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
                        let _ = session.abort(hole.cont_id(), "tool publication rejected".into());
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
                        let _ = session.abort(
                            hole.cont_id(),
                            "tool installer must finish after publication".into(),
                        );
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
                        let _ =
                            session.abort(hole.cont_id(), "tool invocation input rejected".into());
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
                        let _ =
                            session.abort(hole.cont_id(), "after-tool slot input rejected".into());
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
                    let _ = session.abort(&abandoned, reason.clone());
                    aborted += 1;
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
        self.access
            .with_machine(context, move |session, context, source| {
                mount_text_binding(session, context, source, &[], &binding, &result).map_err(
                    |error| {
                        ResidentActorWorkbenchError::InputMount(format!(
                            "the whole result could not be bound as {binding}: {error}"
                        ))
                    },
                )
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
    ) -> Result<(String, String), ResidentActorWorkbenchError> {
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
                        let _ = session
                            .abort(hole.cont_id(), "pure activation preview suspended".into());
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
        let reply = reply_declaration.unwrap_or_else(|| {
            format!("{reply_type} (no declaration captured at the request site)")
        });
        Ok((
            input,
            bounded_activation_text(reply, 4 * 1024, false, &format!("lookup {reply_type}")),
        ))
    }

    pub(crate) async fn prepare_cell(
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
                    .map(|input| mount_json_input(session, context, &source, &type_modules, input))
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
                    session.retire_host_binding_owner(&input.binder);
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
                let answer = crate::lookup::execute(
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
        self.access
            .with_machine(context, move |session, context, _| {
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
                begin_ready_block(
                    session,
                    context,
                    &turn_source,
                    RequestWorkbenchScope {
                        response: response.as_ref(),
                        request,
                        type_modules: &type_modules,
                    },
                    block,
                    *ready,
                    display_budget,
                )
            })
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

    pub(crate) async fn bind_command_job(
        &self,
        context: crate::ActorSessionContext,
        job: String,
    ) -> Result<String, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, source| {
                let scope = context.placement.lexical_scope;
                if let Some(binding) = session.host_text_binding_in(scope, &job) {
                    return Ok(binding);
                }
                let names: Vec<_> = session
                    .workbench_bindings_in(scope)
                    .into_iter()
                    .map(|binding| binding.name)
                    .collect();
                let mut index = session.val_gen().0;
                let binding = loop {
                    let name = format!("job{index}");
                    if !names.contains(&name) {
                        break name;
                    }
                    index += 1;
                };
                mount_command_job(session, context, source, &binding, &job).map_err(|error| {
                    ResidentActorWorkbenchError::InputMount(format!(
                        "command {job} remains owned, but its automatic binding failed: {error}"
                    ))
                })?;
                Ok(binding)
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
        let inspected = tokio::task::spawn_blocking(move || {
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
            let names = bound
                .iter()
                .map(|binder| binder.name.clone())
                .collect::<Vec<_>>();
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
            let display = if let Some((name, presentation, _)) = observation {
                WorkbenchDisplay::Observation {
                    name,
                    budget: display_budget,
                    presentation,
                    source: source.clone(),
                    type_modules: scope.type_modules.to_vec(),
                }
            } else if names.is_empty() {
                WorkbenchDisplay::Opaque
            } else {
                WorkbenchDisplay::Binding(names)
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
        TurnResult::Expr { .. } => Err(ResidentActorWorkbenchError::ActorProtocol(
            "workbench expression compiled without its observation binding".into(),
        )),
    }
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
            let mut installed_bindings = match &fragment.display {
                WorkbenchDisplay::Binding(names) => names.clone(),
                WorkbenchDisplay::Observation { name, .. } => vec![name.clone()],
                WorkbenchDisplay::Opaque | WorkbenchDisplay::Tool => Vec::new(),
            };
            installed_bindings.append(&mut fragment.recovered_jobs);
            let receipt = match fragment.display {
                WorkbenchDisplay::Binding(names) => format!("[bound {}]", names.join(", ")),
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
                        session, context, &source, &type_modules,
                        &name, budget.saturating_sub(fragment.output.iter().map(|text| text.chars().count()).sum::<usize>()), &fragment.presented,
                        presentation,
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
                WorkbenchDisplay::Binding(names) => Some(names.join(", ")),
                WorkbenchDisplay::Opaque
                | WorkbenchDisplay::Tool
                | WorkbenchDisplay::Observation { .. } => None,
            };
            let receipt = projected_binding_receipt(bound_name.as_deref(), &output)?;
            let installed_bindings = match fragment.display {
                WorkbenchDisplay::Binding(names) => names,
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
                fragment,
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
    use super::bounded_activation_text;

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
                        },
                    )
                    .map(ResidentActorBoundary::Start)
                    .map_err(ResidentActorWorkbenchError::StartCapture),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksStartWith(
                        label,
                        _,
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
                                    None => crate::ForkWorkspaceSeed::BoundHead(bound_dirty_policy),
                                }),
                                effect_keys: Some(effect_keys), fork_effort: effort, fork_budget: budget, model, instructions, context: fork_context, lifetime,
                                session_id: context.placement.session, parent_actor: context.actor,
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
                    ResidentRequest::Replies(RepliesReq::AttemptReplyWith(request_id, _)) => {
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
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::ReplyWith(request_id, _)) => {
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
            SourceEvent::Progress(_) | SourceEvent::ProgressClosed => matches!(
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
            .with_host_machine(session_id, None, move |session, _| {
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
            .with_host_machine(placement.session, max_wait.into(), move |session, _| {
                let _ = session.retire_placement(placement.resource_scope, placement.lexical_scope);
                Ok(())
            })
            .await
    }

    pub(crate) async fn provision_root_scope(
        &self,
        session_id: tidepool_repr::SessionId,
    ) -> Result<crate::ActorPlacement, ResidentActorWorkbenchError> {
        self.access
            .with_host_machine(session_id, None, move |session, _| {
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

fn visit_introspection_scope(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    scope: &tidepool_runtime::session::NameScope,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::NameScope;
    match scope {
        NameScope::Current => visit_core(table, visitor, "CurrentScope", |_| Ok(())),
        NameScope::PublicModule(module) => visit_core(table, visitor, "PublicModule", |visitor| {
            module.visit(table, visitor)
        }),
    }
}

fn visit_introspection_query(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    query: &tidepool_runtime::session::NameQuery,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::NameNamespace;
    visit_core(table, visitor, "NameQuery", |visitor| {
        visit_introspection_scope(table, visitor, &query.scope)?;
        visit_core(
            table,
            visitor,
            match query.namespace {
                NameNamespace::Any => "AnyName",
                NameNamespace::Value => "ValueName",
                NameNamespace::Type => "TypeName",
                NameNamespace::Constructor => "ConstructorName",
            },
            |_| Ok(()),
        )?;
        query.name.visit(table, visitor)
    })
}

fn visit_introspection_identifier(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    identifier: &tidepool_runtime::session::IdentifierRef,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::IdentifierNamespace;
    visit_core(table, visitor, "IdentifierRef", |visitor| {
        identifier.module.visit(table, visitor)?;
        identifier.name.visit(table, visitor)?;
        visit_core(
            table,
            visitor,
            match identifier.namespace {
                IdentifierNamespace::Value => "ValueIdentifier",
                IdentifierNamespace::Type => "TypeIdentifier",
                IdentifierNamespace::Constructor => "ConstructorIdentifier",
                IdentifierNamespace::Field => "FieldIdentifier",
            },
            |_| Ok(()),
        )
    })
}

fn visit_introspection_type_expression(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    ty: &tidepool_runtime::session::TypeExpression,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "TypeExpression", |visitor| {
        ty.canonical.visit(table, visitor)?;
        ty.variables.visit(table, visitor)?;
        ty.constraints.visit(table, visitor)
    })
}

fn visit_introspection_provenance(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    provenance: &tidepool_runtime::session::ScopeProvenance,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "ScopeProvenance", |visitor| {
        visit_introspection_scope(table, visitor, &provenance.scope)?;
        actor_haskell_int(provenance.generation, "scope generation")?.visit(table, visitor)?;
        provenance.fingerprint.visit(table, visitor)
    })
}

fn visit_introspection_field(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    field: &tidepool_runtime::session::FieldInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "FieldInfo", |visitor| {
        field.name.visit(table, visitor)?;
        visit_introspection_type_expression(table, visitor, &field.ty)
    })
}

fn visit_structural_list<T>(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    values: &[T],
    mut visit_value: impl FnMut(&T, &mut dyn HaskellVisitor) -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    let nil = tidepool_bridge::get_qualified(table, "GHC.Types.[]", 0)
        .ok_or_else(|| BridgeError::UnknownDataConName("[]".into()))?;
    let cons = tidepool_bridge::get_qualified(table, "GHC.Types.:", 2)
        .ok_or_else(|| BridgeError::UnknownDataConName(":".into()))?;
    for value in values {
        visitor.begin_constructor(cons, 2)?;
        visit_value(value, visitor)?;
    }
    visitor.begin_constructor(nil, 0)?;
    visitor.end_constructor()?;
    for _ in values {
        visitor.end_constructor()?;
    }
    Ok(())
}

fn visit_introspection_constructor(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    constructor: &tidepool_runtime::session::ConstructorInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "ConstructorInfo", |visitor| {
        visit_introspection_identifier(table, visitor, &constructor.identifier)?;
        visit_introspection_type_expression(table, visitor, &constructor.ty)?;
        visit_structural_list(table, visitor, &constructor.arguments, |ty, visitor| {
            visit_introspection_type_expression(table, visitor, ty)
        })?;
        visit_structural_list(table, visitor, &constructor.fields, |field, visitor| {
            visit_introspection_field(table, visitor, field)
        })
    })
}

fn visit_introspection_declaration(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    declaration: &tidepool_runtime::session::DeclarationInfo,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::DeclarationInfo;
    match declaration {
        DeclarationInfo::Value(ty) => visit_core(table, visitor, "ValueDeclaration", |visitor| {
            visit_introspection_type_expression(table, visitor, ty)
        }),
        DeclarationInfo::Data {
            parameters,
            constructors,
        } => visit_core(table, visitor, "DataDeclaration", |visitor| {
            parameters.visit(table, visitor)?;
            visit_structural_list(table, visitor, constructors, |constructor, visitor| {
                visit_introspection_constructor(table, visitor, constructor)
            })
        }),
        DeclarationInfo::Newtype {
            parameters,
            constructor,
        } => visit_core(table, visitor, "NewtypeDeclaration", |visitor| {
            parameters.visit(table, visitor)?;
            visit_introspection_constructor(table, visitor, constructor)
        }),
        DeclarationInfo::TypeSynonym { parameters, body } => {
            visit_core(table, visitor, "TypeSynonymDeclaration", |visitor| {
                parameters.visit(table, visitor)?;
                visit_introspection_type_expression(table, visitor, body)
            })
        }
        DeclarationInfo::Class {
            parameters,
            superclasses,
            methods,
        } => visit_core(table, visitor, "ClassDeclaration", |visitor| {
            parameters.visit(table, visitor)?;
            visit_structural_list(table, visitor, superclasses, |ty, visitor| {
                visit_introspection_type_expression(table, visitor, ty)
            })?;
            visit_structural_list(table, visitor, methods, |method, visitor| {
                visit_core(table, visitor, "ClassMethodInfo", |visitor| {
                    visit_introspection_identifier(table, visitor, &method.identifier)?;
                    visit_introspection_type_expression(table, visitor, &method.ty)
                })
            })
        }),
        DeclarationInfo::Constructor {
            parent,
            constructor,
        } => visit_core(table, visitor, "ConstructorDeclaration", |visitor| {
            visit_introspection_identifier(table, visitor, parent)?;
            visit_introspection_constructor(table, visitor, constructor)
        }),
        DeclarationInfo::RecordSelector { parent, ty } => {
            visit_core(table, visitor, "RecordSelectorDeclaration", |visitor| {
                visit_introspection_identifier(table, visitor, parent)?;
                visit_introspection_type_expression(table, visitor, ty)
            })
        }
    }
}

fn visit_optional_identifier(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    identifier: Option<&tidepool_runtime::session::IdentifierRef>,
) -> Result<(), BridgeError> {
    match identifier {
        Some(identifier) => visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
            visit_introspection_identifier(table, visitor, identifier)
        }),
        None => visit_named(table, visitor, "GHC.Maybe", "Nothing", |_| Ok(())),
    }
}

fn visit_introspection_info(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    info: &tidepool_runtime::session::IdentifierInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "IdentifierInfo", |visitor| {
        visit_introspection_identifier(table, visitor, &info.identifier)?;
        visit_introspection_declaration(table, visitor, &info.declaration)?;
        visit_optional_identifier(table, visitor, info.parent.as_ref())?;
        visit_introspection_provenance(table, visitor, &info.provenance)
    })
}

fn visit_introspection_type(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    info: &tidepool_runtime::session::TypeInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "TypeInfo", |visitor| {
        visit_introspection_identifier(table, visitor, &info.identifier)?;
        visit_introspection_type_expression(table, visitor, &info.expression)?;
        visit_introspection_provenance(table, visitor, &info.provenance)
    })
}

fn visit_introspection_query_error(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    error: &tidepool_runtime::session::QueryError,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::QueryError;
    match error {
        QueryError::Unknown(query) => visit_core(table, visitor, "UnknownIdentifier", |visitor| {
            visit_introspection_query(table, visitor, query)
        }),
        QueryError::Ambiguous(query, candidates) => {
            visit_core(table, visitor, "AmbiguousIdentifier", |visitor| {
                visit_introspection_query(table, visitor, query)?;
                visit_structural_list(table, visitor, candidates, |candidate, visitor| {
                    visit_introspection_identifier(table, visitor, candidate)
                })
            })
        }
        QueryError::UnknownModule(module) => {
            visit_core(table, visitor, "UnknownModule", |visitor| {
                module.visit(table, visitor)
            })
        }
        QueryError::Unsupported(detail) => {
            visit_core(table, visitor, "UnsupportedDeclaration", |visitor| {
                detail.visit(table, visitor)
            })
        }
    }
}

enum StructuredIntrospectionAnswer {
    ScopeChanged {
        before: tidepool_runtime::session::ScopeProvenance,
        after: tidepool_runtime::session::ScopeProvenance,
    },
    Info(tidepool_runtime::session::IdentifierInfo),
    Type(tidepool_runtime::session::TypeInfo),
    QueryError(tidepool_runtime::session::QueryError),
    CompilerUnavailable(String),
}

impl tidepool_bridge::sealed::ToHaskellSealed for StructuredIntrospectionAnswer {}
impl ToHaskell for StructuredIntrospectionAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let right = matches!(self, Self::Info(_) | Self::Type(_));
        visit_named(
            table,
            visitor,
            "Data.Either",
            if right { "Right" } else { "Left" },
            |visitor| match self {
                Self::ScopeChanged { before, after } => {
                    visit_core(table, visitor, "ScopeChanged", |visitor| {
                        visit_introspection_provenance(table, visitor, before)?;
                        visit_introspection_provenance(table, visitor, after)
                    })
                }
                Self::Info(info) => visit_introspection_info(table, visitor, info),
                Self::Type(info) => visit_introspection_type(table, visitor, info),
                Self::QueryError(error) => visit_introspection_query_error(table, visitor, error),
                Self::CompilerUnavailable(detail) => {
                    visit_core(table, visitor, "CompilerUnavailable", |visitor| {
                        detail.visit(table, visitor)
                    })
                }
            },
        )
    }
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
    let view = actor_compile_view(session, context, source, type_modules)?
        .with_workbench_imports(&imports);
    let generation = view.next_value_generation();
    let prepared = source.prepare(&view);
    let templates = resident_workbench_templates(
        &prepared.preamble,
        &context.haskell_effects_alias,
        &prepared.imports,
    );
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
    let retained = session.prepared_retained();
    let result = run_turn(TurnRequest {
        turn_text: &turn,
        templates: &templates,
        include: &include,
        session_root: view.session_root(),
        inject_modules: &prepared.injected,
        gen: generation.0,
        verdict: Some(generated_binds_verdict(&expected_binders)),
        target: None,
        retained_imports: &retained,
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
    Ok((bound.remove(0), compiled, generation))
}

struct MountedHostInput {
    binder: BoundBinder,
    module: String,
    name: String,
}

fn mount_json_input<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    input: &serde_json::Value,
) -> Result<MountedHostInput, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let binding = fresh_host_binding_name(session, context.placement.lexical_scope, "Input");
    let (binder, compiled, generation) = compile_host_binding(
        session,
        context,
        source,
        type_modules,
        &binding,
        "TidepoolHostJson.Value",
        "object [\"anchor\" .= toJSON [TidepoolHostJson.String \"\", TidepoolHostJson.Number (TidepoolHostJson.scientific 0 0), TidepoolHostJson.Bool True, TidepoolHostJson.Null]]",
        SourceImports::from_specs([
            "qualified Tidepool.Aeson as TidepoolHostJson",
            "Tidepool.Aeson (object, (.=), toJSON)",
        ]),
        false,
    )?;
    session
        .mount_json_binding_in(
            context.placement.lexical_scope,
            &binder,
            generation,
            compiled.into_code(),
            input,
        )
        .map_err(ResidentActorWorkbenchError::Resident)?;
    if let Err(error) = session.hide_host_binding_in(context.placement.lexical_scope, &binder) {
        session.retire_host_binding_owner(&binder);
        return Err(ResidentActorWorkbenchError::Resident(error));
    }
    Ok(MountedHostInput {
        module: binder.module.clone(),
        name: binder.name.clone(),
        binder,
    })
}

fn mount_text_binding<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    binding: &str,
    text: &str,
) -> Result<(), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let (binder, compiled, generation) = compile_host_binding(
        session,
        context,
        source,
        type_modules,
        binding,
        "TidepoolHostText.Text",
        "case TidepoolHostExts.noinline (TidepoolHostText.pack \"\") of TidepoolHostTextInternal.Text bytes offset length -> TidepoolHostTextInternal.Text bytes offset length",
        SourceImports::from_specs([
            "qualified Data.Text as TidepoolHostText",
            "qualified Data.Text.Internal as TidepoolHostTextInternal",
            "qualified GHC.Exts as TidepoolHostExts",
            "qualified Tidepool.Aeson as TidepoolHostJson",
        ]),
        true,
    )?;
    session
        .mount_text_binding_in(
            context.placement.lexical_scope,
            &binder,
            generation,
            compiled.into_code(),
            text,
        )
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
    (0_u64..)
        .map(|ordinal| format!("__tidepool{category}{generation}_{ordinal}"))
        .find(|candidate| !used.contains(candidate))
        .expect("unbounded internal host binding namespace")
}

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
        "TidepoolHostJob.Job",
        "TidepoolHostJob.Job (case TidepoolHostExts.noinline (TidepoolHostText.pack \"\") of TidepoolHostTextInternal.Text bytes offset length -> TidepoolHostTextInternal.Text bytes offset length)",
        SourceImports::from_specs([
            "qualified Tidepool.Command.Types as TidepoolHostJob",
            "qualified Data.Text as TidepoolHostText",
            "qualified Data.Text.Internal as TidepoolHostTextInternal",
            "qualified GHC.Exts as TidepoolHostExts",
            "qualified Tidepool.Aeson as TidepoolHostJson",
        ]),
        true,
    )?;
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
        session.retire_host_binding_owner(&binder);
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
    let include_refs: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
    let mut verdict = match checked_verdict {
        Some(checked) => Some(checked.clone()),
        None => tidepool_runtime::session::classify_block(&[&block.source])
            .map_err(|error| ResidentActorWorkbenchError::CompileInfrastructure(error.to_string()))?
            .into_iter()
            .next(),
    };
    let observation = if verdict
        .as_ref()
        .is_some_and(|verdict| verdict.kind == TurnKind::Expr)
    {
        let mut name = format!("observation{}", compile_view.next_value_generation().0);
        let visible = session.workbench_bindings_in(context.placement.lexical_scope);
        while visible.iter().any(|binding| binding.name == name)
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
    // On the prepared route the same compile also projects the turn's program,
    // linked against every live prepared binding; on Core this is `None`.
    let retained = session.prepared_retained();
    let request = TurnRequest {
        turn_text: &block.source,
        templates: &templates,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &prepared.injected,
        gen: compile_view.next_value_generation().0,
        verdict,
        target: None,
        retained_imports: &retained,
    };
    // A failed worker may already have published a thin value interface.
    // Its identity is never reused, whether compilation or execution succeeds.
    if request
        .verdict
        .as_ref()
        .is_none_or(|verdict| verdict.kind != TurnKind::Decl)
    {
        session.reserve_value_generations_through(compile_view.next_value_generation());
    }
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
        ) => StructuredIntrospectionAnswer::Info(*info),
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
        let (mut session, context, source, _session_root) = host_mount_fixture();
        let input = mount_json_input(
            &mut session,
            &context,
            &source,
            &[],
            &serde_json::json!({
                "nested": [true, null, {"long": "x".repeat(16 * 1024)}]
            }),
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
        session.retire_host_binding_owner(&input.binder);
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
}
