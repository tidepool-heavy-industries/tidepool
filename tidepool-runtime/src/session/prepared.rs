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
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    AnswerPlan, CompileError, CompiledProgram, ExecutionError, ImportBindings, PreparedCallOptions,
    PreparedFrameEvidence, PreparedHandle, PreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as CodegenPreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
    RunOptions, MAX_ANSWER_DEPTH,
};
// Re-exported: callers of this module's realm-scoped cancellation API
// (`open_realm`/`cancel_handle`/`close_realm`) need both types without a
// separate `tidepool_codegen` dependency of their own.
pub use tidepool_codegen::jit_machine::CancelHandle;
pub use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::suspension::ContinuationId;
pub use tidepool_codegen::suspension::RealmId;
use tidepool_repr::execution_schema::{
    link_program, Group, HeapRhs, ImportedValue, LinkError, MachineImports, ParseError,
    PreparedProgram, RuntimeRep, Signature, SiteDelivery, SiteRow, SymbolIdentity, TypeNode,
    TypeNodeId, ValueId,
};
use tidepool_repr::{DataConId, DataConTable, Literal, PrincipalId, SessionVarId};

use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

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
    /// A turn shape the prepared route does not carry yet (the cutover lands
    /// them in order: effect suspension with the resume contract, pattern
    /// binds with the multi-binder slice). The turn fails; Core is never
    /// consulted.
    #[error("the prepared route does not yet support {0}")]
    NotYetSupported(&'static str),
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
            | Self::UnsettledEntry { .. }
            | Self::WrongEngine
            | Self::MissingProgram
            | Self::NotYetSupported(_)
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
    /// `constructors`, indexed by qualified identity `(module, occurrence)`
    /// and built once in [`Self::of`], so a leaf lookup
    /// ([`Self::constructor_named`]) is one map lookup rather than a full
    /// scan repeated per leaf of an answer.
    by_identity: BTreeMap<(String, String), DataConId>,
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
        let resume = tops
            .get(&entry)
            .map(|(identity, _)| identity.module.clone())
            .and_then(|module| {
                tops.iter().find_map(|(id, (identity, _))| {
                    (identity.module == module && identity.occurrence == PREPARED_RESUME_TARGET)
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
        Self {
            entry,
            tops,
            settled: SettledIds::of(&by_identity),
            resume,
            sites: prepared.sites().to_vec(),
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
}

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
        self.machine.release(payload);
        let request = match observed {
            Ok(request) => request,
            Err(error) => {
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::Run(error));
            }
        };
        let site = match typed_site_of(&request, table) {
            Some(site) => site,
            None => {
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::UntypedRequest);
            }
        };
        let witness = match self.sites.get(&site).copied() {
            Some(witness) => witness,
            None => {
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::UnknownSite { site });
            }
        };
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
                park.principal,
                park.effect_policy,
                park.live_payload,
                evidence,
            )
            .map_err(PreparedRuntimeError::Run)?;
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
}
