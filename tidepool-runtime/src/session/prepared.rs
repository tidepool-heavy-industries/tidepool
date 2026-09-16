//! Runtime custody for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, compiled-owner construction, execution, cancellation, disposition,
//! and retained-program reuse cross this boundary in that order. The legacy
//! `CoreExpr` machine is not a fallback for any operation in this module.

use std::collections::{BTreeMap, BTreeSet};

use tidepool_bridge::Value;
use tidepool_codegen::binding_table::{
    BindingEntry, BindingTable, BoundValue, PreparedOrigin, PreparedTop,
};
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    AnswerPlan, CompileError, CompiledProgram, ExecutionError, ImportBindings, PreparedCallOptions,
    PreparedFrameEvidence, PreparedHandle, PreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as CodegenPreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
    RunOptions, MAX_ANSWER_DEPTH,
};
use tidepool_codegen::scope::ScopeId;
// Re-exported: callers of this module's realm-scoped cancellation API
// (`open_realm`/`cancel_handle`/`close_realm`) need both types without a
// separate `tidepool_codegen` dependency of their own.
pub use tidepool_codegen::jit_machine::CancelHandle;
pub use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::suspension::ContinuationId;
pub use tidepool_codegen::suspension::RealmId;
use tidepool_repr::execution_schema::{
    link_program, parse_program, DecodeLimits, Group, HeapRhs, ImportedValue, LinkError,
    LinkedProgram, MachineImports, ParseError, PreparedProgram, ProgramRequirements, RuntimeRep,
    Signature, SiteDelivery, SiteRow, SymbolIdentity, TypeNode, TypeNodeId, ValueId,
};
use tidepool_repr::{
    BindingName, DataConId, DataConTable, Generation, Literal, MonotonicIdIssuer, PrincipalId,
    SessionModule, SessionVarId, VarId,
};

use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use super::resident::SessionRunContext;
use super::turn::PREPARED_RESUME_TARGET;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedFailureKind {
    Rejected,
    Language,
    Cancelled,
    Integrity,
}

#[derive(Debug, thiserror::Error)]
pub enum PreparedRuntimeError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Link(Box<LinkError>),
    #[error("prepared execution cancelled")]
    Cancelled,
    #[error("prepared compilation rejected: {0}")]
    Compile(CompileError),
    #[error("prepared execution failed: {0}")]
    Run(ExecutionError),
    #[error("prepared runtime is unavailable after an integrity failure")]
    Unavailable(MachineFailure),
    #[error("session binding {0:?} is not a live prepared binding")]
    UnknownBinding(SessionVarId),
    #[error("no value generation has been started: advance or set the session generation before binding")]
    GenerationNotStarted,
    #[error(
        "session binding {id:?} is leased by {leases} installed program(s) and cannot be released"
    )]
    BindingLeased { id: SessionVarId, leases: usize },
    #[error(
        "a managed argument's handle is not live under realm {realm:?}: it was minted under a \
         different runtime resource scope (or already released)"
    )]
    CrossRealmArgument { realm: RealmId },
    /// A turn program whose entry is not the settled scaffold
    /// (`Tidepool.Internal.Resume.Settled`), or whose settled layer did not
    /// have the declared shape. Every turn template defines `__prepared`,
    /// so this is a stale or foreign artifact, never a user error.
    #[error("prepared program {program:?} has no settled entry layer: {detail}")]
    UnsettledEntry {
        program: ProgramId,
        detail: &'static str,
    },
    /// A prepared-route operation on a session constructed for the Core
    /// engine (or the reverse). Routes are fixed at construction; nothing
    /// falls back.
    #[error("the session does not run on the prepared route")]
    WrongEngine,
    /// A turn reached the prepared route without its prepared program: the
    /// request was compiled Core-only. Never a fallback; the turn fails.
    #[error("the turn was compiled without its prepared program")]
    MissingProgram,
    /// A turn shape the prepared route does not carry yet (the cutover lands
    /// them in order: effect suspension with the resume contract, pattern
    /// binds with the multi-binder slice). The turn fails; Core is never
    /// consulted.
    #[error("the prepared route does not yet support {0}")]
    NotYetSupported(&'static str),
    /// An operation that needs an installed machine ran before the first
    /// program installed one.
    #[error("no prepared program has been installed yet")]
    NoMachine,
    /// One artifact declares the same typed site twice with different
    /// evidence: a projection defect, refused before anything installs.
    #[error("the artifact declares typed site {site} twice with different evidence")]
    DuplicateSite { site: u64 },
    /// A pattern bind's settled value did not carry one managed field per
    /// GHC binder. The extractor projects the binders as one tuple, so this
    /// is a stale or foreign artifact, never a user error.
    #[error("pattern bind produced {fields} fields for {binders} GHC binders")]
    ProjectionShape { binders: usize, fields: usize },
    /// A program being installed declares a typed site another installed
    /// program already declares with different evidence (delivery, wire or
    /// input types). Installation is refused before anything is published;
    /// the existing owner stays canonical.
    #[error("typed site {site} is already installed by program {owner:?} with different evidence")]
    SiteConflict { site: u64, owner: ProgramId },
    /// A suspended request named a typed site no installed program declares.
    /// The request and continuation were released; nothing was parked.
    #[error("the suspended request names typed site {site}, which no installed program declares")]
    UnknownSite { site: u64 },
    /// A suspended request carries no typed site: an ordinary handled effect
    /// (Print, a file read), which the prepared route does not answer yet.
    /// The request and continuation were released; nothing was parked.
    #[error(
        "the prepared route does not yet answer ordinary handled effects (the request carries no typed site)"
    )]
    UntypedRequest,
    /// The turn suspended under `HandleOrError`, and the prepared route
    /// handles no effect yet, so every request is unhandled.
    #[error("the turn requested an effect under HandleOrError; the prepared route handles no effects yet")]
    UnhandledRequest,
    /// The program that produced a suspension admits no resume entry, so its
    /// continuation could never be re-entered. Every turn template defines
    /// the entry; this is a stale or foreign artifact, never a user error.
    #[error("program {program:?} admits no `{entry}` entry, so its suspension cannot be parked")]
    NoResumeEntry {
        program: ProgramId,
        entry: &'static str,
    },
    /// A host-built answer was offered to a site whose delivery is not a host
    /// answer (a live re-entry, an exit-cell fill or a terminal capture).
    /// The frame stays parked.
    #[error("typed site {site} is delivered by {delivery:?}, not by a host-built answer")]
    AnswerDelivery { site: u64, delivery: SiteDelivery },
    /// The answer names a constructor outside the site's declared family
    /// closure (the wrong family, or a constructor the type's rows do not
    /// admit). The frame stays parked.
    #[error("typed site {site} does not admit constructor {host_id:?} in its answer")]
    AnswerConstructor { site: u64, host_id: DataConId },
    /// The answer's shape does not match the site's type evidence (a literal
    /// where a constructor is required, a field count or scalar width
    /// mismatch, a byte array, or excessive nesting). The frame stays parked.
    #[error("typed site {site} rejects the answer: {detail}")]
    AnswerShape { site: u64, detail: &'static str },
    /// The answer reaches a type the host cannot construct. The frame stays
    /// parked.
    #[error("typed site {site} has an unconstructible answer type: {reason}")]
    AnswerUnconstructible { site: u64, reason: String },
}

impl PreparedRuntimeError {
    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_)
            | Self::Link(_)
            | Self::UnknownBinding(_)
            | Self::GenerationNotStarted
            | Self::BindingLeased { .. }
            | Self::UnsettledEntry { .. }
            | Self::WrongEngine
            | Self::MissingProgram
            | Self::NotYetSupported(_)
            | Self::NoMachine
            | Self::DuplicateSite { .. }
            | Self::ProjectionShape { .. }
            | Self::SiteConflict { .. }
            | Self::UnknownSite { .. }
            | Self::UntypedRequest
            | Self::UnhandledRequest
            | Self::NoResumeEntry { .. }
            | Self::AnswerDelivery { .. }
            | Self::AnswerConstructor { .. }
            | Self::AnswerShape { .. }
            | Self::AnswerUnconstructible { .. }
            | Self::CrossRealmArgument { .. } => PreparedFailureKind::Rejected,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Unavailable(_) => PreparedFailureKind::Integrity,
            Self::Compile(_) => PreparedFailureKind::Rejected,
            Self::Run(error) => match error {
                ExecutionError::MissingEntry(_)
                | ExecutionError::Unsupported(_)
                | ExecutionError::Arguments { .. }
                | ExecutionError::ArgumentRepresentation { .. }
                | ExecutionError::UnknownPreparedHandle
                | ExecutionError::ImportShape { .. }
                | ExecutionError::DescriptorShape { .. }
                | ExecutionError::HostIdConflict { .. }
                | ExecutionError::UnknownProgram(_)
                | ExecutionError::UnknownContinuation(_)
                | ExecutionError::Answer(_) => PreparedFailureKind::Rejected,
                ExecutionError::Runtime(failure) => {
                    if failure.disposition == MachineDisposition::Unavailable {
                        PreparedFailureKind::Integrity
                    } else if matches!(
                        failure.cause,
                        tidepool_codegen::host_fns::RuntimeError::Cancelled
                    ) {
                        PreparedFailureKind::Cancelled
                    } else {
                        PreparedFailureKind::Language
                    }
                }
                ExecutionError::Observation(_) | ExecutionError::Static(_) => {
                    PreparedFailureKind::Language
                }
            },
        }
    }
}

impl From<LinkError> for PreparedRuntimeError {
    fn from(error: LinkError) -> Self {
        Self::Link(Box::new(error))
    }
}

#[derive(Debug)]
pub struct PreparedRunResult {
    pub values: Vec<Value>,
    pub collections: u64,
}

/// An opaque value retained by one [`PreparedRuntime`].
///
/// The value is intentionally linear at the runtime boundary: pass and
/// inspect it by borrow, then consume it with [`PreparedRuntime::release`].
/// Its codegen root never escapes this wrapper.
pub struct PreparedValue(PreparedHandle);

/// The identity of one [`PreparedValue`] under a runtime resource scope,
/// for a caller that must name a retained value without holding it (e.g. a
/// [`crate::session::registry::SessionRegistry`] hole). Unlike
/// [`PreparedValue`] this is `Clone + Copy + PartialEq + Debug` and carries
/// no ownership: it does not keep the handle's root alive, and holding one
/// past a [`PreparedRuntime::release`] or [`PreparedRuntime::close_realm`]
/// simply makes [`PreparedRuntime::parked_realm`] answer `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedHole {
    pub realm: RealmId,
    pub k: PreparedHandle,
}

pub enum PreparedArgument<'a> {
    Scalar(u64),
    Managed(&'a PreparedValue),
}

pub enum PreparedValueResult {
    Void,
    Scalar(u64),
    Managed(PreparedValue),
}

pub struct PreparedRetainedResult {
    pub values: Vec<PreparedValueResult>,
    pub collections: u64,
}

/// What closing a realm actually released, from [`PreparedRuntime::close_realm_report`].
/// `frames` counts the parked continuations the realm owned (see
/// [`PreparedMachine::close_realm`]); this test-compatibility runtime never
/// parks one itself, so it reports `0` here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealmRetirement {
    pub frames: usize,
    pub handles_released: usize,
    pub leases_released: usize,
}

/// One constructor layer of a retained value, read without forcing children.
pub enum PreparedOuter {
    Constructor {
        identity: tidepool_repr::DataConId,
        fields: Vec<PreparedValueResult>,
    },
}

/// The facts about an installed program the session still needs after the
/// machine has taken its code: the declared entry (what `run_entry(None)`
/// addresses) and, per top-level binding, the identity and entry signature
/// an importer links against. Recorded on every binding made from the top
/// ([`PreparedOrigin`]) so nothing downstream reconstructs them.
struct ProgramFacts {
    entry: ValueId,
    tops: BTreeMap<ValueId, (SymbolIdentity, Option<Signature>)>,
    /// The `Tidepool.Internal.Resume.Settled` constructors this program
    /// declares, when its entry is a turn's settled scaffold.
    settled: Option<SettledIds>,
    /// The turn's admitted resume entry (`__resume q x = settle (resumeLifted
    /// q x)`, beside the entry in its module), when the artifact retained it.
    /// A suspension of a program without one is refused before parking.
    resume: Option<ValueId>,
    /// The typed sites this program declares and the type graph they point
    /// into, kept for site-evidence resolution and answer validation after
    /// the machine has taken the program's code.
    sites: Vec<SiteRow>,
    types: Vec<TypeNode>,
    /// Constructor identities and bridge ids by this program's local
    /// `ConstructorId`, so two programs' type graphs compare by identity
    /// rather than local index, and a bridge `Value`'s constructor resolves
    /// to the row that admits it.
    constructors: Vec<(SymbolIdentity, DataConId)>,
}

/// Which installed program's site table is authoritative for one site id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SiteWitness {
    owner: ProgramId,
    row: usize,
}

/// The two constructors a turn's settled layer is read by, as this program's
/// own declarations name them (`host_id` is the bridge `DataConId`).
#[derive(Clone, Copy, Debug)]
struct SettledIds {
    done: tidepool_repr::DataConId,
    suspended: tidepool_repr::DataConId,
}

impl SettledIds {
    const MODULE: &'static str = "Tidepool.Internal.Resume";

    fn of(prepared: &PreparedProgram) -> Option<Self> {
        let host_id = |occurrence: &str| {
            prepared
                .constructors()
                .iter()
                .find(|declaration| {
                    declaration.identity.module == Self::MODULE
                        && declaration.identity.occurrence == occurrence
                })
                .map(|declaration| declaration.host_id)
        };
        Some(Self {
            done: host_id("Done")?,
            suspended: host_id("Suspended")?,
        })
    }
}

impl ProgramFacts {
    fn of(prepared: &PreparedProgram) -> Self {
        let tops: BTreeMap<ValueId, (SymbolIdentity, Option<Signature>)> = prepared
            .bindings()
            .iter()
            .flat_map(|group| match group {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops.as_slice(),
            })
            .map(|top| {
                let export = match &top.binding.rhs {
                    HeapRhs::Function { signature, .. } | HeapRhs::Thunk { signature, .. } => {
                        prepared.signatures().get(signature.0 as usize).cloned()
                    }
                    HeapRhs::Constructor { .. } | HeapRhs::Bytes(_) => None,
                };
                (top.binding.id, (top.identity.clone(), export))
            })
            .collect();
        let entry = prepared.entry();
        let resume = tops
            .get(&entry)
            .map(|(identity, _)| identity.module.clone())
            .and_then(|module| {
                tops.iter().find_map(|(id, (identity, _))| {
                    (identity.module == module && identity.occurrence == PREPARED_RESUME_TARGET)
                        .then_some(*id)
                })
            });
        Self {
            entry,
            tops,
            settled: SettledIds::of(prepared),
            resume,
            sites: prepared.sites().to_vec(),
            types: prepared.types().to_vec(),
            constructors: prepared
                .constructors()
                .iter()
                .map(|declaration| (declaration.identity.clone(), declaration.host_id))
                .collect(),
        }
    }

    fn type_node(&self, id: TypeNodeId) -> Option<&TypeNode> {
        self.types.get(id.0 as usize)
    }

    fn constructor_identity(
        &self,
        id: tidepool_repr::execution_schema::ConstructorId,
    ) -> Option<&SymbolIdentity> {
        self.constructors
            .get(id.0 as usize)
            .map(|(identity, _)| identity)
    }

    /// Lower a bridge `Value` offered as the answer at `site` against the type
    /// node `node` of this (evidence-owning) program: every constructor must
    /// be one of the node's rows, every field count must match, every scalar
    /// must fit its declared representation. Text, Integer and Natural leaves
    /// are a later slice; an unconstructible node refuses. Nothing here
    /// touches the machine, so a refusal leaves the frame exactly as parked.
    fn lower_answer(
        &self,
        site: u64,
        node: TypeNodeId,
        value: &Value,
        depth: usize,
    ) -> Result<AnswerPlan, PreparedRuntimeError> {
        if depth > MAX_ANSWER_DEPTH {
            return Err(PreparedRuntimeError::AnswerShape {
                site,
                detail: "the answer nests deeper than the builder admits",
            });
        }
        let shape = |detail| PreparedRuntimeError::AnswerShape { site, detail };
        match self.type_node(node) {
            None => Err(shape("the site's type evidence names an undeclared node")),
            Some(TypeNode::Data { rows, .. }) => {
                let Value::Con(host_id, fields) = value else {
                    return Err(shape("a constructor of the site's answer type is required"));
                };
                let row = rows
                    .iter()
                    .find(|row| {
                        self.constructors
                            .get(row.constructor.0 as usize)
                            .is_some_and(|(_, declared)| declared == host_id)
                    })
                    .ok_or(PreparedRuntimeError::AnswerConstructor {
                        site,
                        host_id: *host_id,
                    })?;
                if row.fields.len() != fields.len() {
                    return Err(shape(
                        "the constructor's field count does not match its declaration",
                    ));
                }
                let mut planned = Vec::with_capacity(fields.len());
                for (field_node, field) in row.fields.iter().zip(fields) {
                    planned.push(self.lower_answer(site, *field_node, field, depth + 1)?);
                }
                Ok(AnswerPlan::Constructor {
                    host_id: *host_id,
                    fields: planned,
                })
            }
            Some(TypeNode::Scalar(rep)) => {
                let Value::Lit(literal) = value else {
                    return Err(shape("a scalar field requires a literal"));
                };
                let bits = scalar_bits(*rep, literal).ok_or_else(|| {
                    shape("the literal does not fit the field's scalar representation")
                })?;
                Ok(AnswerPlan::Scalar { rep: *rep, bits })
            }
            Some(TypeNode::Text | TypeNode::Integer | TypeNode::Natural) => {
                Err(PreparedRuntimeError::NotYetSupported(
                    "Text, Integer and Natural host answers (byte-backed construction lands them)",
                ))
            }
            Some(TypeNode::Unconstructible { reason, .. }) => {
                Err(PreparedRuntimeError::AnswerUnconstructible {
                    site,
                    reason: reason.clone(),
                })
            }
        }
    }
}

/// Target-encode `literal` for a field of representation `rep`: the value's
/// native bytes, of which the builder writes only the field's declared width.
/// `None` when the literal's kind does not match the representation.
fn scalar_bits(rep: RuntimeRep, literal: &Literal) -> Option<[u8; 16]> {
    let word: u128 = match (rep, literal) {
        (RuntimeRep::Int(bits), Literal::LitInt(value)) => {
            // Refuse a value the field cannot hold rather than truncating.
            if bits < 64 && (*value < -(1_i64 << (bits - 1)) || *value >= (1_i64 << (bits - 1))) {
                return None;
            }
            *value as u128
        }
        (RuntimeRep::Word(bits), Literal::LitWord(value)) => {
            if bits < 64 && *value >= (1_u64 << bits) {
                return None;
            }
            u128::from(*value)
        }
        (RuntimeRep::Word(bits), Literal::LitChar(value)) if bits >= 32 => {
            u128::from(*value as u32)
        }
        (RuntimeRep::Float(64), Literal::LitDouble(bits))
        | (RuntimeRep::Float(32), Literal::LitFloat(bits)) => u128::from(*bits),
        _ => return None,
    };
    Some(word.to_ne_bytes())
}

/// Whether two site rows from two programs carry the same evidence: the same
/// delivery mode and structurally equal wire and input type graphs, compared
/// by family and constructor identity with ordered arguments, never by local
/// node or constructor numbers. Cycles (recursive types) are compared
/// coinductively: a node pair already under comparison is taken as equal.
fn sites_equivalent(a: &ProgramFacts, a_row: &SiteRow, b: &ProgramFacts, b_row: &SiteRow) -> bool {
    if a_row.delivery != b_row.delivery || a_row.inputs.len() != b_row.inputs.len() {
        return false;
    }
    let mut visited = BTreeSet::new();
    type_nodes_equivalent(a, a_row.wire, b, b_row.wire, &mut visited)
        && a_row
            .inputs
            .iter()
            .zip(&b_row.inputs)
            .all(|(x, y)| type_nodes_equivalent(a, *x, b, *y, &mut visited))
}

fn type_nodes_equivalent(
    a: &ProgramFacts,
    a_id: TypeNodeId,
    b: &ProgramFacts,
    b_id: TypeNodeId,
    visited: &mut BTreeSet<(u32, u32)>,
) -> bool {
    if !visited.insert((a_id.0, b_id.0)) {
        return true;
    }
    match (a.type_node(a_id), b.type_node(b_id)) {
        (
            Some(TypeNode::Data {
                family: a_family,
                arguments: a_arguments,
                rows: a_rows,
            }),
            Some(TypeNode::Data {
                family: b_family,
                arguments: b_arguments,
                rows: b_rows,
            }),
        ) => {
            a_family == b_family
                && a_arguments.len() == b_arguments.len()
                && a_rows.len() == b_rows.len()
                && a_arguments
                    .iter()
                    .zip(b_arguments)
                    .all(|(x, y)| type_nodes_equivalent(a, *x, b, *y, visited))
                && a_rows.iter().zip(b_rows).all(|(x, y)| {
                    a.constructor_identity(x.constructor) == b.constructor_identity(y.constructor)
                        && x.fields.len() == y.fields.len()
                        && x.fields
                            .iter()
                            .zip(&y.fields)
                            .all(|(f, g)| type_nodes_equivalent(a, *f, b, *g, visited))
                })
        }
        (Some(TypeNode::Text), Some(TypeNode::Text))
        | (Some(TypeNode::Integer), Some(TypeNode::Integer))
        | (Some(TypeNode::Natural), Some(TypeNode::Natural)) => true,
        (Some(TypeNode::Scalar(x)), Some(TypeNode::Scalar(y))) => x == y,
        (
            Some(TypeNode::Unconstructible {
                reason: a_reason,
                rendered: a_rendered,
            }),
            Some(TypeNode::Unconstructible {
                reason: b_reason,
                rendered: b_rendered,
            }),
        ) => a_reason == b_reason && a_rendered == b_rendered,
        _ => false,
    }
}

/// The typed site a suspended request names, read from the request's
/// rendered payload: the protocol's sited helpers place the site id under the
/// `typedSite` key of the request's JSON payload object
/// (`tidepool-protocol`'s `ObjectValue::Site`), the same field the harness
/// classifies a Core suspension by. `None` for a request that carries no such
/// field: an ordinary handled effect.
fn typed_site_of(request: &Value, table: &DataConTable) -> Option<u64> {
    fn find(json: &serde_json::Value, depth: usize) -> Option<u64> {
        if depth > 4 {
            return None;
        }
        match json {
            serde_json::Value::Object(object) => {
                if let Some(site) = object.get("typedSite").and_then(serde_json::Value::as_u64) {
                    return Some(site);
                }
                object.values().find_map(|value| find(value, depth + 1))
            }
            serde_json::Value::Array(items) => {
                items.iter().find_map(|value| find(value, depth + 1))
            }
            _ => None,
        }
    }
    find(&crate::render::value_to_json(request, table, 0), 0)
}

/// One prepared session: a lazily installed machine shared by every program
/// installed into it, the session's retained bindings, and the one
/// generation counter later programs link against.
///
/// The first program is linked at construction and installed on first use;
/// later programs are installed through [`Self::install`] against bindings
/// made by [`Self::bind_top`]. Root registration stays with the machine
/// ([`PreparedMachine::release`] is the only deregistration path); the
/// [`BindingTable`] is the name/generation/lease ledger, reused from the Core
/// session rather than duplicated.
pub struct PreparedRuntime {
    /// The first program, until the machine exists and takes custody of it.
    pending: Option<LinkedProgram>,
    /// Set together: the machine and the id of the first program it was
    /// created with (the program `run_entry` addresses by default).
    machine: Option<(PreparedMachine<'static>, ProgramId)>,
    /// What the session keeps about each installed program once the machine
    /// owns its code (see [`ProgramFacts`]); the linked program tree itself
    /// is not retained.
    programs: BTreeMap<ProgramId, ProgramFacts>,
    bindings: BindingTable,
    /// The session's single value generation counter. A generation is a
    /// turn: every binding made before the next `advance_generation` shares
    /// it (as every binder of one Core turn shares its `Val.G<g>` module),
    /// and an importer's `required_generation` is checked against the
    /// generation the named binding was made at. `Generation(0)` is the
    /// empty session; binding at it is refused.
    val_gen: Generation,
    binding_ids: MonotonicIdIssuer,
    /// Session-var ids leased by each realm's `install_prepared_in` calls,
    /// released together (`self.bindings.release_leases`) when that realm
    /// closes. The import slot a lease guards is the installing program's
    /// own persistent root — the lease exists to protect binding-table
    /// identity/generation from a later mismatched re-`bind_top`/install,
    /// not to keep the underlying value alive — so releasing every lease a
    /// realm holds at `close_realm` is safe even though the machine may
    /// still (independently) retain the value elsewhere.
    realm_leases: BTreeMap<RealmId, Vec<SessionVarId>>,
    /// Ambient actor mount context, set once by `Self::set_actor_execution`
    /// and otherwise unused: this engine has no effect handlers of its own
    /// yet, so `EffectRunPolicy`/`LivePayloadPolicy` are stored for a later
    /// wave rather than acted on.
    actor_execution: Option<(SessionRunContext, EffectRunPolicy, LivePayloadPolicy)>,
}

// SAFETY: `PreparedRuntime` is Send under the same stowed-XOR-running
// discipline as `PersistentSession`/`JitEffectMachine`
// (`tidepool-codegen/src/jit_machine.rs`'s `unsafe impl Send for
// JitEffectMachine`, and this crate's `persistent.rs` threading note above
// `PersistentSession`'s machine lifecycle section): exactly one thread ever
// touches a `PreparedRuntime` at a time, because `SessionRegistry` only ever
// hands it to one checkout, which either runs on the caller's own thread or
// is moved wholesale onto a blocking thread and back (`Checkout::into_parts`)
// -- it is never split, aliased, or driven from two threads at once.
// Field by field:
//  - `pending: Option<LinkedProgram>` -- plain owned data (parsed/linked
//    program tree), no thread affinity.
//  - `machine: Option<(PreparedMachine<'static>, ProgramId)>` -- the only
//    field that is not already auto-`Send`. `PreparedMachine`'s non-`Send`
//    fields are `Rc<MachineState>` and (inside its installed programs)
//    `Rc<CompiledProgram>`. `MachineState` itself already carries `unsafe
//    impl Send` (`tidepool-codegen/src/machine_state.rs`) for the identical
//    reason: it is touched by exactly one thread at a time. An `Rc`'s own
//    non-`Send`-ness is about un-synchronized refcount mutation from two
//    threads concurrently, not about its pointee being thread-affine data;
//    under the single-owner discipline above, no second thread ever holds a
//    clone of either `Rc` while this one moves, so no concurrent refcount
//    access can occur. The raw code pointers, vmctx, and heap buffers
//    reachable through `PreparedMachine` are process-global address space
//    (`tidepool-codegen/CLAUDE.md`'s "JIT allocation" section), valid from
//    any thread, exactly like `JitEffectMachine`'s own code/heap.
//  - `programs: BTreeMap<ProgramId, LinkedProgram>` -- plain owned data.
//  - `bindings: BindingTable` -- already carries its own `unsafe impl Send`
//    (`tidepool-codegen/src/binding_table.rs`) for its `RootSlot(*mut *mut
//    u8)` cells, which are owned by the machine's `OldSpace` and therefore
//    live under the very same single-owner discipline as `machine` above:
//    `BindingTable` never deregisters a GC root itself (that is
//    `JitEffectMachine`/`PreparedMachine`'s job), so it makes no unsynchronized
//    access to the pointee either.
//  - `val_gen: Generation`, `binding_ids: MonotonicIdIssuer` -- plain data.
//  - `realm_leases: BTreeMap<RealmId, Vec<SessionVarId>>` -- plain data.
//  - `actor_execution: Option<(SessionRunContext, EffectRunPolicy,
//    LivePayloadPolicy)>` -- plain data (ids and policy enums).
unsafe impl Send for PreparedRuntime {}

static_assertions::assert_impl_all!(PreparedRuntime: Send);

impl PreparedRuntime {
    pub fn from_artifact(
        artifact: &[u8],
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
        imports: MachineImports,
    ) -> Result<Self, PreparedRuntimeError> {
        let prepared = parse_program(artifact, requirements, limits)?;
        Self::from_prepared(prepared, imports)
    }

    /// Start a session from an already-decoded program (the artifact-bytes
    /// path above parses and then comes here).
    pub fn from_prepared(
        prepared: PreparedProgram,
        imports: MachineImports,
    ) -> Result<Self, PreparedRuntimeError> {
        let linked = link_program(prepared, &imports)?;
        Ok(Self {
            pending: Some(linked),
            machine: None,
            programs: BTreeMap::new(),
            bindings: BindingTable::new(),
            val_gen: Generation::default(),
            binding_ids: MonotonicIdIssuer::starting_at("prepared-binding", 1),
            realm_leases: BTreeMap::new(),
            actor_execution: None,
        })
    }

    /// The id of the session's first program, installing the machine if it
    /// has not been yet.
    pub fn first_program(&mut self) -> Result<ProgramId, PreparedRuntimeError> {
        self.ensure_machine()
    }

    /// The session's retained bindings (read-only; mutation goes through
    /// [`Self::bind_top`], [`Self::install`] and [`Self::release_binding`]).
    #[must_use]
    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }

    /// The current value generation (`Generation(0)` before any turn).
    #[must_use]
    pub fn val_gen(&self) -> Generation {
        self.val_gen
    }

    /// Set the current value generation, e.g. to the generation a caller's
    /// projection was told to retain against. Generations only ever move
    /// forward: a value at or below the current one is refused.
    pub fn set_val_gen(&mut self, generation: Generation) -> Result<(), PreparedRuntimeError> {
        if generation <= self.val_gen {
            return Err(PreparedRuntimeError::GenerationNotStarted);
        }
        self.val_gen = generation;
        Ok(())
    }

    /// Start the next turn's generation and return it.
    pub fn advance_generation(&mut self) -> Generation {
        self.val_gen = self.val_gen.next();
        self.val_gen
    }

    /// Retain one of an installed program's top-level bindings under `name`
    /// at the session's current value generation, without running it. The
    /// returned id is what a later [`Self::install`] names an import by.
    /// Refused at `Generation(0)`: advance or set the generation first.
    pub fn bind_top(
        &mut self,
        program: ProgramId,
        value: ValueId,
        name: &str,
    ) -> Result<SessionVarId, PreparedRuntimeError> {
        if self.val_gen == Generation::default() {
            return Err(PreparedRuntimeError::GenerationNotStarted);
        }
        self.ensure_machine()?;
        let machine = self.machine_mut()?;
        let handle = machine
            .retain_top(program, value)
            .map_err(Self::classify_execution)?;
        let root = machine
            .handle_root(handle)
            .ok_or(PreparedRuntimeError::Run(
                ExecutionError::UnknownPreparedHandle,
            ))?;
        let (identity, export) = self
            .programs
            .get(&program)
            .and_then(|facts| facts.tops.get(&value))
            .cloned()
            .ok_or(PreparedRuntimeError::Run(ExecutionError::MissingEntry(
                value,
            )))?;
        let id = SessionVarId::from_var(VarId(self.binding_ids.next_raw()));
        let entry = BindingEntry {
            name: BindingName(name.to_string()),
            id,
            module: SessionModule::val(self.val_gen),
            value: BoundValue::Prepared {
                root,
                handle,
                origin: Some(PreparedOrigin {
                    identity,
                    export,
                    top: Some(PreparedTop { program, value }),
                }),
            },
            type_display: None,
            defining_expr: None,
            scope: ScopeId::ROOT,
        };
        Ok(self.bindings.bind(entry))
    }

    /// Install a later program that imports session bindings by identity.
    /// Every global the artifact DECLARES is resolved to a live binding:
    /// through `imports` when the caller names that identity explicitly,
    /// otherwise by the identity recorded on the binding at [`Self::bind_top`]
    /// (at the declared `required_generation`, or the newest when none is
    /// declared). The artifact is linked against those bindings' live shape
    /// (representation, settledness, exporting signature, generation) BEFORE
    /// anything is compiled or installed, so a stale generation
    /// (`LinkError::ImportContract`) or an unresolvable identity
    /// (`LinkError::MissingImport`) has no machine side effect. On success
    /// exactly the bindings the program declares are leased for its
    /// lifetime -- a pair in `imports` the artifact never declares leases
    /// nothing.
    pub fn install(
        &mut self,
        artifact: &[u8],
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
        imports: &[(SymbolIdentity, SessionVarId)],
    ) -> Result<ProgramId, PreparedRuntimeError> {
        self.install_in(artifact, requirements, limits, imports, RealmId::ROOT)
    }

    /// [`Self::install`] whose imports' leases are released together when
    /// `realm` closes, instead of being held for the machine's whole life.
    pub fn install_in(
        &mut self,
        artifact: &[u8],
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
        imports: &[(SymbolIdentity, SessionVarId)],
        realm: RealmId,
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let prepared = parse_program(artifact, requirements, limits)?;
        self.install_prepared_in(prepared, imports, realm)
    }

    /// [`Self::install`] for an already-decoded program.
    pub fn install_prepared(
        &mut self,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, SessionVarId)],
    ) -> Result<ProgramId, PreparedRuntimeError> {
        self.install_prepared_in(prepared, imports, RealmId::ROOT)
    }

    /// [`Self::install_in`] for an already-decoded program.
    pub fn install_prepared_in(
        &mut self,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, SessionVarId)],
        realm: RealmId,
    ) -> Result<ProgramId, PreparedRuntimeError> {
        self.ensure_machine()?;
        let mut values = MachineImports::default();
        let mut bindings = ImportBindings::new();
        let mut leased = Vec::new();
        // Resolution is driven by what the PROGRAM declares, never by the
        // caller's list alone: that list can only name a binding for an
        // identity the artifact actually imports.
        for declaration in prepared.globals() {
            let identity = &declaration.identity;
            let Some(id) = imports
                .iter()
                .find(|(named, _)| named == identity)
                .map(|(_, id)| *id)
                .or_else(|| self.resolve_import(identity, declaration.required_generation))
            else {
                // Left absent: `link_program` reports it as the typed
                // `MissingImport` for exactly this identity.
                continue;
            };
            let entry = self
                .bindings
                .get(id)
                .ok_or(PreparedRuntimeError::UnknownBinding(id))?;
            let BoundValue::Prepared { handle, origin, .. } = &entry.value else {
                return Err(PreparedRuntimeError::UnknownBinding(id));
            };
            let handle = *handle;
            let generation = entry.module.gen().0;
            let entry_signature = origin.as_ref().and_then(|origin| origin.export.clone());
            let evaluated = self
                .machine_ref()?
                .handle_is_evaluated(handle)
                .map_err(Self::classify_execution)?;
            values.values.insert(
                identity.clone(),
                ImportedValue {
                    identity: identity.clone(),
                    rep: handle.rep(),
                    entry_signature,
                    evaluated,
                    generation,
                },
            );
            bindings.insert(identity.clone(), handle);
            leased.push(id);
        }
        let facts = ProgramFacts::of(&prepared);
        let linked = link_program(prepared, &values)?;
        let machine = self.machine_mut()?;
        let compiled = machine
            .compile_for_install(&linked)
            .map_err(PreparedRuntimeError::Compile)?;
        let program = machine
            .install_program(compiled, bindings)
            .map_err(Self::classify_execution)?;
        self.bindings.acquire_leases(leased.iter().copied());
        self.realm_leases.entry(realm).or_default().extend(leased);
        self.programs.insert(program, facts);
        Ok(program)
    }

    /// The live binding an artifact's declared import resolves to by
    /// identity: the one bound from a top whose declared identity is
    /// `identity`, at `generation` when the artifact pins one, otherwise the
    /// newest such binding.
    fn resolve_import(
        &self,
        identity: &SymbolIdentity,
        generation: Option<u64>,
    ) -> Option<SessionVarId> {
        self.bindings
            .iter_live()
            .filter(|entry| {
                matches!(
                    &entry.value,
                    BoundValue::Prepared { origin: Some(origin), .. } if &origin.identity == identity
                )
            })
            .filter(|entry| generation.is_none_or(|generation| entry.module.gen().0 == generation))
            .max_by_key(|entry| entry.module.gen())
            .map(|entry| entry.id)
    }

    /// Install one live session turn's projected artifact and bind the name
    /// it introduces at the runtime's current generation. Every
    /// retained-generation import the turn's module declares resolves by
    /// identity to the binding recorded at [`Self::bind_top`] (there is no
    /// caller-supplied import list to get wrong). This is
    /// [`Self::install_prepared`] followed by [`Self::bind_top`] against the
    /// freshly installed program's own entry — the link+install+bind
    /// sequence the resident prepared route drives after it
    /// has projected `prepared` through `ExtractCmd`'s `--target` mode.
    pub fn turn(
        &mut self,
        prepared: PreparedProgram,
        introduces: &str,
    ) -> Result<SessionVarId, PreparedRuntimeError> {
        let entry = prepared.entry();
        let program = self.install_prepared(prepared, &[])?;
        self.bind_top(program, entry, introduces)
    }

    /// Release a session binding's root. Refused, with the lease count,
    /// while any installed program imports it; leases are held for the
    /// importing program's lifetime, which this wave ends only with the
    /// machine.
    pub fn release_binding(&mut self, id: SessionVarId) -> Result<(), PreparedRuntimeError> {
        let leases = self.bindings.lease_count(id);
        if leases > 0 {
            return Err(PreparedRuntimeError::BindingLeased { id, leases });
        }
        let entry = self
            .bindings
            .remove_live(id)
            .ok_or(PreparedRuntimeError::UnknownBinding(id))?;
        let BoundValue::Prepared { handle, .. } = entry.value else {
            return Err(PreparedRuntimeError::UnknownBinding(id));
        };
        if !self.machine_mut()?.release(handle) {
            return Err(PreparedRuntimeError::Run(
                ExecutionError::UnknownPreparedHandle,
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.machine
            .as_ref()
            .map_or(MachineDisposition::Reusable, |(machine, _)| {
                machine.disposition()
            })
    }

    /// Mint a fresh realm id for a new cancellation scope. A realm id is a
    /// free-standing identity (`RealmId::fresh`); it needs no machine and
    /// nothing is registered under it until the first call or inspection
    /// made with it.
    #[must_use]
    pub fn open_realm(&self) -> RealmId {
        RealmId::fresh()
    }

    /// Obtain a clone-able cancellation handle scoped to `realm`, lazily
    /// minting that realm's flag on first request. Cancelling it aborts
    /// only calls made with `realm`; sibling realms are unaffected.
    pub fn cancel_handle(&mut self, realm: RealmId) -> Result<CancelHandle, PreparedRuntimeError> {
        self.ensure_machine()?;
        Ok(self.machine_mut()?.realm_cancel_handle(realm))
    }

    /// SCOPE EXIT: close `realm`, releasing every value handle it owns.
    /// `(0, 0)` if no machine has been installed yet (nothing to close).
    /// See [`PreparedMachine::close_realm`] for the exact contract. Kept for
    /// existing callers; [`Self::close_realm_report`] additionally reports
    /// leases released.
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        let report = self.close_realm_report(realm);
        (report.frames, report.handles_released)
    }

    /// [`Self::close_realm`], also releasing every lease
    /// [`Self::install_prepared_in`] acquired under `realm`
    /// (`self.bindings.release_leases`) and reporting the full receipt. The
    /// leased import slot is the installing program's own persistent root —
    /// the lease protects binding-table identity/generation, not the
    /// value's liveness — so releasing it at realm close never drops a value
    /// out from under a still-running program.
    /// ROOT belongs to the session itself; closing it is a no-op for both
    /// handles and leases. Session teardown releases those resources.
    pub fn close_realm_report(&mut self, realm: RealmId) -> RealmRetirement {
        if realm == RealmId::ROOT {
            return RealmRetirement {
                frames: 0,
                handles_released: 0,
                leases_released: 0,
            };
        }
        let (frames, handles_released) = self
            .machine
            .as_mut()
            .map_or((0, 0), |(machine, _)| machine.close_realm(realm));
        let leases = self.realm_leases.remove(&realm).unwrap_or_default();
        let leases_released = leases.len();
        self.bindings.release_leases(leases);
        RealmRetirement {
            frames,
            handles_released,
            leases_released,
        }
    }

    /// Number of `PreparedValue`s this runtime's machine currently retains.
    /// Diagnostic surface for confirming a caller released every value it
    /// produced (e.g. through a resume loop); zero before any machine is
    /// installed.
    #[must_use]
    pub fn retained_handle_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, |(machine, _)| machine.handle_count())
    }

    /// Run an entry of the session's first program (`None` selects that
    /// program's declared entry).
    pub fn run_entry(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[u64],
        collect: bool,
        realm: RealmId,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        let program = self.ensure_machine()?;
        let entry = self.entry_of(program, binding)?;
        self.run_entry_with_completion_hook(program, entry, arguments, collect, realm, || {})
    }

    /// [`Self::run_entry`] for any installed program.
    pub fn run_entry_in(
        &mut self,
        program: ProgramId,
        entry: ValueId,
        arguments: &[u64],
        collect: bool,
        realm: RealmId,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        self.run_entry_with_completion_hook(program, entry, arguments, collect, realm, || {})
    }

    /// Execute with scalar or borrowed retained arguments and retain managed
    /// results under this runtime's machine owner. Runs the session's first
    /// program (`None` selects its declared entry).
    pub fn run_entry_retained(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[PreparedArgument<'_>],
        collect: bool,
        realm: RealmId,
    ) -> Result<PreparedRetainedResult, PreparedRuntimeError> {
        let program = self.ensure_machine()?;
        let entry = self.entry_of(program, binding)?;
        self.run_entry_retained_in(program, entry, arguments, collect, realm)
    }

    /// [`Self::run_entry_retained`] for any installed program.
    pub fn run_entry_retained_in(
        &mut self,
        program: ProgramId,
        entry: ValueId,
        arguments: &[PreparedArgument<'_>],
        collect: bool,
        realm: RealmId,
    ) -> Result<PreparedRetainedResult, PreparedRuntimeError> {
        self.ensure_available()?;
        self.ensure_machine()?;
        let machine = self.machine_ref()?;
        for argument in arguments {
            if let PreparedArgument::Managed(value) = argument {
                if machine.handle_realm(value.0) != Some(realm) {
                    return Err(PreparedRuntimeError::CrossRealmArgument { realm });
                }
            }
        }
        if self
            .machine_mut()?
            .realm_cancel_handle(realm)
            .is_cancelled()
        {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut lowered = Vec::new();
        lowered.try_reserve_exact(arguments.len()).map_err(|_| {
            PreparedRuntimeError::Run(ExecutionError::Runtime(MachineFailure {
                cause: tidepool_codegen::host_fns::RuntimeError::HeapOverflow,
                disposition: MachineDisposition::Reusable,
            }))
        })?;
        for argument in arguments {
            lowered.push(match argument {
                PreparedArgument::Scalar(word) => PreparedInput::Scalar(*word),
                PreparedArgument::Managed(value) => PreparedInput::Managed(value.0),
            });
        }
        let machine = self.machine_mut()?;
        if machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let result = machine
            .run_entry_retained(
                program,
                entry,
                &lowered,
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: collect,
                },
                realm,
            )
            .map_err(Self::classify_execution)?;
        Ok(self.retain_result(result))
    }

    /// Inspect one retained constructor layer without evaluating its fields.
    /// This never installs a machine for a fabricated value.
    pub fn inspect_outer(
        &mut self,
        value: &PreparedValue,
        realm: RealmId,
    ) -> Result<PreparedOuter, PreparedRuntimeError> {
        self.ensure_available()?;
        let (machine, _) = self.machine.as_mut().ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownPreparedHandle,
        ))?;
        let outer = machine
            .inspect_outer(value.0, realm)
            .map_err(Self::classify_execution)?;
        Ok(self.outer_result(outer))
    }

    /// Consume one retained value's runtime wrapper and release its root.
    /// Releasing an already-closed or foreign value is a no-op.
    pub fn release(&mut self, value: PreparedValue) -> bool {
        self.machine
            .as_mut()
            .is_some_and(|(machine, _)| machine.release(value.0))
    }

    /// This runtime's [`PreparedHole`] for `value` under `realm`. Does not
    /// consume or alter `value`'s liveness; call [`Self::release`] once the
    /// value itself is no longer needed.
    #[must_use]
    pub fn hole_for(&self, value: &PreparedValue, realm: RealmId) -> PreparedHole {
        PreparedHole { realm, k: value.0 }
    }

    /// `Some(hole.realm)` iff `hole`'s handle is still live in this
    /// runtime's machine under that realm; `None` once released (by
    /// [`Self::release`], [`Self::release_binding`], or [`Self::close_realm`])
    /// or if it was never minted under this realm to begin with.
    ///
    /// Answered from `PreparedMachine::handle_realm`, the machine's own
    /// `ResourceLedger` query -- the single owner of the handle-to-realm
    /// fact, not a mirror kept in step with it.
    #[must_use]
    pub fn parked_realm(&self, hole: &PreparedHole) -> Option<RealmId> {
        (self.machine_ref().ok()?.handle_realm(hole.k) == Some(hole.realm)).then_some(hole.realm)
    }

    /// Set the ambient actor mount context for this runtime. See the
    /// `actor_execution` field doc: this engine does not yet act on
    /// `effect_policy`/`live_payload`, but stores them for parity with
    /// [`ActorRunTarget::install_actor_execution`]'s other implementers.
    pub fn set_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) {
        self.actor_execution = Some((context, effect_policy, live_payload));
    }

    fn run_entry_with_completion_hook(
        &mut self,
        program: ProgramId,
        entry: ValueId,
        arguments: &[u64],
        collect: bool,
        realm: RealmId,
        after_lower_success: impl FnOnce(),
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        self.ensure_available()?;
        self.ensure_machine()?;
        if self
            .machine_mut()?
            .realm_cancel_handle(realm)
            .is_cancelled()
        {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: collect,
        };
        let machine = self.machine_mut()?;
        if machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let result = machine
            .run_entry(program, entry, arguments, options, realm)
            .map_err(Self::classify_execution)?;
        // Lower success is the completion point. Cancellation published after
        // it may affect a later entry, but cannot rewrite this result.
        after_lower_success();
        Ok(PreparedRunResult {
            values: result.values,
            collections: result.collections,
        })
    }

    fn ensure_available(&self) -> Result<(), PreparedRuntimeError> {
        if let Some((machine, _)) = &self.machine {
            if machine.disposition() == MachineDisposition::Unavailable {
                return Err(PreparedRuntimeError::Unavailable(
                    machine.failure().unwrap_or(MachineFailure {
                        cause: tidepool_codegen::host_fns::RuntimeError::BadPointer,
                        disposition: MachineDisposition::Unavailable,
                    }),
                ));
            }
        }
        Ok(())
    }

    /// Install the machine with the first program if that has not happened
    /// yet; returns the first program's id either way.
    fn ensure_machine(&mut self) -> Result<ProgramId, PreparedRuntimeError> {
        self.ensure_available()?;
        if let Some((_, program)) = &self.machine {
            return Ok(*program);
        }
        let linked = self.pending.take().ok_or(PreparedRuntimeError::NoMachine)?;
        let facts = ProgramFacts::of(linked.prepared());
        let compiled = match CompiledProgram::compile(&linked) {
            Ok(compiled) => compiled,
            Err(error) => {
                self.pending = Some(linked);
                return Err(PreparedRuntimeError::Compile(error));
            }
        };
        let installed = match PreparedMachine::new(
            compiled,
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        ) {
            Ok(installed) => installed,
            Err(error) => {
                self.pending = Some(linked);
                return Err(Self::classify_execution(error));
            }
        };
        let program = installed.1;
        self.machine = Some(installed);
        self.programs.insert(program, facts);
        Ok(program)
    }

    fn machine_mut(&mut self) -> Result<&mut PreparedMachine<'static>, PreparedRuntimeError> {
        self.machine
            .as_mut()
            .map(|(machine, _)| machine)
            .ok_or(PreparedRuntimeError::NoMachine)
    }

    fn machine_ref(&self) -> Result<&PreparedMachine<'static>, PreparedRuntimeError> {
        self.machine
            .as_ref()
            .map(|(machine, _)| machine)
            .ok_or(PreparedRuntimeError::NoMachine)
    }

    /// `binding`, or the program's declared entry when `None`.
    fn entry_of(
        &self,
        program: ProgramId,
        binding: Option<ValueId>,
    ) -> Result<ValueId, PreparedRuntimeError> {
        if let Some(entry) = binding {
            return Ok(entry);
        }
        self.programs
            .get(&program)
            .map(|facts| facts.entry)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))
    }

    fn retain_result(&self, result: PreparedResultBatch) -> PreparedRetainedResult {
        PreparedRetainedResult {
            values: result.values.into_iter().map(Self::value_result).collect(),
            collections: result.collections,
        }
    }

    fn outer_result(&self, outer: CodegenPreparedOuter) -> PreparedOuter {
        match outer {
            CodegenPreparedOuter::Constructor { identity, fields } => PreparedOuter::Constructor {
                identity,
                fields: fields.into_iter().map(Self::value_result).collect(),
            },
        }
    }

    /// Convert one codegen result. The machine's own `ResourceLedger`
    /// already records a newly minted managed handle's realm at the point it
    /// is minted (`retain_top`/`run_entry_retained`/`inspect_outer`), so
    /// there is no bookkeeping to do here.
    fn value_result(result: PreparedResult) -> PreparedValueResult {
        match result {
            PreparedResult::Void => PreparedValueResult::Void,
            PreparedResult::Scalar(word) => PreparedValueResult::Scalar(word),
            PreparedResult::Managed(handle) => PreparedValueResult::Managed(PreparedValue(handle)),
        }
    }

    fn classify_execution(error: ExecutionError) -> PreparedRuntimeError {
        PreparedRuntimeError::Run(error)
    }
}

/// Run a closed, non-retained entry once against a freshly parsed and linked
/// artifact, discarding the machine afterward. `cancel` is an
/// externally-owned flag (not a realm): this is a one-shot helper with no
/// session to scope a realm against, so a caller may pre-cancel before this
/// function even constructs a machine (e.g. a request already cancelled
/// before compilation started), or flip it mid-call from another thread --
/// the same contract [`tidepool_codegen::prepared_program::CompiledProgram::run_entry`]
/// preserves for the same reason.
pub fn run_prepared_once(
    artifact: &[u8],
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
    imports: MachineImports,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<PreparedRunResult, PreparedRuntimeError> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(PreparedRuntimeError::Cancelled);
    }
    let mut runtime = PreparedRuntime::from_artifact(artifact, requirements, limits, imports)?;
    let program = runtime.ensure_machine()?;
    let entry = runtime.entry_of(program, None)?;
    let machine = runtime.machine_mut()?;
    let result = machine
        .run_entry_with_raw_cancel(
            program,
            entry,
            &[],
            PreparedCallOptions {
                observation_budget: RunOptions::default().observation_budget,
                collect_before_observation: true,
            },
            cancel,
        )
        .map_err(PreparedRuntime::classify_execution)?;
    Ok(PreparedRunResult {
        values: result.values,
        collections: result.collections,
    })
}

// ---------------------------------------------------------------------------
// PreparedEngine — the prepared half of a resident session's engine
// ---------------------------------------------------------------------------

/// The prepared-STG half of a resident session's engine: one machine shared
/// by every program the session's turns install, plus what the session keeps
/// about each program once the machine owns its code. Bindings, generations,
/// scopes and leases stay in `PersistentSession` (the same value plane the
/// Core engine binds into); this owns only code and heap.
///
/// A session on the prepared route bootstraps its machine from its first
/// turn's program ([`Self::bootstrap`]) and installs every later one against
/// the session's live prepared bindings ([`Self::install`]). A turn runs as
/// its settled scaffold ([`Self::run_settled`]): the host reads completion or
/// suspension from one constructor layer and never walks freer data.
pub struct PreparedEngine {
    machine: PreparedMachine<'static>,
    programs: BTreeMap<ProgramId, ProgramFacts>,
    /// The machine-owned site index: which installed program's site table is
    /// authoritative for each typed site id. Extended in the install
    /// transaction before any code is compiled; a conflicting duplicate
    /// refuses the install (see [`Self::install`]).
    sites: BTreeMap<u64, SiteWitness>,
}

/// How a settled entry (the scaffold or a resume) is called: nothing is
/// observed by the call itself, and no collection is forced before the
/// layer is read.
const SETTLE_CALL: PreparedCallOptions = PreparedCallOptions {
    observation_budget: 0,
    collect_before_observation: false,
};

/// One suspension parked by [`PreparedEngine::park_suspension`]: the frame's
/// id and the observed request, as Core reports a suspension.
pub struct PreparedParked {
    pub id: ContinuationId,
    pub request: Value,
}

/// A parked frame re-entered by [`PreparedEngine::resume_parked`]: the
/// settled layer the resume produced, under the frame's realm, by the runner
/// program whose entry re-entered it (the program a further suspension is
/// parked against).
pub struct PreparedResumed {
    pub settlement: PreparedSettlement,
    pub realm: RealmId,
    pub runner: ProgramId,
}

// SAFETY: identical to `PreparedRuntime`'s justification above -- the machine
// is the only non-auto-`Send` field, and `PersistentSession` moves the engine
// between exactly one owning thread at a time (stowed XOR running).
unsafe impl Send for PreparedEngine {}

static_assertions::assert_impl_all!(PreparedEngine: Send);

/// One turn's settled computation, read from the `Settled` layer its
/// `__prepared` scaffold produced. Every handle is retained under the run's
/// realm until the caller adopts or releases it.
#[derive(Debug)]
pub enum PreparedSettlement {
    /// The computation completed; `value` is in weak head normal form.
    Done { value: PreparedHandle },
    /// The computation requested an effect and retained its continuation.
    Suspended {
        request: PreparedHandle,
        continuation: PreparedHandle,
    },
}

/// The live binding an artifact's declared import resolves to by identity:
/// the newest prepared binding whose recorded identity is `identity`, or the
/// one at `generation` when the artifact pins one.
fn resolve_prepared_import<'a>(
    bindings: &'a BindingTable,
    identity: &SymbolIdentity,
    generation: Option<u64>,
) -> Option<&'a BindingEntry> {
    bindings
        .iter_live()
        .filter(|entry| {
            matches!(
                &entry.value,
                BoundValue::Prepared { origin: Some(origin), .. } if &origin.identity == identity
            )
        })
        .filter(|entry| generation.is_none_or(|generation| entry.module.gen().0 == generation))
        .max_by_key(|entry| entry.module.gen())
}

impl PreparedEngine {
    /// Create the session's machine from its first turn's program and
    /// install that program. The first turn can import nothing: no prepared
    /// binding exists before the machine does.
    pub fn bootstrap(prepared: PreparedProgram) -> Result<(Self, ProgramId), PreparedRuntimeError> {
        let facts = ProgramFacts::of(&prepared);
        let linked = link_program(prepared, &MachineImports::default())?;
        let compiled = CompiledProgram::compile(&linked).map_err(PreparedRuntimeError::Compile)?;
        let (machine, program) = PreparedMachine::new(
            compiled,
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .map_err(PreparedRuntimeError::Run)?;
        let mut engine = Self {
            machine,
            programs: BTreeMap::new(),
            sites: BTreeMap::new(),
        };
        // The first program can conflict only with itself.
        let rows = engine.plan_sites(&facts)?;
        engine.programs.insert(program, facts);
        engine.publish_sites(program, rows);
        Ok((engine, program))
    }

    /// The rows of `facts` that installing it would make canonical: every
    /// site no installed program declares yet. A site already installed (by
    /// an earlier program, or earlier in `facts` itself) is accepted only when
    /// the two rows carry the same evidence ([`sites_equivalent`]); the
    /// existing owner stays canonical and nothing is added for it. A
    /// conflicting duplicate is [`PreparedRuntimeError::SiteConflict`] and the
    /// caller installs nothing.
    fn plan_sites(&self, facts: &ProgramFacts) -> Result<Vec<(u64, usize)>, PreparedRuntimeError> {
        let mut planned: Vec<(u64, usize)> = Vec::new();
        for (row, site) in facts.sites.iter().enumerate() {
            let (owner, owner_facts, owner_row) = if let Some(witness) = self.sites.get(&site.site)
            {
                let owner = self
                    .programs
                    .get(&witness.owner)
                    .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                        witness.owner,
                    )))?;
                (Some(witness.owner), owner, &owner.sites[witness.row])
            } else if let Some((_, earlier)) = planned.iter().find(|(id, _)| *id == site.site) {
                (None, facts, &facts.sites[*earlier])
            } else {
                planned.push((site.site, row));
                continue;
            };
            if !sites_equivalent(owner_facts, owner_row, facts, site) {
                return Err(match owner {
                    Some(owner) => PreparedRuntimeError::SiteConflict {
                        site: site.site,
                        owner,
                    },
                    None => PreparedRuntimeError::DuplicateSite { site: site.site },
                });
            }
        }
        Ok(planned)
    }

    /// Make `program` the canonical owner of the planned rows.
    fn publish_sites(&mut self, program: ProgramId, rows: Vec<(u64, usize)>) {
        self.sites.extend(rows.into_iter().map(|(site, row)| {
            (
                site,
                SiteWitness {
                    owner: program,
                    row,
                },
            )
        }));
    }

    /// Install a later turn's program. Every global it declares is resolved
    /// to a live prepared binding in `bindings` by the identity recorded at
    /// bind time (at the declared `required_generation`, or the newest), and
    /// the artifact is linked against those bindings' live shape before
    /// anything is compiled, so a stale generation or an unresolvable
    /// identity is a typed link error with no machine side effect.
    pub fn install(
        &mut self,
        prepared: PreparedProgram,
        bindings: &BindingTable,
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let mut values = MachineImports::default();
        let mut imports = ImportBindings::new();
        for declaration in prepared.globals() {
            let identity = &declaration.identity;
            let Some(entry) =
                resolve_prepared_import(bindings, identity, declaration.required_generation)
            else {
                // Left absent: `link_program` reports the typed `MissingImport`.
                continue;
            };
            let BoundValue::Prepared { handle, origin, .. } = &entry.value else {
                return Err(PreparedRuntimeError::UnknownBinding(entry.id));
            };
            let handle = *handle;
            let evaluated = self
                .machine
                .handle_is_evaluated(handle)
                .map_err(PreparedRuntimeError::Run)?;
            values.values.insert(
                identity.clone(),
                ImportedValue {
                    identity: identity.clone(),
                    rep: handle.rep(),
                    entry_signature: origin.as_ref().and_then(|origin| origin.export.clone()),
                    evaluated,
                    generation: entry.module.gen().0,
                },
            );
            imports.insert(identity.clone(), handle);
        }
        let facts = ProgramFacts::of(&prepared);
        // Site evidence is checked before anything is compiled or published:
        // a conflicting duplicate leaves the machine, its programs and the
        // site index exactly as they were.
        let rows = self.plan_sites(&facts)?;
        let linked = link_program(prepared, &values)?;
        let compiled = self
            .machine
            .compile_for_install(&linked)
            .map_err(PreparedRuntimeError::Compile)?;
        let program = self
            .machine
            .install_program(compiled, imports)
            .map_err(PreparedRuntimeError::Run)?;
        self.programs.insert(program, facts);
        self.publish_sites(program, rows);
        Ok(program)
    }

    /// Run `program`'s settled scaffold under `realm` and read its one
    /// constructor layer. The scaffold value itself is released here; the
    /// layer's fields come back as retained handles.
    pub fn run_settled(
        &mut self,
        program: ProgramId,
        realm: RealmId,
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        let facts = self
            .programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?;
        let entry = facts.entry;
        // The decoder needs the settled constructors; refuse before running
        // an entry whose layer could never be read.
        if facts.settled.is_none() {
            return Err(PreparedRuntimeError::UnsettledEntry {
                program,
                detail: "the program declares no Tidepool.Internal.Resume.Settled constructors",
            });
        }
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let batch = self
            .machine
            .run_entry_retained(program, entry, &[], SETTLE_CALL, realm)
            .map_err(PreparedRuntimeError::Run)?;
        self.settle_batch(program, realm, batch)
    }

    /// Read the settled layer an entry of `program` returned: the batch's one
    /// managed value is the `Settled` constructor (any other managed value is
    /// released), decoded through the shared decoder.
    fn settle_batch(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        batch: PreparedResultBatch,
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        let mut outer = None;
        for value in batch.values {
            match (value, outer) {
                (PreparedResult::Managed(handle), None) => outer = Some(handle),
                (PreparedResult::Managed(handle), Some(_)) => {
                    self.machine.release(handle);
                }
                _ => {}
            }
        }
        let outer = outer.ok_or(PreparedRuntimeError::UnsettledEntry {
            program,
            detail: "the entry returned no managed settled value",
        })?;
        self.decode_settled(program, outer, realm)
    }

    /// The one settled-layer decoder: read `outer` (a `Settled` value some
    /// entry of `program` returned) as `Done`/`Suspended`, releasing `outer`
    /// and retaining its fields under `realm`. Initial runs
    /// ([`Self::run_settled`]) and resumed runs ([`Self::resume_parked`]) both
    /// pass through here.
    fn decode_settled(
        &mut self,
        program: ProgramId,
        outer: PreparedHandle,
        realm: RealmId,
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        let settled = self
            .programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .settled
            .ok_or(PreparedRuntimeError::UnsettledEntry {
                program,
                detail: "the program declares no Tidepool.Internal.Resume.Settled constructors",
            })?;
        let layer = self.machine.inspect_outer(outer, realm);
        self.machine.release(outer);
        let CodegenPreparedOuter::Constructor { identity, fields } =
            layer.map_err(PreparedRuntimeError::Run)?;
        let managed: Vec<PreparedHandle> = fields
            .into_iter()
            .filter_map(|field| match field {
                PreparedResult::Managed(handle) => Some(handle),
                PreparedResult::Void | PreparedResult::Scalar(_) => None,
            })
            .collect();
        let mut shape = |detail| {
            for handle in &managed {
                self.machine.release(*handle);
            }
            Err(PreparedRuntimeError::UnsettledEntry { program, detail })
        };
        if identity == settled.done {
            match managed.as_slice() {
                [value] => Ok(PreparedSettlement::Done { value: *value }),
                _ => shape("Done carried other than one managed field"),
            }
        } else if identity == settled.suspended {
            match managed.as_slice() {
                [request, continuation] => Ok(PreparedSettlement::Suspended {
                    request: *request,
                    continuation: *continuation,
                }),
                _ => shape("Suspended carried other than two managed fields"),
            }
        } else {
            shape("the settled layer is neither Done nor Suspended")
        }
    }

    /// Park a suspension `program`'s settled layer produced under `realm`:
    /// read the `Union` layer of `request`, observe its payload through the
    /// machine observe path (the request the host reports, as on Core), read
    /// the typed site it names, resolve that site through the machine-owned
    /// index to its evidence owner, and park `continuation` with that
    /// evidence and `program`'s admitted resume entry. Every refusal releases
    /// both handles and parks nothing: a runner without a resume entry, a
    /// suspension under `HandleOrError` (nothing is handled on this route
    /// yet), a request without a typed site (an ordinary handled effect, not
    /// yet answered on this route), or a site no installed program declares.
    #[expect(
        clippy::too_many_arguments,
        reason = "one park carries the run's realm, principal and both effect policies beside the two settled-layer handles, as the Core park does"
    )]
    pub fn park_suspension(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        principal: PrincipalId,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
        request: PreparedHandle,
        continuation: PreparedHandle,
        table: &DataConTable,
    ) -> Result<PreparedParked, PreparedRuntimeError> {
        // Every handle this park holds is released here on any refusal;
        // `try_park_suspension` hands them over as it consumes them.
        let mut temporaries = vec![request, continuation];
        let parked = self.try_park_suspension(
            program,
            realm,
            principal,
            effect_policy,
            live_payload,
            table,
            &mut temporaries,
        );
        for handle in temporaries {
            self.machine.release(handle);
        }
        parked
    }

    /// [`Self::park_suspension`]'s body. `temporaries` holds the request
    /// and continuation on entry; a handle is removed as it is consumed
    /// (released here, or parked), so whatever remains on any exit is what the
    /// caller must release.
    #[expect(
        clippy::too_many_arguments,
        reason = "the park's policy arguments plus the temporaries it consumes"
    )]
    fn try_park_suspension(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        principal: PrincipalId,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
        table: &DataConTable,
        temporaries: &mut Vec<PreparedHandle>,
    ) -> Result<PreparedParked, PreparedRuntimeError> {
        let [request, continuation] = *temporaries.as_slice() else {
            return Err(PreparedRuntimeError::UnsettledEntry {
                program,
                detail: "a park needs exactly the request and continuation handles",
            });
        };
        let facts = self
            .programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?;
        let resume_entry = facts.resume.ok_or(PreparedRuntimeError::NoResumeEntry {
            program,
            entry: PREPARED_RESUME_TARGET,
        })?;
        if effect_policy == EffectRunPolicy::HandleOrError {
            return Err(PreparedRuntimeError::UnhandledRequest);
        }
        // The `Union` layer: an unpacked tag word and the lazy payload.
        let CodegenPreparedOuter::Constructor { fields, .. } = self
            .machine
            .inspect_outer(request, realm)
            .map_err(PreparedRuntimeError::Run)?;
        temporaries.retain(|handle| *handle != request);
        self.machine.release(request);
        let mut payload = None;
        for field in fields {
            match (field, payload) {
                (PreparedResult::Managed(handle), None) => payload = Some(handle),
                (PreparedResult::Managed(handle), Some(_)) => {
                    self.machine.release(handle);
                }
                (PreparedResult::Void | PreparedResult::Scalar(_), _) => {}
            }
        }
        let payload = payload.ok_or(PreparedRuntimeError::UnsettledEntry {
            program,
            detail: "the suspended Union carried no managed payload",
        })?;
        temporaries.push(payload);
        // The request is observed (forced) through the existing observe
        // path, exactly the value Core reports for a suspension.
        let request = self
            .machine
            .observe_handle(program, payload, RunOptions::default().observation_budget)
            .map_err(PreparedRuntimeError::Run)?;
        temporaries.retain(|handle| *handle != payload);
        self.machine.release(payload);
        let site = typed_site_of(&request, table).ok_or(PreparedRuntimeError::UntypedRequest)?;
        let witness = self
            .sites
            .get(&site)
            .copied()
            .ok_or(PreparedRuntimeError::UnknownSite { site })?;
        let evidence = PreparedFrameEvidence {
            owner: witness.owner,
            site,
            runner: program,
            resume_entry,
            continuation_rep: continuation.rep(),
        };
        let id = self
            .machine
            .park(
                continuation,
                realm,
                principal,
                effect_policy,
                live_payload,
                evidence,
            )
            .map_err(PreparedRuntimeError::Run)?;
        temporaries.retain(|handle| *handle != continuation);
        Ok(PreparedParked { id, request })
    }

    /// Peek at a parked frame's evidence and realm without consuming it.
    #[must_use]
    pub fn parked(&self, id: ContinuationId) -> Option<(RealmId, PreparedFrameEvidence)> {
        self.machine
            .parked(id)
            .map(|(realm, evidence)| (realm, *evidence))
    }

    /// The runtime resource scope owning the frame parked under `id`.
    #[must_use]
    pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId> {
        self.machine.parked_realm(id)
    }

    /// The ids currently parked on this engine's machine, ascending.
    #[must_use]
    pub fn parked_ids(&self) -> Vec<ContinuationId> {
        self.machine.parked_ids()
    }

    /// Re-enter the frame parked under `id` with `answer`, a handle the
    /// caller has already validated against the frame's site evidence and
    /// retained under the frame's realm: take the frame, enter the runner's
    /// resume entry with the continuation and the answer, and read the
    /// settled layer through the shared decoder. `answer` is consumed on
    /// every path. Every failure before the take (unknown id, an answer from
    /// another realm, cancellation) leaves the frame parked and rooted; a
    /// failure after the take is a run failure, as on Core.
    pub fn resume_parked(
        &mut self,
        id: ContinuationId,
        answer: PreparedHandle,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let taken = self.take_for_resume(id, answer);
        let (continuation, evidence, realm) = match taken {
            Ok(taken) => taken,
            Err(error) => {
                self.machine.release(answer);
                return Err(error);
            }
        };
        let batch = self.machine.run_entry_retained(
            evidence.runner,
            evidence.resume_entry,
            &[
                PreparedInput::Managed(continuation),
                PreparedInput::Managed(answer),
            ],
            SETTLE_CALL,
            realm,
        );
        self.machine.release(continuation);
        self.machine.release(answer);
        let batch = batch.map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(evidence.runner, realm, batch)?;
        Ok(PreparedResumed {
            settlement,
            realm,
            runner: evidence.runner,
        })
    }

    /// The pre-take checks of a resume, then the take: the frame exists,
    /// `answer` is live under its realm, the realm is not cancelled.
    fn take_for_resume(
        &mut self,
        id: ContinuationId,
        answer: PreparedHandle,
    ) -> Result<(PreparedHandle, PreparedFrameEvidence, RealmId), PreparedRuntimeError> {
        let (realm, _) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        if self.machine.handle_realm(answer) != Some(realm) {
            return Err(PreparedRuntimeError::CrossRealmArgument { realm });
        }
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let (continuation, evidence) = self
            .machine
            .take_parked(id)
            .map_err(PreparedRuntimeError::Run)?;
        Ok((continuation, evidence, realm))
    }

    /// Validate `value` as the host-built answer for the frame parked under
    /// `id` and lower it to a build plan: the frame's site row must be
    /// delivered by a host answer, and the value must fit the site's wire
    /// type evidence in the evidence owner's tables. Nothing here touches the
    /// machine; every refusal leaves the frame exactly as parked.
    fn answer_plan(
        &self,
        id: ContinuationId,
        value: &Value,
    ) -> Result<AnswerPlan, PreparedRuntimeError> {
        let (_, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let owner = self
            .programs
            .get(&evidence.owner)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                evidence.owner,
            )))?;
        let row = owner
            .sites
            .iter()
            .find(|row| row.site == evidence.site)
            .ok_or(PreparedRuntimeError::UnknownSite {
                site: evidence.site,
            })?;
        if row.delivery != SiteDelivery::HostAnswer {
            return Err(PreparedRuntimeError::AnswerDelivery {
                site: row.site,
                delivery: row.delivery,
            });
        }
        owner.lower_answer(row.site, row.wire, value, 0)
    }

    /// Re-enter the frame parked under `id` with a host-built answer: peek,
    /// validate and lower `value` against the site evidence, build it into a
    /// realm-owned handle, then take the frame and enter the resume entry
    /// ([`Self::resume_parked`]). Every failure before the take leaves the
    /// frame parked with the handle and root counts unchanged.
    pub fn resume_with_answer(
        &mut self,
        id: ContinuationId,
        value: &Value,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let plan = self.answer_plan(id, value)?;
        let (realm, _) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let answer = self
            .machine
            .build_answer(realm, &plan)
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, answer)
    }

    /// Consume the frame parked under `id` without entering it: the
    /// continuation is released and nothing runs. An unknown id is a typed
    /// error with nothing changed.
    pub fn abort_parked(&mut self, id: ContinuationId) -> Result<(), PreparedRuntimeError> {
        let (continuation, _) = self
            .machine
            .take_parked(id)
            .map_err(PreparedRuntimeError::Run)?;
        self.machine.release(continuation);
        Ok(())
    }

    /// Materialize a retained value as a bridge `Value`, forcing its lazy
    /// fields through `program`'s force adapter. The handle stays retained.
    pub fn observe(
        &mut self,
        program: ProgramId,
        handle: PreparedHandle,
    ) -> Result<Value, PreparedRuntimeError> {
        self.machine
            .observe_handle(program, handle, RunOptions::default().observation_budget)
            .map_err(PreparedRuntimeError::Run)
    }

    /// The managed fields of one constructor layer of a retained value,
    /// each retained as its own handle under `realm`, without forcing. The
    /// pattern-bind lane reads a settled tuple this way: the extractor
    /// projects `(x, y) <- ...` as one tuple entry, so the tuple's fields ARE
    /// the binders, in order. `handle` itself stays retained; the caller
    /// releases it.
    pub fn fields(
        &mut self,
        handle: PreparedHandle,
        realm: RealmId,
        binders: usize,
    ) -> Result<Vec<PreparedHandle>, PreparedRuntimeError> {
        let CodegenPreparedOuter::Constructor { fields, .. } = self
            .machine
            .inspect_outer(handle, realm)
            .map_err(PreparedRuntimeError::Run)?;
        let produced = fields.len();
        let managed: Vec<PreparedHandle> = fields
            .into_iter()
            .filter_map(|field| match field {
                PreparedResult::Managed(handle) => Some(handle),
                PreparedResult::Void | PreparedResult::Scalar(_) => None,
            })
            .collect();
        if managed.len() != binders || produced != binders {
            for handle in managed {
                self.machine.release(handle);
            }
            return Err(PreparedRuntimeError::ProjectionShape {
                binders,
                fields: produced,
            });
        }
        Ok(managed)
    }

    /// Hand a run result to the session value plane: the handle moves into
    /// the machine's ROOT scope (no realm close releases it) and its
    /// persistent root slot is returned for the binding to load through.
    pub fn adopt(
        &mut self,
        handle: PreparedHandle,
    ) -> Option<tidepool_codegen::old_space::RootSlot> {
        self.machine
            .adopt_handle(handle)
            .then(|| self.machine.handle_root(handle))
            .flatten()
    }

    /// Release one retained handle and deregister its root.
    pub fn release(&mut self, handle: PreparedHandle) -> bool {
        self.machine.release(handle)
    }

    /// Close a runtime resource scope: `(frames, handles_released)`.
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        self.machine.close_realm(realm)
    }

    pub fn cancel_handle(&mut self, realm: RealmId) -> CancelHandle {
        self.machine.realm_cancel_handle(realm)
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.machine.disposition()
    }

    #[must_use]
    pub fn failure(&self) -> Option<MachineFailure> {
        self.machine.failure()
    }

    #[must_use]
    pub fn handle_count(&self) -> usize {
        self.machine.handle_count()
    }

    #[must_use]
    pub fn persistent_roots_count(&self) -> usize {
        self.machine.total_persistent_roots()
    }

    /// The stowed roots of parked frames (accounting class 1, root half).
    #[must_use]
    pub fn stowed_roots_count(&self) -> usize {
        self.machine.stowed_roots_count()
    }

    /// The parked continuations (accounting class 1, frame half).
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.machine.parked_count()
    }

    /// The unit every home module of `program` was compiled in -- what a
    /// later program's import identity for one of this turn's session
    /// binders names.
    #[must_use]
    pub fn entry_unit(&self, program: ProgramId) -> Option<String> {
        let facts = self.programs.get(&program)?;
        facts
            .tops
            .get(&facts.entry)
            .map(|(identity, _)| identity.unit.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::host_fns::RuntimeError;
    use tidepool_codegen::machine_state::MachineFailure;
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, ImportedValue, TargetDescriptor, EXECUTION_ABI_VERSION,
        SCHEMA_VERSION,
    };

    fn head(major: u8, length: usize) -> Vec<u8> {
        assert!(length < 24);
        vec![(major << 5) | length as u8]
    }

    fn array(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        let values: Vec<_> = values.into_iter().collect();
        let mut result = head(4, values.len());
        for value in values {
            result.extend(value);
        }
        result
    }

    fn uint(value: u64) -> Vec<u8> {
        if value <= 23 {
            vec![value as u8]
        } else if value <= u8::MAX as u64 {
            vec![0x18, value as u8]
        } else if value <= u16::MAX as u64 {
            let mut result = vec![0x19];
            result.extend((value as u16).to_be_bytes());
            result
        } else if value <= u32::MAX as u64 {
            let mut result = vec![0x1a];
            result.extend((value as u32).to_be_bytes());
            result
        } else {
            let mut result = vec![0x1b];
            result.extend(value.to_be_bytes());
            result
        }
    }

    fn text(value: &str) -> Vec<u8> {
        let mut result = head(3, value.len());
        result.extend(value.as_bytes());
        result
    }

    fn rep_lifted() -> Vec<u8> {
        array([uint(1)])
    }

    fn symbol(namespace: &str, module: &str, occurrence: &str) -> Vec<u8> {
        array([
            text("fixture"),
            text(module),
            text(namespace),
            text(occurrence),
            array([uint(0)]),
        ])
    }

    fn terminal_fixture() -> Vec<u8> {
        let constructor = array([
            symbol("value", "PreparedStrict", "Box"),
            symbol("type", "PreparedStrict", "BoxFamily"),
            array([]),
            array([]),
            array([array([]), uint(1), uint(0), array([])]),
            rep_lifted(),
            uint(1),
            uint(1),
            uint(100),
        ]);
        let expression = array([uint(4), uint(0), array([])]);
        let function = array([uint(0), uint(0), array([]), array([]), uint(0)]);
        let top = array([
            symbol("value", "PreparedStrict", "entry"),
            array([uint(0), function]),
        ]);
        let binding_group = array([uint(0), top]);
        array([
            text("TPSTG"),
            uint(SCHEMA_VERSION),
            text("ghc-9.12-prepared-stg"),
            text("ghc-9.12.2"),
            uint(EXECUTION_ABI_VERSION),
            array([
                uint(0),
                uint(0),
                uint(64),
                uint(64),
                text("sysv64"),
                array([]),
            ]),
            array([array([array([]), array([uint(0), array([rep_lifted()])])])]),
            array([]),
            array([constructor]),
            array([]),
            array([expression]),
            array([binding_group]),
            uint(0),
            array([]),
            array([]),
        ])
    }

    fn m3_runtime() -> PreparedRuntime {
        let artifact = terminal_fixture();
        let requirements = ProgramRequirements {
            schema_version: SCHEMA_VERSION,
            projection_profile: "ghc-9.12-prepared-stg".into(),
            toolchain: "ghc-9.12.2".into(),
            execution_abi_version: EXECUTION_ABI_VERSION,
            target: TargetDescriptor {
                architecture: Architecture::X86_64,
                endianness: Endianness::Little,
                pointer_width: 64,
                word_width: 64,
                abi: "sysv64".into(),
                features: vec![],
            },
        };
        let prepared = parse_program(&artifact, &requirements, DecodeLimits::default()).unwrap();
        let imports = MachineImports {
            values: prepared
                .globals()
                .iter()
                .map(|global| {
                    let value = ImportedValue {
                        identity: global.identity.clone(),
                        rep: global.rep,
                        entry_signature: global
                            .entry_signature
                            .map(|id| prepared.signatures()[id.0 as usize].clone()),
                        evaluated: global.required_evaluated,
                        generation: global.required_generation.unwrap_or(0),
                    };
                    (value.identity.clone(), value)
                })
                .collect(),
        };
        PreparedRuntime::from_artifact(&artifact, &requirements, DecodeLimits::default(), imports)
            .unwrap()
    }

    #[test]
    fn integrity_failure_is_typed_independently_from_its_cause() {
        let failure = MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        };
        let error = PreparedRuntimeError::Unavailable(failure.clone());
        assert_eq!(error.kind(), PreparedFailureKind::Integrity);
        assert!(matches!(
            error,
            PreparedRuntimeError::Unavailable(retained) if retained == failure
        ));
    }

    #[test]
    fn uninstalled_failure_does_not_create_a_second_terminal_owner() {
        let mut runtime = m3_runtime();
        let failure = MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        };
        let reported =
            PreparedRuntime::classify_execution(ExecutionError::Runtime(failure.clone()));
        assert!(matches!(
            reported,
            PreparedRuntimeError::Run(ExecutionError::Runtime(retained))
                if retained == failure
        ));

        let realm = runtime.open_realm();
        runtime.cancel_handle(realm).unwrap().cancel();
        let replayed = runtime.run_entry(None, &[], false, realm).unwrap_err();
        assert!(matches!(replayed, PreparedRuntimeError::Cancelled));
    }

    #[test]
    fn cancellation_after_compiled_success_does_not_veto_completion() {
        let mut runtime = m3_runtime();
        let realm = runtime.open_realm();

        let program = runtime.first_program().expect("first program installs");
        let entry = runtime
            .entry_of(program, None)
            .expect("first program has an entry");
        let cancel = runtime.cancel_handle(realm).unwrap();
        let result =
            runtime.run_entry_with_completion_hook(program, entry, &[], false, realm, || {
                cancel.cancel();
            });

        assert!(result.is_ok());
        assert!(runtime.cancel_handle(realm).unwrap().is_cancelled());

        let next_realm = runtime.open_realm();
        runtime
            .run_entry(None, &[], false, next_realm)
            .expect("cancellation published after completion must not poison reuse");
    }

    #[test]
    fn prepared_machine_reuses_one_heap_across_settled_entries() {
        let mut runtime = m3_runtime();
        let first = runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("first prepared entry settles");
        let second = runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("second prepared entry reuses the machine");

        assert_eq!(
            format!("{:?}", first.values),
            format!("{:?}", second.values)
        );
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn cancelled_admission_does_not_poison_the_retained_machine() {
        let mut runtime = m3_runtime();
        runtime
            .run_entry(None, &[], false, RealmId::ROOT)
            .expect("first prepared entry installs the machine");

        let cancelled_realm = runtime.open_realm();
        runtime.cancel_handle(cancelled_realm).unwrap().cancel();
        assert!(matches!(
            runtime.run_entry(None, &[], false, cancelled_realm),
            Err(PreparedRuntimeError::Cancelled)
        ));

        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("cancelled admission leaves machine reusable");
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn compiled_language_and_cancellation_are_not_integrity_failures() {
        for (cause, expected) in [
            (RuntimeError::Cancelled, PreparedFailureKind::Cancelled),
            (RuntimeError::HeapOverflow, PreparedFailureKind::Language),
            (RuntimeError::DivisionByZero, PreparedFailureKind::Language),
        ] {
            let error = PreparedRuntimeError::Run(ExecutionError::Runtime(MachineFailure {
                cause,
                disposition: MachineDisposition::Reusable,
            }));
            assert_eq!(error.kind(), expected);
        }
    }

    // ---- S4: session custody -- bind, import by generation, leases --------

    use tidepool_repr::execution_schema::{
        testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
        GlobalDecl, GlobalId, Group, HeapRhs, ResultContract, RuntimeRep, ScalarLiteral, Signature,
        SignatureId, UpdatePolicy, ValueRef,
    };

    fn producer_identity() -> SymbolIdentity {
        testing::identity("S4Session", "producer")
    }

    /// A memoized CAF returning `Field(99)`: unevaluated until first run,
    /// then an updated indirection to an evaluated constructor.
    fn producer_program() -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S4Session", "Field"),
            family: testing::identity("S4Session", "Field"),
            host_id: tidepool_repr::DataConId(980),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::Int(64)],
            strict_fields: vec![true],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::Int(64),
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![false],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 99_i64.to_be_bytes().to_vec(),
            })],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = producer_identity();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("producer fixture")
    }

    /// A program whose only entry returns its one imported global. The
    /// declaration carries the producer top's own `[] -> LiftedRef` entry
    /// signature, so linking also exercises the exported-signature check.
    fn consumer_program(
        required_evaluated: bool,
        required_generation: Option<u64>,
    ) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.globals = vec![GlobalDecl {
            identity: producer_identity(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated,
            required_generation,
        }];
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("consumer fixture")
    }

    fn session() -> (PreparedRuntime, ProgramId) {
        let mut runtime =
            PreparedRuntime::from_prepared(producer_program(), MachineImports::default())
                .expect("producer links closed");
        let first = runtime.first_program().expect("first program installs");
        runtime
            .set_val_gen(Generation(1))
            .expect("the first turn starts at generation 1");
        (runtime, first)
    }

    #[test]
    fn bind_install_run_reads_the_bound_top_by_generation() {
        let (mut runtime, first) = session();
        // Force the CAF once so it is an evaluated (updated) constructor.
        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("producer entry runs");
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("producer top binds");
        assert_eq!(
            runtime.bindings().get(id).map(|entry| entry.module.gen()),
            Some(Generation(1)),
            "a binding is made at the session's current generation"
        );
        let consumer = runtime
            .install_prepared(
                consumer_program(true, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect("consumer links against generation 1 and installs");
        assert_ne!(consumer, first);
        assert_eq!(runtime.bindings().lease_count(id), 1);

        let read = runtime
            .run_entry_retained_in(consumer, ValueId(0), &[], true, RealmId::ROOT)
            .expect("consumer reads its import through the slot");
        let mut values = read.values;
        let PreparedValueResult::Managed(value) = values.remove(0) else {
            panic!("consumer must return the imported managed value");
        };
        let PreparedOuter::Constructor { identity, fields } = runtime
            .inspect_outer(&value, RealmId::ROOT)
            .expect("imported value inspects through the shared machine");
        assert_eq!(identity, tidepool_repr::DataConId(980));
        assert!(matches!(
            fields.as_slice(),
            [PreparedValueResult::Scalar(99)]
        ));
        assert!(runtime.release(value));
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn stale_generation_is_refused_by_link_before_any_install_side_effect() {
        let (mut runtime, first) = session();
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("producer top binds at generation 1");
        let handles_before = runtime.retained_handle_count();
        let error = runtime
            .install_prepared(
                consumer_program(false, Some(7)),
                &[(producer_identity(), id)],
            )
            .expect_err("a consumer linked against generation 7 must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        assert_eq!(error.kind(), PreparedFailureKind::Rejected);
        assert_eq!(runtime.retained_handle_count(), handles_before);
        assert_eq!(
            runtime.bindings().lease_count(id),
            0,
            "a refused link leases nothing"
        );
        // The same session still installs a correctly-linked consumer.
        runtime
            .install_prepared(
                consumer_program(false, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect("the refused link left the machine installable");
    }

    #[test]
    fn missing_import_is_refused_by_link() {
        let (mut runtime, _first) = session();
        let error = runtime
            .install_prepared(consumer_program(false, None), &[])
            .expect_err("a declared global with no binding must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::MissingImport(_))),
            "expected MissingImport, got {error:?}"
        );
    }

    #[test]
    fn required_evaluated_is_checked_against_the_live_value() {
        let (mut runtime, first) = session();
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("the unforced CAF binds");
        let error = runtime
            .install_prepared(
                consumer_program(true, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect_err("an unforced thunk does not satisfy required_evaluated");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("forcing the CAF updates the bound top in place");
        runtime
            .install_prepared(
                consumer_program(true, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect("the same binding now satisfies required_evaluated");
    }

    #[test]
    fn release_refuses_a_leased_binding_and_releases_an_unleased_one() {
        let (mut runtime, first) = session();
        let leased = runtime
            .bind_top(first, ValueId(0), "leased")
            .expect("binds");
        runtime.advance_generation();
        let free = runtime
            .bind_top(first, ValueId(0), "free")
            .expect("binds again at the next generation");
        assert_eq!(
            runtime.bindings().get(free).map(|entry| entry.module.gen()),
            Some(Generation(2))
        );
        runtime
            .install_prepared(
                consumer_program(false, Some(1)),
                &[(producer_identity(), leased)],
            )
            .expect("consumer installs against the leased binding");
        let error = runtime
            .release_binding(leased)
            .expect_err("a leased binding must not release");
        assert!(matches!(
            error,
            PreparedRuntimeError::BindingLeased { id, leases: 1 } if id == leased
        ));
        let handles_before = runtime.retained_handle_count();
        runtime
            .release_binding(free)
            .expect("an unleased binding releases");
        assert_eq!(runtime.retained_handle_count(), handles_before - 1);
        assert!(matches!(
            runtime.release_binding(free),
            Err(PreparedRuntimeError::UnknownBinding(id)) if id == free
        ));
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
    }

    // ---- G2: identity and leases come from the program -------------------

    /// A bound top records the identity and entry signature its producing
    /// artifact declared -- exactly what an importer's `GlobalDecl` names --
    /// so an install resolves the import by identity with no caller list.
    #[test]
    fn a_bound_top_records_the_identity_an_importer_declares() {
        let (mut runtime, first) = session();
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("producer top binds");
        let BoundValue::Prepared {
            origin: Some(origin),
            ..
        } = &runtime.bindings().get(id).expect("live binding").value
        else {
            panic!("a bound top records its origin");
        };
        assert_eq!(origin.identity, producer_identity());
        let top = origin.top.expect("a bound top records which top");
        assert_eq!(top.program, first);
        assert_eq!(top.value, ValueId(0));
        assert_eq!(
            origin.export,
            Some(Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            }),
            "a thunk top exports its zero-argument entry signature"
        );

        let consumer = runtime
            .install_prepared(consumer_program(false, Some(1)), &[])
            .expect("the declared import resolves by the recorded identity");
        assert_ne!(consumer, first);
        assert_eq!(runtime.bindings().lease_count(id), 1);
    }

    /// Leases follow what the PROGRAM declares. A caller that names a
    /// binding the artifact never imports (the phantom-lease shape every
    /// session turn used to produce by passing every live binding) leases
    /// nothing, and that binding still releases.
    #[test]
    fn an_install_leases_only_the_bindings_its_program_declares() {
        let (mut runtime, first) = session();
        let imported = runtime
            .bind_top(first, ValueId(0), "imported")
            .expect("binds at generation 1");
        runtime.advance_generation();
        let unrelated = runtime
            .bind_top(first, ValueId(0), "unrelated")
            .expect("binds again at generation 2");
        let undeclared = SymbolIdentity {
            occurrence: "neverImported".into(),
            ..producer_identity()
        };

        runtime
            .install_prepared(consumer_program(false, Some(1)), &[(undeclared, unrelated)])
            .expect("the declared generation-1 import resolves by identity");
        assert_eq!(runtime.bindings().lease_count(imported), 1);
        assert_eq!(
            runtime.bindings().lease_count(unrelated),
            0,
            "a pair for an identity the program does not declare leases nothing"
        );
        runtime
            .release_binding(unrelated)
            .expect("the never-imported binding releases");
        assert!(matches!(
            runtime.release_binding(imported),
            Err(PreparedRuntimeError::BindingLeased { leases: 1, .. })
        ));
    }

    // ---- A4: Send, realm-owned leases, cross-realm refusal, holes --------

    /// An entry taking one managed `LiftedRef` argument and returning it
    /// unchanged -- used to exercise a managed argument crossing (or
    /// failing to cross) a realm boundary, which the zero-argument fixtures
    /// above (`producer_program`, `consumer_program`) cannot do.
    fn identity_program() -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(50)))]);
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(50)],
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("identity fixture")
    }

    #[test]
    fn prepared_runtime_moves_across_a_thread_boundary_with_a_live_machine() {
        let mut runtime = m3_runtime();
        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("first entry runs on the constructing thread");

        let mut runtime = std::thread::spawn(move || {
            runtime
                .run_entry(None, &[], true, RealmId::ROOT)
                .expect("second entry runs on a different thread");
            runtime
        })
        .join()
        .expect("PreparedRuntime crosses the thread boundary intact");

        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("runtime carries a still-usable machine back on the original thread");
    }

    #[test]
    fn leases_acquired_under_a_realm_are_released_when_it_closes() {
        let (mut runtime, first) = session();
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("producer top binds");
        let realm = runtime.open_realm();
        runtime
            .install_prepared_in(
                consumer_program(false, Some(1)),
                &[(producer_identity(), id)],
                realm,
            )
            .expect("consumer installs under a realm-scoped lease");
        assert_eq!(runtime.bindings().lease_count(id), 1);

        let report = runtime.close_realm_report(realm);
        assert_eq!(
            report.leases_released, 1,
            "the realm's one lease is released"
        );
        assert_eq!(runtime.bindings().lease_count(id), 0);

        runtime
            .release_binding(id)
            .expect("the binding releases once its only lease is gone");
    }

    #[test]
    fn closing_root_preserves_session_bindings_and_import_leases() {
        let (mut runtime, first) = session();
        let id = runtime.bind_top(first, ValueId(0), "producer").unwrap();
        runtime
            .install_prepared_in(
                consumer_program(false, Some(1)),
                &[(producer_identity(), id)],
                RealmId::ROOT,
            )
            .unwrap();
        let handles = runtime.retained_handle_count();
        for _ in 0..2 {
            let report = runtime.close_realm_report(RealmId::ROOT);
            assert_eq!(
                (
                    report.frames,
                    report.handles_released,
                    report.leases_released
                ),
                (0, 0, 0)
            );
            assert_eq!(runtime.retained_handle_count(), handles);
            assert_eq!(runtime.bindings().lease_count(id), 1);
        }
        runtime
            .install_prepared_in(
                consumer_program(false, Some(1)),
                &[(producer_identity(), id)],
                RealmId::ROOT,
            )
            .expect("root binding remains a valid import");
    }

    #[test]
    fn managed_argument_from_another_realm_is_rejected_before_any_machine_call() {
        let (mut runtime, _first) = session();
        let realm_a = runtime.open_realm();
        let produced = runtime
            .run_entry_retained(None, &[], true, realm_a)
            .expect("producer entry runs and retains its result under realm_a");
        let PreparedValueResult::Managed(value) = produced
            .values
            .into_iter()
            .next()
            .expect("the producer entry returns one value")
        else {
            panic!("producer's entry returns a managed value");
        };

        // The producer's own entry takes no arguments; a second program
        // whose entry actually accepts one managed `LiftedRef` is needed to
        // exercise passing `value` as an argument at all.
        let identity = runtime
            .install_prepared_in(identity_program(), &[], realm_a)
            .expect("identity program installs alongside the producer");

        let realm_b = runtime.open_realm();
        let error = match runtime.run_entry_retained_in(
            identity,
            ValueId(0),
            &[PreparedArgument::Managed(&value)],
            false,
            realm_b,
        ) {
            Err(error) => error,
            Ok(_) => {
                panic!("a handle minted under realm_a must not run as an argument under realm_b")
            }
        };
        assert!(matches!(
            error,
            PreparedRuntimeError::CrossRealmArgument { realm } if realm == realm_b
        ));
        assert_eq!(error.kind(), PreparedFailureKind::Rejected);

        // The same handle is still accepted back under its own realm.
        runtime
            .run_entry_retained_in(
                identity,
                ValueId(0),
                &[PreparedArgument::Managed(&value)],
                false,
                realm_a,
            )
            .expect("the refused call left the machine and the handle usable");
    }

    #[test]
    fn parked_realm_reports_liveness_and_clears_on_release() {
        let (mut runtime, _first) = session();
        let realm = runtime.open_realm();
        let produced = runtime
            .run_entry_retained(None, &[], true, realm)
            .expect("producer entry runs and retains its result");
        let PreparedValueResult::Managed(value) = produced
            .values
            .into_iter()
            .next()
            .expect("the producer entry returns one value")
        else {
            panic!("producer's entry returns a managed value");
        };

        let hole = runtime.hole_for(&value, realm);
        assert_eq!(runtime.parked_realm(&hole), Some(realm));

        assert!(runtime.release(value));
        assert_eq!(
            runtime.parked_realm(&hole),
            None,
            "a released handle's hole is no longer live under any realm"
        );
    }
}
