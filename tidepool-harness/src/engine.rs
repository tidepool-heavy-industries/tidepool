//! The turn engine — the conversation driver that turns a `ModelProvider`
//! into a resident-session turn loop, plus the hole classification and
//! transcript store that the golden path threads through.
//!
//! # The golden path this drives
//!
//! A node is forced → a resident session bootstraps → the turn engine drives
//! the calling model: assemble a prompt (transcript prefix + system framing),
//! call the provider, extract the LAST fenced ```haskell block, compile + run
//! it as a resident turn. The turn either COMPLETES (the node is done) or
//! SUSPENDS at an `AskWith` — which the engine classifies from the request
//! payload:
//!
//! - `{typedSite, fork:true}` → `runLLMTurnFork`: PARK. The parent stays
//!   suspended; a child answerer node is registered (transcript forked at the
//!   checkpoint) and, once forced, drives its own turn loop to produce a typed
//!   answer that `run_child`s against the parent and resumes it. The parent's
//!   continuation takes `Either InvocationExit T` — a forked window is a
//!   BRANCH POSITION, and PRD 21 locked decision 6 folds its abnormal exit as
//!   data there rather than as an exception over its siblings (see
//!   [`InvocationExit`], [`build_child_answer_value`]).
//! - `{typedSite}` (no fork) → `runLLMTurn`: the SAME model answers in
//!   context by evaluating `resume expr :: T`.
//! - `AskUserWith shape` (own constructor, answerer-only) → `askUserRaw`:
//!   OPERATOR routing — the typed `FormShape` renders as a form; the
//!   operator's submission resumes the turn.
//!
//! Answer validation is GHC end-to-end: an ill-typed `resume expr` fails to
//! compile, and the compiler error is fed back verbatim as the retry prompt —
//! the continuation is never consumed by a bad attempt.
//!
//! # What this module does NOT own
//!
//! The event log, node tree, and registry lifecycle live in [`crate::forcing`]
//! / [`crate::registry`]; the engine calls them. The actual compile mechanics
//! (spawn `tidepool-extract`, read its output directory, deserialize,
//! diagnostics, the invocation-keyed memo) live in
//! `tidepool_runtime::artifacts` — this module's [`compile_turn`]/
//! [`compile_turns`] are thin wrappers mapping a
//! [`tidepool_runtime::CompiledArtifacts`] onto this crate's own turn/node
//! vocabulary ([`CompiledTurn`]) and attributing timing to a (node, round)
//! pair (architecture review finding 3, 2026-08-17 — this module absorbed
//! the former `tidepool_harness::compile`). The provider boundary is
//! [`crate::provider`]. The web protocol / SSE is `tidepool-web`. This
//! module is the glue that sequences them into a turn loop.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_extract_cmd::ResolvedExtractBin;
pub use tidepool_extract_cmd::{extract_spawn_count, reset_extract_spawn_count};
use tidepool_repr::{CoreExpr, DataConTable};
pub use tidepool_runtime::AsksSidecar;
use tidepool_runtime::{compile_targets, CompileError, CompiledArtifacts};

use crate::provider::{
    DynModelProvider, Message, ProviderError, ReasoningItem, Role, StreamSink, TurnRequest,
    TurnResponse, Usage,
};
use crate::timing;
use crate::tree::FanBadge;

/// A compiled turn: the Core expression, its constructor table, and the
/// typed-yield sidecar. A thin per-target mapping over
/// [`tidepool_runtime::CompiledArtifacts`] onto this crate's own turn
/// vocabulary — the actual compile mechanics live in
/// `tidepool_runtime::artifacts`, see this module's doc.
pub struct CompiledTurn {
    pub expr: CoreExpr,
    pub table: DataConTable,
    pub asks: AsksSidecar,
}

/// Compile `source` with entry binder `target`, searching `include` for
/// modules, into a [`CompiledTurn`]. `node`/`round` attribute this compile's
/// timing stages (pass [`timing::NO_NODE`]/[`timing::NO_ROUND`] when the
/// caller has no answerer-round context). A thin wrapper over
/// [`compile_turns`] (a one-element target slice).
pub fn compile_turn(
    extract_bin: &ResolvedExtractBin,
    source: &str,
    target: &str,
    include: &[PathBuf],
    node: u64,
    round: u64,
) -> Result<CompiledTurn, CompileError> {
    let mut turns = compile_turns(extract_bin, source, &[target], include, node, round)?;
    turns
        .remove(target)
        .ok_or_else(|| CompileError::MissingOutput(PathBuf::from(format!("{target}.cbor"))))
}

/// As [`compile_turn`], but compiles `targets` in ONE `tidepool-extract`
/// spawn against a SHARED merged `meta.cbor` / [`DataConTable`], returning
/// one [`CompiledTurn`] per target — [`tidepool_runtime::compile_targets`]
/// with this crate's timing attributed via `on_stage` and the shared table
/// distributed onto each per-target [`CompiledTurn`].
pub fn compile_turns(
    extract_bin: &ResolvedExtractBin,
    source: &str,
    targets: &[&str],
    include: &[PathBuf],
    node: u64,
    round: u64,
) -> Result<HashMap<String, CompiledTurn>, CompileError> {
    let CompiledArtifacts {
        table,
        targets: artifacts,
        ..
    } = compile_targets(
        source,
        targets,
        include,
        Some(extract_bin),
        |stage, elapsed, bytes| {
            timing::record_stage(node, round, stage, elapsed, bytes);
        },
    )?;
    Ok(artifacts
        .into_iter()
        .map(|(name, a)| {
            (
                name,
                CompiledTurn {
                    expr: a.expr,
                    table: table.clone(),
                    asks: a.asks,
                },
            )
        })
        .collect())
}

/// Which surface verb produced a [`HoleRouting::Fork`] suspension. Two
/// effects share that routing, and they DIFFER in the shape their parked
/// continuation expects back, so the answering side must know which it is:
///
/// - [`ForkSource::ForkEffect`] — `Tidepool.Fork`'s `fork`/`forkAll`
///   (`ForkWith`/`ForkAllWith`), which resume with a bare `T` / `[T]`.
/// - [`ForkSource::RunLLMTurn`] — `runLLMTurnFork`/`runLLMTurnFanout`
///   (`RunLLMTurnWith` carrying `fork: true`), which resume with
///   `Either InvocationExit T` / `[Either InvocationExit T]` (PRD 21 locked
///   decision 6 — see [`InvocationExit`]).
///
/// A plain `ty`/`fan` inspection cannot tell them apart (both record the
/// child's answer type the same way), which is exactly why this is carried
/// rather than re-derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkSource {
    ForkEffect,
    RunLLMTurn,
}

/// Which outer-row effect a [`HoleRouting::OuterEffect`] suspension names —
/// see that variant's doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OuterEffectKind {
    Console,
    Worktree,
    RepoEvent,
    Exec,
    Journal,
}

/// How a suspended request routes — decoded from its constructor name +
/// payload: `Ask`, `RunLLMTurn`, and `Finalize`
/// are each their own GADT/union-tag, see [`classify_hole`].
///
/// `PartialEq` only (not `Eq`): [`HoleRouting::AskUser`] carries a
/// [`crate::selfharness::operator::FormShape`], which derives `PartialEq` but
/// not `Eq` (the frozen `operator.rs` contract) — do not add `Eq` back there.
#[derive(Debug, Clone, PartialEq)]
pub enum HoleRouting {
    /// `runLLMTurn @T` — the same calling model answers in context.
    RunLLMTurn {
        site: crate::tree::SiteId,
        ty: Option<String>,
    },
    /// Park a suspension into a bounded fan-out with a join. Produced by two
    /// sources that share this routing: the `Fork` effect (`ForkWith` →
    /// `fan: None`, one child; `ForkAllWith` → `fan: Some(_)`, N children —
    /// `Tidepool.Fork`'s `fork`/`forkAll`), and the general Agent stack's
    /// `runLLMTurn` fork payload (`runLLMTurnFork`/`runLLMTurnFanout`). `ty` is
    /// the RENDERED answer type: the element type `T` for a plain fork, the
    /// LIST type `[T]` for a fanout (`engine::strip_list_type` recovers `T`).
    /// `prompts` carries the per-child prompt text, one per fanout child, in
    /// declaration order (empty for a plain fork). `source` says WHICH of the
    /// two verbs raised it, because they differ in the shape their parked
    /// continuation expects back — see [`ForkSource`].
    Fork {
        site: crate::tree::SiteId,
        ty: Option<String>,
        fan: Option<FanBadge>,
        prompts: Vec<String>,
        source: ForkSource,
    },
    /// `askUserRaw shape` — a typed form
    /// suspends to a HUMAN OPERATOR, routed by CONSTRUCTOR NAME
    /// (`AskUserWith`), not JSON-key probing. `shape` is the decoded
    /// [`crate::selfharness::operator::FormShape`] the operator gate renders.
    AskUser {
        shape: crate::selfharness::operator::FormShape,
    },
    /// `noteRaw text` (`Tidepool.Form.note`) — a NON-BLOCKING display
    /// channel riding the SAME `AskUser` GADT as `AskUserWith`, routed by
    /// CONSTRUCTOR NAME (`NoteWith`), not JSON-key probing. The driver
    /// services this by posting `text` to the operator's accumulating feed
    /// and resuming immediately with `()` — it never presents anything via
    /// `OperatorGate::present_form`, and does not count against
    /// `ASKUSER_MAX_REPROMPTS` (that budget is scoped to `askUser`
    /// re-presentations, a genuinely different failure mode).
    Note { text: String },

    /// A `getStateJson` suspension (`ReadStateWith`) — the answerer reading
    /// the loop's durable state. Serviced IMMEDIATELY by the driver with the
    /// cycle's entry state as JSON (note's service shape: no operator, no
    /// model round).
    ReadState,
    /// A `freezeContext` suspension (`RunLLMTurnFreezeWith`) — PRD 21 lane C3
    /// GAP 1: mint a `ContextRef` naming the CURRENT loop's per-loop answerer
    /// window's frozen prefix, right now. Serviced IMMEDIATELY (`ReadState`'s
    /// shape: no operator, no model round) by
    /// [`crate::selfharness::driver::SelfHarnessDriver`] freezing that node's
    /// transcript ([`crate::harness::Harness::freeze_snapshot`]) and
    /// constructing the `ContextRef` `Value` directly against the table
    /// ([`build_context_ref_value`]) — never round-tripped through JSON, since
    /// `RunLLMTurnFreezeWith`'s answer type is fixed, not model-chosen.
    FreezeContext,
    /// A `runLLMTurnBranch \@T ref prompt` suspension — the OTHER half of GAP
    /// 1: fork a FRESH child window off the frozen prefix `context_ref` names
    /// (never an empty root, PRD 21 locked decision 2), drive it to
    /// `finalize \@T`, and resume with `(T, ContextRef)` — the child's answer
    /// plus a ref to ITS OWN post-finalize frozen prefix, for branching
    /// further. Rides the SAME `RunLLMTurnWith` wire constructor as
    /// [`HoleRouting::RunLLMTurn`]/[`HoleRouting::Fork`] (a `branch`/`ref`
    /// payload flag, decoded by [`classify_runllmturn_payload`]) rather than a
    /// new GADT constructor — the same "one constructor, several payload
    /// shapes" discipline fork/fanout already use. `context_ref` is the RAW
    /// wire digest string — UNVALIDATED here; the driver's servicing is where
    /// it is resolved to a [`crate::harness::ContextRef`]
    /// (`Harness::resolve_context_ref`), the one typed checkpoint an
    /// unknown/stale ref is refused at (never a silent fresh-root fallback).
    /// `site`/`ty` mirror `Fork`'s shape — `ty` is the branch's OWN answer type
    /// `T`, not the wrapping pair. `label`, when the child was opened via
    /// `runLLMTurnBranchLabeled` rather than plain `runLLMTurnBranch`, is the
    /// caller-chosen Text stamped on the SAME payload (`label` key, decoded by
    /// [`classify_runllmturn_payload`]) — `None` for an ordinary unlabeled
    /// branch, byte-identical to today. The driver uses a present label to
    /// route this child's asks/notes to a per-node operator gate
    /// ([`crate::selfharness::operator::OperatorGate::node_gate`]) instead of
    /// the default one (PRD 21 C5 GUI lane).
    Branch {
        site: crate::tree::SiteId,
        ty: Option<String>,
        context_ref: String,
        label: Option<String>,
    },
    /// A `runLLMTurnBranchFanout \@T ref labeledPrompts` suspension — the BULK
    /// sibling of [`HoleRouting::Branch`] (operator decision: sibling branch
    /// windows are ALWAYS driven concurrently, transparently — scheduling is
    /// never a model-visible choice). Every `(label, prompt)` pair forks its
    /// OWN child window off the SAME frozen `context_ref` (never an empty
    /// root), driven CONCURRENTLY via the same machinery
    /// [`HoleRouting::Fork`]'s fanout servicing already uses
    /// ([`crate::selfharness::driver::SelfHarnessDriver::service_outer_branch_fanout`]
    /// — per-child realm, `set_concurrency_cap`, declaration-order
    /// reassembly). `ty` is the RENDERED LIST answer type `[T]` (mirroring
    /// [`HoleRouting::Fork`]'s fanout shape — `engine::strip_list_type`
    /// recovers the per-child element type `T`); `labels`/`prompts` are
    /// parallel, one entry per sibling, in declaration order. Each sibling
    /// window is a BRANCH POSITION exactly like [`HoleRouting::Branch`]'s
    /// (PRD 21 locked decision 6): its own abnormal exit folds as `Left` at
    /// its own position in the resumed list, never erasing a sibling's
    /// already-finished answer.
    BranchFanout {
        site: crate::tree::SiteId,
        ty: Option<String>,
        context_ref: String,
        labels: Vec<String>,
        prompts: Vec<String>,
    },
    /// A Subagent verb (`SubagentSpawn`/`SubagentBegin`/`SubagentResume`/
    /// `SubagentSpawnAsync`/`SubagentAwait`/`SubagentCancel` —
    /// `spawnAgentRaw`/`agentBeginRaw`/`agentResumeRaw`/`agentSpawnAsyncRaw`/
    /// `agentAwaitRaw`/`agentCancelRaw`) raised by the
    /// AUTHORED outer loop. Routed by CONSTRUCTOR NAME only; the payload is
    /// NEVER decoded here (its args are bridged ADTs, not JSON — the
    /// servicing site decodes the ORIGINAL request `Value` via the generated
    /// `SubagentReq: FromCore` and dispatches it to the driver-owned
    /// `SubagentHandler`, suspension-serviced because the outer row's
    /// handled prefix must stay EMPTY on the shared machine).
    Subagent,
    /// A Console/Worktree/RepoEvent/Exec/Journal verb (`say`/`createWorktree`/
    /// `lookupWorktree`/`listWorktrees`/`worktreeBranch`/`worktreeHead`/
    /// `withHandler`'s subscribe-drain-unsubscribe/`run`/`runIn`/`runArgv`/
    /// `record`) raised by the AUTHORED outer loop — routed by CONSTRUCTOR
    /// NAME, same discipline as [`HoleRouting::Subagent`]. One variant
    /// carrying WHICH effect rather than five near-identical ones: the
    /// servicing site decodes/dispatches identically for all five (decode
    /// the ORIGINAL request `Value` via the generated `<Eff>Req: FromCore`,
    /// dispatch into a driver-owned handler, resume with the response),
    /// differing only in which handler it reaches (and, for Console, an
    /// extra observer-feed post — see `SelfHarnessDriver::service_outer_effect`).
    OuterEffect(OuterEffectKind),
    /// `finalize @T x` — an Agent turn hands a
    /// typed value UP to the parent `runLLMTurn` hole and TERMINATES its own
    /// turn loop, rather than resuming in context like [`HoleRouting::RunLLMTurn`]
    /// does. Its own GADT/union-tag (`Finalize`/`FinalizeWith`), decoded by
    /// [`classify_hole`] from `FinalizeWith`'s own wire shape (`Con(_, [site
    /// :: Int, value])`) — NOT `AskWith`'s `typedSite` field. `site`/`ty`
    /// mirror `RunLLMTurn`'s shape (same asks.json sidecar lookup); the raw
    /// finalized VALUE is not carried here (it crosses in-heap, may be
    /// non-serializable) — the caller recovers it from the original
    /// suspended request `Value`.
    Finalize {
        site: crate::tree::SiteId,
        ty: Option<String>,
    },
    /// An `Async*With` verb (`AsyncSpawnWith`/`AsyncDoneWith`/
    /// `AsyncJoinAnyWith`/`AsyncStatusWith`/`AsyncResultWith`/
    /// `AsyncCancelWith` — `Tidepool.Async`'s substrate, PRD 20 S1-L4) raised
    /// by the AUTHORED outer loop or by a green thread's own body. Routed by
    /// CONSTRUCTOR NAME only, same discipline as [`HoleRouting::Subagent`];
    /// the payload is NEVER decoded here — field 1 of `AsyncSpawnWith`/
    /// `AsyncDoneWith` may carry a live closure (the thread body / a
    /// closure-valued result), so decoding happens at the servicing site
    /// (`SelfHarnessDriver::service_green_hole`), which also owns the
    /// driver's thread table and ready queue. Unlike `Subagent`/
    /// `OuterEffect`, servicing a `Green` suspension is not a single
    /// dispatch-then-resume: `AsyncSpawnWith` starts a NEW suspension-capable
    /// top-level run and `AsyncJoinAnyWith` may park the caller rather than
    /// answer immediately.
    Green,
    /// A plain `ask schema prompt` (structured operator elicitation) or an
    /// unrecognized payload — operator routing with the raw payload attached.
    Ask { payload: Json },
    /// `takeDelegatedBranches path` (PRD 21 C5's final wiring) —
    /// `Harness.hs`'s `foldAt`, and ONLY `foldAt` (this decl is absent from
    /// `answerer_decls`/`answerer_decls_with_delegate`, so no model window
    /// can ever name it), reading back this node's own runtime-stamped
    /// delegation branch(es) — see the module doc on
    /// [`crate::selfharness::driver::SelfHarnessDriver`]'s
    /// `branch_node_paths`/`delegated_branches` for where they are
    /// recorded. `path` is the node's rendered `NodePath` text (the same
    /// text `coalgebraPrompt` embeds as `"NODE {path} — DISCOVER"` — see
    /// [`parse_companion_node_path`]). Serviced IMMEDIATELY (`ReadState`'s
    /// shape: no operator, no model round); the driver's own record for
    /// `path` is CONSUMED on read.
    DelegatedBranches { path: String },
}

/// A classified suspension: the routing plus the human-facing prompt text.
#[derive(Debug, Clone)]
pub struct ClassifiedHole {
    pub routing: HoleRouting,
    pub prompt: String,
}

/// A malformed decode of a continuation-ROUTING field (a site id, a fan
/// count, a fan/prompt-cardinality pair) inside [`classify_hole`]. These
/// fields select which suspended typed continuation a reply resumes — a
/// plausible default (site 0, a truncated prompt list) can resume the WRONG
/// site rather than surface the corruption, so every routing field is
/// validated rather than defaulted. Display/prompt-text fields (a fork's
/// `brief`, `AskWith`'s payload) are unaffected — only fields that pick a
/// continuation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ClassifyError {
    #[error("{constructor}: missing or non-numeric `{field}`")]
    MissingField {
        constructor: &'static str,
        field: &'static str,
    },
    #[error("{constructor}: `{field}` {value} does not fit in a u32")]
    OutOfRange {
        constructor: &'static str,
        field: &'static str,
        value: u64,
    },
    #[error(
        "{constructor}: fan declares {declared} prompt(s) but the wire carries {actual} — \
         refusing to silently drop the difference"
    )]
    FanMismatch {
        constructor: &'static str,
        declared: usize,
        actual: usize,
    },
    #[error("{constructor}: request is not a constructor application")]
    Malformed { constructor: &'static str },
}

/// Pull a JSON payload's `field` as a [`crate::tree::SiteId`] — `Err` on
/// missing/non-numeric or a value that doesn't fit (see [`ClassifyError`]'s
/// doc). The one mint point for a `SiteId` decoded from a JSON payload key.
fn require_site_field(
    payload: &Json,
    constructor: &'static str,
    field: &'static str,
) -> Result<crate::tree::SiteId, ClassifyError> {
    let raw = payload
        .get(field)
        .and_then(Json::as_u64)
        .ok_or(ClassifyError::MissingField { constructor, field })?;
    crate::tree::SiteId::try_from(raw).map_err(|_| ClassifyError::OutOfRange {
        constructor,
        field,
        value: raw,
    })
}

/// Pull a `Con`'s positional field `idx`, decoded to JSON, as a
/// [`crate::tree::SiteId`] — the [`Value::Con`] counterpart to
/// [`require_site_field`] for constructors whose site is a positional field
/// rather than a JSON payload key.
fn require_con_site(
    fields: &[Value],
    idx: usize,
    table: &DataConTable,
    constructor: &'static str,
    field: &'static str,
) -> Result<crate::tree::SiteId, ClassifyError> {
    let raw = fields
        .get(idx)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_u64())
        .ok_or(ClassifyError::MissingField { constructor, field })?;
    crate::tree::SiteId::try_from(raw).map_err(|_| ClassifyError::OutOfRange {
        constructor,
        field,
        value: raw,
    })
}

/// Decode a suspended request `Value` into a [`ClassifiedHole`] — the ONE
/// shared classify path `Ask`, `RunLLMTurn`, and `Finalize` all go through.
/// Each is its own GADT/union-tag, so this
/// dispatches on the request Con's CONSTRUCTOR NAME first, then decodes that
/// constructor's own wire shape:
///
/// - `RunLLMTurnWith` (prompt, payload) — the `typedSite`/`fork`/`branchFanout`/
///   `branch`/`fan`/`prompts` payload shape carried on the `RunLLMTurn`
///   constructor: `fork` → [`HoleRouting::Fork`] (the general Agent stack's
///   `runLLMTurnFork`/`runLLMTurnFanout`); else `branchFanout` →
///   [`HoleRouting::BranchFanout`] (the bulk `runLLMTurnBranchFanout`, one
///   parent `ref` plus parallel `labels`/`prompts` lists); else `branch` →
///   [`HoleRouting::Branch`]; else [`HoleRouting::RunLLMTurn`]. `asks`
///   resolves `typedSite` to its rendered answer type.
/// - `ForkWith` (site, brief) / `ForkAllWith` (site, prompts) — the `Fork`
///   effect (`Tidepool.Fork`'s `fork`/`forkAll`), routed by CONSTRUCTOR NAME
///   to [`HoleRouting::Fork`] (`fan: None` for one child, `fan: Some(_)` for a
///   batch). The site id is a constructor field, not a payload key; `asks`
///   resolves it to the rendered answer type the same way.
/// - `FinalizeWith` (site, value) — [`HoleRouting::Finalize`]. The VALUE field
///   is never JSON-decoded here (it crosses in-heap, may be non-serializable —
///   e.g. a closure); only the leading `Int` site id is. `asks` resolves the
///   site the same way as `RunLLMTurn`. The raw value itself is recovered from
///   the original request `Value` by the caller (`Harness` retains it), not
///   through this JSON-shaped `ClassifiedHole`.
/// - `AskUserWith` (shape) — a real constructor arm, routed by CONSTRUCTOR
///   NAME (no JSON-key probing): the request's sole field decodes as a
///   [`crate::selfharness::operator::FormShape`] → [`HoleRouting::AskUser`].
///   A malformed shape (the decode fails) falls through to the plain-Ask
///   fallback below instead of hanging, so a bad payload surfaces loudly at
///   the driver.
/// - `NoteWith` (text) — a real constructor arm on the SAME `AskUser` GADT,
///   routed by CONSTRUCTOR NAME → [`HoleRouting::Note`]. The sole field is a
///   bare `Text`, decoded directly (no shape/schema involved).
/// - `TakeDelegatedBranchesWith` (path) — PRD 21 C5's read-back verb, routed
///   by CONSTRUCTOR NAME → [`HoleRouting::DelegatedBranches`]. The sole
///   field is a bare `Text` (a rendered `NodePath`), decoded the same way
///   `NoteWith`'s is. Absent from every model-facing row — see that
///   variant's doc.
/// - `Print` (Console) / `WorktreeCreate`/`WorktreeLookup`/`WorktreeList`/
///   `WorktreeBranchOf`/`WorktreeHeadOf` (Worktree) / `RepoEventSubscribe`/
///   `RepoEventDrain`/`RepoEventAwait`/`RepoEventUnsubscribe` (RepoEvent) /
///   `Run`/`RunIn`/`RunArgv` (Exec) / `RecordStep` (Journal) — routed by
///   CONSTRUCTOR NAME to [`HoleRouting::OuterEffect`], same discipline as
///   `Subagent` below.
/// - `AsyncSpawnWith`/`AsyncDoneWith`/`AsyncJoinAnyWith`/`AsyncStatusWith`/
///   `AsyncResultWith`/`AsyncCancelWith` (`Tidepool.Async`'s substrate) —
///   routed by CONSTRUCTOR NAME to [`HoleRouting::Green`], same discipline as
///   `Subagent` above.
/// - `AskWith` (prompt, payload) — plain [`HoleRouting::Ask`] (a structured
///   `ask schema prompt`).
/// - anything else (an unrecognized Con) — treated as a bare Ask with an empty
///   prompt/`Null` payload, same fallback `decode_askwith` always had.
///
/// The routing-selecting fields above (`typedSite`, a fork's site, a fan's
/// declared count against its prompt list) are VALIDATED, not defaulted —
/// see [`ClassifyError`]'s doc. A malformed value there stops the turn with a
/// diagnostic instead of silently resuming/finalizing a different typed
/// continuation.
pub fn classify_hole(
    request: &Value,
    table: &DataConTable,
    asks: &AsksSidecar,
) -> Result<ClassifiedHole, ClassifyError> {
    let hole = match con_name(request, table) {
        Some("RunLLMTurnWith") => {
            let (prompt, payload) = decode_prompt_payload(request, table);
            ClassifiedHole {
                routing: classify_runllmturn_payload(&payload, asks)?,
                prompt,
            }
        }
        Some("FinalizeWith") => {
            let (site, ty) = decode_finalize_site(request, table, asks)?;
            ClassifiedHole {
                routing: HoleRouting::Finalize { site, ty },
                prompt: String::new(),
            }
        }
        Some("ForkWith") => {
            let (site, brief) = decode_fork_one(request, table)?;
            ClassifiedHole {
                routing: HoleRouting::Fork {
                    site,
                    ty: asks.type_of(site.get()).map(str::to_string),
                    fan: None,
                    prompts: Vec::new(),
                    source: ForkSource::ForkEffect,
                },
                prompt: brief,
            }
        }
        Some("ForkAllWith") => {
            let (site, prompts) = decode_fork_all(request, table)?;
            ClassifiedHole {
                prompt: prompts.join("\n"),
                routing: HoleRouting::Fork {
                    site,
                    ty: asks.type_of(site.get()).map(str::to_string),
                    fan: Some(FanBadge::Exact {
                        n: prompts.len() as u32,
                    }),
                    prompts,
                    source: ForkSource::ForkEffect,
                },
            }
        }
        Some("AskUserWith") => match decode_askuser_spec(request, table) {
            Some(shape) => ClassifiedHole {
                routing: HoleRouting::AskUser { shape },
                prompt: String::new(),
            },
            None => {
                let (prompt, payload) = decode_askwith(request, table);
                ClassifiedHole {
                    routing: HoleRouting::Ask { payload },
                    prompt,
                }
            }
        },
        Some("ReadStateWith") => ClassifiedHole {
            routing: HoleRouting::ReadState,
            prompt: String::new(),
        },
        Some("RunLLMTurnFreezeWith") => ClassifiedHole {
            routing: HoleRouting::FreezeContext,
            prompt: String::new(),
        },
        Some("SubagentSpawn")
        | Some("SubagentBegin")
        | Some("SubagentResume")
        | Some("SubagentSpawnAsync")
        | Some("SubagentAwait")
        | Some("SubagentCancel") => ClassifiedHole {
            routing: HoleRouting::Subagent,
            prompt: String::new(),
        },
        Some("Print") => ClassifiedHole {
            routing: HoleRouting::OuterEffect(OuterEffectKind::Console),
            prompt: String::new(),
        },
        Some("WorktreeCreate")
        | Some("WorktreeLookup")
        | Some("WorktreeList")
        | Some("WorktreeBranchOf")
        | Some("WorktreeHeadOf") => ClassifiedHole {
            routing: HoleRouting::OuterEffect(OuterEffectKind::Worktree),
            prompt: String::new(),
        },
        Some("RepoEventSubscribe")
        | Some("RepoEventDrain")
        | Some("RepoEventAwait")
        | Some("RepoEventUnsubscribe")
        // Capability mailboxes (PRD 20 S1-L4 wave 2) ride the SAME
        // `RepoEvent` effect as the watch/subscribe verbs above (a mailbox
        // IS an event source) — see `tidepool-mcp/src/effect_defs.rs`'s
        // `event_effect_def!` and `RepoEventHandler`'s `mailbox_new`/
        // `mailbox_send`/`mailbox_drop`. Without these three arms a
        // `MailboxNew`/`MailboxSend`/`MailboxDrop` suspension falls through
        // to the wildcard below and is misclassified as `HoleRouting::Ask`
        // — which the outer session (no `Ask` in its row) then hard-fails
        // as an unserviceable hole.
        | Some("MailboxNew")
        | Some("MailboxSend")
        | Some("MailboxDrop") => ClassifiedHole {
            routing: HoleRouting::OuterEffect(OuterEffectKind::RepoEvent),
            prompt: String::new(),
        },
        Some("Run") | Some("RunIn") | Some("RunArgv") => ClassifiedHole {
            routing: HoleRouting::OuterEffect(OuterEffectKind::Exec),
            prompt: String::new(),
        },
        Some("RecordStep") => ClassifiedHole {
            routing: HoleRouting::OuterEffect(OuterEffectKind::Journal),
            prompt: String::new(),
        },
        Some("NoteWith") => {
            let text = decode_note_text(request, table);
            ClassifiedHole {
                routing: HoleRouting::Note { text },
                prompt: String::new(),
            }
        }
        // PRD 21 C5's read-back verb — see [`HoleRouting::DelegatedBranches`].
        // The sole field is a bare `Text` (the rendered `NodePath`), decoded
        // the same way `NoteWith`'s bare `Text` field is.
        Some("TakeDelegatedBranchesWith") => {
            let path = decode_note_text(request, table);
            ClassifiedHole {
                routing: HoleRouting::DelegatedBranches { path },
                prompt: String::new(),
            }
        }
        Some("AsyncSpawnWith")
        | Some("AsyncDoneWith")
        | Some("AsyncJoinAnyWith")
        | Some("AsyncStatusWith")
        | Some("AsyncResultWith")
        | Some("AsyncCancelWith") => ClassifiedHole {
            routing: HoleRouting::Green,
            prompt: String::new(),
        },
        _ => {
            let (prompt, payload) = decode_askwith(request, table);
            ClassifiedHole {
                routing: HoleRouting::Ask { payload },
                prompt,
            }
        }
    };
    tracing::info!(routing = ?hole.routing, prompt = %hole.prompt, "suspension classified");
    Ok(hole)
}

/// Pull `text` out of a `NoteWith`-shaped request (`Con(_, [text :: Text])`)
/// — a bare `Text` field, no shape/schema involved (unlike `AskUserWith`).
fn decode_note_text(request: &Value, table: &DataConTable) -> String {
    let Value::Con(_, fields) = request else {
        return String::new();
    };
    fields
        .first()
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Decode an `AskUserWith`-shaped request (`Con(_, [shape])`) into a
/// [`crate::selfharness::operator::FormShape`]. `None` on any shape/decode
/// mismatch — the caller falls back to the plain-Ask routing.
///
/// `askUser @T` (`Tidepool.Form`) sends a bare
/// [`crate::selfharness::operator::FormShape`] — exactly the JSON
/// `selfharness::operator`'s module docs specify — straight through for the
/// in-process gate.
fn decode_askuser_spec(
    request: &Value,
    table: &DataConTable,
) -> Option<crate::selfharness::operator::FormShape> {
    let Value::Con(_, fields) = request else {
        return None;
    };
    let field = fields.first()?;
    let json = tidepool_runtime::value_to_json(field, table, 0);
    serde_json::from_value(json).ok()
}

/// The `typedSite`/`fork`/`fan`/`prompts` payload classification a
/// `RunLLMTurnWith` request carries — factored out of [`classify_hole`] so
/// this shape is documented once rather than at every call site.
///
/// `typedSite` and, when present, `fan` are validated ROUTING fields (see
/// [`ClassifyError`]'s doc): missing/non-numeric/out-of-range is an `Err`,
/// never a `0`/truncated default. A non-`Text` element in `prompts` is
/// likewise rejected rather than silently dropped — filtering it out would
/// under-report the fan's true cardinality; and when `fan` is present, it
/// must agree with the (validated) prompt count.
fn classify_runllmturn_payload(
    payload: &Json,
    asks: &AsksSidecar,
) -> Result<HoleRouting, ClassifyError> {
    let site = require_site_field(payload, "RunLLMTurnWith", "typedSite")?;
    let ty = asks.type_of(site.get()).map(str::to_string);
    if payload.get("fork").and_then(Json::as_bool).unwrap_or(false) {
        let fan = payload
            .get("fan")
            .and_then(Json::as_u64)
            .map(|n| {
                u32::try_from(n).map_err(|_| ClassifyError::OutOfRange {
                    constructor: "RunLLMTurnWith",
                    field: "fan",
                    value: n,
                })
            })
            .transpose()?;
        let raw_prompts = payload
            .get("prompts")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let prompts: Vec<String> = raw_prompts
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if prompts.len() != raw_prompts.len() {
            return Err(ClassifyError::FanMismatch {
                constructor: "RunLLMTurnWith",
                declared: raw_prompts.len(),
                actual: prompts.len(),
            });
        }
        if let Some(n) = fan {
            if n as usize != prompts.len() {
                return Err(ClassifyError::FanMismatch {
                    constructor: "RunLLMTurnWith",
                    declared: n as usize,
                    actual: prompts.len(),
                });
            }
        }
        Ok(HoleRouting::Fork {
            site,
            ty,
            fan: fan.map(|n| FanBadge::Exact { n }),
            prompts,
            source: ForkSource::RunLLMTurn,
        })
    } else if payload
        .get("branchFanout")
        .and_then(Json::as_bool)
        .unwrap_or(false)
    {
        let context_ref = payload
            .get("ref")
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or(ClassifyError::MissingField {
                constructor: "RunLLMTurnWith",
                field: "ref",
            })?;
        let raw_labels = payload
            .get("labels")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let labels: Vec<String> = raw_labels
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if labels.len() != raw_labels.len() {
            return Err(ClassifyError::FanMismatch {
                constructor: "RunLLMTurnWith",
                declared: raw_labels.len(),
                actual: labels.len(),
            });
        }
        let raw_prompts = payload
            .get("prompts")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let prompts: Vec<String> = raw_prompts
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if prompts.len() != raw_prompts.len() {
            return Err(ClassifyError::FanMismatch {
                constructor: "RunLLMTurnWith",
                declared: raw_prompts.len(),
                actual: prompts.len(),
            });
        }
        if labels.len() != prompts.len() {
            return Err(ClassifyError::FanMismatch {
                constructor: "RunLLMTurnWith",
                declared: labels.len(),
                actual: prompts.len(),
            });
        }
        if let Some(n) = payload.get("fan").and_then(Json::as_u64) {
            if n as usize != prompts.len() {
                return Err(ClassifyError::FanMismatch {
                    constructor: "RunLLMTurnWith",
                    declared: n as usize,
                    actual: prompts.len(),
                });
            }
        }
        Ok(HoleRouting::BranchFanout {
            site,
            ty,
            context_ref,
            labels,
            prompts,
        })
    } else if payload
        .get("branch")
        .and_then(Json::as_bool)
        .unwrap_or(false)
    {
        let context_ref = payload
            .get("ref")
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or(ClassifyError::MissingField {
                constructor: "RunLLMTurnWith",
                field: "ref",
            })?;
        let label = payload
            .get("label")
            .and_then(Json::as_str)
            .map(str::to_string);
        Ok(HoleRouting::Branch {
            site,
            ty,
            context_ref,
            label,
        })
    } else {
        Ok(HoleRouting::RunLLMTurn { site, ty })
    }
}

/// Strip one layer of `[...]` from a rendered type string — the FANOUT
/// element-type derivation: a `runLLMTurnFanout` site's recorded
/// asks.json type is the LIST type `[T]`; the harness recovers the
/// per-child element type `T` by stripping the outer brackets. `None` if
/// `ty` isn't bracket-wrapped.
pub fn strip_list_type(ty: &str) -> Option<&str> {
    ty.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
}

/// The request `Value`'s constructor name, when it is a `Con` — `None` for
/// any other `Value` shape (a suspended Ask/RunLLMTurn/Finalize request is
/// always a `Con`, by construction of their `*With` GADT constructors).
pub(crate) fn con_name<'a>(request: &Value, table: &'a DataConTable) -> Option<&'a str> {
    let Value::Con(con_id, _) = request else {
        return None;
    };
    table.name_of(*con_id)
}

/// Pull `(prompt, payload)` out of a `Con(_, [prompt :: Text, payload ::
/// Value])`-shaped request — the wire shape `AskWith` and `RunLLMTurnWith`
/// both use (this is the shared decode `Ask` and `RunLLMTurn` consume; only
/// the constructor NAME differs, checked by the caller via [`con_name`]
/// before dispatching here). `Finalize`'s wire shape is different (its value
/// field is never JSON-decoded) and has its own decode, [`decode_finalize_site`].
fn decode_prompt_payload(request: &Value, table: &DataConTable) -> (String, Json) {
    let Value::Con(_, fields) = request else {
        return (String::new(), Json::Null);
    };
    let prompt = fields
        .first()
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default();
    let payload = fields
        .get(1)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .unwrap_or(Json::Null);
    (prompt, payload)
}

/// Pull `(site, ty)` out of a `FinalizeWith`-shaped request (`Con(_, [site ::
/// Int, value])`). Only the leading `Int` site id is JSON-decoded — the
/// value field crosses in-heap and is deliberately left untouched here (see
/// [`classify_hole`]'s doc); `asks` resolves the site to its rendered type
/// the same way [`classify_runllmturn_payload`] does. `site` is a ROUTING
/// field (see [`ClassifyError`]'s doc) — missing/non-numeric/out-of-range is
/// an `Err`, never a `0` default.
fn decode_finalize_site(
    request: &Value,
    table: &DataConTable,
    asks: &AsksSidecar,
) -> Result<(crate::tree::SiteId, Option<String>), ClassifyError> {
    let Value::Con(_, fields) = request else {
        return Err(ClassifyError::Malformed {
            constructor: "FinalizeWith",
        });
    };
    let site = require_con_site(fields, 0, table, "FinalizeWith", "site")?;
    let ty = asks.type_of(site.get()).map(str::to_string);
    Ok((site, ty))
}

/// Pull `(site, brief)` out of a `ForkWith`-shaped request (`Con(_, [site ::
/// Int, brief :: Text])`) — a single `fork @T brief` suspension. The site id
/// selects the recorded answer type (a ROUTING field, validated — see
/// [`ClassifyError`]'s doc); the brief is display text, decoded as before.
fn decode_fork_one(
    request: &Value,
    table: &DataConTable,
) -> Result<(crate::tree::SiteId, String), ClassifyError> {
    let Value::Con(_, fields) = request else {
        return Err(ClassifyError::Malformed {
            constructor: "ForkWith",
        });
    };
    let site = require_con_site(fields, 0, table, "ForkWith", "site")?;
    let brief = fields
        .get(1)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default();
    Ok((site, brief))
}

/// Pull `(site, prompts)` out of a `ForkAllWith`-shaped request (`Con(_, [site
/// :: Int, prompts :: [Text]])`) — a `forkAll @T briefs` suspension. `site` is
/// a ROUTING field, validated (see [`ClassifyError`]'s doc). `prompts` is the
/// per-child brief list in declaration order; a non-`Text` element is
/// rejected rather than silently dropped — dropping it would under-report the
/// fan's true cardinality to the caller (`Harness::answer_fanout`).
fn decode_fork_all(
    request: &Value,
    table: &DataConTable,
) -> Result<(crate::tree::SiteId, Vec<String>), ClassifyError> {
    let Value::Con(_, fields) = request else {
        return Err(ClassifyError::Malformed {
            constructor: "ForkAllWith",
        });
    };
    let site = require_con_site(fields, 0, table, "ForkAllWith", "site")?;
    let raw_prompts = fields
        .get(1)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_array().cloned())
        .unwrap_or_default();
    let prompts: Vec<String> = raw_prompts
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if prompts.len() != raw_prompts.len() {
        return Err(ClassifyError::FanMismatch {
            constructor: "ForkAllWith",
            declared: raw_prompts.len(),
            actual: prompts.len(),
        });
    }
    Ok((site, prompts))
}

/// Pull the prompt (Text) and payload (JSON object) out of an `AskWith` Con.
/// Mirrors `tidepool_repl::ask::extract_ask_request`, but keeps the payload as
/// structured JSON (not opaque) so the engine can classify it. A non-`AskWith`
/// Con (or any other request shape) decodes to an empty prompt / `Null`
/// payload — the same fallback [`classify_hole`] uses for an unrecognized Con.
fn decode_askwith(request: &Value, table: &DataConTable) -> (String, Json) {
    if con_name(request, table) != Some("AskWith") {
        return (String::new(), Json::Null);
    }
    decode_prompt_payload(request, table)
}

// ---------------------------------------------------------------------------
// Prompt assembly
// ---------------------------------------------------------------------------

/// The framing that teaches the model its ONE tool (an eval block) and how to
/// answer a hole. Deliberately terse — the API is the prompt.
pub const SYSTEM_FRAMING: &str = "\
You drive a resident Haskell (tidepool) session. Your runnable output is fenced \
```haskell code blocks: EVERY such block in your reply runs, in order, as one \
sequence — consecutive GHCi entries, so later blocks see earlier blocks' \
declarations and bindings. Inside a block, each unindented line is its own GHCi \
statement (a declaration, a bind `x <- expr`, or an expression of type `M a` — the \
same effect-monad surface as tidepool eval, with verbs like `run`, `grepGlob`, \
`readGlob`, `llm`, `runLLMTurn`, `runLLMTurnFork`); an indented line continues the \
statement above it, just like GHCi layout. Sequence effectful steps inside a single \
`do` block; declare the types and helpers they use in a separate statement before it. \
If a block fails, everything before it has still run and persists — you'll be told \
which block failed and why; continue from that block. Prose outside the blocks is \
ignored by the runtime.\n\
\n\
Verbs return typed DATA you unwrap — failures are `Either`, NOT exceptions. PREFER a typed \
verb over shelling out with `run` (there is a verb for files, http, git, kv):\n\
- `run :: Text -> M (Either ExecError Proc)` — a shell command. `Right p <- run \"cmd\"`, then \
`p.stdout` / `p.exitCode` (record-dot). `run` does NOT return `Text`.\n\
- `readFile :: FilePath -> M (Either FsError Text)` / `writeFile :: FilePath -> Text -> M (Either FsError ())` \
(mkdir-p) — read/write ONE file (do NOT `run \"cat/awk …\"` or `run \"… > file\"`); process text with `lines`, \
`T.` fns. `readGlob :: Text -> M [FileRead]` for a glob (each `.path`, `.contents`). To edit, `update path old new`.\n\
- `grepGlob :: Text -> FilePath -> M (Either FsError [Hit])` — regex FIRST, path-glob SECOND (each `.path`/`.line`/`.text`).\n\
- `httpGet :: Text -> M (Either HttpError Value)` — HTTP GET → JSON (do NOT `run \"curl …\"`); \
extract with `v ^? key \"f\" . _Int` / `_String`.\n\
- Git (not `run \"git …\"`): `gitLog`, `gitStatus`, `gitShow \"HEAD\" :: M (Either GitError Commit)` \
(`.sha`/`.subject`/`.author`/`.files`).\n\
- KV store: `kvSet key (toJSON v)`, `kvGet key :: M (Maybe Value)`.\n\
- JSON: `object [\"k\" .= v]`, `toJSON`; extract with `v ^? key \"f\" . _String`.\n\
Unwrap an `Either` via `Right x <- verb …` or `verb … >>= liftEither`. Avoid `read`-parsing — \
use the typed verbs + optics.\n\
\n\
To SUSPEND for a typed answer, evaluate `runLLMTurn @T \"prompt\"` (answered in your \
own context) or `runLLMTurnFork @T \"prompt\"` (answered by a forked sub-agent).\n\
\n\
The session PERSISTS across turns like GHCi: a value you bind with `x <- …` this turn \
— a `runLLMTurn`/`runLLMTurnFork` answer — is a LIVE binding in your NEXT turn, so \
you can BRANCH on it. A branching dialogue is \
exactly that: bind a choice, then next turn pick the follow-up from it. E.g. turn 1 \
`lane <- runLLMTurn @Text \"which lane — alpha or beta?\"`; turn 2 reads `lane` and \
presents the form for that branch. Bind what you'll need later instead of re-asking.\n\
\n\
When you are answering a HOLE, your LAST block's value IS the answer: write `resume expr` \
where `expr :: T` matches the hole's declared type. `resume` is the identity here — \
`resume Approve` just yields `Approve`.";

/// The answerer's `resume :: a -> M a` helper — the identity, so a fork/return
/// answerer writes `resume expr` and the block's value is `expr`. Injected into
/// the answerer turn's `helpers` so `resume` is in scope.
pub const RESUME_HELPER: &str = "resume :: a -> M a\nresume = pure";

/// Assemble the provider request from a transcript and per-node framing. The
/// system message is always first; the transcript follows in order.
///
/// `framing` is the node's own system message — the self-iterating harness's
/// per-loop answerer session passes `render`'s output here (wired end-to-end
/// so the distilled `render` conditional actually reaches the model, not just
/// the observational `CycleOutcome`). `None` falls back to the default
/// [`SYSTEM_FRAMING`] that teaches the full eval surface — the shape every
/// ordinary Agent node still uses.
pub fn assemble_request(
    transcript: &[Message],
    max_tokens: Option<u32>,
    framing: Option<&str>,
) -> TurnRequest {
    let mut messages = Vec::with_capacity(transcript.len() + 1);
    messages.push(Message {
        role: Role::System,
        content: framing.unwrap_or(SYSTEM_FRAMING).to_string(),
        reasoning_items: Vec::new(), // the synthetic system message never carries any
    });
    messages.extend_from_slice(transcript);
    TurnRequest {
        messages,
        max_tokens,
    }
}

/// A fenced GHC-style `data`-declaration block to append to a hole card, or
/// an empty string when there is nothing to show: no table (the caller
/// couldn't reach one — see call sites), or
/// [`crate::synopsis::type_document`] degraded all the way to the bare type
/// name (already stated elsewhere in the card, so repeating it here would
/// add nothing). Never an invented/partial shape — a type document renders
/// only when every constructor's field types are known; a harness author
/// never hand-embeds an answer type's declaration in a prompt.
fn type_shape_line(ty: &str, table: Option<&DataConTable>) -> String {
    match table.map(|t| crate::synopsis::type_document(t, ty)) {
        Some(doc) if doc != ty => format!("Its shape:\n\n```haskell\n{doc}\n```\n\n"),
        _ => String::new(),
    }
}

/// Render a hole card as a user-turn message: the prompt plus a `resume :: T`
/// signature the answerer fills. This is what a fork/return answerer sees as
/// its task. `table` is the [`DataConTable`] the hole's type was classified
/// from (when the caller has one in hand) — used only to render a names-only
/// shape synopsis, never to invent field types.
pub fn hole_card(prompt: &str, ty: Option<&str>, table: Option<&DataConTable>) -> String {
    match ty {
        Some(ty) => {
            let shape = type_shape_line(ty, table);
            format!(
                "A parent computation is suspended and needs a typed answer.\n\n\
                 {prompt}\n\n\
                 Answer by evaluating `resume expr` where:\n\n\
                 ```haskell\nresume :: {ty} -> M {ty}\n```\n\n\
                 {shape}Your ```haskell block's value must be of type `{ty}`."
            )
        }
        None => format!(
            "A parent computation is suspended and needs an answer.\n\n{prompt}\n\n\
             Answer by evaluating `resume expr` in a ```haskell block."
        ),
    }
}

/// The hole card for a SELF-ITERATING-HARNESS answerer (scoped `[AskUser,
/// Finalize]` stack), which can ONLY resolve a hole via `finalize @T` — it has
/// no `resume` (the generic [`hole_card`] tells the model to write `resume
/// expr`, which does not compile against this scoped stack and costs a needless
/// compile-error/retry round). This card names
/// `finalize @T` directly.
/// `imports` are the author modules the turn already imports
/// (`crate::harness::AnswerContract`) — say so, because a model that believes
/// `{ty}` is out of scope stops trying to build one and finalizes whatever does
/// compile instead (e.g. a `Text`/tuple) rather than the real type.
///
/// Prescribes bare `finalize @{ty} value`, with no outer `:: M {ty}`
/// annotation — `__anchor` (`template_turn_for`) resolves the
/// ambiguous-`a0` defect at the template level, so the bare shape compiles
/// for a pinned `Finalize T` row
/// (`finalize_type_pinning::bare_finalize_with_no_annotation_compiles_when_pinned`).
/// An annotated form still compiles too; it is just unnecessary to prescribe.
///
/// `table` is the [`DataConTable`] the hole's answer type was resolved from,
/// when the caller has one in hand — used only to render a names-only shape
/// synopsis (see [`hole_card`]), never to invent field types.
pub fn answerer_hole_card(
    prompt: &str,
    ty: Option<&str>,
    imports: &[String],
    table: Option<&DataConTable>,
    effect_row: &[String],
) -> String {
    let ty = ty.unwrap_or("A");
    // Parenthesize a compound answer type wherever it follows `@` — the card
    // is executable advice, and `finalize @State -> State` is ill-typed.
    let ty_at = if ty.contains(' ') {
        format!("({ty})")
    } else {
        ty.to_string()
    };
    let shape = type_shape_line(ty, table);
    // State the row IN the window-opening message (operator decision,
    // 2026-08-20): a branch child's inherited frozen context may carry a
    // DIFFERENT row's framing, and windows were observed discovering their
    // real capabilities through compile-error rounds (4 of 7 failed rounds
    // in the first interaction-surface turn were row mismatches). The card
    // is composed fresh per window, so it is the authoritative place.
    let row = if effect_row.is_empty() {
        String::new()
    } else {
        format!(
            "Your effect row THIS WINDOW is `[{}]` — these effects and only \
             these compile here, whatever any earlier framing listed.\n\n",
            effect_row.join(", ")
        )
    };
    format!(
        "The loop needs a typed answer of type `{ty}`.\n\n\
         {prompt}\n\n\
         {row}{shape}This request holds your window open: take the rounds you need \
         (```haskell blocks, run in order — explore, define, `note`, `askUser`), \
         then answer by evaluating `finalize @{ty_at} value` — THAT ends the \
         window and hands the value back to the loop.{scope}",
        scope = if imports.is_empty() {
            String::new()
        } else {
            format!(
                " `{ty}` is already in scope (this turn imports {}) — and `finalize` \
                 is PINNED to `{ty}` in your row, so a wrong-typed answer is a \
                 compile error naming the row, not a value that silently crosses. \
                 Construct a real `{ty}`, do not substitute a tuple or `Text`. (The \
                 pin constrains `finalize`'s type only — your row's other effects, \
                 and define/explore rounds, remain available as your system framing \
                 says.)",
                imports.join(", ")
            )
        }
    )
}

/// Render the "Available effects" cheatsheet a SYSTEM-level framing folds
/// over its ACTUAL compiling decl list — the single source for a self-
/// iterating-harness surface's verb docs, so a row with a different effect
/// set (more effects, fewer) gets a correspondingly different section with
/// no separate hand-authored verb table. Each decl contributes its
/// [`tidepool_mcp::EffectDecl::prompt_card`] (a compact per-turn card:
/// signatures plus one or two examples) when set, falling back to the full
/// `description` for a decl that hasn't defined one.
///
/// Sent ONCE per node's system framing (not re-derived per hole/round), so
/// this is the right place for the compact-but-not-terse grain — unlike a
/// per-hole card, which is re-sent every model round.
pub fn available_effects_section(decls: &[tidepool_mcp::EffectDecl]) -> String {
    let cards: Vec<String> = decls
        .iter()
        .map(|d| {
            format!(
                "- **{}**: {}",
                d.type_name,
                d.prompt_card.unwrap_or(d.description)
            )
        })
        .collect();
    format!("Available effects this turn:\n{}", cards.join("\n"))
}

// ---------------------------------------------------------------------------
// Eval-block extraction
// ---------------------------------------------------------------------------

/// Extract EVERY fenced ```haskell block from a model reply, in order.
/// Empty when the reply has no fenced haskell block (a pure-prose turn — the
/// engine treats that as "no eval to run", loops or completes per policy).
///
/// Matches ```haskell / ```hs (case-insensitive) opening fences; a bare ```
/// fence is NOT treated as haskell (avoids grabbing a shell/text block).
/// Every block is `trim_end()`ed — the turn templates' `{{TURN}}` splice
/// relies on turn text never carrying a trailing newline.
///
/// FUSED FENCES: a closing fence and the next block's opener on ONE line
/// (` ``````haskell `, no newline between them) closes the current block and
/// opens the next. Models emit this shape routinely (every organic
/// multi-block reply in the dogfood corpus fused its fences); under the old
/// per-line parse the opener was consumed as part of the closer and the
/// SECOND BLOCK SILENTLY VANISHED — the companion's first live packed
/// `askUser` lost its ask block exactly this way (dogfood, 2026-08-14).
pub fn extract_haskell_blocks(reply: &str) -> Vec<String> {
    fn opens_haskell(s: &str) -> bool {
        let lang = s.strip_prefix("```").map(|l| l.trim().to_ascii_lowercase());
        matches!(lang.as_deref(), Some("haskell") | Some("hs"))
    }
    fn close(blocks: &mut Vec<String>, body: &mut Option<String>) {
        if let Some(b) = body.take() {
            let b = b.trim_end().to_string();
            if !b.is_empty() {
                blocks.push(b);
            }
        }
    }
    let mut blocks = Vec::new();
    let mut body: Option<String> = None;
    for line in reply.lines() {
        let trimmed = line.trim_start();
        if body.is_some() {
            if let Some(rest) = trimmed.strip_prefix("```") {
                close(&mut blocks, &mut body);
                if opens_haskell(rest.trim_start()) {
                    body = Some(String::new());
                }
            } else if let Some(b) = body.as_mut() {
                b.push_str(line);
                b.push('\n');
            }
        } else if opens_haskell(trimmed) {
            body = Some(String::new());
        }
    }
    // An unterminated final block still counts.
    close(&mut blocks, &mut body);
    blocks
}

// ---------------------------------------------------------------------------
// Multi-block sequences
// ---------------------------------------------------------------------------

/// One line of a sequence receipt: which block ran and what it produced. The
/// block is identified by its first source line (the model wrote it this
/// turn — it needs a pointer, not a re-print), the outcome by `rendered`'s
/// first line.
pub fn block_receipt(n: usize, source: &str, rendered: &str) -> String {
    let head = truncate_chars(source.lines().next().unwrap_or("").trim(), 72);
    let out = truncate_chars(rendered.lines().next().unwrap_or("").trim(), 96);
    format!("block {n} ({head}) — {out}")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// The corrective payload for a failure at block `n` of a multi-block
/// sequence: which blocks already ran (and persist), which never ran, where
/// to resume, then the GHC error. Slotted where a single-block turn carries
/// the bare GHC error, so every corrective wrapper (the driver's window
/// loop, [`crate::Harness::run_to_hole_or_done`]) forwards it without
/// knowing about sequences.
pub fn sequence_failure_context(receipts: &[String], n: usize, total: usize, ghc: &str) -> String {
    let ran = if receipts.is_empty() {
        "(none — the first block failed)".to_string()
    } else {
        receipts.join("\n")
    };
    let unrun = if n < total {
        if n + 1 == total {
            format!("Block {total} did not run.\n")
        } else {
            format!("Blocks {}–{total} did not run.\n", n + 1)
        }
    } else {
        String::new()
    };
    format!(
        "Block {n} of {total} failed. Blocks that already ran persist (their \
         declarations, bindings, and effects):\n{ran}\n{unrun}\
         Continue from block {n}: your next reply starts at a corrected \
         block {n} — everything before it already ran.\n\n{ghc}"
    )
}

/// Split a model-written block into (imports, expression). A model sometimes
/// puts `import Foo` lines at the top of its ```haskell block; those are NOT
/// legal inside the templated `M a` EXPRESSION position, so they are peeled off
/// and routed to `template_haskell`'s `imports` field.
///
/// No implicit import is added here: `Tidepool.Form` is already
/// auto-imported by the turn preamble when `AskUser` is in the compiling
/// stack (`preamble::pragmas_and_imports`), and an Agent-stack turn (which
/// has no `AskUser`) must NOT get it force-imported — `Tidepool.Form` would
/// fail to resolve there (it depends on `askUserRaw`).
pub fn split_imports(block: &str) -> (String, String) {
    let mut imports: Vec<String> = Vec::new();
    let mut body = Vec::new();
    let mut in_body = false;
    for line in block.lines() {
        let trimmed = line.trim_start();
        if !in_body && trimmed.starts_with("import ") {
            // `import Qualified.Mod (names)` — keep everything after `import `.
            let rest = trimmed.trim_start_matches("import ").trim().to_string();
            if !rest.is_empty() {
                imports.push(rest);
            }
        } else if !in_body && trimmed.is_empty() {
            // Blank lines before the body are skipped (don't start the body).
        } else {
            in_body = true;
            body.push(line);
        }
    }
    (imports.join("\n"), body.join("\n"))
}

/// Split a turn's (already import-stripped, see [`split_imports`]) block text
/// into top-level items — the harness-side counterpart of `tidepool-repl`'s
/// caller-supplied `session_run { items: [String] }` array. A model writes ONE
/// fenced block with several top-level chunks (a helper declaration, then the
/// answer expression); this recovers the item boundaries `run_multi_item_block`
/// hands to [`tidepool_runtime::session::classify_block`].
///
/// GHCi statement semantics, not a blank-line paragraph convention: an
/// UNINDENTED line starts a new item; an indented line (a `do`/`where`/`let`
/// continuation, a multi-line `data`/record field list, an `in` clause) stays
/// part of the item it follows — the same rule a real GHCi paste already
/// applies to a script. A blank line still closes an open item, but is no
/// longer REQUIRED to: a model writing contiguous GHCi-style bind lines
/// (`seed <- pure 1\nrunningTotal <- pure (sum seed)`) with no blank line
/// between them now gets one item PER LINE, exactly like typing each line at
/// a real GHCi prompt. Before this, a contiguous run like that stayed ONE
/// item and compiled as a single wrapped `do`-expression — the binds ran
/// transiently and never persisted as session bindings, which read as
/// "session state is broken" a turn later. A block with no unindented
/// boundary past its first line is a single item — the common case,
/// `trim_end()`ed identically to the whole-block text `run_block`'s existing
/// single-item path already receives.
pub fn split_block_items(block: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in block.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                current.push('\n');
            }
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented && !current.is_empty() {
            items.push(current.trim_end().to_string());
            current.clear();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        items.push(current.trim_end().to_string());
    }
    items
}

// ---------------------------------------------------------------------------
// Include-path resolution + effect-stack config
// ---------------------------------------------------------------------------

/// Everything the engine needs to compile + run turns: the extract binary, the
/// include search paths (prelude + effects module + optional project lib), the
/// effect decls, the Ask tag, and the effect-row names.
pub struct EngineConfig {
    pub extract_bin: tidepool_extract_cmd::ResolvedExtractBin,
    pub include: Vec<PathBuf>,
    pub effect_names: Vec<String>,
    /// The full [`EffectDecl`]s this config was built from — the SOURCE of both
    /// the turn preamble (the effect verb helpers) and `effect_names`. Carried
    /// so a turn templates its preamble against the config's ACTUAL effect set,
    /// not a hardcoded Agent stack: the self-iterating harness's answerer
    /// session is built from [`crate::selfharness::driver::answerer_decls`]
    /// (gui + finalize only), and its turns must NOT advertise verbs
    /// (`run`/`runLLMTurn`/…) it cannot compile.
    pub decls: Vec<tidepool_mcp::EffectDecl>,
    /// The suspend THRESHOLD: the tag (position) of the FIRST interposed
    /// effect (`Ask`|`RunLLMTurn`|`Finalize`) in [`Self::decls`] — every effect
    /// at or past this tag suspends the machine rather than dispatching to a
    /// handler. Named for what it is, the suspend threshold, not just
    /// `Ask`, since `RunLLMTurn`/`Finalize` share the same suspend path.
    pub suspend_tag: u64,
    /// The stdlib include dir this config was built from (`include[0]`,
    /// carried separately so a caller building a NARROWER decls list against
    /// the same stdlib — e.g. the self-iterating harness's outer `Eff
    /// '[RunLLMTurn]` compile — doesn't have to reverse-engineer it out
    /// of `include`).
    pub prelude_dir: PathBuf,
    /// The project-lib dir this config was built from, if any (see
    /// `prelude_dir`'s doc).
    pub project_lib: Option<PathBuf>,
    /// The config's own default effects-module SHIM dir (the entry
    /// [`Self::from_decls`] pushed into [`Self::include`], at the config's
    /// default row) — `Tidepool/Effects.hs` + `Tidepool/Orchestrate.hs`.
    /// Tracked separately — not just "the last entry of `include`" — because
    /// callers routinely append MORE dirs to `include` after construction (a
    /// project source dir, `examples/harness` in tests): [`Self::turn_target`]
    /// finds and replaces THIS specific entry for a pinned turn, so it stays
    /// correct regardless of what else got appended later.
    ///
    /// Distinct from [`Self::core_dir`] (the STABLE `Tidepool.Effects.Core`
    /// dir, vocabulary-keyed): only the shim changes per pinned turn — Core
    /// never needs finding-and-replacing, because its content never depends
    /// on a hole's answer type.
    effects_dir: PathBuf,
    /// The config's stable effects-VOCABULARY dir — `Tidepool/Effects/Core.hs`,
    /// content-addressed on [`Self::decls`] (+ `RunLLMTurn`, via
    /// [`vocab_with_runllmturn`]) ALONE. Pushed into [`Self::include`] once, at
    /// construction, and never swapped by [`Self::turn_target`] — every pinned
    /// `Finalize <T>` turn this config ever compiles shares this SAME dir, so
    /// `Console`/`KV`/`Finalize`/… stay one stable tycon across every window.
    core_dir: PathBuf,
    /// Per-CHILD turn cap for a `runLLMTurnFanout` answerer: each of the N
    /// children gets this budget independently, so one
    /// pathological child can't consume the whole node's turn allowance the
    /// way a single shared cap would. Plain fork/return-control answerers
    /// still use [`DEFAULT_MAX_TURNS`].
    pub max_child_turns: u32,
    /// The context-window budget (in tokens) the runtime watches for MID-LOOP
    /// emergency compaction (self-iterating-harness). DISTINCT from
    /// [`DEFAULT_MAX_TOKENS`], the per-turn *output* cap — this is the whole
    /// answerer session's
    /// accumulated *context* size, summed across its turns. At ~80% of this,
    /// the driver forces a compact-to-text summary of the answerer transcript
    /// and replaces its context IN PLACE so the loop continues under a smaller
    /// window ([`crate::selfharness::driver::SelfHarnessDriver`]'s
    /// `maybe_compact_answerer`). `None` disables the emergency trigger
    /// (structural compaction alone).
    pub context_window_tokens: Option<u32>,
    /// PRD 21 C5: when set, every single-item turn this config compiles
    /// (`Harness::run_block`'s `Expr`/`Bind`/`BindDiscard` candidates, never
    /// the pure `Decl` one) has its block text wrapped as
    /// `runDelegate $ do <block>` before it reaches the extract compile —
    /// see `Tidepool.Agent.Delegate.runDelegate`'s doc for the mechanism.
    /// `false` by default; opt in via [`Self::with_delegate_wrap`]. The
    /// CALLER'S responsibility: this is only sound for a `decls` that
    /// actually carries `Subagent` (e.g.
    /// `selfharness::driver::answerer_decls_with_delegate`) — `Subagent`'s
    /// presence is what makes `Tidepool.Agent.Delegate` auto-import in the
    /// first place (`extra_imports_for!(Subagent)`); turning this on for a
    /// row without `Subagent` fails loud with `runDelegate` simply "not in
    /// scope". A block whose terminal statement carries an explicit
    /// `:: M T` annotation (never the prompt-taught shape, but a model
    /// COULD write one) fails to compile under the wrap — the annotation
    /// asks for the full row directly, which can never equal `runDelegate`'s
    /// expected `Eff (Delegate ': effs) T` argument; an ordinary, recoverable
    /// compile error via the corrective-retry loop, not a hang or a trap.
    pub delegate_wrap: bool,
    /// How long [`crate::harness::Harness::escalate_to_operator`] (the
    /// escalation ladder's rung 2) waits for an operator decision before
    /// failing the turn with `HarnessError::EscalationTimeout` instead of
    /// hanging forever. Defaults to [`DEFAULT_ESCALATION_TIMEOUT`]; a test
    /// overrides this field directly (the same idiom [`Self::max_child_turns`]
    /// already uses) to exercise the timeout path without a real wait.
    pub escalation_timeout: Duration,
}

/// Default context-window budget the emergency-compaction trigger watches.
/// A representative small-model context window;
/// distinct from [`DEFAULT_MAX_TOKENS`] (the per-turn output cap).
/// The driver's `compaction_threshold_percent` (~80%) is taken against THIS.
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u32 = 128_000;

/// Default [`EngineConfig::escalation_timeout`]: long enough for a human
/// operator to notice a stuck-node popup and act on it (checking a
/// notification, reading the transcript preview, clicking a decision) while
/// still bounding an unreachable/forgotten operator to a finite wait rather
/// than hanging the turn (and everything up-stack awaiting it) forever.
pub const DEFAULT_ESCALATION_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Per-node turn cap — a model that never emits a runnable/answering block is
/// stopped after this many turns. Not a configurable knob: nothing in the
/// workspace ever overrides it, so it is a plain constant rather than an
/// `EngineConfig` field.
pub const DEFAULT_MAX_TURNS: u32 = 8;

/// Per-turn output-token cap handed to the provider. Not a configurable
/// knob: nothing in the workspace ever overrides it, so it is a plain
/// constant rather than an `EngineConfig` field.
pub const DEFAULT_MAX_TOKENS: u32 = 2048;

/// The Agent turn engine's decl list: `standard_decls()` (base9 + Ask +
/// RunLLMTurn) with `Finalize` appended last —
/// its own interposed effect/tag, sharing `Ask`/`RunLLMTurn`'s suspend path
/// (see `jit_machine::drive_effect_loop`'s `suspend_tag` threshold: every tag
/// from the FIRST interposed effect onward suspends, so appending a third
/// interposed effect here needs no further Rust-side dispatch change).
/// `Agent` isn't a literal Haskell type anywhere — it's this decl list, used
/// wherever an Agent turn (an Agent node driven by `Harness::run_to_hole_or_done`,
/// including the self-iterating-harness's nested Agent sessions answering a
/// `runLLMTurn` hole via `finalize`) is compiled.
fn agent_decls() -> Vec<tidepool_mcp::EffectDecl> {
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::finalize_decl());
    decls
}

/// This landing's effect-VOCABULARY policy (extract-wave item 0b —
/// `plans/post-restart/extract-wave/boot/00-spec.md`): `row ∪ {RunLLMTurn}`,
/// applied at every [`EngineConfig`] compile ([`EngineConfig::from_decls`]'s
/// default row and [`EngineConfig::turn_target`]'s `Finalize`-pinned row
/// alike). `RunLLMTurn`'s GADT + helpers are declared row-polymorphic
/// (`EffectDecl::helpers_row_polymorphic`), so this only ever makes
/// `runLLMTurn`/`RunLLMTurn` NAMEABLE — never adds it to `type M` — and
/// `Member RunLLMTurn effs` still fails loudly at any call site whose actual
/// row (`decls`) doesn't carry it, e.g. the answerer's `[AskUser, Fork, ReadState,
/// Finalize]`.
///
/// A no-op, returning `decls` unchanged, whenever `RunLLMTurn` is already in
/// the row (the outer harness session's `[RunLLMTurn, AskUser]`, a general
/// Agent turn's `standard_decls()`-based stack) — those compiles' generated
/// source stays byte-identical to before this policy existed.
fn vocab_with_runllmturn(decls: &[tidepool_mcp::EffectDecl]) -> Vec<tidepool_mcp::EffectDecl> {
    if decls.iter().any(|d| d.type_name == "RunLLMTurn") {
        return decls.to_vec();
    }
    let mut vocab = decls.to_vec();
    vocab.push(tidepool_mcp::runllmturn_decl());
    vocab
}

// PRD 21 C5's delegation surface does NOT need a vocab/row split for
// `Worktree`: `Tidepool.Agent.Spawn` (auto-imported alongside
// `Tidepool.Agent.Delegate` whenever `Subagent` is in the row —
// `extra_imports_for!(Subagent)`) imports `Tidepool.Worktree
// (renderWorktreeError)`, and `Tidepool.Worktree.hs` is a WHOLE module GHC
// must typecheck to import anything from it — including its OWN `M`-typed
// bindings (`worktreeBranch`, `worktreeHead`), which need `Worktree`
// GENUINELY in the row, not merely nameable. So `Worktree` rides into
// `type M` for real (`selfharness::driver::answerer_decls_with_delegate`
// puts it second, right after `Subagent`) and `Tidepool.Agent.Delegate`'s
// `runDelegate` re-adds BOTH freshly via `reinterpret2` — see its doc for
// why that still keeps `Worktree` unreachable from the MODEL's own block
// (freshly re-added effects are never members of the row a `reinterpret`
// call's ARGUMENT shares).

impl EngineConfig {
    /// The canonical effect stack's decls + ask tag + effect names, resolving
    /// the extract binary from `TIDEPOOL_EXTRACT` (falling back to
    /// `tidepool-extract` on PATH) and the include paths from `prelude_dir`
    /// (the stdlib `haskell/lib`) plus a freshly-materialized effects module.
    ///
    /// `project_lib` (a `.tidepool/lib` dir) is appended when present so evals
    /// can `import Library` verbs; `None` for the bare stack.
    pub fn standard(
        prelude_dir: PathBuf,
        project_lib: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        Self::from_decls(agent_decls(), prelude_dir, project_lib)
    }

    /// A config for unit tests that never compile a turn: no extract binary,
    /// no includes, and an effects dir that is never read. `effect_names` is
    /// the one field such a test does read — `Harness::flush_effects` maps an
    /// effect's stack tag through it.
    #[cfg(test)]
    pub(crate) fn inert(effect_names: Vec<String>) -> Self {
        EngineConfig {
            extract_bin: tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("unused"),
            include: Vec::new(),
            effect_names,
            decls: Vec::new(),
            suspend_tag: 0,
            prelude_dir: PathBuf::from("."),
            project_lib: None,
            effects_dir: PathBuf::from("."),
            core_dir: PathBuf::from("."),
            max_child_turns: 1,
            context_window_tokens: None,
            delegate_wrap: false,
            escalation_timeout: DEFAULT_ESCALATION_TIMEOUT,
        }
    }

    /// Build a config for an EXPLICIT decls list — not necessarily the full
    /// Agent stack `standard()` hardcodes. The self-iterating harness's outer
    /// driver uses this for its `Eff '[RunLLMTurn]`-only compile
    /// (`vec![tidepool_mcp::runllmturn_decl()]`), so `Harness = M` resolves
    /// to that literal single-effect row rather than the full Agent stack.
    pub fn from_decls(
        decls: Vec<tidepool_mcp::EffectDecl>,
        prelude_dir: PathBuf,
        project_lib: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        // `suspend_tag` is the suspend THRESHOLD: the index of the first
        // interposed effect. For the full Agent stack that's `Ask`; for the
        // answerer stack it's `AskUser`; for a narrower stack (e.g.
        // RunLLMTurn-only) there is no `Ask`/`AskUser` entry at all, so fall
        // back to the first of the other interposed effects — found by name,
        // not by position, since none of them is necessarily the list's last
        // entry.
        let suspend_tag = decls
            .iter()
            .position(|d| {
                matches!(
                    d.type_name,
                    "Ask" | "AskUser" | "RunLLMTurn" | "Fork" | "Finalize"
                )
            })
            .unwrap_or(decls.len()) as u64;
        let effect_names = decls.iter().map(|d| d.type_name.to_string()).collect();
        let vocab = vocab_with_runllmturn(&decls);
        let dirs = tidepool_mcp::ensure_effects_module_with_vocab(
            &decls,
            &vocab,
            &tidepool_mcp::RowArgs::default(),
        )
        .map_err(|e| EngineError::Setup(format!("materialize effects module: {e}")))?;
        let mut include = vec![prelude_dir.clone()];
        if let Some(lib) = &project_lib {
            include.push(lib.clone());
        }
        include.push(dirs.core.clone());
        include.push(dirs.shim.clone());
        let extract_bin = tidepool_runtime::toolchain::extract_command_name()
            .map_err(|e| EngineError::Setup(format!("resolve extract binary: {e}")))?;
        Ok(EngineConfig {
            extract_bin,
            include,
            effect_names,
            decls,
            suspend_tag,
            prelude_dir,
            project_lib,
            effects_dir: dirs.shim,
            core_dir: dirs.core,
            max_child_turns: 4,
            context_window_tokens: Some(DEFAULT_CONTEXT_WINDOW_TOKENS),
            delegate_wrap: false,
            escalation_timeout: DEFAULT_ESCALATION_TIMEOUT,
        })
    }

    /// Opt this config into [`Self::delegate_wrap`] — see its doc for what
    /// that does and the precondition on `decls` it relies on.
    #[must_use]
    pub fn with_delegate_wrap(mut self) -> Self {
        self.delegate_wrap = true;
        self
    }

    /// The effect-row NAMES a delegating config's model-facing block actually
    /// compiles against — [`Self::effect_names`] with `Subagent`/`Worktree`
    /// dropped and `Delegate` prepended, mirroring [`delegate_row_text`]'s
    /// promoted-row transform (the actual turn-templating change). DISPLAY
    /// use only (the hole card's "your effect row" line,
    /// [`answerer_hole_card`]) — [`Self::effect_names`] itself MUST stay the
    /// real dispatched row: `Harness::flush_effects` maps an effect's stack
    /// tag through it POSITIONALLY, and the machine genuinely dispatches
    /// `Subagent`/`Worktree` tags regardless of what the model's own block
    /// can name. `false`/non-delegating: identical to `effect_names`.
    #[must_use]
    pub fn hole_card_effect_row(&self) -> Vec<String> {
        if !self.delegate_wrap {
            return self.effect_names.clone();
        }
        let mut row = vec!["Delegate".to_string()];
        row.extend(
            self.effect_names
                .iter()
                .filter(|n| n.as_str() != "Subagent" && n.as_str() != "Worktree")
                .cloned(),
        );
        row
    }

    /// The promoted-list effect-stack string (`'[Console, KV, …, Finalize
    /// Void]`) for `template_haskell` at the config's default row — every
    /// decl including Ask, each parameterized effect applied to its
    /// [`EffectDecl::default_row_args`]. Routes through
    /// [`tidepool_mcp::build_effect_stack_type`] (not a bare join of
    /// `effect_names`, which carries no type arguments and would emit a
    /// bare `Finalize` — a kind error in the promoted list, since every
    /// sibling entry has kind `* -> *`).
    fn effect_stack_type(&self) -> String {
        tidepool_mcp::build_effect_stack_type(&self.decls)
    }

    /// This config's stable `Tidepool.Effects.Core` dir — the same one for
    /// every turn this config ever compiles, regardless of which `Finalize`
    /// hole [`Self::turn_target`] pins. Exposed for a caller that wants to
    /// assert cross-turn/cross-window Core stability directly (e.g. an
    /// acceptance test comparing this path across two `EngineConfig`s built
    /// from the same decls).
    #[must_use]
    pub fn core_dir(&self) -> &Path {
        &self.core_dir
    }

    /// Resolve ONE turn's compile target: the include search path and the
    /// promoted effect-row string it must be compiled against — both derived
    /// from the SAME [`tidepool_mcp::RowArgs`], so they cannot name different
    /// rows.
    ///
    /// `finalize`, when `Some((ty, imports))`, instantiates the row's
    /// `Finalize` entry at `ty` (the hole's answer type) — importing
    /// `imports` (the author modules that define it) — and materializes ITS
    /// OWN shim dir via [`tidepool_mcp::ensure_effects_shim_module`],
    /// swapping it in for [`Self::effects_dir`] wherever it sits in
    /// [`Self::include`] (found by VALUE, not by position — a caller may have
    /// appended more dirs after construction, e.g. a project source dir).
    /// The dir is content-addressed on the generated source, so repeats of
    /// the same answer type are free and two answer types can never be
    /// served each other's module.
    ///
    /// `None` keeps the config's own default row (`Finalize Void`) and
    /// `include` unchanged — the shape every turn that isn't answering a
    /// typed hole compiles against.
    ///
    /// The include set MINUS the generated effects-module SHIM dir — the
    /// shared decl plane's VALIDATION context (one-session living structure).
    /// The STABLE `Tidepool.Effects.Core` dir stays IN this set (stable-
    /// effects-core): a model-authored declaration naming an effect surface
    /// via `Member <Eff> effs => ... -> Eff effs T` validates and persists,
    /// because Core's tycons are the same ones every later turn's compile
    /// sees. A declaration that instead spells the per-window `M` alias
    /// persists identically — `M` still never resolves against this include
    /// set (the shim isn't on it), but `tidepool_mcp::pure_decl_module_env`'s
    /// plane strips the M-mentioning signature before compiling and lets GHC
    /// infer the same `Member`-polymorphic shape. A declaration that instead
    /// spells `import Tidepool.Effects` (the shim itself, not `M`) still
    /// fails validation with an ordinary "not in scope" GHC error — the
    /// narrowed structural guard, not an import scanner.
    pub fn validation_include(&self) -> Vec<PathBuf> {
        self.include
            .iter()
            .filter(|p| **p != self.effects_dir)
            .cloned()
            .collect()
    }

    pub fn turn_target(
        &self,
        finalize: Option<(&str, &[String])>,
    ) -> Result<TurnTarget, EngineError> {
        let Some((ty, imports)) = finalize else {
            return Ok(TurnTarget {
                include: self.include.clone(),
                stack: self.effect_stack_type(),
            });
        };
        let row = tidepool_mcp::RowArgs::at("Finalize", [ty]).importing(imports.iter().cloned());
        // Only the SHIM depends on `row` (the applied `Finalize <ty>` and its
        // import) — the stable Core dir is untouched and stays wherever
        // `from_decls` already put it in `include`, so it is never found-and-
        // replaced here the way `effects_dir` is below.
        let effects_dir = tidepool_mcp::ensure_effects_shim_module(&self.decls, &row)
            .map_err(|e| EngineError::Setup(format!("materialize effects module: {e}")))?;
        validate_finalize_row(
            &self.extract_bin,
            &self.decls,
            &row,
            &self.validation_include(),
        )?;
        let mut include = self.include.clone();
        match include.iter().position(|p| p == &self.effects_dir) {
            Some(pos) => include[pos] = effects_dir,
            None => include.push(effects_dir),
        }
        Ok(TurnTarget {
            include,
            stack: tidepool_mcp::build_effect_stack_type_at(&self.decls, &row),
        })
    }
}

/// Process-level memo: which generated-effects-module content hashes have
/// already been probe-validated, and with what outcome. A repeated pin (every
/// round of the SAME answerer hole reuses the SAME `Finalize <T>` row) is a
/// cheap in-memory hit, not a second `tidepool-extract` spawn.
fn finalize_probe_memo() -> &'static Mutex<HashMap<u64, Result<(), String>>> {
    static MEMO: OnceLock<Mutex<HashMap<u64, Result<(), String>>>> = OnceLock::new();
    MEMO.get_or_init(Default::default)
}

/// Probe-compile a pinned `Finalize` row's generated `Tidepool.Effects`
/// source STANDALONE — as its own compile TARGET (renamed to `Expr`), not as
/// something a turn module imports — so an applied row type with no
/// resolving import (e.g. `Finalize Decision` pinned with `row.imports()`
/// empty) is caught HERE, with GHC's own direct "Not in scope" diagnostic on
/// the generated module's `type M` line.
///
/// This sidesteps a real hazard in extract's `tidepool-extract-bin`
/// (`GhcPipeline.hs`'s `normalVariant`): when the generated module's OWN
/// `type M = Eff '[..., Finalize T]` fails to resolve `T`, that failure sits
/// in a DEPENDENCY of the turn module (which imports `Tidepool.Effects`), not
/// in the turn module itself — and extract's diagnostic-recovery pass, which
/// re-typechecks every module in the compile to recover a spanned error, does
/// so in non-topological order. Whichever module happens to be visited before
/// `Tidepool.Effects` gets its own turn reports a confusing cascade
/// ("attempting to use module `Tidepool.Effects' ... which is not loaded")
/// instead of the real error. Compiling the SAME generated source as the
/// SOLE target (no separate importer racing it) avoids the hazard entirely —
/// exactly the shape `wrong_typed_finalize_is_a_compile_error` already proves
/// works cleanly through this same redo-loop, just with the error moved from
/// the turn module into the (renamed) generated module. See
/// `tidepool-harness/tests/finalize_type_pinning.rs`'s
/// `pinned_finalize_needs_the_type_in_scope`.
///
/// `include` is the caller's [`EngineConfig::validation_include`] — every
/// author module a row might name, minus the generated-effects dir itself
/// (irrelevant here: the probe source IS that module's body, renamed, not an
/// importer of it).
fn validate_finalize_row(
    extract_bin: &ResolvedExtractBin,
    decls: &[tidepool_mcp::EffectDecl],
    row: &tidepool_mcp::RowArgs,
    include: &[PathBuf],
) -> Result<(), EngineError> {
    // Only the SHIM needs probing: it is the one place a pinned row's applied
    // type (`Finalize Decision`) is spelled, in `type M`. The stable Core
    // module is a pure function of the vocabulary alone and never mentions a
    // per-turn answer type, so it cannot be the source of an unresolved-type
    // failure this probe exists to catch early.
    let generated = tidepool_mcp::effects_shim_module_source(decls, row);

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    generated.hash(&mut hasher);
    let key = hasher.finish();

    if let Some(cached) = finalize_probe_memo().lock().get(&key) {
        return cached.clone().map_err(EngineError::Setup);
    }

    const HEADER_PREFIX: &str = "module Tidepool.Effects (";
    const HEADER_SUFFIX: &str = ") where\n";
    let Some(prefix_pos) = generated.find(HEADER_PREFIX) else {
        return Err(EngineError::Setup(format!(
            "shim module source must contain `{HEADER_PREFIX}...{HEADER_SUFFIX}` — got:\n{generated}"
        )));
    };
    let Some(suffix_rel) = generated[prefix_pos..].find(HEADER_SUFFIX) else {
        return Err(EngineError::Setup(format!(
            "shim module source's `{HEADER_PREFIX}` header has no closing `{HEADER_SUFFIX}` — got:\n{generated}"
        )));
    };
    let header_end = prefix_pos + suffix_rel + HEADER_SUFFIX.len();
    // Keep the PRAGMA preamble before the header (NoImplicitPrelude etc. —
    // needed for the probe to compile identically to the real shim) AND
    // everything after it (imports, `type M`, `__shimProbe`); only the
    // module-header LINE itself is replaced, exactly as the old single-line
    // `HEADER` swap did.
    let probe_source = format!(
        "{}module Expr where\n{}",
        &generated[..prefix_pos],
        &generated[header_end..]
    );

    // `__shimProbe :: M ()` is emitted unconditionally alongside `type M`
    // (see `effects_shim_module_source`) so GHC must elaborate the whole
    // applied row — including a pinned `Finalize <T>` answer type — to
    // typecheck it; target choice doesn't matter beyond "some real binder".
    let outcome = match compile_targets(
        &probe_source,
        &["__shimProbe"],
        include,
        Some(extract_bin),
        |_, _, _| {},
    ) {
        Ok(_) => Ok(()),
        Err(CompileError::Diagnostics(diags)) => Err(diags
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")),
        Err(other) => Err(other.to_string()),
    };
    finalize_probe_memo().lock().insert(key, outcome.clone());
    outcome.map_err(EngineError::Setup)
}

/// One turn's compile target — the include search path and the promoted
/// effect-row string, resolved together by [`EngineConfig::turn_target`] so
/// they can never disagree.
#[derive(Debug, Clone)]
pub struct TurnTarget {
    pub include: Vec<PathBuf>,
    pub stack: String,
}

// ---------------------------------------------------------------------------
// Turn templating
// ---------------------------------------------------------------------------

/// Wrap a model-written `M a` block as a full templated module the extract can
/// compile, with optional extra `helpers` (e.g. the answerer's `resume`) and
/// `imports` (e.g. `Tidepool.Form`). The result is `toJSON`'d — the JSON-render
/// contract of a NORMAL turn (its terminal value is displayed).
///
/// `stack` is the promoted effect-row string this turn compiles against —
/// callers answering a typed `finalize` hole resolve it (and the matching
/// include dir) via [`EngineConfig::turn_target`], so `finalize`'s pin lives
/// in the ROW (`Member (Finalize T) stack`), not in a shimmed/shadowed
/// binding: the ordinary [`tidepool_mcp::build_preamble`] is used unconditionally.
pub fn template_turn(
    cfg: &EngineConfig,
    stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
) -> String {
    template_turn_for(&cfg.decls, stack, code, imports, helpers, cfg.delegate_wrap)
}

/// Like [`template_turn`], but for an EXPLICIT decls list rather than the
/// hardcoded Agent stack — the self-iterating harness driver's outer `Eff
/// '[RunLLMTurn]` compile needs a preamble matching ITS OWN (narrower)
/// decls, not the Agent's. `stack` must be rendered from the SAME `decls` —
/// see [`EngineConfig::turn_target`].
///
/// `stack` pinned to a real (non-`Void`) `Finalize T` entry routes
/// through [`tidepool_mcp::template_haskell_anchored`] instead of the plain
/// [`tidepool_mcp::template_haskell`] — see [`finalize_pin_active`]'s doc for
/// why, and `template_haskell_anchored`'s doc (`eval_prep.rs`) for the
/// mechanism. Every other row (no `Finalize` entry, or the `Void`
/// default) compiles exactly as before.
///
/// `delegate_wrap`: PRD 21 C5 — when `true`, this turn's preamble routes
/// through [`delegate_aware_preamble`] (a local `type M` naming the narrow
/// `Delegate`-form row, so a model-authored `:: M T` annotation resolves
/// against the row its code actually compiles at) and the entry body applies
/// `runDelegate` at the RESULT position (`TurnTemplate::delegate_wrap`) —
/// `code` itself is never textually rewritten, so the SAME text compiles
/// correctly whether classified as an expr, a bind, or (unwrapped, this flag
/// plays no part) a top-level decl. `false` byte-identical to before this
/// parameter existed.
pub fn template_turn_for(
    decls: &[tidepool_mcp::EffectDecl],
    stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
    delegate_wrap: bool,
) -> String {
    if delegate_wrap {
        let preamble = delegate_aware_preamble(decls, stack);
        tidepool_mcp::TurnTemplate {
            preamble: &preamble,
            effect_stack: stack,
            code,
            imports,
            helpers,
            anchor_result: finalize_pin_active(stack),
            delegate_wrap: true,
            ..Default::default()
        }
        .render()
    } else {
        let preamble = tidepool_mcp::build_preamble(decls, false);
        if finalize_pin_active(stack) {
            tidepool_mcp::template_haskell_anchored(
                &preamble, stack, code, imports, helpers, None, None,
            )
        } else {
            tidepool_mcp::template_haskell(&preamble, stack, code, imports, helpers, None, None)
        }
    }
}

/// The shared preamble for a DELEGATING config's turn module (PRD 21 C5):
/// identical to [`tidepool_mcp::build_preamble`], except `M` is redefined
/// LOCALLY to the narrow `[Delegate, AskUser, Fork, ReadState, Finalize T]`
/// row `runDelegate`'s `reinterpret2` signature peels its argument back to —
/// so a model-authored `:: M T` annotation, INSIDE the `runDelegate`-wrapped
/// computation, resolves against the row its own code genuinely compiles at,
/// instead of the outer (`Subagent`/`Worktree`-carrying) row the MACHINE
/// dispatches.
///
/// `M` cannot be redefined to name `Delegate` inside the GENERATED
/// `Tidepool.Effects` module itself: `Tidepool.Agent.Delegate` imports
/// `Tidepool.Effects` (for `Subagent`/`Worktree`'s own types), so the reverse
/// import would be a cyclic module dependency GHC cannot resolve. It has to
/// live here, in the per-turn module that already imports
/// `Tidepool.Agent.Delegate` (`extra_imports_for!(Subagent)`,
/// `tidepool-mcp/src/effect_defs.rs`).
///
/// Two consequences of redefining `M` in a module `build_preamble` ALSO uses
/// for outer-row infrastructure:
/// - `M` leaks into this module's unqualified scope from TWO imports with no
///   export list of their own — `Tidepool.Effects` directly, and
///   `Tidepool.Orchestrate` (which itself imports `Tidepool.Effects`
///   unqualified and re-exports everything in scope, per Haskell's "omitted
///   export list exports every top-level name, including imports" rule).
///   Both must be hidden, or the local redefinition below is a "Conflicting
///   definitions" compile error.
/// - `paginateResult`'s own signature (`paginate_alias`, `preamble.rs`) is
///   spelled `Int -> Value -> M Value` and is ALWAYS emitted (whenever the
///   decl list is non-empty) — it runs at `result`'s own OUTER-row position,
///   never inside the `runDelegate`-wrapped block, so it cannot be typechecked
///   against the narrow `M` this function defines. Its signature is respelled
///   against `outer_stack` (the literal promoted row, unaffected by the local
///   `M`) instead, decoupling it from whatever `M` means.
///
/// `outer_stack` is the REAL row this turn's `result`/`__result` binding
/// compiles against (`EngineConfig::turn_target`'s `stack`) — untouched by
/// this function; only the narrow row `type M` names is new.
fn delegate_aware_preamble(decls: &[tidepool_mcp::EffectDecl], outer_stack: &str) -> String {
    let preamble = tidepool_mcp::build_preamble(decls, false);
    let hidden = preamble
        .replacen(
            "import Tidepool.Effects\n",
            "import Tidepool.Effects hiding (M)\n",
            1,
        )
        .replacen(
            "import Tidepool.Orchestrate\n",
            "import Tidepool.Orchestrate hiding (M)\n",
            1,
        );
    let decoupled = hidden.replacen(
        "paginateResult :: Int -> Value -> M Value\n",
        &format!("paginateResult :: Int -> Value -> Eff {outer_stack} Value\n"),
        1,
    );
    assert!(
        decoupled.contains("import Tidepool.Effects hiding (M)\n")
            && decoupled.contains("import Tidepool.Orchestrate hiding (M)\n")
            && decoupled.contains(&format!(
                "paginateResult :: Int -> Value -> Eff {outer_stack} Value\n"
            )),
        "delegate-aware preamble patches must all apply — a delegating decls \
         list must carry the shape `build_preamble` always emits, else this \
         would silently generate a module with a mis-scoped `M`"
    );
    // AFTER the `default (...)` decl, not before it: `TurnTemplate::render`/
    // `template_haskell` splice USER imports at `preamble.find("default
    // (Int")` too (right BEFORE that line) — inserting `type M` there would
    // land it ahead of a later-spliced `import HarnessTypes`, an ordinary
    // Haskell syntax error (every import must precede every other top-level
    // declaration in a module).
    const DEFAULT_DECL: &str = "default (Int, Double, Text)\n";
    let insert = decoupled
        .find(DEFAULT_DECL)
        .map(|pos| pos + DEFAULT_DECL.len())
        .unwrap_or(decoupled.len());
    format!(
        "{}\ntype M = Eff {}\n\n{}",
        &decoupled[..insert],
        delegate_row_text(outer_stack),
        &decoupled[insert..]
    )
}

/// The narrow `type M` row text a delegating config's model-facing block
/// compiles against: `outer_stack` (the real, dispatched row — `'[Subagent,
/// Worktree, ...]`) with its `Subagent, Worktree` PREFIX replaced by
/// `Delegate` — the row `runDelegate`'s `reinterpret2` signature (`Eff
/// (Delegate ': effs) a -> Eff (Subagent ': Worktree ': effs) a`) peels its
/// argument back to. `answerer_decls_with_delegate` locks this exact prefix
/// order (see its doc), so a plain string strip is correct here and avoids
/// re-deriving the row from `EffectDecl`s — `Delegate` has no `EffectDecl` of
/// its own at all (it is a plain Haskell GADT, never a Rust-registered
/// effect or a `RowArgs` entry).
fn delegate_row_text(outer_stack: &str) -> String {
    const PREFIX: &str = "'[Subagent, Worktree, ";
    match outer_stack.strip_prefix(PREFIX) {
        Some(rest) => format!("'[Delegate, {rest}"),
        None => panic!(
            "delegate_wrap set on a row not starting with `{PREFIX}` — \
             answerer_decls_with_delegate's Subagent/Worktree prefix order is \
             what runDelegate's reinterpret2 signature relies on: {outer_stack}"
        ),
    }
}

/// Like [`template_turn_for`], but additionally renders `extra_entries` into
/// the SAME module as `result` — each `(name, code)` pair becomes its own
/// top-level `Eff <stack> Value` entry, rendered through the identical shared
/// path `result` itself uses
/// ([`tidepool_mcp::TurnTemplate::extra_entries`]), so a caller compiling
/// `result` and an extra entry as two `--targets` of ONE `tidepool-extract`
/// spawn ([`compile_turns`]) gets entries that are identical by
/// construction rather than a hand-copied second `result`-shaped binder — the
/// self-iterating harness driver's render+loop fusion is the first caller
/// (`SelfHarnessDriver::compile_cycle_entry`). Routes through the SAME
/// anchor/preamble decision [`template_turn_for`] makes; `extra_entries`
/// empty renders byte-identical to it.
pub fn template_turn_for_fused(
    decls: &[tidepool_mcp::EffectDecl],
    stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
    extra_entries: &[(&str, &str)],
) -> String {
    let preamble = tidepool_mcp::build_preamble(decls, false);
    tidepool_mcp::TurnTemplate {
        preamble: &preamble,
        effect_stack: stack,
        code,
        imports,
        helpers,
        anchor_result: finalize_pin_active(stack),
        extra_entries,
        // The fused entries' JSON is DRIVER-CONSUMED, not displayed: the loop
        // entry's output round-trips back in as the next cycle's State, and
        // the render entry feeds the operator page whole. Pagination here
        // stubbed the state's `ideas` array to a string once it outgrew 4096
        // bytes, killing the next cycle's decode.
        unpaginated: true,
        ..Default::default()
    }
    .render()
}

/// Whether `stack` (the promoted row string a turn compiles against, e.g.
/// `'[AskUser, Finalize Decision]`) pins `Finalize` to a REAL author type —
/// `false` for the uninhabited default `Finalize Void` (a turn not
/// currently answering a typed hole — every non-answerer turn, and an
/// answerer turn before its first `AnswerContract` is set) or a row with no
/// `Finalize` entry at all (the general Agent stack never carries one).
///
/// A presence check, not a type extraction: `template_turn_for` only needs a
/// boolean (route this turn's `_r` through the anchor, or don't), never the
/// concrete `T` itself — `EngineConfig::turn_target`'s caller already has `T`
/// in hand were it needed for anything else, so there is nothing to recover
/// from this string, only whether to flip the anchor on.
fn finalize_pin_active(stack: &str) -> bool {
    stack.contains("Finalize ") && !stack.contains("Finalize Void")
}

/// Wrap an ANSWERER block as a module whose `result` returns the RAW value —
/// NOT `toJSON`'d. A fork/return answerer's block is `resume expr :: M T`, and
/// the value fed to the parent's continuation must be the raw `T` (an `Int`,
/// an ADT — whatever the hole's type is), because `runLLMTurn`/`Fork`'s
/// `unsafeCoerce` relabels the SAME runtime bytes back to `T`. `template_turn`'s
/// `toJSON _r` would instead hand back an Aeson `Value` (a `Number`, an
/// `Object`), which the parent's `T`-typed continuation then case-traps on.
///
/// So this emits `result :: Eff stack a; result = <block>` — GHC infers `a`
/// from the block's `resume :: T -> M T` (or the polymorphic identity), and the
/// value stays in its native `T` representation.
///
/// `stack` is the promoted effect-row string the answering block ACTUALLY
/// dispatches against — a fork/return-control answerer resolves it the same
/// way `run_block` does, via [`EngineConfig::turn_target`] pinned at the
/// hole's own answer contract (when it has one), so `Finalize`'s row entry
/// here can genuinely match a `finalize @T` call the block makes — never the
/// config's bare default `Finalize Void` row.
///
/// `cfg.delegate_wrap` (PRD 21 C5): when `true`, this turn's preamble routes
/// through [`delegate_aware_preamble`] exactly as [`template_turn_for`]'s
/// does — a model-authored `:: M T` annotation, or the `resume`/`paginateResult`
/// helpers' own `M`-typed signatures, resolve against the narrow `Delegate`-form
/// row instead of the outer `Subagent`/`Worktree`-carrying one — and the
/// RESULT binding applies `runDelegate` at the wrap position:
/// `result = runDelegate (let { __b = <block> } in __b)`. No explicit type
/// signature is needed to anchor this (unlike [`TurnTemplate::render_entry_body`]'s
/// toJSON'd entries, which rely on an outer `Eff <stack> Value` signature): the
/// narrow local `type M` is a fully closed, concrete promoted list, so
/// `runDelegate`'s `Eff (Delegate ': effs) a -> Eff (Subagent ': Worktree ':
/// effs) a` signature structurally decomposes it and fixes `effs` by ordinary
/// unification — the same mechanism that already resolves the raw binding's
/// `a`/`effs` from `resume`'s helper signature when `code` itself carries no
/// annotation. `false` is byte-identical to before this parameter existed.
pub fn template_answer_turn(
    cfg: &EngineConfig,
    stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let preamble = if cfg.delegate_wrap {
        delegate_aware_preamble(&cfg.decls, stack)
    } else {
        tidepool_mcp::build_preamble(&cfg.decls, false)
    };

    let mut out = String::new();
    // Insert user imports right before the `default` decl (same insertion point
    // template_haskell uses), else append the preamble whole.
    if imports.trim().is_empty() {
        out.push_str(&preamble);
    } else {
        let insert = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert]);
        for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str("import ");
            out.push_str(imp);
            out.push('\n');
        }
        out.push_str(&preamble[insert..]);
    }
    out.push_str("-- [user]\n");
    if !helpers.trim().is_empty() {
        out.push_str(helpers);
        if !helpers.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    // The RAW-result binding: no toJSON, no paginate. No explicit type
    // signature — GHC infers the result type from the block (the answerer's
    // `resume :: T -> M T` helper fixes `T` when the hole type is known). The
    // block is embedded verbatim inside an explicit let-bracket (same
    // layout-suspension trick as template_haskell) so unindented multi-line
    // blocks stay valid.
    if cfg.delegate_wrap {
        out.push_str("result = runDelegate (let {\n __b =\n");
    } else {
        out.push_str("result = let {\n __b =\n");
    }
    out.push_str(code);
    if !code.ends_with('\n') {
        out.push('\n');
    }
    if cfg.delegate_wrap {
        out.push_str(" } in __b)\n");
    } else {
        out.push_str(" } in __b\n");
    }
    out
}

/// Wrap a value-plane BIND turn (`x <- e`) as a session module whose `__result`
/// runs the bind statement and yields the bound name — the shape
/// `compile_session_turn` expects (target `__result`, `Eff <stack> _` so GHC
/// infers the bound type from the block). Mirrors the repl's `wrap_bind_source`;
/// the harness preamble/effect-stack differ, the `__result`/session-bind contract
/// is identical. `stmt` is the raw `x <- e` block; `binder` is the bound name.
pub fn template_session_bind(
    cfg: &EngineConfig,
    stmt: &str,
    binder: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let stack = cfg.effect_stack_type();
    let preamble = if cfg.delegate_wrap {
        delegate_aware_preamble(&cfg.decls, &stack)
    } else {
        tidepool_mcp::build_preamble(&cfg.decls, false)
    };

    let mut out = String::new();
    // Insert user imports right before the `default` decl (the same insertion
    // point `template_haskell`/`template_answer_turn` use).
    if imports.trim().is_empty() {
        out.push_str(&preamble);
    } else {
        let insert = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert]);
        for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str("import ");
            out.push_str(imp);
            out.push('\n');
        }
        out.push_str(&preamble[insert..]);
    }
    out.push_str("-- [user]\n");
    if !helpers.trim().is_empty() {
        out.push_str(helpers);
        if !helpers.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&format!("__result :: Eff {stack} _\n"));
    if cfg.delegate_wrap {
        out.push_str("__result = runDelegate (do {\n");
    } else {
        out.push_str("__result = do {\n");
    }
    push_braced_stmt(&mut out, stmt);
    if cfg.delegate_wrap {
        out.push_str(&format!(" ; pure {binder}\n }})\n"));
    } else {
        out.push_str(&format!(" ; pure {binder}\n }}\n"));
    }
    out
}

/// Embed a turn statement verbatim inside an explicit `do { }` block (mirrors the
/// repl's `push_braced_stmt`): a bare `let x = e` gets explicit `let { }`
/// brackets so an unindented continuation is legal; every other statement is
/// embedded as-is. Explicit brackets suspend the layout algorithm so multi-line
/// / quasiquote payloads keep byte fidelity.
fn push_braced_stmt(out: &mut String, turn_text: &str) {
    let trimmed = turn_text.trim_start();
    let let_rest = trimmed
        .strip_prefix("let")
        .filter(|r| r.starts_with(|c: char| c.is_whitespace()));
    match let_rest {
        Some(rest) if !rest.trim_start().starts_with('{') => {
            out.push_str("let {");
            out.push_str(rest);
            if !rest.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(" }\n");
        }
        _ => {
            out.push_str(turn_text);
            if !turn_text.ends_with('\n') {
                out.push('\n');
            }
        }
    }
}

/// Build the EXPR turn template a [`tidepool_runtime::session::TurnRequest`]
/// carries: [`template_turn`] with a one-line `{{TURN}}` placeholder standing
/// in for the real block (the verdict — and so which template applies — is
/// not known until `run_turn` returns), with its `-- [user-lines] S:E`
/// annotation repaired to the range the REAL block would occupy, and its
/// compile target renamed to `__result` (see [`retarget_result_binder`]).
///
/// `template_haskell` computes the `[user-lines]` annotation's END line from
/// the spliced code's own newline count, so a one-line placeholder yields a
/// CORRECT start line (it depends only on content BEFORE the placeholder) but
/// a WRONG end line for any block that isn't itself exactly one line. Left
/// unrepaired, every GHC diagnostic the corrective-retry loop feeds back for a
/// multi-line block would cite the wrong line range.
///
/// Byte-exactness is the contract these repairs buy: splicing `block` back in
/// via [`tidepool_runtime::session::render_template`] must reproduce
/// [`template_turn`] called directly on `block`, up to the deliberate binder
/// rename — see `expr_turn_template_byte_identical_to_template_turn` for the
/// pin.
///
/// Requires `block` not to end in `\n` — `{{TURN}}`'s splice is a dumb
/// VERBATIM substitution (no newline normalization of its own), so this
/// relies on `template_haskell`'s fixed one-newline-after-code padding
/// (baked in once, at build time, from the one-line placeholder) being the
/// SAME padding a trailing-newline-free `block` needs; a `block` that already
/// ended in `\n` would double up. `run_block`'s only source of turn text,
/// `extract_haskell_blocks`, always `trim_end()`s each block it extracts, so
/// this holds for every real caller.
pub fn expr_turn_template(
    cfg: &EngineConfig,
    stack: &str,
    block: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let placeholder = template_turn(cfg, stack, "{{TURN}}", imports, helpers);
    let repaired = repair_user_lines_end(&placeholder, content_line_count(block));
    retarget_result_binder(&repaired)
}

/// Build a BIND/BINDDISCARD turn template: the same preamble/imports/helpers/
/// `__result` scaffolding [`template_session_bind`] builds around a REAL
/// statement, but with the bare `{{TURN_STMT}}` marker placed DIRECTLY —
/// deliberately NOT through [`push_braced_stmt`].
///
/// `push_braced_stmt` (and `render_template`'s own `place_turn_stmt`, which
/// mirrors it) ends its output in exactly one trailing newline, ADDING one
/// when its input doesn't already have one — correct for a REAL statement,
/// but the 13-character marker token `"{{TURN_STMT}}"` itself never ends in
/// `\n`, so routing the MARKER through the same function bakes an extra
/// trailing newline into the template. At splice time `render_template`
/// substitutes the marker with `place_turn_stmt`'s OWN newline-terminated
/// output, so that baked-in newline becomes a genuine duplicate — a blank
/// line between the turn statement and `; pure …` that `template_session_bind`
/// called directly on the same text never produces. Placing the marker bare
/// leaves supplying the separator entirely to the splice's own normalization,
/// which is what makes the two agree — see
/// `bind_template_byte_identical_to_template_session_bind` for the pin.
///
/// `binder` is `"{{BINDERS}}"` for a real bind (its names are comma-joined
/// and spliced in later) or the literal `"()"` for a discarding bind
/// (`TemplateSelector::BindDiscard` — no `{{BINDERS}}` placeholder at all,
/// "splicing no binder").
pub fn session_bind_template(
    cfg: &EngineConfig,
    binder: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let stack = cfg.effect_stack_type();
    let preamble = if cfg.delegate_wrap {
        delegate_aware_preamble(&cfg.decls, &stack)
    } else {
        tidepool_mcp::build_preamble(&cfg.decls, false)
    };

    let mut out = String::new();
    if imports.trim().is_empty() {
        out.push_str(&preamble);
    } else {
        let insert = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert]);
        for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str("import ");
            out.push_str(imp);
            out.push('\n');
        }
        out.push_str(&preamble[insert..]);
    }
    out.push_str("-- [user]\n");
    if !helpers.trim().is_empty() {
        out.push_str(helpers);
        if !helpers.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&format!("__result :: Eff {stack} _\n"));
    if cfg.delegate_wrap {
        out.push_str("__result = runDelegate (do {\n");
    } else {
        out.push_str("__result = do {\n");
    }
    out.push_str("{{TURN_STMT}}");
    if cfg.delegate_wrap {
        out.push_str(&format!(" ; pure {binder}\n }})\n"));
    } else {
        out.push_str(&format!(" ; pure {binder}\n }}\n"));
    }
    out
}

/// Rename the compiled EXPR module's top-level binder from `result`
/// (`template_haskell`'s fixed name — shared with the stateless eval server,
/// so that function can't change it) to `__result`, the name every OTHER
/// template `run_turn` carries here already uses:
/// [`template_session_bind`]'s fixed `__result`, and `run_turn`'s own default
/// target when no `--target` is supplied. `--target` is ONE flag for the
/// whole `--turn` spawn, shared across whichever verdict GHC actually picks
/// — a caller supplying a `result`-targeted expr template alongside an
/// `__result`-targeted bind template has no single `--target` value that
/// works for both. Confirmed empirically against the built extract:
/// `--target result` against a module that only declares `__result` fails
/// with `translateModule: exported top-level binding 'result' not found`.
/// So every compiling template here shares `__result`, and `run_turn` is
/// called with no `--target` override at all.
///
/// Scoped to the text AFTER the `[user-lines]` marker (`template_haskell`'s
/// own `result ::`/`result = do` lines always follow it) so a `result ::`/
/// `result =` line inside caller-supplied `helpers`/`imports` (which precede
/// the marker) is never touched.
fn retarget_result_binder(src: &str) -> String {
    const MARKER: &str = " -- [user-lines] ";
    let Some(marker_pos) = src.find(MARKER) else {
        return src.to_string();
    };
    let (head, tail) = src.split_at(marker_pos);
    let tail = tail
        .replacen("\nresult :: Eff ", "\n__result :: Eff ", 1)
        .replacen("\nresult = do\n", "\n__result = do\n", 1);
    format!("{head}{tail}")
}

/// The 1-based inclusive line count `code` occupies once embedded — mirrors
/// `tidepool_mcp::eval_prep`'s `TurnTemplate::render` end-line computation
/// exactly (an empty block is 1 line; a trailing newline doesn't count as an
/// extra line).
pub(crate) fn content_line_count(code: &str) -> usize {
    if code.is_empty() {
        1
    } else if code.ends_with('\n') {
        code.matches('\n').count()
    } else {
        code.matches('\n').count() + 1
    }
}

/// Rewrite a `-- [user-lines] S:E` annotation's END line to
/// `start + content_lines - 1`, leaving the START line untouched. `src` is
/// expected to contain exactly one such marker (as every [`template_turn`]
/// output does); a src without one is returned unchanged rather than panicking
/// — a caller error surfaces downstream as an unrepaired annotation, not here.
fn repair_user_lines_end(src: &str, content_lines: usize) -> String {
    const MARKER: &str = " -- [user-lines] ";
    let Some(pos) = src.find(MARKER) else {
        return src.to_string();
    };
    let after = &src[pos + MARKER.len()..];
    let Some(colon) = after.find(':') else {
        return src.to_string();
    };
    let start_str = &after[..colon];
    let Ok(start) = start_str.parse::<usize>() else {
        return src.to_string();
    };
    let rest = &after[colon + 1..];
    let end_digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let real_end = start + content_lines - 1;

    let mut out = String::with_capacity(src.len());
    out.push_str(&src[..pos + MARKER.len()]);
    out.push_str(start_str);
    out.push(':');
    out.push_str(&real_end.to_string());
    out.push_str(&rest[end_digits..]);
    out
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine setup failed: {0}")]
    Setup(String),
    #[error("provider call failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("turn compile failed:\n{0}")]
    Compile(String),
    #[error("resident turn failed: {0}")]
    Run(String),
    #[error("the model produced no runnable ```haskell block after {turns} turns")]
    NoBlock { turns: u32 },
}

/// The outcome of driving ONE model turn against a resident session.
pub enum TurnOutcome {
    /// The compiled block ran to completion; the node is done.
    Completed { rendered: String },
    /// The compiled block suspended at an `AskWith`; the hole is classified and
    /// the machine's continuation id is `hole`. The compiled turn's table is
    /// NOT carried here — an answer's value bridges against the constructor
    /// set stashed in the node's `convo.suspend_table`.
    Suspended {
        hole: String,
        classified: ClassifiedHole,
    },
    /// The model replied with no runnable block — the caller decides whether to
    /// loop (feed a nudge) or stop.
    NoBlock { reply: String },
}

/// One assistant turn's provider result plus its extracted blocks (empty for
/// a pure-prose reply).
pub struct DrivenTurn {
    pub reply: String,
    pub usage: Usage,
    /// The turn's reasoning-summary ("thinking"), when the provider surfaced one.
    pub reasoning: Option<String>,
    /// The encrypted reasoning items the provider surfaced for this turn (see
    /// [`ReasoningItem`]) — distinct from `reasoning` above.
    pub reasoning_items: Vec<ReasoningItem>,
    /// Every fenced ```haskell block in the reply, in order — the runnable
    /// sequence ([`extract_haskell_blocks`]).
    pub blocks: Vec<String>,
}

/// Call the provider once with the assembled transcript and extract the block.
/// `framing` is the node's per-turn system message (see [`assemble_request`]);
/// `sink`, when `Some`, receives streaming deltas as the provider reads them.
pub async fn drive_model_turn(
    provider: &dyn DynModelProvider,
    transcript: &[Message],
    max_tokens: Option<u32>,
    framing: Option<&str>,
    sink: Option<StreamSink>,
) -> Result<DrivenTurn, EngineError> {
    let req = assemble_request(transcript, max_tokens, framing);
    let TurnResponse {
        text,
        usage,
        reasoning,
        reasoning_items,
    } = provider.complete_boxed(req, sink).await?;
    let blocks = extract_haskell_blocks(&text);
    if let Some(r) = reasoning.as_deref().filter(|r| !r.is_empty()) {
        tracing::info!("model reasoning:\n{r}");
    }
    Ok(DrivenTurn {
        reply: text,
        usage,
        reasoning,
        reasoning_items,
        blocks,
    })
}

/// Bridge a JSON answer (from a form submission or an in-context resume value)
/// to a Core `Value` against `table`, for feeding to `ResidentSession::resume`.
pub fn json_answer_to_value(answer: &Json, table: &DataConTable) -> Result<Value, EngineError> {
    use tidepool_bridge::ToCore;
    answer
        .to_value(table)
        .map_err(|e| EngineError::Run(format!("bridge answer to Value: {e}")))
}

/// Assemble N raw per-child answer `Value`s into a genuine `[T]` list
/// `Value` for a `runLLMTurnFanout` resume — the same "hand back the native
/// representation, not an Aeson wrapper" discipline a single fork's
/// `unsafeCoerce` relies on. `items` must
/// already be in declaration order; `table` only needs to know the
/// always-wired-in `:`/`[]` constructors (any `DataConTable` from the same
/// compiled program qualifies — `DataConId`s are stable hashes, not
/// table-local indices, so a table from a DIFFERENT compile of the same
/// program resolves to the same ids, exactly how a single fork's answer
/// already crosses from the child's compiled table into the parent's heap).
pub fn build_list_value(items: Vec<Value>, table: &DataConTable) -> Result<Value, EngineError> {
    let nil_id = tidepool_bridge::get_resilient(table, "[]", 0).ok_or_else(|| {
        EngineError::Run("build_list_value: no [] constructor in table".to_string())
    })?;
    let cons_id = tidepool_bridge::get_resilient(table, ":", 2).ok_or_else(|| {
        EngineError::Run("build_list_value: no : constructor in table".to_string())
    })?;
    let mut result = Value::Con(nil_id, vec![]);
    for item in items.into_iter().rev() {
        result = Value::Con(cons_id, vec![item, result]);
    }
    Ok(result)
}

/// Wrap a frozen-snapshot digest as a genuine `ContextRef` `Value` — the Core
/// counterpart of `Tidepool.Effects`'s `data ContextRef = ContextRef Text`
/// (spliced into every `RunLLMTurn`-row compile's generated module, so
/// `"ContextRef"` always resolves in `table` there), for resuming a
/// `freezeContext`/`runLLMTurnBranch` continuation with a directly
/// constructed value rather than an Aeson round-trip — the same "hand back
/// the native representation" discipline [`build_list_value`] uses for `[T]`.
pub fn build_context_ref_value(digest: &str, table: &DataConTable) -> Result<Value, EngineError> {
    use tidepool_bridge::ToCore;
    let con_id = tidepool_bridge::get_resilient(table, "ContextRef", 1).ok_or_else(|| {
        EngineError::Run("build_context_ref_value: no ContextRef constructor in table".to_string())
    })?;
    let text = digest
        .to_string()
        .to_value(table)
        .map_err(|e| EngineError::Run(format!("bridge digest to Value: {e}")))?;
    Ok(Value::Con(con_id, vec![text]))
}

/// Assemble a genuine 2-tuple `Value` — `(a, b)` — for a `runLLMTurnBranch`
/// resume: the pair counterpart of [`build_list_value`]'s list assembly, over
/// the always-wired-in `"(,)"` constructor.
pub fn build_pair_value(a: Value, b: Value, table: &DataConTable) -> Result<Value, EngineError> {
    let pair_id = tidepool_bridge::get_resilient(table, "(,)", 2).ok_or_else(|| {
        EngineError::Run("build_pair_value: no (,) constructor in table".to_string())
    })?;
    Ok(Value::Con(pair_id, vec![a, b]))
}

/// Best-effort: pull the recursive companion's own `NodePath` text out of a
/// coalgebra prompt shaped `"NODE {path} — DISCOVER (...)"`
/// (`harness-dogfooding/recursive-companion/Harness.hs`'s `coalgebraPrompt`,
/// which renders exactly that literal prefix — not merely a test needle).
/// This is the ONLY place a delegating branch child's domain identity is
/// observable from the runtime side: `NodePath` never crosses into Rust as a
/// typed value (`HarnessTypes.hs`'s own module doc explains why — a window
/// type declared beside `loop` is unnameable by the answerer it is asked to
/// finalize), so parsing the one place it is already rendered as text is the
/// least invasive correlation available. `None` for any other harness's
/// prompt shape — delegation-branch recording ([`crate::selfharness::driver::
/// SelfHarnessDriver::branch_node_paths`]) is then simply never populated,
/// which is harmless: no other harness calls `delegate`.
pub(crate) fn parse_companion_node_path(prompt: &str) -> Option<String> {
    let rest = prompt.strip_prefix("NODE ")?;
    let (path, _) = rest.split_once(" — DISCOVER")?;
    Some(path.to_string())
}

/// Decode a completed `SubagentAwait` response (`Either SpawnError
/// SpawnOutcome`) into the delegated cycle's bound worktree branch, on a
/// `Right` — `None` on a `Left` (the delegation failed; nothing to record)
/// or any malformed shape.
///
/// `tidepool_bridge_effects::AgSpawnOutcome` (the Haskell `SpawnOutcome`'s
/// wire type) is deliberately ToCore-ONLY — never round-tripped back into a
/// Rust struct as a whole (its own doc: the model never gets to hand this
/// back). But its `outcome_run` FIELD is `AgWorkerRun`, and — like every
/// other bridged Worktree/Agent record — `AgWorkerRun` derives `FromCore`
/// too; only the OUTER container skips the derive. So this decodes the
/// OUTER `Either`/`SpawnOutcome` shell by hand (two ordinary `Con` peels —
/// the constructor names and field ORDER are the wire contract, exactly as
/// `tidepool-bridge-effects/src/generated/*.rs`'s module doc states), then
/// hands the ONE nested field that matters to a real typed decode.
pub(crate) fn decode_completed_delegation_branch(
    value: &Value,
    table: &DataConTable,
) -> Option<String> {
    use tidepool_bridge::FromCore;
    if con_name(value, table) != Some("Right") {
        return None;
    }
    let Value::Con(_, right_fields) = value else {
        return None;
    };
    let outcome = right_fields.first()?;
    let Value::Con(_, outcome_fields) = outcome else {
        return None;
    };
    // `AgSpawnOutcome`'s first field is `outcome_run: AgWorkerRun`.
    let run_field = outcome_fields.first()?;
    let run = tidepool_bridge_effects::AgWorkerRun::from_value(run_field, table).ok()?;
    Some(run.run_worktree.handle_receipt.branch.raw)
}

/// Why one forked cognition window ended WITHOUT a typed answer — the Rust
/// side of the `InvocationExit` generated into `Tidepool.Effects`
/// (`tidepool_mcp::runllmturn_effect_def!`'s `type_defs`). The constructor
/// names here ARE that ADT's, and [`build_invocation_exit_value`] resolves
/// them by name against the turn's own `DataConTable`.
///
/// **The line this type draws** (PRD 21 locked decision 6): an
/// `InvocationExit` describes a failure ATTRIBUTABLE TO ONE CHILD'S WINDOW —
/// its rounds ran out, it ended on something that is not an answer, it was
/// cancelled, its own provider call failed. Those fold as DATA at that
/// child's branch position so its siblings' finished results survive. A
/// failure of the MECHANISM around the children — fan cardinality, list/sum
/// assembly against the table, session bookkeeping, the per-loop
/// inference-call runaway cap — is NOT an `InvocationExit` and must hard-fail
/// the turn: reporting a broken mechanism as "the model failed" would be a
/// false receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationExit {
    /// The window burned its round budget without finalizing.
    RoundsExhausted(String),
    /// The window ended on something that is not an answer.
    NotFinalized(String),
    /// The window was cancelled before it could answer. No producer in the
    /// self-harness driver today — cancellation of a live branch is PRD 21
    /// lane C5's (draining a recursive scope under structured concurrency).
    /// The constructor exists because decision 6 enumerates it and a caller
    /// matching exhaustively should not have to be rewritten when C5 lands.
    Cancelled(String),
    /// The window's own turn failed at runtime (its provider call errored).
    RuntimeFailure(String),
}

impl InvocationExit {
    /// The Haskell constructor name this variant builds — the one place the
    /// Rust variant ↔ `Tidepool.Effects` constructor correspondence is
    /// spelled.
    fn constructor(&self) -> &'static str {
        match self {
            InvocationExit::RoundsExhausted(_) => "ExitRoundsExhausted",
            InvocationExit::NotFinalized(_) => "ExitNotFinalized",
            InvocationExit::Cancelled(_) => "ExitCancelled",
            InvocationExit::RuntimeFailure(_) => "ExitRuntimeFailure",
        }
    }

    /// The detail text the constructor carries.
    pub fn detail(&self) -> &str {
        match self {
            InvocationExit::RoundsExhausted(d)
            | InvocationExit::NotFinalized(d)
            | InvocationExit::Cancelled(d)
            | InvocationExit::RuntimeFailure(d) => d,
        }
    }
}

impl std::fmt::Display for InvocationExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.constructor(), self.detail())
    }
}

/// Build the `InvocationExit` `Value` for `exit` against `table`.
///
/// Same loud-failure discipline as [`build_list_value`]: a constructor the
/// table does not carry is a HARD error, never a defaulted or omitted value.
/// The alternative — resuming with some other constructor — would feed the
/// parent's `case` a value of the wrong shape, which case-traps far from the
/// cause.
pub fn build_invocation_exit_value(
    exit: &InvocationExit,
    table: &DataConTable,
) -> Result<Value, EngineError> {
    use tidepool_bridge::ToCore;
    let name = exit.constructor();
    let con = tidepool_bridge::get_resilient(table, name, 1).ok_or_else(|| {
        EngineError::Run(format!(
            "build_invocation_exit_value: no `{name}` constructor in table — the \
             compiling row generated no `InvocationExit`, so a fork/fanout child's \
             typed exit cannot be delivered"
        ))
    })?;
    let detail =
        exit.detail().to_string().to_value(table).map_err(|e| {
            EngineError::Run(format!("build_invocation_exit_value: detail text: {e}"))
        })?;
    Ok(Value::Con(con, vec![detail]))
}

/// Assemble ONE fork/fanout child's outcome into the `Either InvocationExit T`
/// `Value` its branch position resumes with — `Ok(v)` → `Right v`, `Err(exit)`
/// → `Left (…)`.
///
/// This is the shape `runLLMTurnFork`/`runLLMTurnFanout` promise (PRD 21
/// locked decision 6); `Tidepool.Fork`'s `fork`/`forkAll` do NOT go through
/// it — they still resume with a bare `T` (see [`ForkSource`]).
/// [`build_list_value`]'s loud-failure discipline throughout: a missing
/// `Left`/`Right` is a hard error.
pub fn build_child_answer_value(
    outcome: Result<Value, InvocationExit>,
    table: &DataConTable,
) -> Result<Value, EngineError> {
    let (name, payload) = match outcome {
        Ok(v) => ("Right", v),
        Err(exit) => ("Left", build_invocation_exit_value(&exit, table)?),
    };
    let con = tidepool_bridge::get_resilient(table, name, 1).ok_or_else(|| {
        EngineError::Run(format!(
            "build_child_answer_value: no `{name}` constructor in table — a \
             fork/fanout answer is `Either InvocationExit T`, so both `Left` and \
             `Right` must be reachable from the compiling row"
        ))
    })?;
    Ok(Value::Con(con, vec![payload]))
}

/// Shared handle to a provider, so the engine and its forked answerers all use
/// the same signed-in client.
pub type SharedProvider = Arc<dyn DynModelProvider>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Message, ReasoningItem, Role};

    fn user(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.to_string(),
            reasoning_items: Vec::new(),
        }
    }

    fn assistant(content: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: content.to_string(),
            reasoning_items: Vec::new(),
        }
    }

    /// A `Some(framing)` becomes the request's System message verbatim,
    /// NOT the default `SYSTEM_FRAMING` — the self-iterating harness's
    /// `render` output must reach the model as-is.
    #[test]
    fn assemble_request_uses_framing_as_system_message() {
        let framing = "RENDERED: you are in Deciding mode, loop 3.";
        let transcript = [user("answer me")];
        let req = assemble_request(&transcript, Some(2048), Some(framing));

        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(
            req.messages[0].content, framing,
            "the framing must be the System message verbatim"
        );
        assert_ne!(
            req.messages[0].content, SYSTEM_FRAMING,
            "a Some(framing) must OVERRIDE the default SYSTEM_FRAMING"
        );
        // The transcript follows the system message in order.
        assert_eq!(req.messages[1].content, "answer me");
    }

    /// A `None` framing falls back to the default full-surface `SYSTEM_FRAMING`
    /// — the shape every ordinary Agent node still uses.
    #[test]
    fn assemble_request_none_framing_falls_back_to_default() {
        let req = assemble_request(&[user("hi")], Some(2048), None);
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages[0].content, SYSTEM_FRAMING);
    }

    // -- within-window prefix stability (cache-prefix wave) -----------------
    //
    // `assemble_request` is the ONE assembly path — `[system(framing)] ++
    // transcript verbatim` — so provider-side prompt-cache reuse within a
    // window depends entirely on every round's request being a
    // byte-identical-prefix EXTENSION of the previous round's. These three
    // tests script the three within-window round shapes and diff the
    // serialized request bytes directly, rather than trusting that the code
    // that builds each round only ever appends.

    /// The byte stream a provider actually walks for one round: each
    /// assembled message serialized INDEPENDENTLY and concatenated, in
    /// order — NOT the whole `TurnRequest` as one JSON value. A single
    /// enclosing array/object necessarily closes at the true end (`]}`),
    /// so a shorter round's whole-request bytes can never be a literal
    /// prefix of a longer round's even when every message is byte-identical
    /// — that mismatch is a JSON-envelope artifact, not evidence of
    /// instability. A provider's own request shape mirrors this: the
    /// Responses-API `input` array is built by mapping each `Message` to
    /// its own item(s) and concatenating (`oauth.rs`'s `to_input_items`),
    /// so per-message concatenation is the right level to assert prefix
    /// stability at. `framing`/`max_tokens` held fixed across the window,
    /// exactly as every real round does (`convo.framing` is set once per
    /// node and never rewritten mid-window; see `harness.rs`).
    fn round_bytes(transcript: &[Message], framing: Option<&str>) -> Vec<u8> {
        assemble_request(transcript, Some(2048), framing)
            .messages
            .iter()
            .flat_map(|m| serde_json::to_vec(m).expect("Message serializes"))
            .collect()
    }

    /// `after` must be `before` verbatim plus zero or more bytes appended —
    /// the property that lets a provider's automatic prompt cache actually
    /// engage from round 2 on (`tidepool-harness/CLAUDE.md`'s "provider
    /// cache-metric gap"). A shorter `after`, or any byte inside `before`'s
    /// span changing, means round N+1 was not a pure append over round N.
    fn assert_prefix_extension(before: &[u8], after: &[u8]) {
        assert!(
            after.len() >= before.len(),
            "round shrank: {} bytes -> {} bytes",
            before.len(),
            after.len()
        );
        assert_eq!(
            &after[..before.len()],
            before,
            "round N+1's assembled request is not a byte-identical-prefix \
             extension of round N's"
        );
    }

    /// Shape (a): an ORDINARY next round — the assistant's reply lands, then
    /// the driver appends its own next user turn ("round complete, your
    /// window continues"). Pins that `assemble_request` never needs to touch
    /// anything already sent.
    #[test]
    fn within_window_ordinary_round_is_prefix_extension() {
        let framing = Some("You answer typed holes. Row: [AskUser, Finalize].");
        let mut transcript = vec![user(
            "The loop needs a typed answer of type `Decision`. Please decide.",
        )];
        let round1 = round_bytes(&transcript, framing);

        transcript.push(assistant(
            "```haskell\nsubstrate0 = Substrate { samples = [2,3], revision = 0 }\n```",
        ));
        transcript.push(user(
            "Round complete — your window continues, and that round's \
             definitions/bindings persist. The request still awaits its \
             answer: when ready, evaluate `finalize @Decision value`.",
        ));
        let round2 = round_bytes(&transcript, framing);

        assert_prefix_extension(&round1, &round2);
    }

    /// Shape (b): a CORRECTIVE-RETRY round — the model's Haskell didn't
    /// compile, so the driver feeds the GHC error back verbatim as the next
    /// user turn (`run_to_hole_or_done`/`drive_answerer_to_finalize`'s shared
    /// idiom). The corrective text is per-round-variable (it embeds a fresh
    /// GHC error each time) — it must land at the TAIL, never rewrite
    /// anything earlier, across repeated corrective rounds.
    #[test]
    fn within_window_corrective_retry_round_is_prefix_extension() {
        let framing = Some("You answer typed holes. Row: [AskUser, Finalize].");
        let mut transcript = vec![user("The loop needs a typed answer of type `Decision`.")];
        let round1 = round_bytes(&transcript, framing);

        transcript.push(assistant("```haskell\nfinalize @Decision (Approve\n```"));
        transcript.push(user(&format!(
            "A block did not compile — your window continues; everything \
             that already ran persists. Reply with corrected ```haskell \
             blocks.\n\nGHC error:\n{}",
            "parse error on input '}' (round 3 corrective retry)"
        )));
        let round2 = round_bytes(&transcript, framing);
        assert_prefix_extension(&round1, &round2);

        // A SECOND corrective retry (a different round, a different GHC
        // error) — still only ever appended.
        transcript.push(assistant("```haskell\nfinalize @Decision (Approve\n```"));
        transcript.push(user(&format!(
            "A block did not compile — your window continues; everything \
             that already ran persists. Reply with corrected ```haskell \
             blocks.\n\nGHC error:\n{}",
            "parse error on input '}' (round 4 corrective retry)"
        )));
        let round3 = round_bytes(&transcript, framing);
        assert_prefix_extension(&round2, &round3);
    }

    /// Shape (c): a round FOLLOWING a note/askUser resume. `answer_dialog`/
    /// `answer_note` resume the suspended Haskell continuation directly
    /// (`Harness::resume_parent`) WITHOUT pushing anything to the transcript
    /// themselves — no model call happens for the resume itself. The next
    /// actual model round therefore sees the transcript exactly as the
    /// suspending assistant turn left it, plus whatever corrective/nudge
    /// text the driver appends once the resumed chain settles. Pinned
    /// separately from shape (a) so a future change to the resume path
    /// cannot silently start rewriting the pre-suspension transcript.
    #[test]
    fn within_window_round_after_note_askuser_resume_is_prefix_extension() {
        let framing = Some("You answer typed holes. Row: [AskUser, Finalize].");
        let mut transcript = vec![user("The loop needs a typed answer of type `Decision`.")];
        transcript.push(assistant(
            "```haskell\nnote \"about to ask\"\nchoice <- askUser @Confirm \"proceed?\"\npure ()\n```",
        ));
        // The assistant's askUser-suspending turn is the last thing on the
        // wire before the resume — this is what round 1 actually sent.
        let round1 = round_bytes(&transcript, framing);

        // `answer_note`/`answer_dialog` resume server-side here — NO
        // transcript push, hence no intermediate round to capture. The chain
        // resolves without finalizing, so the driver appends its corrective
        // nudge before the next model round.
        transcript.push(user(
            "That did not resolve the request. Answer by evaluating \
             `(finalize @Decision value :: M Decision)` — the whole \
             expression must carry the type annotation, not just the argument.",
        ));
        let round2 = round_bytes(&transcript, framing);

        assert_prefix_extension(&round1, &round2);
    }

    /// Shape (d): the actual PROVIDER-VISIBLE prefix, reasoning items
    /// included. `round_bytes` above serializes each `Message` directly via
    /// `serde_json::to_vec`, but `Message::reasoning_items` is
    /// `#[serde(skip)]` — so shapes (a)-(c) can never observe what the real
    /// wire carries once an assistant turn has reasoning state, and cannot
    /// catch a regression there. The real request-assembly path
    /// (`oauth.rs`'s `codex_responses`) excludes system messages (they ride
    /// `instructions`) and flat_maps every other message through
    /// `to_input_items`, which echoes `reasoning_items` ahead of the message
    /// item itself — this test calls that SAME function (not a re-derived
    /// mirror) to pin the item sequence, not just role+content.
    fn wire_items(transcript: &[Message]) -> Vec<serde_json::Value> {
        transcript
            .iter()
            .filter(|m| m.role != Role::System)
            .flat_map(crate::provider::oauth::to_input_items)
            .collect()
    }

    #[test]
    fn within_window_reasoning_items_extend_the_provider_visible_prefix() {
        let mut transcript = vec![user("The loop needs a typed answer of type `Decision`.")];
        let round1 = wire_items(&transcript);

        transcript.push(Message {
            role: Role::Assistant,
            content: "```haskell\nfinalize @Decision Approve\n```".to_string(),
            reasoning_items: vec![ReasoningItem(serde_json::json!({
                "type": "reasoning",
                "id": "rs_1",
                "encrypted_content": "opaque-blob-1",
            }))],
        });
        transcript.push(user(
            "Round complete — your window continues, and that round's \
             definitions/bindings persist.",
        ));
        let round2 = wire_items(&transcript);

        assert!(
            round2.len() >= round1.len(),
            "round shrank: {} items -> {} items",
            round1.len(),
            round2.len()
        );
        assert_eq!(
            &round2[..round1.len()],
            &round1[..],
            "round N+1's provider-visible item sequence is not a pure \
             extension of round N's once reasoning_items are involved"
        );
        // The reasoning item genuinely reached the wire ahead of its
        // message, at the position `to_input_items` puts it — not merely
        // "the lengths worked out".
        assert_eq!(round2[round1.len()]["type"], "reasoning");
        assert_eq!(round2[round1.len()]["id"], "rs_1");
        assert_eq!(round2[round1.len() + 1]["type"], "message");
    }

    // -- multi-block extraction ---------------------------------------------

    /// Every ```haskell block is extracted, in reply order — the multi-block
    /// contract's parsing half. Each block is trim_end()ed (the `{{TURN}}`
    /// splice invariant).
    #[test]
    fn extract_haskell_blocks_returns_all_in_order() {
        let reply = "First declare:\n```haskell\ndata Mood = Rested | Wired\n```\n\
                     then use it:\n```hs\nmood <- askUser @Mood \"how?\"\n```\n\
                     done.";
        let blocks = extract_haskell_blocks(reply);
        assert_eq!(
            blocks,
            vec![
                "data Mood = Rested | Wired".to_string(),
                "mood <- askUser @Mood \"how?\"".to_string(),
            ]
        );
    }

    /// FUSED FENCES — the live-caught shape (companion dogfood, 2026-08-14):
    /// the model closes one block and opens the next on a single line
    /// (` ``````haskell `). Both blocks must extract; under the per-line
    /// parse the second silently vanished, which is exactly how the
    /// companion's first packed `askUser` lost its ask block (the `signal`
    /// not-in-scope failure).
    #[test]
    fn extract_haskell_blocks_handles_fused_fences() {
        let reply = "```haskell\ndata HarnessSignal = FormWorked\n  deriving (Generic)\n\
                     ``````haskell\ndo\n  signal <- askUser @HarnessSignal\n  pure ()\n```";
        let blocks = extract_haskell_blocks(reply);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert!(blocks[0].starts_with("data HarnessSignal"), "{blocks:?}");
        assert!(blocks[1].starts_with("do"), "{blocks:?}");

        // Fused close+bare-open (` `````` `) just closes — a bare fence is
        // still not haskell.
        let reply = "```haskell\npure ()\n``````\nprose\n```";
        assert_eq!(extract_haskell_blocks(reply), vec!["pure ()".to_string()]);
    }

    /// Bare ``` and non-haskell fences are not runnable blocks; a reply of
    /// only those extracts to empty (the NoBlock path). Case-insensitive
    /// ```HASKELL still matches; an empty haskell fence is skipped.
    #[test]
    fn extract_haskell_blocks_ignores_non_haskell_fences() {
        let reply = "```\nplain fence\n```\n```text\nquoted code\n```\n\
                     ```HASKELL\npure ()\n```\n```haskell\n\n```";
        let blocks = extract_haskell_blocks(reply);
        assert_eq!(blocks, vec!["pure ()".to_string()]);

        assert!(extract_haskell_blocks("no code here at all").is_empty());
    }

    /// The sequence-failure payload names the failed block, lists what ran
    /// (receipts) and what never ran, and states the resume point — the
    /// contract taught by the framing ("continue from that block").
    #[test]
    fn sequence_failure_context_names_ran_failed_and_unrun() {
        let receipts = vec![
            block_receipt(1, "data Mood = Rested | Wired", "declared (gen 3)"),
            block_receipt(2, "mood <- askUser @Mood \"how?\"", "bound"),
        ];
        let msg = sequence_failure_context(&receipts, 3, 4, "GHC says no");
        assert!(msg.contains("Block 3 of 4 failed"), "{msg}");
        assert!(
            msg.contains("block 1 (data Mood = Rested | Wired)"),
            "{msg}"
        );
        assert!(msg.contains("Block 4 did not run."), "{msg}");
        assert!(msg.contains("Continue from block 3"), "{msg}");
        assert!(msg.ends_with("GHC says no"), "{msg}");

        // First block failing: no receipts, an explicit (none) line, and a
        // multi-block unrun range.
        let msg = sequence_failure_context(&[], 1, 3, "boom");
        assert!(msg.contains("(none — the first block failed)"), "{msg}");
        assert!(msg.contains("Blocks 2–3 did not run."), "{msg}");

        // Last block failing: nothing unrun, no unrun line at all.
        let msg = sequence_failure_context(&receipts, 4, 4, "boom");
        assert!(!msg.contains("did not run"), "{msg}");
    }

    // -- block item splitting -------------------------------------------------

    /// A block with no blank-line-separated top-level boundary is one item —
    /// the common case, and the one `run_block` must route to its unchanged
    /// single-item path.
    #[test]
    fn split_block_items_single_expression_is_one_item() {
        assert_eq!(split_block_items("sq 7"), vec!["sq 7".to_string()]);
        assert_eq!(
            split_block_items("let xs = [1, 2, 3]\n in sum xs"),
            vec!["let xs = [1, 2, 3]\n in sum xs".to_string()]
        );
    }

    /// GHCi statement semantics: a type signature and its equation are two
    /// SEPARATE unindented lines, so they are two separate items — exactly
    /// like typing them as two separate GHCi entries. The downstream decl
    /// batcher (`run_multi_item_block`) re-joins a maximal run of
    /// consecutive decl-shaped items into one generation, so the signature
    /// and its binding still typecheck together.
    #[test]
    fn split_block_items_decl_then_expr_splits_by_line() {
        let block = "sq :: Int -> Int\nsq x = x * x\n\nsq 7";
        assert_eq!(
            split_block_items(block),
            vec![
                "sq :: Int -> Int".to_string(),
                "sq x = x * x".to_string(),
                "sq 7".to_string()
            ]
        );
    }

    /// An indented continuation (inside a `do`/`where` block) never starts a
    /// new item — only a non-indented line does. Blank lines inside the
    /// continuation don't split it either.
    #[test]
    fn split_block_items_indented_continuation_stays_in_one_item() {
        let block = "f = do\n  x <- pure 1\n\n  pure (x + 1)";
        assert_eq!(split_block_items(block), vec![block.to_string()]);
    }

    /// A multi-line `data` record declaration (every field line indented)
    /// stays one item, no blank line required.
    #[test]
    fn split_block_items_data_record_decl_stays_one_item() {
        let block = "data Foo = Foo\n  { fooA :: Int\n  , fooB :: Text\n  }";
        assert_eq!(split_block_items(block), vec![block.to_string()]);
    }

    /// A `where`-continued declaration (the clause indented under the
    /// equation it belongs to) stays one item.
    #[test]
    fn split_block_items_where_continuation_stays_one_item() {
        let block = "f x = y\n  where y = x + 1";
        assert_eq!(split_block_items(block), vec![block.to_string()]);
    }

    /// Three items in a row (two helper decls, then the answer) all split out
    /// — blank lines between them are incidental, not required (each is
    /// already its own unindented line).
    #[test]
    fn split_block_items_three_items() {
        let block = "helper1 x = x + 1\n\nhelper2 x = x * 2\n\nhelper2 (helper1 5)";
        assert_eq!(
            split_block_items(block),
            vec![
                "helper1 x = x + 1".to_string(),
                "helper2 x = x * 2".to_string(),
                "helper2 (helper1 5)".to_string(),
            ]
        );
    }

    /// The motivating bug: contiguous GHCi-style bind lines with NO blank
    /// line between them must split into one item per statement, exactly
    /// like pasting each line at a real GHCi prompt — previously this stayed
    /// ONE item and compiled as a single wrapped `do`-expression, so the
    /// binds ran transiently and never persisted as session bindings.
    #[test]
    fn split_block_items_contiguous_binds_split_per_line() {
        let block = "seed <- pure [2, 3, 5, 7]\nrunningTotal <- pure (sum seed)\nrunningTotal + 1";
        assert_eq!(
            split_block_items(block),
            vec![
                "seed <- pure [2, 3, 5, 7]".to_string(),
                "runningTotal <- pure (sum seed)".to_string(),
                "runningTotal + 1".to_string(),
            ]
        );
    }

    /// A contiguous decl-then-binds-then-expr block (no blank lines at all)
    /// splits per unindented line, in order — the mixed-shape case.
    #[test]
    fn split_block_items_mixed_decl_binds_expr_splits_per_line() {
        let block = "sq :: Int -> Int\nsq x = x * x\nseed <- pure 1\nsq seed";
        assert_eq!(
            split_block_items(block),
            vec![
                "sq :: Int -> Int".to_string(),
                "sq x = x * x".to_string(),
                "seed <- pure 1".to_string(),
                "sq seed".to_string(),
            ]
        );
    }

    // -- hole card type synopsis --------------------------------------------

    use tidepool_repr::{DataCon, DataConId};

    fn nullary_dc(id: u64, name: &str, tag: u32, type_name: &str) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some(format!("{type_name}.{name}")),
            type_name: type_name.to_string(),
        }
    }

    fn record_table() -> DataConTable {
        let mut table = DataConTable::new();
        let dc = DataCon {
            id: DataConId(1),
            name: "Contribution".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Contribution.Contribution".to_string()),
            type_name: "Contribution".to_string(),
        };
        table.insert(dc.clone());
        table.set_field_labels(
            dc.id,
            vec![
                "addedIdeas".to_string(),
                "draftDelta".to_string(),
                "advance".to_string(),
            ],
        );
        table.set_field_types(
            dc.id,
            vec!["[Text]".to_string(), "Text".to_string(), "Bool".to_string()],
        );
        table
    }

    fn nullary_sum_table() -> DataConTable {
        let mut table = DataConTable::new();
        table.insert(nullary_dc(3, "Abort", 3, "Verdict"));
        table.insert(nullary_dc(1, "Advance", 1, "Verdict"));
        table.insert(nullary_dc(2, "Hold", 2, "Verdict"));
        table
    }

    #[test]
    fn hole_card_renders_record_selector_names_and_types() {
        let table = record_table();
        let card = hole_card("answer this", Some("Contribution"), Some(&table));
        assert!(
            card.contains(
                "Contribution { addedIdeas :: [Text], draftDelta :: Text, advance :: Bool }"
            ),
            "{card}"
        );
    }

    #[test]
    fn answerer_hole_card_renders_nullary_sum_in_tag_order() {
        let table = nullary_sum_table();
        let card = answerer_hole_card("decide", Some("Verdict"), &[], Some(&table), &[]);
        assert!(card.contains("Advance | Hold | Abort"), "{card}");
    }

    /// The card states the window's ACTUAL effect row when the caller
    /// supplies one — a branch child's inherited framing may describe a
    /// different row, and a window must never have to discover its
    /// capabilities through compile-error rounds (dogfood, 2026-08-20).
    /// An empty row (callers without one) adds no line.
    #[test]
    fn answerer_hole_card_states_the_effect_row() {
        let row: Vec<String> = ["Subagent", "AskUser", "Finalize"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let card = answerer_hole_card("decide", Some("Verdict"), &[], None, &row);
        assert!(
            card.contains("Your effect row THIS WINDOW is `[Subagent, AskUser, Finalize]`"),
            "{card}"
        );

        let bare = answerer_hole_card("decide", Some("Verdict"), &[], None, &[]);
        assert!(!bare.contains("effect row"), "{bare}");
    }

    /// PRD 21 C5: the hole card's effect-row line must teach the row a
    /// delegating window's model text actually compiles against —
    /// `Delegate`, never `Subagent`/`Worktree`, which the machine dispatches
    /// but the model's own block can never name.
    #[test]
    fn hole_card_effect_row_is_delegate_aware() {
        let names: Vec<String> = ["Subagent", "Worktree", "AskUser", "Fork", "Finalize"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let non_delegating = EngineConfig::inert(names.clone());
        assert_eq!(non_delegating.hole_card_effect_row(), names);

        let mut delegating = EngineConfig::inert(names);
        delegating.delegate_wrap = true;
        assert_eq!(
            delegating.hole_card_effect_row(),
            vec!["Delegate", "AskUser", "Fork", "Finalize"]
        );
    }

    /// A positional-sum answer type — no field labels at all — renders as a
    /// fenced `data` document.
    #[test]
    fn hole_card_renders_positional_sum_instead_of_degrading() {
        let mut table = DataConTable::new();
        let circle = DataCon {
            id: DataConId(1),
            name: "Circle".to_string(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: Some("Shape.Circle".to_string()),
            type_name: "Shape".to_string(),
        };
        table.insert(circle.clone());
        table.set_field_types(circle.id, vec!["Double".to_string()]);
        let rect = DataCon {
            id: DataConId(2),
            name: "Rect".to_string(),
            tag: 2,
            rep_arity: 2,
            field_bangs: vec![],
            qualified_name: Some("Shape.Rect".to_string()),
            type_name: "Shape".to_string(),
        };
        table.insert(rect.clone());
        table.set_field_types(rect.id, vec!["Double".to_string(), "Double".to_string()]);

        let card = hole_card("answer this", Some("Shape"), Some(&table));
        assert!(card.contains("Its shape:"), "{card}");
        assert!(
            card.contains("Circle Double | Rect Double Double"),
            "{card}"
        );
    }

    /// The per-hole card carries no verb documentation at all — the generated
    /// Available-effects section moved to SYSTEM-level framing
    /// ([`crate::selfharness::driver::answerer_framing_suffix`]), sent once
    /// per loop instead of re-narrated on every hole.
    #[test]
    fn answerer_hole_card_carries_no_hand_written_verb_docs() {
        let table = nullary_sum_table();
        let card = answerer_hole_card("decide", Some("Verdict"), &[], Some(&table), &[]);
        assert!(
            !card.contains("choose [(label"),
            "the per-hole card must not hand-narrate `choose`: {card}"
        );
        assert!(
            !card.contains("Available effects"),
            "the per-hole card must not carry the generated section: {card}"
        );
    }

    /// [`available_effects_section`] folds over the ACTUAL decl list passed
    /// in — a row with a different effect set yields a correspondingly
    /// different section, and every decl's `type_name` appears.
    #[test]
    fn available_effects_section_reflects_the_actual_decl_row() {
        let narrow = [tidepool_mcp::askuser_decl()];
        let wide = [
            tidepool_mcp::askuser_decl(),
            tidepool_mcp::fork_decl(),
            tidepool_mcp::finalize_decl(),
        ];

        let narrow_section = available_effects_section(&narrow);
        let wide_section = available_effects_section(&wide);

        assert!(narrow_section.contains("AskUser"));
        assert!(!narrow_section.contains("Fork"));
        assert!(!narrow_section.contains("Finalize"));
        for decl in &wide {
            assert!(
                wide_section.contains(decl.type_name),
                "missing {} in: {wide_section}",
                decl.type_name
            );
        }
        assert_ne!(
            narrow_section, wide_section,
            "a different row must yield a different section"
        );
    }

    /// A decl with a `prompt_card` set uses it (not the full `description`
    /// essay) — the compact-per-turn grain the fold is meant to produce.
    #[test]
    fn available_effects_section_prefers_prompt_card_over_description() {
        let decl = tidepool_mcp::finalize_decl();
        let card = decl
            .prompt_card
            .expect("finalize_decl must set a compact prompt_card");
        let section = available_effects_section(&[decl]);
        assert!(section.contains(card), "{section}");
    }

    /// Mutation-close the degrade path at the hole-card level: an unsupported
    /// shape (here, a type the table has no constructors for) must NOT add a
    /// partial/invented shape line — the card falls back to naming the type
    /// alone (already stated elsewhere in the card text).
    #[test]
    fn hole_card_unsupported_shape_adds_no_shape_line() {
        let table = DataConTable::new();
        let card = hole_card("answer this", Some("Mystery"), Some(&table));
        assert!(
            !card.contains("Its shape:"),
            "an unsupported shape must not render a shape line: {card}"
        );
    }

    #[test]
    fn hole_card_with_no_table_adds_no_shape_line() {
        let card = hole_card("answer this", Some("Contribution"), None);
        assert!(!card.contains("Its shape:"), "{card}");
    }

    /// Derived from the table, not hardcoded: a renamed field changes what
    /// the hole card shows.
    #[test]
    fn hole_card_shape_follows_table_field_rename() {
        let table = record_table();
        let card = hole_card("answer this", Some("Contribution"), Some(&table));
        assert!(card.contains("addedIdeas"), "{card}");

        let mut renamed = DataConTable::new();
        let dc = DataCon {
            id: DataConId(1),
            name: "Contribution".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Contribution.Contribution".to_string()),
            type_name: "Contribution".to_string(),
        };
        renamed.insert(dc.clone());
        renamed.set_field_labels(
            dc.id,
            vec![
                "ideasAdded".to_string(),
                "deltaDraft".to_string(),
                "advance".to_string(),
            ],
        );
        renamed.set_field_types(
            dc.id,
            vec!["[Text]".to_string(), "Text".to_string(), "Bool".to_string()],
        );
        let renamed_card = hole_card("answer this", Some("Contribution"), Some(&renamed));
        assert!(renamed_card.contains("ideasAdded"), "{renamed_card}");
        assert!(!renamed_card.contains("addedIdeas"), "{renamed_card}");
    }

    #[test]
    fn finalize_pin_active_true_for_a_real_answer_type() {
        assert!(finalize_pin_active("'[AskUser, Finalize Decision]"));
        assert!(finalize_pin_active("'[Finalize (Int -> Int)]"));
    }

    #[test]
    fn finalize_pin_active_false_for_the_noanswer_sentinel() {
        assert!(!finalize_pin_active("'[AskUser, Finalize Void]"));
    }

    #[test]
    fn finalize_pin_active_false_with_no_finalize_entry() {
        assert!(!finalize_pin_active("'[Console, KV]"));
    }

    // -- run_turn template byte-identity ------------------------------------
    //
    // `expr_turn_template`/`session_bind_template` are what `run_block` hands
    // `run_turn` as its `--turn-template` sources. These pin that splicing
    // `block`/`stmt` back into the built template (via
    // `tidepool_runtime::session::render_template`, the same substitution
    // `--turn` performs at runtime) reproduces exactly what calling the
    // underlying builder directly on the real text would produce — the only
    // thing standing between the `[user-lines]`/binder repairs and a silently
    // wrong error-line mapping or a stray placeholder reaching GHC.
    //
    // Every fixture below deliberately does NOT end in `\n`: `run_block`'s
    // only source of turn text, `extract_haskell_blocks`, always
    // `trim_end()`s each fenced block it extracts, so a turn's raw text never
    // carries a trailing newline in production. That invariant is load-bearing
    // for `expr_turn_template` specifically — its `{{TURN}}` placement is a
    // dumb VERBATIM splice with no newline normalization of its own, so it
    // relies on `template_haskell`'s fixed one-newline-after-code padding
    // (computed once, at template-build time, from the one-line placeholder)
    // matching what a trailing-newline-free real block needs. A block that
    // DID end in `\n` would double up — not exercised here because it can't
    // reach this code from `run_block`.

    use tidepool_runtime::session::render_template;

    const STACK: &str = "'[]";

    fn multiline_block() -> &'static str {
        "let x = 1\n    y = 2\nin x + y"
    }

    fn quasiquote_block() -> &'static str {
        "let s = [fmt|line one\nline two|]\nin s"
    }

    #[test]
    fn expr_turn_template_byte_identical_to_template_turn() {
        let cfg = EngineConfig::inert(vec![]);
        for block in ["1 + 1", multiline_block(), quasiquote_block()] {
            let tmpl = expr_turn_template(&cfg, STACK, block, "", "");
            let spliced = render_template(&tmpl, block, &[]);
            // `expr_turn_template` deliberately renames the compiled binder
            // from `result` to `__result` (see `retarget_result_binder`) —
            // apply the same rename to the direct-call reference so the pin
            // still catches any OTHER divergence (imports, helpers,
            // `[user-lines]` repair, splice placement).
            let direct = retarget_result_binder(&template_turn(&cfg, STACK, block, "", ""));
            assert_eq!(spliced, direct, "block: {block:?}");
        }
    }

    #[test]
    fn expr_turn_template_targets_underscore_result_not_result() {
        let cfg = EngineConfig::inert(vec![]);
        let tmpl = expr_turn_template(&cfg, STACK, "1 + 1", "", "");
        assert!(tmpl.contains("\n__result :: Eff "));
        assert!(tmpl.contains("\n__result = do\n"));
        assert!(
            !tmpl.contains("\nresult :: Eff ") && !tmpl.contains("\nresult = do\n"),
            "the retargeted template must not also carry the original `result` binder:\n{tmpl}"
        );
    }

    #[test]
    fn bind_template_byte_identical_to_template_session_bind() {
        let cfg = EngineConfig::inert(vec![]);
        for (stmt, name) in [
            ("x <- pure 1", "x"),
            ("let y = 2", "y"),
            (multiline_block(), "z"),
        ] {
            let tmpl = session_bind_template(&cfg, "{{BINDERS}}", "", "");
            let spliced = render_template(&tmpl, stmt, &[name.to_string()]);
            let direct = template_session_bind(&cfg, stmt, name, "", "");
            assert_eq!(spliced, direct, "stmt: {stmt:?}");
        }
    }

    /// The discarding-bind template is [`session_bind_template`] with the
    /// literal binder `"()"` (no `{{BINDERS}}` placeholder at all — "splicing
    /// no binder", per `plans/one-spawn-turn-protocol.md`'s four-shape note) —
    /// pinned the same way as the bind template.
    #[test]
    fn binddiscard_template_byte_identical_to_template_session_bind_with_unit_binder() {
        let cfg = EngineConfig::inert(vec![]);
        let stmt = "_ <- pure ()";
        let tmpl = session_bind_template(&cfg, "()", "", "");
        assert!(
            !tmpl.contains("{{BINDERS}}"),
            "binddiscard template must splice no binder placeholder:\n{tmpl}"
        );
        let spliced = render_template(&tmpl, stmt, &[]);
        let direct = template_session_bind(&cfg, stmt, "()", "", "");
        assert_eq!(spliced, direct);
    }

    #[test]
    fn repair_user_lines_end_fixes_only_the_end_line() {
        let src = "head\n } in __b  -- [user-lines] 5:5\ntail";
        let out = repair_user_lines_end(src, 3);
        assert_eq!(out, "head\n } in __b  -- [user-lines] 5:7\ntail");
    }

    #[test]
    fn content_line_count_matches_template_haskell_impl_convention() {
        assert_eq!(content_line_count(""), 1);
        assert_eq!(content_line_count("a"), 1);
        assert_eq!(content_line_count("a\n"), 1);
        assert_eq!(content_line_count("a\nb"), 2);
        assert_eq!(content_line_count("a\nb\n"), 2);
    }

    // -- classify_hole: routing-field validation -----------------------------
    //
    // These are continuation-ROUTING inputs (which suspended typed site a
    // reply resumes) — a malformed value must stop the turn with a
    // diagnostic, never resume/finalize a plausible-but-wrong site. See
    // `ClassifyError`'s doc.

    #[test]
    fn classify_runllmturn_payload_rejects_missing_site() {
        let asks = AsksSidecar::from_pairs(vec![]);
        let payload = serde_json::json!({});
        let err = classify_runllmturn_payload(&payload, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::MissingField {
                    constructor: "RunLLMTurnWith",
                    field: "typedSite"
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn classify_runllmturn_payload_rejects_non_numeric_site() {
        let asks = AsksSidecar::from_pairs(vec![]);
        let payload = serde_json::json!({ "typedSite": "not-a-number" });
        let err = classify_runllmturn_payload(&payload, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::MissingField {
                    constructor: "RunLLMTurnWith",
                    field: "typedSite"
                }
            ),
            "{err:?}"
        );
    }

    /// `2^32` must not alias site `0` via an `as u32` truncation.
    #[test]
    fn classify_runllmturn_payload_rejects_out_of_range_site() {
        let asks = AsksSidecar::from_pairs(vec![]);
        let huge = (u32::MAX as u64) + 1;
        let payload = serde_json::json!({ "typedSite": huge });
        let err = classify_runllmturn_payload(&payload, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::OutOfRange {
                    constructor: "RunLLMTurnWith",
                    field: "typedSite",
                    value
                } if value == huge
            ),
            "{err:?}"
        );
    }

    #[test]
    fn classify_runllmturn_payload_rejects_fan_out_of_range() {
        let asks = AsksSidecar::from_pairs(vec![(0, "Text".to_string())]);
        let huge = (u32::MAX as u64) + 1;
        let payload = serde_json::json!({
            "typedSite": 0,
            "fork": true,
            "fan": huge,
            "prompts": []
        });
        let err = classify_runllmturn_payload(&payload, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::OutOfRange {
                    constructor: "RunLLMTurnWith",
                    field: "fan",
                    value
                } if value == huge
            ),
            "{err:?}"
        );
    }

    #[test]
    fn classify_runllmturn_payload_rejects_declared_fan_prompt_mismatch() {
        let asks = AsksSidecar::from_pairs(vec![(0, "Text".to_string())]);
        let payload = serde_json::json!({
            "typedSite": 0,
            "fork": true,
            "fan": 2,
            "prompts": ["only one"]
        });
        let err = classify_runllmturn_payload(&payload, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::FanMismatch {
                    constructor: "RunLLMTurnWith",
                    declared: 2,
                    actual: 1
                }
            ),
            "{err:?}"
        );
    }

    /// A non-Text element in `prompts` must not be silently dropped: the raw
    /// array length and the filtered length disagree, which is exactly the
    /// corruption this now rejects instead of under-reporting the fan's true
    /// cardinality.
    #[test]
    fn classify_runllmturn_payload_rejects_non_text_prompt_element() {
        let asks = AsksSidecar::from_pairs(vec![(0, "Text".to_string())]);
        let payload = serde_json::json!({
            "typedSite": 0,
            "fork": true,
            "prompts": ["fine", 42, "also fine"]
        });
        let err = classify_runllmturn_payload(&payload, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::FanMismatch {
                    constructor: "RunLLMTurnWith",
                    declared: 3,
                    actual: 2
                }
            ),
            "{err:?}"
        );
    }

    /// The happy path still classifies exactly as before — the regression pin
    /// that the added validation doesn't reject well-formed wire data.
    #[test]
    fn classify_runllmturn_payload_accepts_well_formed_fanout() {
        let asks = AsksSidecar::from_pairs(vec![(3, "[Text]".to_string())]);
        let payload = serde_json::json!({
            "typedSite": 3,
            "fork": true,
            "fan": 2,
            "prompts": ["first", "second"]
        });
        let routing =
            classify_runllmturn_payload(&payload, &asks).expect("well-formed payload classifies");
        match routing {
            HoleRouting::Fork {
                site, fan, prompts, ..
            } => {
                assert_eq!(site.get(), 3);
                assert_eq!(fan, Some(FanBadge::Exact { n: 2 }));
                assert_eq!(prompts, vec!["first".to_string(), "second".to_string()]);
            }
            other => panic!("expected Fork routing, got {other:?}"),
        }
    }

    /// A bare (non-fork) `runLLMTurn` with a missing site is rejected the
    /// same way — the validation is not fork-only.
    #[test]
    fn classify_runllmturn_payload_rejects_missing_site_on_plain_turn() {
        let asks = AsksSidecar::from_pairs(vec![]);
        let payload = serde_json::json!({ "fork": false });
        assert!(classify_runllmturn_payload(&payload, &asks).is_err());
    }

    #[test]
    fn decode_finalize_site_rejects_missing_site() {
        let table = DataConTable::new();
        let asks = AsksSidecar::from_pairs(vec![]);
        let request = Value::Con(DataConId(1), vec![]);
        let err = decode_finalize_site(&request, &table, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::MissingField {
                    constructor: "FinalizeWith",
                    field: "site"
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn decode_finalize_site_rejects_out_of_range_site() {
        let table = DataConTable::new();
        let asks = AsksSidecar::from_pairs(vec![]);
        let huge = (u32::MAX as u64) + 1;
        let request = Value::Con(
            DataConId(1),
            vec![Value::Lit(tidepool_repr::Literal::LitWord(huge))],
        );
        let err = decode_finalize_site(&request, &table, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::OutOfRange {
                    constructor: "FinalizeWith",
                    field: "site",
                    value
                } if value == huge
            ),
            "{err:?}"
        );
    }

    #[test]
    fn decode_fork_one_rejects_missing_site() {
        let table = DataConTable::new();
        let request = Value::Con(DataConId(1), vec![]);
        let err = decode_fork_one(&request, &table).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::MissingField {
                    constructor: "ForkWith",
                    field: "site"
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn decode_fork_one_rejects_out_of_range_site() {
        let table = DataConTable::new();
        let huge = (u32::MAX as u64) + 1;
        let request = Value::Con(
            DataConId(1),
            vec![Value::Lit(tidepool_repr::Literal::LitWord(huge))],
        );
        let err = decode_fork_one(&request, &table).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::OutOfRange {
                    constructor: "ForkWith",
                    field: "site",
                    value
                } if value == huge
            ),
            "{err:?}"
        );
    }

    #[test]
    fn decode_fork_all_rejects_missing_site() {
        let table = DataConTable::new();
        let request = Value::Con(DataConId(1), vec![]);
        let err = decode_fork_all(&request, &table).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::MissingField {
                    constructor: "ForkAllWith",
                    field: "site"
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn decode_fork_all_rejects_out_of_range_site() {
        let table = DataConTable::new();
        let huge = (u32::MAX as u64) + 1;
        let request = Value::Con(
            DataConId(1),
            vec![
                Value::Lit(tidepool_repr::Literal::LitWord(huge)),
                Value::Con(DataConId(2), vec![]),
            ],
        );
        let err = decode_fork_all(&request, &table).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::OutOfRange {
                    constructor: "ForkAllWith",
                    field: "site",
                    value
                } if value == huge
            ),
            "{err:?}"
        );
    }

    /// A non-`Text` element among the prompts must not be silently filtered
    /// out: that would under-report the fan's true cardinality to
    /// `Harness::answer_fanout`. Builds a real `[]`/`:` cons list (the shape
    /// `value_to_json` actually renders as a JSON array) with a stray `Int`
    /// in the middle.
    #[test]
    fn decode_fork_all_rejects_non_text_prompt_element() {
        use tidepool_repr::Literal;
        let mut table = DataConTable::new();
        table.insert(dc(10, "[]", 0, 0));
        table.insert(dc(11, ":", 1, 2));
        let nil = Value::Con(DataConId(10), vec![]);
        let list = Value::Con(
            DataConId(11),
            vec![
                Value::Lit(Literal::LitString(b"fine".to_vec())),
                Value::Con(
                    DataConId(11),
                    vec![
                        Value::Lit(Literal::LitInt(42)),
                        Value::Con(
                            DataConId(11),
                            vec![Value::Lit(Literal::LitString(b"also fine".to_vec())), nil],
                        ),
                    ],
                ),
            ],
        );
        let request = Value::Con(DataConId(1), vec![Value::Lit(Literal::LitWord(0)), list]);
        let err = decode_fork_all(&request, &table).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::FanMismatch {
                    constructor: "ForkAllWith",
                    declared: 3,
                    actual: 2
                }
            ),
            "{err:?}"
        );
    }

    /// The exact scenario the review's suggested rewrite checks: a fork's
    /// declared `fan` must equal its `prompts` cardinality, not silently
    /// drift when a non-Text element is present.
    #[test]
    fn classify_hole_end_to_end_rejects_malformed_site() {
        let mut table = DataConTable::new();
        table.insert(dc(1, "FinalizeWith", 1, 2));
        let asks = AsksSidecar::from_pairs(vec![]);
        // `FinalizeWith`'s leading field is a non-numeric site.
        let request = Value::Con(
            DataConId(1),
            vec![
                Value::Lit(tidepool_repr::Literal::LitString(b"not-a-site".to_vec())),
                Value::Con(DataConId(99), vec![]),
            ],
        );
        let err = classify_hole(&request, &table, &asks).unwrap_err();
        assert!(
            matches!(
                err,
                ClassifyError::MissingField {
                    constructor: "FinalizeWith",
                    field: "site"
                }
            ),
            "{err:?}"
        );
    }

    fn dc(id: u64, name: &str, tag: u32, rep_arity: u32) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity,
            field_bangs: vec![],
            qualified_name: Some(format!("Effects.{name}")),
            type_name: "Effects".to_string(),
        }
    }
}
