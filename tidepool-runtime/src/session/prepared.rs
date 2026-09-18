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
use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};

use super::binding_table::BindingIndex;
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    AnswerPlan, CompileError, CompiledProgram, ExecutionError, ImportBindings, ParkRequest,
    PreparedCallOptions, PreparedFrameEvidence, PreparedHandle, PreparedInput, PreparedMachine,
    PreparedMachineOptions, PreparedOuter as CodegenPreparedOuter, PreparedResult,
    PreparedResultBatch, ProgramId, RunOptions, MAX_ANSWER_DEPTH,
};
// Re-exported: callers of this module's realm-scoped cancellation API
// (`open_realm`/`cancel_handle`/`close_realm`) need both types without a
// separate `tidepool_codegen` dependency of their own.
pub use tidepool_codegen::jit_machine::CancelHandle;
pub use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::suspension::ContinuationId;
pub use tidepool_codegen::suspension::{RealmId, ValueHandle};
use tidepool_repr::execution_schema::{
    link_program, CtorRow, Group, HeapRhs, ImportedValue, LinkError, MachineImports, ParseError,
    PreparedProgram, RuntimeRep, Signature, SiteDelivery, SiteRow, SymbolIdentity, TypeNode,
    TypeNodeId, ValueId,
};
use tidepool_repr::{DataConId, DataConTable, Literal, PrincipalId, SessionVarId};

use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use super::turn::{
    PREPARED_APPLY_ENTRY_TARGET, PREPARED_APPLY_VALUE_TARGET, PREPARED_DECODE_TARGET,
    PREPARED_RESUME_TARGET,
};

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
    #[error("session binding {0:?} is not a live prepared binding")]
    UnknownBinding(SessionVarId),
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
    /// A suspended request is not a constructor, so it names no site and no
    /// verb. The request and continuation were released; nothing was parked.
    #[error("the suspended request {constructor} names no typed site or verb")]
    UntypedRequest { constructor: String },
    /// A host-built answer with fields was offered to a frame parked for an
    /// open-reply request ([`UNSITED`]): there is no wire evidence to build
    /// it against, so the frame re-enters only by handle or with a
    /// field-less constructor. The frame stays parked.
    #[error("the parked request has an open reply type and accepts only handle or field-less constructor delivery")]
    UnsitedAnswer,
    /// The turn suspended under `HandleOrError`. The prepared route parks
    /// nothing under that policy: handled effects are answered from the
    /// parked frame ([`crate::session::ResidentSession`] offers every parked
    /// request to the session's handler stack), so a run that must not park
    /// cannot be handled either.
    #[error("the turn requested an effect under HandleOrError; the prepared route parks nothing under that policy")]
    UnhandledRequest,
    /// The session's effect handler stack claimed a parked request and then
    /// failed. The frame was aborted; nothing stays parked.
    #[error("effect handler for `{constructor}` failed: {detail}")]
    Handler { constructor: String, detail: String },
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
    /// The program that produced a suspension admits no decode entry
    /// (`__decodeValue`), so a `Value`-carrying leaf of its answer could
    /// never be lowered. Every turn template defines the entry; this is a
    /// stale or foreign artifact, never a user error.
    #[error("program {program:?} admits no `{entry}` entry, so a Value-carrying answer cannot be decoded")]
    NoDecodeEntry {
        program: ProgramId,
        entry: &'static str,
    },
    /// A `Value`-carrying leaf's JSON rendering did not decode back to a
    /// `Tidepool.Aeson.Value.Value` (aeson's decoder disagrees with the
    /// bridge renderer that produced the text, or the leaf reached a
    /// malformed shape the renderer could not fully express). The frame
    /// stays parked; every handle built before the failure is released.
    #[error("typed site {site} rejects a Value-carrying answer: {detail}")]
    AnswerRejected { site: u64, detail: String },
    /// A resumed handle (bare or framed) is not live in this engine's
    /// ledger: unknown, released, or minted under a different engine. The
    /// frame stays parked.
    #[error("resume delivered a handle that is not live in this engine's ledger")]
    UnknownHandle,
    /// [`PreparedEngine::run_rooted_entry`]/[`PreparedEngine::run_rooted_application`]
    /// found no installed program that both owns the rooted closure's object
    /// and admits the generic apply roots, and no OTHER installed program
    /// admits them either. Every executable template emits
    /// `__applyEntry`/`__applyValue` beside its settled scaffold, so this is
    /// only reachable before any program is installed.
    #[error(
        "no installed program admits `{}`/`{}`, so a rooted apply has nowhere to run",
        PREPARED_APPLY_ENTRY_TARGET,
        PREPARED_APPLY_VALUE_TARGET
    )]
    NoHostingProgram,
    /// The program hosting a rooted apply admits no `__applyEntry` entry.
    /// Every executable template defines it beside its settled scaffold;
    /// this is a stale or foreign artifact, never a user error.
    #[error("program {program:?} admits no `{entry}` entry, so a rooted apply cannot run")]
    NoApplyEntryEntry {
        program: ProgramId,
        entry: &'static str,
    },
    /// The program hosting a rooted apply admits no `__applyValue` entry. See
    /// [`Self::NoApplyEntryEntry`].
    #[error("program {program:?} admits no `{entry}` entry, so a rooted apply cannot run")]
    NoApplyValueEntry {
        program: ProgramId,
        entry: &'static str,
    },
}

impl PreparedRuntimeError {
    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_)
            | Self::Link(_)
            | Self::UnknownBinding(_)
            | Self::UnsettledEntry { .. }
            | Self::WrongEngine
            | Self::MissingProgram
            | Self::DuplicateSite { .. }
            | Self::ProjectionShape { .. }
            | Self::SiteConflict { .. }
            | Self::UnknownSite { .. }
            | Self::UntypedRequest { .. }
            | Self::UnsitedAnswer
            | Self::UnhandledRequest
            | Self::NoResumeEntry { .. }
            | Self::NoDecodeEntry { .. }
            | Self::AnswerDelivery { .. }
            | Self::AnswerConstructor { .. }
            | Self::AnswerShape { .. }
            | Self::AnswerUnconstructible { .. }
            | Self::AnswerRejected { .. }
            | Self::UnknownHandle
            | Self::NoHostingProgram
            | Self::NoApplyEntryEntry { .. }
            | Self::NoApplyValueEntry { .. }
            | Self::CrossRealmArgument { .. } => PreparedFailureKind::Rejected,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Compile(_) => PreparedFailureKind::Rejected,
            // A handler fault is this turn's own failure: the machine stays
            // reusable, exactly as Core reports a handler `EffectError`.
            Self::Handler { .. } => PreparedFailureKind::Language,
            Self::Run(error) => match error {
                ExecutionError::MissingEntry(_)
                | ExecutionError::Unsupported(_)
                | ExecutionError::Arguments { .. }
                | ExecutionError::ArgumentRepresentation { .. }
                | ExecutionError::UnknownPreparedHandle
                | ExecutionError::ImportShape { .. }
                | ExecutionError::DescriptorShape { .. }
                | ExecutionError::HostIdConflict { .. }
                | ExecutionError::ForeignExternals
                | ExecutionError::UnknownProgram(_)
                | ExecutionError::UnknownContinuation(_)
                | ExecutionError::Answer(_)
                | ExecutionError::Invariant(_)
                | ExecutionError::NotQuiescent => PreparedFailureKind::Rejected,
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
    /// The turn's admitted decode entry (`__decodeValue :: Text -> Either
    /// Text Value`, beside the entry in its module), when the artifact
    /// retained it. A `Value`-carrying leaf of an answer to a program
    /// without one is refused before anything is built.
    decode: Option<ValueId>,
    /// The turn's admitted generic apply entries (`__applyEntry f n = settle
    /// (f (I# n))`, `__applyValue f x = settle (f x)`, beside the entry in
    /// its module), when the artifact retained them. Looked up by
    /// [`PreparedEngine::run_rooted_entry`]/
    /// [`PreparedEngine::run_rooted_application`] to apply a rooted closure
    /// without compiling a fresh Core fragment for it.
    apply_entry: Option<ValueId>,
    apply_value: Option<ValueId>,
    /// The typed sites this program declares and the type graph they point
    /// into, kept for site-evidence resolution and answer validation after
    /// the machine has taken the program's code.
    sites: Vec<SiteRow>,
    types: Vec<TypeNode>,
    /// The request constructors this program answers at a synthetic site,
    /// by bridge id, each with the index of its row in `sites`.
    verb_sites: Vec<(DataConId, usize)>,
    /// Constructor identities and bridge ids by this program's local
    /// `ConstructorId`, so two programs' type graphs compare by identity
    /// rather than local index, and a bridge `Value`'s constructor resolves
    /// to the row that admits it.
    constructors: Vec<(SymbolIdentity, DataConId)>,
    /// `constructors`, indexed by qualified identity `(module, occurrence)`
    /// and built once in [`Self::of`], so a leaf lookup
    /// ([`Self::constructor_named`]) is one map lookup rather than a full
    /// scan repeated per leaf of an answer.
    by_identity: BTreeMap<(String, String), DataConId>,
}

/// What installing one program adds to the machine-owned evidence indexes.
struct EvidencePlan {
    sites: Vec<(u64, usize)>,
    verb_sites: Vec<(DataConId, usize)>,
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

    fn of(by_identity: &BTreeMap<(String, String), DataConId>) -> Option<Self> {
        let host_id =
            |occurrence: &str| by_identity.get(&(Self::MODULE.to_string(), occurrence.to_string()));
        Some(Self {
            done: *host_id("Done")?,
            suspended: *host_id("Suspended")?,
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
        let entry_module = tops
            .get(&entry)
            .map(|(identity, _)| identity.module.clone());
        let resume = entry_module.clone().and_then(|module| {
            tops.iter().find_map(|(id, (identity, _))| {
                (identity.module == module && identity.occurrence == PREPARED_RESUME_TARGET)
                    .then_some(*id)
            })
        });
        let decode = entry_module.clone().and_then(|module| {
            tops.iter().find_map(|(id, (identity, _))| {
                (identity.module == module && identity.occurrence == PREPARED_DECODE_TARGET)
                    .then_some(*id)
            })
        });
        let apply_entry = entry_module.clone().and_then(|module| {
            tops.iter().find_map(|(id, (identity, _))| {
                (identity.module == module && identity.occurrence == PREPARED_APPLY_ENTRY_TARGET)
                    .then_some(*id)
            })
        });
        let apply_value = entry_module.and_then(|module| {
            tops.iter().find_map(|(id, (identity, _))| {
                (identity.module == module && identity.occurrence == PREPARED_APPLY_VALUE_TARGET)
                    .then_some(*id)
            })
        });
        let constructors: Vec<(SymbolIdentity, DataConId)> = prepared
            .constructors()
            .iter()
            .map(|declaration| (declaration.identity.clone(), declaration.host_id))
            .collect();
        let by_identity: BTreeMap<(String, String), DataConId> = constructors
            .iter()
            .map(|(identity, host_id)| {
                (
                    (identity.module.clone(), identity.occurrence.clone()),
                    *host_id,
                )
            })
            .collect();
        let sites = prepared.sites().to_vec();
        // Validation guarantees every entry names a declared constructor and
        // an admitted row.
        let verb_sites = prepared
            .verb_sites()
            .iter()
            .filter_map(|(constructor, site)| {
                let (_, host_id) = constructors.get(constructor.0 as usize)?;
                let row = sites.iter().position(|row| row.site == *site)?;
                Some((*host_id, row))
            })
            .collect();
        Self {
            entry,
            tops,
            settled: SettledIds::of(&by_identity),
            resume,
            decode,
            apply_entry,
            apply_value,
            sites,
            verb_sites,
            types: prepared.types().to_vec(),
            constructors,
            by_identity,
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
    /// are a later slice; an unconstructible node refuses, EXCEPT the family
    /// this checks first: `Tidepool.Aeson.Value.Value` itself is
    /// unconstructible field-by-field (its `Object` row needs
    /// `Data.Map.Internal.Map`, its `Number` row needs `Scientific`'s
    /// unpacked fields), so any node of that family is lowered whole as a
    /// [`AnswerPlan::Json`] leaf instead of walking its rows — the leaf
    /// adapter `session::prepared`'s resume path resolves through the
    /// program's decode root before building. `table` names constructors for
    /// that rendering only; nothing here touches the machine, so a refusal
    /// leaves the frame exactly as parked.
    fn lower_answer(
        &self,
        site: u64,
        node: TypeNodeId,
        value: &Value,
        depth: usize,
        table: &DataConTable,
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
            Some(TypeNode::Data { family, .. }) if is_aeson_value(family) => {
                let rendered = crate::value_to_json(value, table, 0);
                Ok(AnswerPlan::Json(rendered.to_string()))
            }
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
                    planned.push(self.lower_answer(site, *field_node, field, depth + 1, table)?);
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
            Some(TypeNode::Text) => self.lower_text(site, value),
            Some(TypeNode::Integer) => self.lower_integer(site, value),
            Some(TypeNode::Natural) => self.lower_natural(site, value),
            Some(TypeNode::Unconstructible { reason, .. }) => {
                Err(PreparedRuntimeError::AnswerUnconstructible {
                    site,
                    reason: reason.clone(),
                })
            }
        }
    }

    /// The bridge id of a declared constructor, by qualified identity.
    fn constructor_named(&self, module: &str, occurrence: &str) -> Option<DataConId> {
        self.by_identity
            .get(&(module.to_string(), occurrence.to_string()))
            .copied()
    }

    /// The declared row for `host_id` among `node`'s rows, when `node` is a
    /// `Data` node. A framed-handle delivery validates its prefix fields
    /// against this row the same way an ordinary answer's constructor does.
    fn row_for(&self, node: TypeNodeId, host_id: DataConId) -> Option<&CtorRow> {
        match self.type_node(node)? {
            TypeNode::Data { rows, .. } => rows.iter().find(|row| {
                self.constructors
                    .get(row.constructor.0 as usize)
                    .is_some_and(|(_, declared)| *declared == host_id)
            }),
            _ => None,
        }
    }

    /// One byte-backed leaf: the constructor `module.occurrence` over a
    /// `ByteArray#` field followed by `scalars`.
    fn bytes_plan(
        &self,
        site: u64,
        module: &str,
        occurrence: &str,
        bytes: Vec<u8>,
        scalars: impl IntoIterator<Item = AnswerPlan>,
    ) -> Result<AnswerPlan, PreparedRuntimeError> {
        let host_id = self.constructor_named(module, occurrence).ok_or(
            PreparedRuntimeError::AnswerShape {
                site,
                detail:
                    "the site's program declares no constructor for its byte-backed answer type",
            },
        )?;
        let mut fields = vec![AnswerPlan::Bytes(bytes)];
        fields.extend(scalars);
        Ok(AnswerPlan::Constructor { host_id, fields })
    }

    /// `Text`: the bridge's `Text backing off len` (as `String::to_value`
    /// builds it) or a bare string literal; the slice must be in bounds and
    /// valid UTF-8. Built as `Text bytes 0 len` over a fresh byte array.
    fn lower_text(&self, site: u64, value: &Value) -> Result<AnswerPlan, PreparedRuntimeError> {
        let shape = |detail| PreparedRuntimeError::AnswerShape { site, detail };
        let text = self.constructor_named(TEXT_MODULE, "Text");
        let bytes = match value {
            Value::Lit(Literal::LitString(bytes)) => bytes.clone(),
            Value::Con(id, fields) if Some(*id) == text && fields.len() == 3 => {
                let backing = byte_backing(&fields[0])
                    .ok_or(shape("a Text answer's backing must be a byte array"))?;
                let (Value::Lit(Literal::LitInt(off)), Value::Lit(Literal::LitInt(len))) =
                    (&fields[1], &fields[2])
                else {
                    return Err(shape(
                        "a Text answer's offset and length must be Int literals",
                    ));
                };
                usize::try_from(*off)
                    .ok()
                    .zip(usize::try_from(*len).ok())
                    .and_then(|(off, len)| backing.get(off..off.checked_add(len)?))
                    .ok_or(shape("a Text answer's slice is out of bounds"))?
                    .to_vec()
            }
            _ => return Err(shape("a Text answer requires Text or a string literal")),
        };
        if std::str::from_utf8(&bytes).is_err() {
            return Err(shape("a Text answer must be valid UTF-8"));
        }
        let len = bytes.len() as i64;
        self.bytes_plan(
            site,
            TEXT_MODULE,
            "Text",
            bytes,
            [
                scalar_plan(RuntimeRep::Int(64), 0),
                scalar_plan(RuntimeRep::Int(64), len as u128),
            ],
        )
    }

    /// `Integer`: `IS Int#`, or `IP`/`IN` over canonical little-endian
    /// 64-bit limbs whose magnitude does not fit `IS` (GHC's invariant, which
    /// generated comparisons and conversions rely on). A bare `Int` literal
    /// is an `IS`.
    fn lower_integer(&self, site: u64, value: &Value) -> Result<AnswerPlan, PreparedRuntimeError> {
        let shape = |detail| PreparedRuntimeError::AnswerShape { site, detail };
        let named = |occurrence: &str| self.constructor_named(INTEGER_MODULE, occurrence);
        let small = |host_id: DataConId, value: i64| AnswerPlan::Constructor {
            host_id,
            fields: vec![scalar_plan(RuntimeRep::Int(64), value as u128)],
        };
        match value {
            Value::Lit(Literal::LitInt(value)) => {
                let is =
                    named("IS").ok_or(shape("the site's program declares no IS constructor"))?;
                Ok(small(is, *value))
            }
            Value::Con(id, fields) if Some(*id) == named("IS") => match fields.as_slice() {
                [Value::Lit(Literal::LitInt(value))] => Ok(small(*id, *value)),
                _ => Err(shape("IS takes one Int literal")),
            },
            Value::Con(id, fields) if Some(*id) == named("IP") || Some(*id) == named("IN") => {
                let positive = Some(*id) == named("IP");
                let limbs = bignat_limbs(fields).ok_or(shape(
                    "IP and IN take one canonical BigNat# payload of whole limbs",
                ))?;
                // Beyond the `IS` range: `IP` above i64::MAX, `IN` below i64::MIN.
                let fits_small = <[u8; 8]>::try_from(limbs.as_slice()).is_ok_and(|limb| {
                    let limb = u64::from_le_bytes(limb);
                    if positive {
                        limb <= i64::MAX as u64
                    } else {
                        limb <= 1_u64 << 63
                    }
                });
                if fits_small {
                    return Err(shape("a BigNat# payload must lie beyond the IS range"));
                }
                self.bytes_plan(
                    site,
                    INTEGER_MODULE,
                    if positive { "IP" } else { "IN" },
                    limbs,
                    [],
                )
            }
            _ => Err(shape(
                "an Integer answer requires IS, IP, IN or an Int literal",
            )),
        }
    }

    /// `Natural`: `NS Word#`, or `NB` over canonical limbs above `u64::MAX`.
    /// A bare word literal, or a non-negative `Int` literal, is an `NS`.
    fn lower_natural(&self, site: u64, value: &Value) -> Result<AnswerPlan, PreparedRuntimeError> {
        let shape = |detail| PreparedRuntimeError::AnswerShape { site, detail };
        let named = |occurrence: &str| self.constructor_named(NATURAL_MODULE, occurrence);
        let small = |host_id: DataConId, value: u64| AnswerPlan::Constructor {
            host_id,
            fields: vec![scalar_plan(RuntimeRep::Word(64), u128::from(value))],
        };
        let ns = || named("NS").ok_or(shape("the site's program declares no NS constructor"));
        match value {
            Value::Lit(Literal::LitWord(value)) => Ok(small(ns()?, *value)),
            Value::Lit(Literal::LitInt(value)) => {
                let value = u64::try_from(*value)
                    .map_err(|_| shape("a Natural answer cannot be negative"))?;
                Ok(small(ns()?, value))
            }
            Value::Con(id, fields) if Some(*id) == named("NS") => match fields.as_slice() {
                [Value::Lit(Literal::LitWord(value))] => Ok(small(*id, *value)),
                _ => Err(shape("NS takes one Word literal")),
            },
            Value::Con(id, fields) if Some(*id) == named("NB") => {
                let limbs = bignat_limbs(fields).ok_or(shape(
                    "NB takes one canonical BigNat# payload of whole limbs",
                ))?;
                if limbs.len() < 16 {
                    return Err(shape("a BigNat# payload must lie beyond the NS range"));
                }
                self.bytes_plan(site, NATURAL_MODULE, "NB", limbs, [])
            }
            _ => Err(shape("a Natural answer requires NS, NB or a word literal")),
        }
    }
}

const TEXT_MODULE: &str = "Data.Text.Internal";
const INTEGER_MODULE: &str = "GHC.Num.Integer";
const NATURAL_MODULE: &str = "GHC.Num.Natural";
const AESON_VALUE_MODULE: &str = "Tidepool.Aeson.Value";
const AESON_VALUE_OCCURRENCE: &str = "Value";

/// Whether `family` names the vendored `Tidepool.Aeson.Value.Value` type
/// (module+name, never the numeric `TypeNodeId`, which is per-artifact) —
/// the one family [`ProgramFacts::lower_answer`] lowers whole as JSON rather
/// than walking rows.
fn is_aeson_value(family: &SymbolIdentity) -> bool {
    family.module == AESON_VALUE_MODULE && family.occurrence == AESON_VALUE_OCCURRENCE
}

/// The raw bytes behind a bridge byte-array value, in any of the forms the
/// bridge emits for a `ByteArray#` backing.
fn byte_backing(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::ByteArray(bytes) => Some(
            bytes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        ),
        Value::Lit(Literal::LitByteArray(bytes) | Literal::LitString(bytes)) => Some(bytes.clone()),
        _ => None,
    }
}

/// The one `BigNat#` payload of an `IP`/`IN`/`NB` constructor as canonical
/// little-endian limbs: whole 64-bit words, at least one, top limb nonzero.
fn bignat_limbs(fields: &[Value]) -> Option<Vec<u8>> {
    let [payload] = fields else {
        return None;
    };
    let limbs = byte_backing(payload)?;
    let canonical = !limbs.is_empty()
        && limbs.len() % 8 == 0
        && limbs[limbs.len() - 8..].iter().any(|byte| *byte != 0);
    canonical.then_some(limbs)
}

fn scalar_plan(rep: RuntimeRep, word: u128) -> AnswerPlan {
    AnswerPlan::Scalar {
        rep,
        bits: word.to_ne_bytes(),
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

/// The site a frame parks under when its request has an open reply type.
/// Zero is never a declared site (`check_sites` refuses it).
const UNSITED: u64 = 0;

/// A boxed or unboxed non-negative `Int` field: the leading site argument of
/// an extractor-sited kernel request.
fn site_field(field: &Value, table: &DataConTable) -> Option<u64> {
    match field {
        Value::Lit(Literal::LitInt(n)) => u64::try_from(*n).ok(),
        Value::Con(id, inner) if table.name_of(*id) == Some("I#") => match inner.as_slice() {
            [Value::Lit(Literal::LitInt(n))] => u64::try_from(*n).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// The typed site a suspended request names, read from the request's
/// rendered payload: the protocol's sited helpers place the site id under the
/// `typedSite` key of the request's JSON payload object
/// (`tidepool-protocol`'s `ObjectValue::Site`), the same field the harness
/// classifies a Core suspension by. `None` for a request that carries no such
/// field: an ordinary handled effect.
fn typed_site_of(request: &Value, table: &DataConTable) -> Option<u64> {
    /// The `Text`/string-literal content of an aeson `Key`/`Value` leaf.
    fn value_text(value: &Value, table: &DataConTable) -> Option<String> {
        match value {
            Value::Lit(Literal::LitString(bytes)) => {
                std::str::from_utf8(bytes).ok().map(str::to_owned)
            }
            Value::Con(id, fields) if table.name_of(*id) == Some("Text") => {
                tidepool_bridge::shapes::text_bytes_clamped(fields, table)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            }
            _ => None,
        }
    }
    /// The numeric content of a `typedSite` leaf: an unboxed or boxed
    /// integral literal, or an aeson `Number` wrapping one. This is the one
    /// leaf this walk ever renders through the generic JSON decoder — never
    /// the whole request.
    fn value_u64(value: &Value, table: &DataConTable) -> Option<u64> {
        match value {
            Value::Lit(Literal::LitInt(n)) => u64::try_from(*n).ok(),
            Value::Lit(Literal::LitWord(n)) => Some(*n),
            Value::Con(_, _) => match crate::render::value_to_json(value, table, 0) {
                serde_json::Value::Number(n) => n.as_u64(),
                _ => None,
            },
            _ => None,
        }
    }
    /// Walk the request's `Value` tree directly (never its JSON rendering)
    /// looking for an aeson `Object` layer with a `typedSite` entry, the same
    /// depth bound (4) the JSON walk used.
    fn search(value: &Value, table: &DataConTable, depth: usize) -> Option<u64> {
        if depth > 4 {
            return None;
        }
        let Value::Con(id, fields) = value else {
            return None;
        };
        if table.name_of(*id) == Some("Object") {
            if let [map_val] = fields.as_slice() {
                let mut found = None;
                tidepool_bridge::shapes::walk_map_entries(
                    map_val,
                    table,
                    0,
                    MAX_ANSWER_DEPTH,
                    &mut |key, val, _| {
                        if found.is_none() && value_text(key, table).as_deref() == Some("typedSite")
                        {
                            found = value_u64(val, table);
                        }
                    },
                );
                if found.is_some() {
                    return found;
                }
            }
        }
        fields
            .iter()
            .find_map(|field| search(field, table, depth + 1))
    }
    search(request, table, 0)
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
    /// The machine-owned verb index: which installed program's synthetic
    /// site row answers an ordinary effect request, by the request's outer
    /// constructor. Extended in the same install transaction as `sites`,
    /// under the same structural-equivalence rule.
    verb_sites: BTreeMap<DataConId, SiteWitness>,
    /// Prepared old-space bytes as of the last successful
    /// [`Self::quiesce_and_collect`] (the compacted figure
    /// `RetirementReceipt::old_bytes` reports); `0` before any collection
    /// has run.
    old_bytes: usize,
    /// Programs installed ([`Self::bootstrap`] counts as the first one)
    /// since the last successful major collection. Reset to `0` only when
    /// [`Self::quiesce_and_collect_now`] actually runs `collect_major`; a
    /// `NotQuiescent` refusal leaves it untouched so the next eligible turn
    /// retries with the same count. Read by [`Self::major_collection_due`].
    installs_since_major: usize,
    /// [`PreparedMachine::old_bytes_live`] as of the last successful major
    /// collection -- a live read of promoted bytes, not the install-count
    /// proxy `residency().block_words` (which only grows with installs and
    /// so cannot see promotion happening between them). `0` before any
    /// collection has run, which disables the growth trigger until there is
    /// a real baseline to grow from (see [`Self::major_collection_due`]).
    old_bytes_at_last_major: usize,
    /// Count of major collections [`Self::quiesce_and_collect_now`] has
    /// actually run (incremented only on success, never on a `NotQuiescent`
    /// refusal). The prepared route's analogue of
    /// `JitEffectMachine::heap_stats`'s `gc_count` -- see [`Self::heap_stats`].
    major_collections: u64,
    /// Package-defined tops already installed on this machine, offered to
    /// every later turn as executable imports (see [`CodeExport`] and
    /// [`exportable_code_tops`]). Grows once, on the turns that first reach
    /// a package definition, and is never invalidated: a package's code
    /// cannot change under a live session.
    code_exports: BTreeMap<SymbolIdentity, CodeExport>,
}

/// One installed package top a later turn may import instead of projecting
/// its own copy of the body.
///
/// `handle` roots the top's value on this machine ([`PreparedMachine::retain_top`]),
/// which also keeps its defining program -- and therefore its code -- alive
/// for as long as anything can import it. `entry` is the PRODUCER's own
/// signature for the top, so [`link_program`] compares a later turn's
/// declared entry evidence against the real callable rather than against an
/// echo of itself.
#[derive(Clone)]
struct CodeExport {
    handle: PreparedHandle,
    entry: Option<Signature>,
}

/// The generation every code export is offered at. Package code is fixed for
/// the life of a process -- a different package set is a different extractor
/// deployment, which the toolchain fingerprint already refuses to mix -- so
/// there is nothing for a generation to distinguish, and every turn is told
/// the same number it will be handed back at install.
const CODE_EXPORT_GENERATION: u64 = 0;

/// GHC's unit id for everything compiled from source in this session: the
/// turn target, the session decl/value planes, the workspace source layer,
/// and the Tidepool library modules on the include path. All of it can
/// differ from one turn to the next, so none of it is ever exported.
const HOME_UNIT: &str = "main";

/// Which of `prepared`'s own tops later turns may import rather than project
/// a body for: the ones an INSTALLED PACKAGE defines.
///
/// A package's unit id (`ghc-internal`, `text-2.1.2-2594`, ...) names a
/// built, content-versioned package in the compiler's package database. Its
/// unfoldings cannot change while one session runs, so a top recovered from
/// one is the same code this turn, next turn, and after a source reload --
/// which is exactly what makes it safe to hand a later turn the already
/// compiled copy. Everything under [`HOME_UNIT`] is excluded for the
/// opposite reason.
///
/// Byte tops (GHC `StgTopStringLit`) are excluded: they have no managed
/// value to retain. A constructor top is excluded unless its result is a
/// lifted reference, because that is the only representation
/// [`PreparedMachine::retain_top`] mints a handle for and a representation
/// disagreement at link time would fail the turn rather than fall back.
fn exportable_code_tops(
    prepared: &PreparedProgram,
) -> Vec<(SymbolIdentity, ValueId, Option<Signature>)> {
    let signature = |id: tidepool_repr::execution_schema::SignatureId| {
        prepared.signatures().get(id.0 as usize).cloned()
    };
    let mut exports = Vec::new();
    for group in prepared.bindings() {
        let tops = match group {
            Group::NonRecursive(top) => std::slice::from_ref(top),
            Group::Recursive(tops) => tops.as_slice(),
        };
        for top in tops {
            if top.identity.unit == HOME_UNIT || top.identity.namespace != "value" {
                continue;
            }
            let entry = match &top.binding.rhs {
                HeapRhs::Bytes(_) => continue,
                HeapRhs::Constructor { constructor, .. } => {
                    let lifted = prepared
                        .constructors()
                        .get(constructor.0 as usize)
                        .is_some_and(|row| row.result_rep == RuntimeRep::LiftedRef);
                    if !lifted {
                        continue;
                    }
                    None
                }
                HeapRhs::Function { signature: id, .. } | HeapRhs::Thunk { signature: id, .. } => {
                    signature(*id)
                }
            };
            exports.push((top.identity.clone(), top.binding.id, entry));
        }
    }
    exports
}

/// How many programs may install between major collections before one runs
/// regardless of byte growth. Bounds the residency test's `programs` count:
/// `programs <= live_bindings + 1 + MAJOR_COLLECTION_INSTALL_INTERVAL`.
const MAJOR_COLLECTION_INSTALL_INTERVAL: usize = 4;

/// Live old-space growth since the last major collection, in bytes, that
/// forces one early even inside the install-count window -- whichever this
/// or the 50% relative threshold in [`PreparedEngine::major_collection_due`]
/// reaches first.
const MAJOR_COLLECTION_GROWTH_BYTES: usize = 1024 * 1024;

/// The run's parking policy, carried onto the eval thread beside the settle
/// plan: what a suspension is parked with.
#[derive(Clone, Copy)]
pub(crate) struct ParkPolicy {
    pub(crate) principal: PrincipalId,
    pub(crate) effect_policy: EffectRunPolicy,
    pub(crate) live_payload: LivePayloadPolicy,
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
///
/// `index` answers the "which id" question in O(1) (amortized) instead of
/// scanning every live binding -- see [`super::binding_table`] -- and `get`
/// on the resolved id is an ordinary `BindingTable` id lookup.
fn resolve_prepared_import<'a>(
    bindings: &'a BindingTable,
    index: &BindingIndex,
    identity: &SymbolIdentity,
    generation: Option<u64>,
) -> Option<&'a BindingEntry> {
    let id = index.resolve_prepared(identity, generation)?;
    bindings.get(id)
}

impl PreparedEngine {
    /// Create the session's machine from its first turn's program and
    /// install that program. The first turn can import nothing: no prepared
    /// binding exists before the machine does.
    pub fn bootstrap(prepared: PreparedProgram) -> Result<(Self, ProgramId), PreparedRuntimeError> {
        let facts = ProgramFacts::of(&prepared);
        let exports = exportable_code_tops(&prepared);
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
            verb_sites: BTreeMap::new(),
            old_bytes: 0,
            installs_since_major: 0,
            old_bytes_at_last_major: 0,
            major_collections: 0,
            code_exports: BTreeMap::new(),
        };
        // The first program can conflict only with itself.
        let plan = engine.plan_evidence(&facts)?;
        engine.programs.insert(program, facts);
        engine.publish_evidence(program, plan);
        engine.publish_code_exports(program, exports);
        // Held live across the install-to-first-run gap; the turn's
        // bind/complete path (`resident.rs`) unpins it once the run's
        // outcome is bound, released or parked.
        engine
            .machine
            .pin(program)
            .map_err(PreparedRuntimeError::Run)?;
        engine.installs_since_major += 1;
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

    /// The verb-index entries of `facts` that installing it would make
    /// canonical, under [`Self::plan_sites`]'s rule keyed by request
    /// constructor: a constructor already indexed (or earlier in `facts`)
    /// must be answered by a structurally equivalent row, and keeps its
    /// existing owner; otherwise the install is refused.
    fn plan_verb_sites(
        &self,
        facts: &ProgramFacts,
    ) -> Result<Vec<(DataConId, usize)>, PreparedRuntimeError> {
        let mut planned: Vec<(DataConId, usize)> = Vec::new();
        for &(host_id, row) in &facts.verb_sites {
            let site = &facts.sites[row];
            let (owner, owner_facts, owner_row) =
                if let Some(witness) = self.verb_sites.get(&host_id) {
                    let owner =
                        self.programs
                            .get(&witness.owner)
                            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                                witness.owner,
                            )))?;
                    (Some(witness.owner), owner, &owner.sites[witness.row])
                } else if let Some((_, earlier)) = planned.iter().find(|(id, _)| *id == host_id) {
                    (None, facts, &facts.sites[*earlier])
                } else {
                    planned.push((host_id, row));
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

    /// Both indexes' plans for installing `facts`, checked before anything
    /// is compiled or published.
    fn plan_evidence(&self, facts: &ProgramFacts) -> Result<EvidencePlan, PreparedRuntimeError> {
        Ok(EvidencePlan {
            sites: self.plan_sites(facts)?,
            verb_sites: self.plan_verb_sites(facts)?,
        })
    }

    /// Offer `program`'s package tops to every later turn as executable
    /// imports, so the next turn's projection drops their bodies instead of
    /// handing this machine a second copy to compile.
    ///
    /// Called only after `program` is installed and published, so a refused
    /// install offers nothing. An identity already exported keeps its
    /// existing handle: the first program to define it stays the one every
    /// later turn imports, and no second root is taken for it. A top that
    /// will not retain (no managed value at its slot) is simply not offered
    /// -- later turns keep projecting their own body for it, exactly as
    /// before -- so this can lose a speedup but never a program.
    fn publish_code_exports(
        &mut self,
        program: ProgramId,
        exports: Vec<(SymbolIdentity, ValueId, Option<Signature>)>,
    ) {
        for (identity, value, entry) in exports {
            if self.code_exports.contains_key(&identity) {
                continue;
            }
            let Ok(handle) = self.machine.retain_export_top(program, value) else {
                continue;
            };
            self.code_exports
                .insert(identity, CodeExport { handle, entry });
        }
    }

    /// Every package top this machine already carries, as the extractor
    /// wants them: `(identity, generation)` pairs whose bodies the next
    /// turn's projection drops in favour of a declared global.
    pub(crate) fn code_export_retentions(
        &self,
    ) -> impl Iterator<Item = (SymbolIdentity, u64)> + '_ {
        self.code_exports
            .keys()
            .map(|identity| (identity.clone(), CODE_EXPORT_GENERATION))
    }

    /// How many package tops later turns can import instead of recompiling.
    #[must_use]
    pub fn code_export_count(&self) -> usize {
        self.code_exports.len()
    }

    /// Make `program` the canonical owner of the planned rows and verb
    /// entries.
    fn publish_evidence(&mut self, program: ProgramId, plan: EvidencePlan) {
        let witness = |row| SiteWitness {
            owner: program,
            row,
        };
        self.sites.extend(
            plan.sites
                .into_iter()
                .map(|(site, row)| (site, witness(row))),
        );
        self.verb_sites.extend(
            plan.verb_sites
                .into_iter()
                .map(|(host_id, row)| (host_id, witness(row))),
        );
    }

    /// Install a later turn's program. Every global it declares is resolved
    /// to a live prepared binding in `bindings` by the identity recorded at
    /// bind time (at the declared `required_generation`, or the newest), and
    /// the artifact is linked against those bindings' live shape before
    /// anything is compiled, so a stale generation or an unresolvable
    /// identity is a typed link error with no machine side effect.
    pub(crate) fn install(
        &mut self,
        prepared: PreparedProgram,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let mut clock = std::time::Instant::now();
        let mut lap = || {
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(clock);
            clock = now;
            elapsed.as_millis() as u64
        };
        let mut values = MachineImports::default();
        let mut imports = ImportBindings::new();
        for declaration in prepared.globals() {
            let identity = &declaration.identity;
            let Some(entry) =
                resolve_prepared_import(bindings, index, identity, declaration.required_generation)
            else {
                // Not a session value binding: it may be a package top an
                // earlier turn already installed and this turn was told to
                // import (see `code_exports`). Absent from both: left out,
                // and `link_program` reports the typed `MissingImport`.
                if let Some(export) = self.code_exports.get(identity).cloned() {
                    let evaluated = self
                        .machine
                        .handle_is_evaluated(export.handle)
                        .map_err(PreparedRuntimeError::Run)?;
                    values.values.insert(
                        identity.clone(),
                        ImportedValue {
                            identity: identity.clone(),
                            rep: export.handle.rep(),
                            entry_signature: export.entry,
                            evaluated,
                            generation: CODE_EXPORT_GENERATION,
                        },
                    );
                    imports.insert(identity.clone(), export.handle);
                }
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
        let resolve_imports_ms = lap();
        let import_count = imports.len();
        let exports = exportable_code_tops(&prepared);
        let facts = ProgramFacts::of(&prepared);
        // Site evidence is checked before anything is compiled or published:
        // a conflicting duplicate leaves the machine, its programs and the
        // site index exactly as they were.
        let plan = self.plan_evidence(&facts)?;
        let evidence_ms = lap();
        let linked = link_program(prepared, &values)?;
        let link_ms = lap();
        let compiled = self
            .machine
            .compile_for_install(&linked)
            .map_err(PreparedRuntimeError::Compile)?;
        let compile_ms = lap();
        let program = self
            .machine
            .install_program(compiled, imports)
            .map_err(PreparedRuntimeError::Run)?;
        let install_ms = lap();
        tracing::info!(
            target: "tidepool_runtime::prepared_install",
            resolve_imports_ms,
            evidence_ms,
            link_ms,
            compile_ms,
            install_ms,
            imports = import_count,
            "prepared install"
        );
        self.programs.insert(program, facts);
        self.publish_evidence(program, plan);
        self.publish_code_exports(program, exports);
        // Held live across the install-to-first-run gap; the turn's
        // bind/complete path (`resident.rs`) unpins it once the run's
        // outcome is bound, released or parked.
        self.machine
            .pin(program)
            .map_err(PreparedRuntimeError::Run)?;
        self.installs_since_major += 1;
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
        let outer =
            self.take_first_managed(batch.values)
                .ok_or(PreparedRuntimeError::UnsettledEntry {
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
            self.release_all(managed.iter().copied());
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

    /// The admitted resume entry of `program`, read without holding a borrow
    /// past this call so callers remain free to release handles afterward.
    fn resume_entry_of(&self, program: ProgramId) -> Result<ValueId, PreparedRuntimeError> {
        self.programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .resume
            .ok_or(PreparedRuntimeError::NoResumeEntry {
                program,
                entry: PREPARED_RESUME_TARGET,
            })
    }

    /// The admitted decode entry of `program`, read without holding a
    /// borrow past this call.
    fn decode_entry_of(&self, program: ProgramId) -> Result<ValueId, PreparedRuntimeError> {
        self.programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .decode
            .ok_or(PreparedRuntimeError::NoDecodeEntry {
                program,
                entry: PREPARED_DECODE_TARGET,
            })
    }

    /// The admitted generic apply-entry entry (`__applyEntry`) of `program`.
    fn apply_entry_of(&self, program: ProgramId) -> Result<ValueId, PreparedRuntimeError> {
        self.programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .apply_entry
            .ok_or(PreparedRuntimeError::NoApplyEntryEntry {
                program,
                entry: PREPARED_APPLY_ENTRY_TARGET,
            })
    }

    /// The admitted generic apply-value entry (`__applyValue`) of `program`.
    fn apply_value_of(&self, program: ProgramId) -> Result<ValueId, PreparedRuntimeError> {
        self.programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .apply_value
            .ok_or(PreparedRuntimeError::NoApplyValueEntry {
                program,
                entry: PREPARED_APPLY_VALUE_TARGET,
            })
    }

    /// The program that should host a rooted apply of `handle`: the program
    /// whose descriptor owns the object `handle` refers to, when that
    /// program is still installed and admits both generic apply roots; else
    /// the most recently installed program that admits them. Every
    /// executable template emits `__applyEntry`/`__applyValue` beside its
    /// settled scaffold, so any installed program is normally a candidate —
    /// preferring the object's own owner keeps a rooted apply within the
    /// program whose code produced the closure whenever that is still live,
    /// without depending on install order to matter for correctness.
    pub(crate) fn hosting_program(&self, handle: PreparedHandle) -> Option<ProgramId> {
        fn admits_apply_roots(facts: &ProgramFacts) -> bool {
            facts.apply_entry.is_some() && facts.apply_value.is_some()
        }
        if let Some(owner) = self.machine.owner_of_handle(handle) {
            if self.programs.get(&owner).is_some_and(admits_apply_roots) {
                return Some(owner);
            }
        }
        self.programs
            .iter()
            .rev()
            .find(|&(_, facts)| admits_apply_roots(facts))
            .map(|(id, _)| *id)
    }

    /// Apply a rooted `Int -> M a` closure `f` to `argument` through the
    /// hosting program's `__applyEntry` root and finish the settled layer as
    /// far as reading its `Done`/`Suspended` shape — the caller
    /// ([`crate::session::resident::ResidentSession::run_rooted_entry_borrowed`])
    /// finishes a `Done` value or parks a `Suspended` one exactly as an
    /// ordinary prepared turn does. `f` is BORROWED: never released here,
    /// whatever the outcome. `argument` crosses as a bare unboxed scalar,
    /// matching `__applyEntry`'s `Int#` parameter.
    pub(crate) fn run_rooted_entry(
        &mut self,
        f: PreparedHandle,
        argument: i64,
        realm: RealmId,
    ) -> Result<(ProgramId, PreparedSettlement), PreparedRuntimeError> {
        let program = self
            .hosting_program(f)
            .ok_or(PreparedRuntimeError::NoHostingProgram)?;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let entry = self.apply_entry_of(program)?;
        let batch = self
            .machine
            .run_entry_retained(
                program,
                entry,
                &[
                    PreparedInput::Managed(f),
                    PreparedInput::Scalar(argument as u64),
                ],
                SETTLE_CALL,
                realm,
            )
            .map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(program, realm, batch)?;
        Ok((program, settlement))
    }

    /// Apply one rooted Haskell function `f` to one rooted Haskell value `x`
    /// through the hosting program's `__applyValue` root. Both handles are
    /// BORROWED: neither is released here, whatever the outcome. See
    /// [`Self::run_rooted_entry`] for the shared hosting-program and
    /// finishing contract
    /// ([`crate::session::resident::ResidentSession::run_rooted_application`]
    /// is the caller).
    pub(crate) fn run_rooted_application(
        &mut self,
        f: PreparedHandle,
        x: PreparedHandle,
        realm: RealmId,
    ) -> Result<(ProgramId, PreparedSettlement), PreparedRuntimeError> {
        let program = self
            .hosting_program(f)
            .ok_or(PreparedRuntimeError::NoHostingProgram)?;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let entry = self.apply_value_of(program)?;
        let batch = self
            .machine
            .run_entry_retained(
                program,
                entry,
                &[PreparedInput::Managed(f), PreparedInput::Managed(x)],
                SETTLE_CALL,
                realm,
            )
            .map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(program, realm, batch)?;
        Ok((program, settlement))
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
    pub(crate) fn park_suspension(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        park: ParkPolicy,
        request: PreparedHandle,
        continuation: PreparedHandle,
        table: &DataConTable,
    ) -> Result<PreparedParked, PreparedRuntimeError> {
        let resume_entry = match self.resume_entry_of(program) {
            Ok(resume_entry) => resume_entry,
            Err(error) => {
                self.release_all([request, continuation]);
                return Err(error);
            }
        };
        if park.effect_policy == EffectRunPolicy::HandleOrError {
            self.release_all([request, continuation]);
            return Err(PreparedRuntimeError::UnhandledRequest);
        }
        // The `Union` layer: an unpacked tag word and the lazy payload.
        let outer = self.machine.inspect_outer(request, realm);
        self.machine.release(request);
        let CodegenPreparedOuter::Constructor { fields, .. } = match outer {
            Ok(outer) => outer,
            Err(error) => {
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::Run(error));
            }
        };
        let payload = match self.take_first_managed(fields) {
            Some(payload) => payload,
            None => {
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::UnsettledEntry {
                    program,
                    detail: "the suspended Union carried no managed payload",
                });
            }
        };
        // The request is observed (forced) through the existing observe
        // path, exactly the value Core reports for a suspension.
        let observed =
            self.machine
                .observe_handle(program, payload, RunOptions::default().observation_budget);
        let request = match observed {
            Ok(request) => request,
            Err(error) => {
                self.machine.release(payload);
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::Run(error));
            }
        };
        // A live-payload policy names one field of THIS request Con (the
        // convention's field 1) as the value crossing the runtime boundary
        // by reference; mirror it into a persistent root BEFORE releasing
        // `payload`, exactly as `JitEffectMachine::run_suspendable_shared`
        // tenures the field's raw pointer on Core -- see
        // `Self::tenure_live_payload`.
        let live_payload_root =
            match self.tenure_live_payload(payload, realm, park.live_payload, &request) {
                Ok(root) => root,
                Err(error) => {
                    self.machine.release(payload);
                    self.machine.release(continuation);
                    return Err(PreparedRuntimeError::Run(error));
                }
            };
        self.machine.release(payload);
        // A request carrying a dynamic site names it; an ordinary effect
        // request is classified by its outer constructor through the verb
        // index, which names the synthetic row answering it.
        let classified = match typed_site_of(&request, table) {
            Some(site) => self
                .sites
                .get(&site)
                .map(|witness| (site, witness.owner))
                .ok_or(PreparedRuntimeError::UnknownSite { site }),
            None => match &request {
                Value::Con(host_id, fields) => Ok(self
                    .verb_sites
                    .get(host_id)
                    .and_then(|witness| {
                        let row = self.programs.get(&witness.owner)?.sites.get(witness.row)?;
                        Some((row.site, *witness))
                    })
                    .or_else(|| {
                        // Kernel requests of an extractor-sited verb
                        // (`receiveSited`, `serveSited`) carry the site id as
                        // their first `Int` field; the reply index is open,
                        // so they have no synthetic row.
                        let site = fields.first().and_then(|field| site_field(field, table))?;
                        self.sites.get(&site).map(|witness| (site, *witness))
                    })
                    // An open-reply request (an actor `call`'s `result`)
                    // has no wire evidence to build an answer against; it
                    // parks unsited and re-enters only by handle, as on Core.
                    .map_or((UNSITED, program), |(site, witness)| (site, witness.owner))),
                _ => Err(PreparedRuntimeError::UntypedRequest {
                    constructor: "a non-constructor value".to_owned(),
                }),
            },
        };
        let (site, owner) = match classified {
            Ok(classified) => classified,
            Err(error) => {
                self.machine.release(continuation);
                return Err(error);
            }
        };
        let evidence = PreparedFrameEvidence {
            owner,
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
                live_payload_root,
                ParkRequest {
                    principal: park.principal,
                    effect_policy: park.effect_policy,
                    live_payload: park.live_payload,
                    evidence,
                },
            )
            .map_err(PreparedRuntimeError::Run)?;
        Ok(PreparedParked { id, request })
    }

    /// Retain one field of `payload` (the request Con `park_suspension` just
    /// observed) as a persistent root, per `policy` -- the prepared route's
    /// analogue of `JitEffectMachine`'s `tenure_live_payload`, built from the
    /// primitives this layer actually has above the JIT boundary:
    /// `payload`'s OWN fields are read through `PreparedMachine::inspect_outer`
    /// (which mints a fresh handle per managed field), the policy's chosen
    /// field's handle is adopted into a bare root via
    /// [`PreparedMachine::take_handle_root`], and every other minted field
    /// handle is released immediately (`inspect_outer` is non-consuming and
    /// re-mints on every call, so nothing here is `payload`'s own retained
    /// registration).
    ///
    /// `LivePayloadPolicy::ValueField(field)` retains `field` whenever the
    /// bridged `request` has that many fields, whatever its shape;
    /// `ClosureField(field)` retains it only when the bridge found the
    /// closure sentinel there -- both read `request`, exactly as
    /// `JitEffectMachine`'s own `request_has_field`/
    /// `request_field_carries_closure_sentinel` do for Core. `None` when the
    /// policy names no field, the constructor doesn't have it, or (rare: a
    /// nullary/scalar-only request) the field is not itself managed.
    fn tenure_live_payload(
        &mut self,
        payload: PreparedHandle,
        realm: RealmId,
        policy: LivePayloadPolicy,
        request: &Value,
    ) -> Result<Option<tidepool_codegen::old_space::RootSlot>, ExecutionError> {
        let field = match policy {
            LivePayloadPolicy::None => None,
            LivePayloadPolicy::ClosureField(field) => {
                matches!(request, Value::Con(_, fields)
                    if fields.get(field).is_some_and(tidepool_codegen::heap_bridge::contains_closure_sentinel))
                .then_some(field)
            }
            LivePayloadPolicy::ValueField(field) => {
                matches!(request, Value::Con(_, fields) if field < fields.len()).then_some(field)
            }
        };
        let Some(field) = field else {
            return Ok(None);
        };
        let CodegenPreparedOuter::Constructor { fields, .. } =
            self.machine.inspect_outer(payload, realm)?;
        let mut root = None;
        for (index, value) in fields.into_iter().enumerate() {
            match value {
                PreparedResult::Managed(handle) if index == field => {
                    root = self.machine.take_handle_root(handle)?;
                }
                PreparedResult::Managed(handle) => {
                    self.machine.release(handle);
                }
                PreparedResult::Void | PreparedResult::Scalar(_) => {}
            }
        }
        Ok(root)
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

    /// Mint a [`ValueHandle`] over the declared live payload of the frame
    /// parked under `id`, mirroring `JitEffectMachine::handle_from_live_payload`
    /// on the Core route (`PreparedMachine::take_live_payload_handle`). The
    /// frame stays parked; `None` when `id` is not parked or its frame holds
    /// no untaken live payload.
    pub fn live_payload_handle(
        &mut self,
        id: ContinuationId,
    ) -> Result<Option<ValueHandle>, PreparedRuntimeError> {
        self.machine
            .take_live_payload_handle(id)
            .map_err(PreparedRuntimeError::Run)
    }

    /// [`Self::live_payload_handle`] with the handle owned by `realm` rather
    /// than the frame's own realm (see `PreparedMachine::take_live_payload_handle_owned_by`).
    pub fn live_payload_handle_owned_by(
        &mut self,
        id: ContinuationId,
        realm: RealmId,
    ) -> Result<Option<ValueHandle>, PreparedRuntimeError> {
        self.machine
            .take_live_payload_handle_owned_by(id, Some(realm))
            .map_err(PreparedRuntimeError::Run)
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

    /// [`Self::resume_parked`], but `answer` is BORROWED rather than
    /// consumed: it is delivered to the resume entry and left exactly as
    /// live afterward, custody unchanged — the prepared analogue of Core's
    /// `ResumeInput::Handle` delivery
    /// (`docs/continuation-parking-contract.md`), which reads a handle's
    /// current heap pointer without releasing its root. No realm check: a
    /// handle is meant to move between parked continuations across resource
    /// scopes (see `ValueHandle`'s own doc), unlike a freshly built answer,
    /// which is always realm-scoped to the frame it answers.
    fn resume_parked_borrowed(
        &mut self,
        id: ContinuationId,
        answer: PreparedHandle,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let (realm, _) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let (continuation, evidence) = self
            .machine
            .take_parked(id)
            .map_err(PreparedRuntimeError::Run)?;
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
        // `answer` stays live: its custody is the caller's, before and after.
        let batch = batch.map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(evidence.runner, realm, batch)?;
        Ok(PreparedResumed {
            settlement,
            realm,
            runner: evidence.runner,
        })
    }

    /// Re-enter the frame parked under `id` by delivering an
    /// already-retained value verbatim — no materialization, closures
    /// included, the same shape Core's `ResumeInput::Handle` delivers. `raw`
    /// must be live in this engine's ledger; the only check possible on this
    /// route is its `RuntimeRep` (every handle this engine mints is
    /// `LiftedRef`), matching Core's own lack of a deeper type check on this
    /// path. The handle's root is a BORROW: this call does not release it.
    pub fn resume_with_handle(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let handle = self
            .machine
            .prepared_handle_of(raw)
            .ok_or(PreparedRuntimeError::UnknownHandle)?;
        self.resume_parked_borrowed(id, handle)
    }

    /// Re-enter the frame parked under `id` with a constructor whose final
    /// field borrows `raw` verbatim: `prefix` is lowered against the site's
    /// declared row for `constructor` exactly as an ordinary answer's fields
    /// are (`Value`-carrying prefix fields resolve through the decode entry
    /// the same way), and the borrowed field is spliced in unvalidated
    /// beyond its `RuntimeRep`, mirroring Core's own framed delivery
    /// (`docs/continuation-parking-contract.md`). The built constructor is
    /// released as usual once the resume entry has read it; `raw`'s root is
    /// untouched throughout.
    pub fn resume_with_framed_handle(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
        constructor: DataConId,
        prefix: Vec<Value>,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let handle = self
            .machine
            .prepared_handle_of(raw)
            .ok_or(PreparedRuntimeError::UnknownHandle)?;
        let (realm, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let site = evidence.site;
        if site == UNSITED {
            return Err(PreparedRuntimeError::UnsitedAnswer);
        }
        // Copied out before the cancellation check below takes `self.machine`
        // mutably: `evidence` itself stays borrowed from it, so a field read
        // after that point would conflict.
        let runner = evidence.runner;
        let owner = self
            .programs
            .get(&evidence.owner)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                evidence.owner,
            )))?;
        let row = owner
            .sites
            .iter()
            .find(|row| row.site == site)
            .ok_or(PreparedRuntimeError::UnknownSite { site })?;
        if row.delivery != SiteDelivery::HostAnswer {
            return Err(PreparedRuntimeError::AnswerDelivery {
                site,
                delivery: row.delivery,
            });
        }
        let ctor_row = owner.row_for(row.wire, constructor).ok_or(
            PreparedRuntimeError::AnswerConstructor {
                site,
                host_id: constructor,
            },
        )?;
        if ctor_row.fields.len() != prefix.len() + 1 {
            return Err(PreparedRuntimeError::AnswerShape {
                site,
                detail: "the framed constructor's declared field count does not match the \
                         supplied prefix plus the borrowed handle field",
            });
        }
        let field_nodes = ctor_row.fields[..prefix.len()].to_vec();
        let mut fields = Vec::with_capacity(prefix.len() + 1);
        for (field_node, field) in field_nodes.iter().zip(&prefix) {
            fields.push(owner.lower_answer(site, *field_node, field, 0, table)?);
        }
        fields.push(AnswerPlan::Handle(handle));
        let plan = AnswerPlan::Constructor {
            host_id: constructor,
            fields,
        };
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut produced = Vec::new();
        let built = self
            .resolve_json_leaves(site, runner, realm, plan, table, &mut produced)
            .and_then(|plan| {
                self.machine
                    .build_answer(realm, &plan)
                    .map_err(PreparedRuntimeError::Run)
            });
        // Decoded prefix leaves are rooted by the built constructor from here
        // on (or by nothing, on failure); the borrowed final field is not in
        // `produced` and is untouched either way.
        self.release_all(produced);
        self.resume_parked(id, built?)
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
        table: &DataConTable,
    ) -> Result<AnswerPlan, PreparedRuntimeError> {
        let (_, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        if evidence.site == UNSITED {
            // A field-less constructor is the one host-built answer an
            // open-reply frame accepts: it has no payload for wire evidence
            // to shape, so building it needs only the constructor's own
            // interned descriptor (`Nothing` closing a stateful actor's
            // receive on drain). Anything with a field re-enters by handle.
            return match value {
                Value::Con(host_id, fields) if fields.is_empty() => Ok(AnswerPlan::Constructor {
                    host_id: *host_id,
                    fields: Vec::new(),
                }),
                _ => Err(PreparedRuntimeError::UnsitedAnswer),
            };
        }
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
        owner.lower_answer(row.site, row.wire, value, 0, table)
    }

    /// Resolve every [`AnswerPlan::Json`] leaf of `plan` (a `Value`-carrying
    /// field [`ProgramFacts::lower_answer`] could not walk into rows) to an
    /// [`AnswerPlan::Handle`]: render the leaf as a retained `Text`, enter
    /// `runner`'s admitted decode entry, and project `Right v` to `v`'s
    /// handle. `Left _` is a typed [`PreparedRuntimeError::AnswerRejected`]
    /// refusal. Every handle this pass decodes is pushed onto `produced`,
    /// and ONLY those: a caller-supplied [`AnswerPlan::Handle`] (a borrowed
    /// framed-delivery field) is never listed there. The caller releases
    /// `produced` on any failure, so a refusal leaves nothing extra rooted,
    /// and again once `build_answer` has copied the decoded words into the
    /// built answer, which roots them from then on. The frame stays parked
    /// throughout (this runs before the take, like [`Self::answer_plan`] and
    /// `build_answer`).
    fn resolve_json_leaves(
        &mut self,
        site: u64,
        runner: ProgramId,
        realm: RealmId,
        plan: AnswerPlan,
        table: &DataConTable,
        produced: &mut Vec<PreparedHandle>,
    ) -> Result<AnswerPlan, PreparedRuntimeError> {
        match plan {
            AnswerPlan::Json(text) => {
                let handle = self.decode_json_leaf(site, runner, realm, &text, table)?;
                produced.push(handle);
                Ok(AnswerPlan::Handle(handle))
            }
            AnswerPlan::Constructor { host_id, fields } => {
                let mut resolved = Vec::with_capacity(fields.len());
                for field in fields {
                    resolved.push(
                        self.resolve_json_leaves(site, runner, realm, field, table, produced)?,
                    );
                }
                Ok(AnswerPlan::Constructor {
                    host_id,
                    fields: resolved,
                })
            }
            other @ (AnswerPlan::Scalar { .. } | AnswerPlan::Bytes(_) | AnswerPlan::Handle(_)) => {
                Ok(other)
            }
        }
    }

    /// Decode one `Value`-carrying leaf's JSON text through `runner`'s
    /// admitted decode entry, returning the decoded value's retained handle.
    fn decode_json_leaf(
        &mut self,
        site: u64,
        runner: ProgramId,
        realm: RealmId,
        text: &str,
        table: &DataConTable,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let decode_entry = self.decode_entry_of(runner)?;
        let owner = self.programs.get(&runner).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownProgram(runner),
        ))?;
        let reject = |detail: &str| PreparedRuntimeError::AnswerRejected {
            site,
            detail: detail.to_string(),
        };
        // `Either`'s constructors are never walked by `TypePolicy` (it only
        // interns a SITE's own answer type; `__decodeValue`'s signature is
        // not a site), so `owner`'s own declared-constructor table never
        // carries them. The session-wide `DataConTable` does: every
        // compile's `prTyCons` registers every constructor GHC's own type
        // checker sees, `Either`'s included, regardless of which types a
        // site happens to answer with. `DataConId` is the same bridge-wide
        // space `inspect_outer`'s `identity` reads from, so the two compare
        // directly.
        let left = table.get_by_qualified_name("Data.Either.Left");
        let right = table.get_by_qualified_name("Data.Either.Right");
        let (left, right) = match (left, right) {
            (Some(left), Some(right)) => (left, right),
            _ => {
                return Err(reject(
                    "the runner declares no Either constructors to read the decode result",
                ))
            }
        };
        let text_plan = owner.lower_text(
            site,
            &Value::Lit(Literal::LitString(text.as_bytes().to_vec())),
        )?;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let text_handle = self
            .machine
            .build_answer(realm, &text_plan)
            .map_err(PreparedRuntimeError::Run)?;
        let batch = self.machine.run_entry_retained(
            runner,
            decode_entry,
            &[PreparedInput::Managed(text_handle)],
            SETTLE_CALL,
            realm,
        );
        self.machine.release(text_handle);
        let batch = batch.map_err(PreparedRuntimeError::Run)?;
        let outer = self
            .take_first_managed(batch.values)
            .ok_or_else(|| reject("the decode entry returned no managed Either value"))?;
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
        if identity == right {
            match managed.as_slice() {
                [value] => Ok(*value),
                _ => {
                    self.release_all(managed);
                    Err(reject("Right carried other than one managed field"))
                }
            }
        } else {
            self.release_all(managed);
            if identity == left {
                Err(reject("the Value-carrying answer failed to decode"))
            } else {
                Err(reject("the decode entry returned neither Left nor Right"))
            }
        }
    }

    /// Re-enter the frame parked under `id` with a host-built answer: peek,
    /// validate and lower `value` against the site evidence, resolve any
    /// `Value`-carrying leaves through the decode entry
    /// ([`Self::resolve_json_leaves`]), build the result into a realm-owned
    /// handle, then take the frame and enter the resume entry
    /// ([`Self::resume_parked`]). A plan that is itself one resolved `Value`
    /// leaf (the site's whole answer type is `Value`) delivers that leaf's
    /// handle directly — a decoded `Value` is already a retained heap object,
    /// so wrapping it in another constructor is unnecessary. Every failure
    /// before the take leaves the frame parked with the handle and root
    /// counts unchanged.
    pub fn resume_with_answer(
        &mut self,
        id: ContinuationId,
        value: &Value,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let plan = self.answer_plan(id, value, table)?;
        let (realm, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let site = evidence.site;
        let runner = evidence.runner;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut produced = Vec::new();
        let plan = match self.resolve_json_leaves(site, runner, realm, plan, table, &mut produced) {
            Ok(plan) => plan,
            Err(error) => {
                self.release_all(produced);
                return Err(error);
            }
        };
        if let AnswerPlan::Handle(handle) = plan {
            // The whole answer is one decoded leaf: it is `produced`'s only
            // entry, and `resume_parked` releases it after the entry reads it.
            return self.resume_parked(id, handle);
        }
        let built = self
            .machine
            .build_answer(realm, &plan)
            .map_err(PreparedRuntimeError::Run);
        // The built answer now roots every decoded leaf it copied in; the
        // leaves' own handles are released whether or not the build succeeded.
        self.release_all(produced);
        self.resume_parked(id, built?)
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

    /// [`Self::observe`] under the same budget, but a budget that runs out
    /// CUTS the walk instead of failing it: the result is a bounded SELECTION
    /// carrying `tidepool_codegen::heap_bridge::OVERSIZE_SENTINEL` wherever a
    /// subtree was left unread. Use it where a display-sized limit must not
    /// discard work that already ran; the handle stays retained, so the part
    /// the cut omitted is still reachable through the binding.
    pub fn observe_bounded(
        &mut self,
        program: ProgramId,
        handle: PreparedHandle,
    ) -> Result<Value, PreparedRuntimeError> {
        self.machine
            .observe_handle_bounded(program, handle, RunOptions::default().observation_budget)
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
            self.release_all(managed);
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

    /// [`Self::release`] by the bare cross-engine [`ValueHandle`] id --
    /// `ResidentSession::settle_dropped_custody`'s deferred-cleanup path,
    /// which only ever recovers a dropped [`crate::session::RootCustody`]'s
    /// raw id (see `PreparedMachine::discard_handle`'s doc).
    pub fn discard_handle(&mut self, handle: ValueHandle) -> bool {
        self.machine.discard_handle(handle)
    }

    /// Move a live handle to another runtime resource scope
    /// (`PreparedMachine::rehome_handle`).
    pub fn rehome_handle(&mut self, handle: ValueHandle, owner: RealmId) -> bool {
        self.machine.rehome_handle(handle, owner)
    }

    /// The persistent root slot behind a retained handle, by its bare
    /// cross-engine [`ValueHandle`] id -- `ResidentSession::run_rooted_entry`'s
    /// slot lookup, which only ever holds a `RootCustody`'s raw id (see
    /// [`Self::discard_handle`]'s doc for the same shape).
    #[must_use]
    pub fn handle_slot(
        &self,
        handle: ValueHandle,
    ) -> Option<tidepool_codegen::old_space::RootSlot> {
        self.machine.handle_slot(handle)
    }

    /// Look up a bare cross-engine [`ValueHandle`] (a [`crate::session::RootCustody`]'s
    /// raw id) as this engine's own [`PreparedHandle`], when it is live in
    /// this machine's ledger -- `settle_rooted_entry`/`settle_rooted_application`'s
    /// resolution of a rooted apply's borrowed argument handles.
    #[must_use]
    pub fn prepared_handle_of(&self, handle: ValueHandle) -> Option<PreparedHandle> {
        self.machine.prepared_handle_of(handle)
    }

    /// Release every handle in `handles`.
    pub fn release_all(&mut self, handles: impl IntoIterator<Item = PreparedHandle>) {
        for handle in handles {
            self.machine.release(handle);
        }
    }

    /// Take the first managed value out of a batch of call results, releasing
    /// every other managed value the batch carried. Non-managed results
    /// (`Void`, `Scalar`) are ignored.
    fn take_first_managed(
        &mut self,
        values: impl IntoIterator<Item = PreparedResult>,
    ) -> Option<PreparedHandle> {
        let mut first = None;
        for value in values {
            match (value, &first) {
                (PreparedResult::Managed(handle), None) => first = Some(handle),
                (PreparedResult::Managed(handle), Some(_)) => {
                    self.machine.release(handle);
                }
                (PreparedResult::Void | PreparedResult::Scalar(_), _) => {}
            }
        }
        first
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

    /// The machine's residency counters at this point (only meaningful
    /// right after [`Self::quiesce_and_collect`] has run; otherwise an
    /// ordinary live snapshot).
    #[must_use]
    pub fn residency(&self) -> tidepool_codegen::prepared_program::ResidencyCounts {
        self.machine.residency()
    }

    /// Lifetime Cranelift work this session's installs have paid for:
    /// `(functions, code_bytes)`. Diff it across one turn to see how much
    /// code generation that turn caused.
    #[must_use]
    pub fn codegen_totals(&self) -> (u64, u64) {
        (
            self.machine.compiled_functions(),
            self.machine.compiled_code_bytes(),
        )
    }

    /// Release a pin taken at install time ([`Self::install`],
    /// [`Self::bootstrap`]). `false` if it was not held (already unpinned,
    /// or an unknown program).
    pub fn unpin(&mut self, program: ProgramId) -> bool {
        self.machine.unpin(program)
    }

    /// Whether [`Self::quiesce_and_collect`] should actually run a major
    /// collection at this between-turn point, rather than return without
    /// touching the machine: either the install-count window has closed
    /// (`installs_since_major >= MAJOR_COLLECTION_INSTALL_INTERVAL`), or live
    /// old-space bytes have grown by at least [`MAJOR_COLLECTION_GROWTH_BYTES`]
    /// or 50% since the baseline recorded at the last major collection
    /// ([`Self::old_bytes_at_last_major`]). The growth check is skipped while
    /// there is no baseline yet (`0`, before any collection has run) -- the
    /// install count alone gates the first collection, matching the
    /// residency test's bound.
    ///
    /// `PreparedMachine::old_bytes_live` is read fresh here: unlike
    /// `residency().block_words` (which only grows with installs and is
    /// therefore redundant with the install-count trigger above), it reads
    /// the old space's own live byte count, so a turn that promotes a large
    /// structure without installing another program still trips the growth
    /// trigger early.
    #[must_use]
    fn major_collection_due(&self) -> bool {
        if self.installs_since_major >= MAJOR_COLLECTION_INSTALL_INTERVAL {
            return true;
        }
        if self.old_bytes_at_last_major == 0 {
            return false;
        }
        let baseline_bytes = self.old_bytes_at_last_major;
        let current_bytes = self.machine.old_bytes_live();
        let grown = current_bytes.saturating_sub(baseline_bytes);
        grown >= MAJOR_COLLECTION_GROWTH_BYTES
            || current_bytes.saturating_mul(2) >= baseline_bytes.saturating_mul(3)
    }

    /// Programs installed since the last successful major collection --
    /// [`Self::bootstrap`] and every accepted [`Self::install`] count.
    /// Exposed for tests and diagnostics; production code drives this only
    /// through [`Self::quiesce_and_collect`].
    #[must_use]
    pub fn installs_since_major(&self) -> usize {
        self.installs_since_major
    }

    /// The between-turn quiescent point, gated by [`Self::major_collection_due`]:
    /// when the policy is not due, this returns `Ok(())` without proving
    /// quiescence or touching the machine at all -- most turns pay nothing
    /// here. When it is due, this proves the machine is quiescent and, if
    /// so, runs a major collection exactly as [`Self::quiesce_and_collect_now`]
    /// does. A `quiesce` refusal ([`ExecutionError::NotQuiescent`]: the
    /// machine is mid-call, holds temporary roots, or an observation borrows
    /// old space) is not reported: the caller is simply not at a quiescent
    /// point yet, the policy's counters are left exactly as they are, and
    /// the next eligible turn retries.
    pub fn quiesce_and_collect(&mut self) -> Result<(), PreparedRuntimeError> {
        if !self.major_collection_due() {
            return Ok(());
        }
        self.quiesce_and_collect_now()
    }

    /// Force the between-turn quiescent point regardless of
    /// [`Self::major_collection_due`]: prove the machine is quiescent
    /// (`PreparedMachine::quiesce`) and, if so, run a major collection and
    /// drain its retirement receipt -- removing each retired program's
    /// [`ProgramFacts`] and re-homing or dropping the site witnesses it
    /// canonically owned ([`Self::retire_site_witnesses`]). A `quiesce`
    /// refusal ([`ExecutionError::NotQuiescent`]) is not reported and resets
    /// nothing (see [`Self::quiesce_and_collect`]'s doc); every other
    /// failure of the gate or the collection is returned. On a successful
    /// collection, [`Self::installs_since_major`] resets to `0` and
    /// [`Self::old_bytes_at_last_major`] is rebaselined from the
    /// post-collection live old-space bytes. The prepared route currently
    /// leases nothing per program, so there are no leases to release here
    /// (see the S4/G2 test module doc below). Used directly by tests and by
    /// any explicit session-level "collect now" entry point.
    pub fn quiesce_and_collect_now(&mut self) -> Result<(), PreparedRuntimeError> {
        let token = match self.machine.quiesce() {
            Ok(token) => token,
            Err(ExecutionError::NotQuiescent) => return Ok(()),
            Err(error) => return Err(PreparedRuntimeError::Run(error)),
        };
        let receipt = match self.machine.collect_major(token) {
            Ok(receipt) => receipt,
            Err(ExecutionError::NotQuiescent) => return Ok(()),
            Err(error) => return Err(PreparedRuntimeError::Run(error)),
        };
        self.old_bytes = receipt.old_bytes;
        for program in &receipt.programs {
            if let Some(facts) = self.programs.remove(program) {
                self.retire_site_witnesses(*program, &facts);
            }
        }
        self.installs_since_major = 0;
        self.old_bytes_at_last_major = self.machine.old_bytes_live();
        self.major_collections += 1;
        Ok(())
    }

    /// Read-only heap/GC snapshot mirroring `JitEffectMachine::heap_stats` on
    /// the Core route. Field mapping onto the prepared machine's own
    /// accounting (`docs/GLOSSARY.md`'s vocabulary does not yet cover this
    /// route, so the mapping is documented here instead):
    ///
    /// - `fragments` ↔ installed programs ([`Self::residency`]'s
    ///   `programs`) -- BOUNDED by [`Self::quiesce_and_collect_now`]'s
    ///   retirement, unlike Core's monotonic compiled-function count, so the
    ///   harness's fragment-ceiling rotation is a no-op on this engine by
    ///   design: this count can fall as programs retire, and never needs the
    ///   rotation Core relies on to bound it;
    /// - `live_bytes` ↔ prepared old-space bytes as of the last successful
    ///   major collection ([`Self::old_bytes`]);
    /// - `gc_count` ↔ major collections actually run
    ///   ([`Self::major_collections`]) -- there is no nursery-collection
    ///   counter exposed at this boundary;
    /// - `nursery_bytes` ↔ `0`: `PreparedMachine` does not expose its
    ///   nursery capacity today, and no consumer of this snapshot reads it.
    #[must_use]
    pub fn heap_stats(&self) -> tidepool_codegen::jit_machine::HeapStats {
        tidepool_codegen::jit_machine::HeapStats {
            nursery_bytes: 0,
            live_bytes: self.old_bytes,
            gc_count: self.major_collections,
            fragments: self.residency().programs as u64,
        }
    }

    /// Prepared old-space bytes as of the last successful
    /// [`Self::quiesce_and_collect`] -- the compacted figure
    /// `RetirementReceipt::old_bytes` reported, not a live recount.
    #[must_use]
    pub fn old_bytes(&self) -> usize {
        self.old_bytes
    }

    /// Site and verb witnesses `retired` canonically owned: each moves to a
    /// still-installed program that declares a structurally equivalent row
    /// for the same site id (or request constructor) ([`sites_equivalent`]),
    /// or is dropped if none remains -- a later install can re-claim it
    /// fresh.
    fn retire_site_witnesses(&mut self, retired: ProgramId, facts: &ProgramFacts) {
        let owned: Vec<u64> = self
            .sites
            .iter()
            .filter(|(_, witness)| witness.owner == retired)
            .map(|(site, _)| *site)
            .collect();
        for site in owned {
            let Some(row) = facts.sites.iter().find(|row| row.site == site) else {
                self.sites.remove(&site);
                continue;
            };
            let successor = self
                .programs
                .iter()
                .find_map(|(candidate, candidate_facts)| {
                    candidate_facts
                        .sites
                        .iter()
                        .position(|candidate_row| {
                            candidate_row.site == site
                                && sites_equivalent(facts, row, candidate_facts, candidate_row)
                        })
                        .map(|row_index| (*candidate, row_index))
                });
            match successor {
                Some((owner, row)) => {
                    self.sites.insert(site, SiteWitness { owner, row });
                }
                None => {
                    self.sites.remove(&site);
                }
            }
        }
        let owned: Vec<(DataConId, usize)> = self
            .verb_sites
            .iter()
            .filter(|(_, witness)| witness.owner == retired)
            .map(|(host_id, witness)| (*host_id, witness.row))
            .collect();
        for (host_id, row) in owned {
            let Some(row) = facts.sites.get(row) else {
                self.verb_sites.remove(&host_id);
                continue;
            };
            let successor = self
                .programs
                .iter()
                .find_map(|(candidate, candidate_facts)| {
                    candidate_facts
                        .verb_sites
                        .iter()
                        .find(|(candidate_host, candidate_row)| {
                            *candidate_host == host_id
                                && sites_equivalent(
                                    facts,
                                    row,
                                    candidate_facts,
                                    &candidate_facts.sites[*candidate_row],
                                )
                        })
                        .map(|(_, candidate_row)| SiteWitness {
                            owner: *candidate,
                            row: *candidate_row,
                        })
                });
            match successor {
                Some(witness) => {
                    self.verb_sites.insert(host_id, witness);
                }
                None => {
                    self.verb_sites.remove(&host_id);
                }
            }
        }
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
        testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
        GlobalDecl, GlobalId, ResultContract, ScalarLiteral, SignatureId, UpdatePolicy, ValueRef,
    };

    #[test]
    fn integrity_failure_is_typed_independently_from_its_cause() {
        let failure = MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        };
        let error = PreparedRuntimeError::Run(ExecutionError::Runtime(failure.clone()));
        assert_eq!(error.kind(), PreparedFailureKind::Integrity);
        assert!(matches!(
            error,
            PreparedRuntimeError::Run(ExecutionError::Runtime(retained)) if retained == failure
        ));
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

    // ---- S4/G2: `link_program`'s identity/generation-linked import
    // contract, ported from the deleted `PreparedRuntime`'s own
    // `bind_top`/`install_prepared` bookkeeping.
    //
    // `PreparedEngine` (the production owner) only ever runs a turn's settled
    // scaffold (`run_settled`); it exposes no general-purpose entry call, so
    // it cannot force an arbitrary CAF the way these tests need to flip a
    // binding from unevaluated to evaluated. What is under test here is
    // `link_program`'s contract check itself (stale generation, missing
    // import, `required_evaluated` against live handle state) plus
    // `PreparedMachine::install_program`'s import verification --
    // exactly the mechanism `PreparedEngine::install` wraps thinly one layer
    // up (and which `tidepool-runtime/tests/prepared_turn.rs`'s
    // `notebook_turns` exercises end to end on the happy path through the
    // real session). Driving `PreparedMachine` directly here, instead of
    // through either session wrapper, tests that mechanism without
    // reintroducing a session-shaped duplicate of it.
    //
    // Session-side bookkeeping the deleted `PreparedRuntime` also asserted on
    // top of this contract -- `BindingTable` lease counts, `BindingLeased`,
    // realm-scoped lease release -- is not ported: `PreparedEngine::install`
    // (`tidepool-runtime/src/session/prepared.rs`, this file) never calls
    // `BindingTable::acquire_leases`/`release_leases`, so on the production
    // prepared route no import is ever leased in the first place (leasing
    // for the prepared route's `BindingTable` entries is dead code deleted
    // alongside `PreparedRuntime`; Core-route continuation captures are the
    // only live caller of `acquire_leases`/`release_leases`, see
    // `tidepool-runtime/src/session/resident.rs`).

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

    /// Bootstrap a machine from `producer_program()`, returning it alongside
    /// the producer's own program id and its one top's [`ValueId`] -- the
    /// same shape [`PreparedEngine::bootstrap`] returns, minus the
    /// [`ProgramFacts`] bookkeeping this test module drives by hand.
    fn producer_machine() -> (PreparedMachine<'static>, ProgramId, ValueId) {
        let prepared = producer_program();
        let top = prepared.entry();
        let linked = link_program(prepared, &MachineImports::default()).expect("producer links");
        let compiled = CompiledProgram::compile(&linked).expect("producer compiles");
        let (machine, program) = PreparedMachine::new(
            compiled,
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("producer installs");
        (machine, program, top)
    }

    /// Link and install `prepared` against exactly the imports named,
    /// exactly [`PreparedEngine::install`]'s own link-then-compile-then-
    /// install sequence, minus the identity-resolution and [`ProgramFacts`]
    /// bookkeeping this test module either drives by hand or does not need.
    fn install_importing(
        machine: &mut PreparedMachine<'static>,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, PreparedHandle, ImportedValue)],
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let mut values = MachineImports::default();
        let mut bindings = ImportBindings::new();
        for (identity, handle, imported) in imports {
            values.values.insert(identity.clone(), imported.clone());
            bindings.insert(identity.clone(), *handle);
        }
        let linked = link_program(prepared, &values)?;
        let compiled = machine
            .compile_for_install(&linked)
            .map_err(PreparedRuntimeError::Compile)?;
        machine
            .install_program(compiled, bindings)
            .map_err(PreparedRuntimeError::Run)
    }

    /// The [`ImportedValue`] `link_program` checks a declared import
    /// against, read from `handle`'s live state under `generation` -- the
    /// same facts [`PreparedEngine::install`] assembles per import before
    /// linking.
    fn imported_value_of(
        machine: &PreparedMachine<'static>,
        identity: &SymbolIdentity,
        handle: PreparedHandle,
        generation: u64,
    ) -> ImportedValue {
        ImportedValue {
            identity: identity.clone(),
            rep: handle.rep(),
            entry_signature: Some(Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            }),
            evaluated: machine.handle_is_evaluated(handle).expect("handle is live"),
            generation,
        }
    }

    #[test]
    fn bind_install_run_reads_the_bound_top_by_generation() {
        let (mut machine, first, top) = producer_machine();
        // Force the CAF once so it is an evaluated (updated) constructor.
        machine
            .run_entry(
                first,
                top,
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("producer entry runs");
        let handle = machine.retain_top(first, top).expect("producer top binds");
        let identity = producer_identity();
        let imported = imported_value_of(&machine, &identity, handle, 1);
        let consumer = install_importing(
            &mut machine,
            consumer_program(true, Some(1)),
            &[(identity, handle, imported)],
        )
        .expect("consumer links against generation 1 and installs");
        assert_ne!(consumer, first);

        let read = machine
            .run_entry_retained(
                consumer,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("consumer reads its import through the slot");
        let mut values = read.values;
        let PreparedResult::Managed(value) = values.remove(0) else {
            panic!("consumer must return the imported managed value");
        };
        let CodegenPreparedOuter::Constructor { identity, fields } = machine
            .inspect_outer(value, RealmId::ROOT)
            .expect("imported value inspects through the shared machine");
        assert_eq!(identity, tidepool_repr::DataConId(980));
        assert!(matches!(fields.as_slice(), [PreparedResult::Scalar(99)]));
        assert!(machine.release(value));
    }

    #[test]
    fn stale_generation_is_refused_by_link_before_any_install_side_effect() {
        let (mut machine, first, top) = producer_machine();
        let handle = machine
            .retain_top(first, top)
            .expect("producer top binds unevaluated");
        let identity = producer_identity();
        let imported = imported_value_of(&machine, &identity, handle, 1);
        let handles_before = machine.handle_count();
        let error = install_importing(
            &mut machine,
            consumer_program(false, Some(7)),
            &[(identity.clone(), handle, imported.clone())],
        )
        .expect_err("a consumer linked against generation 7 must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        assert_eq!(error.kind(), PreparedFailureKind::Rejected);
        assert_eq!(machine.handle_count(), handles_before);
        // The same producer binding still installs a correctly-linked
        // consumer -- the refused link left the machine installable.
        let mut imported_at_1 = imported;
        imported_at_1.generation = 1;
        install_importing(
            &mut machine,
            consumer_program(false, Some(1)),
            &[(identity, handle, imported_at_1)],
        )
        .expect("the refused link left the machine installable");
    }

    #[test]
    fn missing_import_is_refused_by_link() {
        let (mut machine, _first, _top) = producer_machine();
        let error = install_importing(&mut machine, consumer_program(false, None), &[])
            .expect_err("a declared global with no binding must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::MissingImport(_))),
            "expected MissingImport, got {error:?}"
        );
    }

    #[test]
    fn required_evaluated_is_checked_against_the_live_value() {
        let (mut machine, first, top) = producer_machine();
        let handle = machine
            .retain_top(first, top)
            .expect("the unforced CAF binds");
        let identity = producer_identity();
        let unforced = imported_value_of(&machine, &identity, handle, 1);
        assert!(!unforced.evaluated, "the CAF has not been forced yet");
        let error = install_importing(
            &mut machine,
            consumer_program(true, Some(1)),
            &[(identity.clone(), handle, unforced)],
        )
        .expect_err("an unforced thunk does not satisfy required_evaluated");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        machine
            .run_entry(
                first,
                top,
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("forcing the CAF updates the bound top in place");
        let forced = imported_value_of(&machine, &identity, handle, 1);
        assert!(forced.evaluated, "the CAF is now forced");
        install_importing(
            &mut machine,
            consumer_program(true, Some(1)),
            &[(identity, handle, forced)],
        )
        .expect("the same binding now satisfies required_evaluated");
    }

    /// A program that constructs nothing but declares an effect request
    /// constructor (bridge id 77) answered at synthetic site `site` whose
    /// reply evidence is `reply`.
    fn verb_program(site: u64, reply: TypeNode) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.constructors = vec![ConstructorDecl {
            identity: testing::identity("Fixture.Effects", "Print"),
            family: testing::identity("Fixture.Effects", "Console"),
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
            tag: 1,
            family_size: 1,
            host_id: DataConId(77),
        }];
        wire.types = vec![reply];
        wire.sites = vec![SiteRow {
            site,
            origin: "Fixture.Effects.Print".into(),
            ordinal: 0,
            delivery: SiteDelivery::HostAnswer,
            wire: TypeNodeId(0),
            inputs: vec![],
        }];
        wire.verb_sites = vec![(ConstructorId(0), site)];
        testing::prepare(wire).expect("verb fixture validates")
    }

    #[test]
    fn programs_declaring_the_same_effect_constructor_share_one_verb_witness() {
        use tidepool_repr::execution_schema::SYNTHETIC_SITE_BIT;
        let site = SYNTHETIC_SITE_BIT | 5;
        let (mut engine, first) =
            PreparedEngine::bootstrap(verb_program(site, TypeNode::Text)).expect("bootstrap");
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let second = engine
            .install(verb_program(site, TypeNode::Text), &bindings, &index)
            .expect("an equivalent duplicate is not a SiteConflict");
        assert_ne!(first, second);
        let witness = engine.verb_sites[&DataConId(77)];
        assert_eq!(witness.owner, first, "the existing owner stays canonical");
        assert_eq!(engine.sites[&site].owner, first);

        // The same constructor answered by different evidence under another
        // synthetic id is refused by the verb index, and installs nothing.
        let error = engine
            .install(
                verb_program(SYNTHETIC_SITE_BIT | 6, TypeNode::Integer),
                &bindings,
                &index,
            )
            .expect_err("a conflicting verb reply refuses the install");
        assert!(
            matches!(error, PreparedRuntimeError::SiteConflict { owner, .. } if owner == first),
            "expected SiteConflict, got {error:?}"
        );
        assert_eq!(engine.programs.len(), 2);
        assert!(!engine.sites.contains_key(&(SYNTHETIC_SITE_BIT | 6)));
    }
}
