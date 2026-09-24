//! Runtime ownership for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, compiled-owner construction, execution, cancellation,
//! disposition, and retained-program reuse cross this boundary in that order.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tidepool_bridge::{BridgeError, HaskellValue, HaskellVisitor};
use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};

use super::binding_table::BindingIndex;
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    CompileError, CompiledProgram, ExecutionError, ImageRegistry, ImportBindings, ManagedBuilder,
    ManagedField, ManagedNode, ParkRequest, PreparedCallOptions, PreparedFrameEvidence,
    PreparedHandle, PreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as CodegenPreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
    RunOptions, MAX_ANSWER_DEPTH,
};
// Re-exported: callers of this module's resource-scope cancellation API
// (`open_realm`/`cancel_handle`/`close_realm`) need both types without a
// separate `tidepool_codegen` dependency of their own.
pub use tidepool_codegen::machine::CancelHandle;
pub use tidepool_codegen::machine::MachineDisposition;
use tidepool_codegen::suspension::ContinuationId;
pub use tidepool_codegen::suspension::{RealmId, ValueHandle};
use tidepool_repr::execution_schema::{
    link_program, CtorRow, Group, HeapRhs, ImportedValue, JsonLayout, LinkError, MachineImports,
    ParseError, PreparedProgram, RuntimeRep, Signature, SiteDelivery, SiteRow, SymbolIdentity,
    TypeNode, TypeNodeId, ValueId,
};
use tidepool_repr::{DataConId, DataConTable, Literal, PrincipalId, SessionVarId};

use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use super::turn::{
    PREPARED_APPLY_ENTRY_TARGET, PREPARED_APPLY_VALUE_TARGET, PREPARED_RESUME_TARGET,
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
    /// An operation requires the prepared machine before its first program
    /// has installed it.
    #[error("the prepared machine is not installed")]
    MachineNotInstalled,
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
    /// Structural conversion failed at the dispatch/resume boundary. The
    /// frame stays parked and no answer root is published.
    #[error("typed site {site} rejects its structural answer: {source}")]
    AnswerRejected {
        site: u64,
        #[source]
        source: tidepool_bridge::BridgeError,
    },
    /// A resumed handle (bare or framed) is not live in this engine's
    /// ledger: unknown, released, or minted under a different engine. The
    /// frame stays parked.
    #[error("resume delivered a handle that is not live in this engine's ledger")]
    UnknownHandle,
    /// A host value could not be streamed into the authenticated managed
    /// builder. No binding root is published on this path.
    #[error("host value mount rejected: {detail}")]
    HostMount { detail: String },
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
            | Self::MachineNotInstalled
            | Self::DuplicateSite { .. }
            | Self::ProjectionShape { .. }
            | Self::SiteConflict { .. }
            | Self::UnknownSite { .. }
            | Self::UntypedRequest { .. }
            | Self::UnsitedAnswer
            | Self::UnhandledRequest
            | Self::NoResumeEntry { .. }
            | Self::AnswerDelivery { .. }
            | Self::AnswerConstructor { .. }
            | Self::AnswerShape { .. }
            | Self::AnswerUnconstructible { .. }
            | Self::AnswerRejected { .. }
            | Self::UnknownHandle
            | Self::HostMount { .. }
            | Self::NoHostingProgram
            | Self::NoApplyEntryEntry { .. }
            | Self::NoApplyValueEntry { .. }
            | Self::CrossRealmArgument { .. } => PreparedFailureKind::Rejected,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Compile(_) => PreparedFailureKind::Rejected,
            // A handler fault is this turn's own failure, so the machine stays reusable.
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
/// machine has taken its code: the declared entry and, per top-level binding,
/// the identity and entry signature an importer links against.
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
    /// The turn's admitted generic apply entries (`__applyEntry f n = settle
    /// (f (I# n))`, `__applyValue f x = settle (f x)`, beside the entry in
    /// its module), when the artifact retained them. Looked up by
    /// [`PreparedEngine::run_rooted_entry`]/
    /// [`PreparedEngine::run_rooted_application`] to apply a rooted closure
    /// without compiling a fresh fragment for it.
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
    /// rather than local index, and a bridge `HaskellValue`'s constructor resolves
    /// to the row that admits it.
    constructors: Vec<(SymbolIdentity, DataConId)>,
    /// Compiler-authenticated runtime IDs for the JSON constructors. This is
    /// the only JSON role inventory consumed by answer validation.
    json_layout: Option<JsonLayout<DataConId>>,
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

/// Everything [`PreparedEngine::snapshot_install`] produces under a machine
/// checkout for an off-checkout compile: owned data with no reference to
/// the machine or its checkout. `values`/`imports` are kept alongside
/// `linked` (which consumes the resolved imports into linkage order) so
/// [`PreparedEngine::revalidate_and_install`] can compare a fresh resolve
/// against exactly what this snapshot resolved, by value.
pub(crate) struct InstallSnapshot {
    linked: tidepool_repr::execution_schema::LinkedProgram,
    values: MachineImports,
    imports: ImportBindings,
    facts: ProgramFacts,
    plan: EvidencePlan,
    exports: Vec<(SymbolIdentity, ValueId, Option<Signature>)>,
    compile: tidepool_codegen::prepared_program::PreparedCompileSnapshot,
    /// This engine's registry, carried into the off-checkout step so a miss
    /// there can still be inserted for every other machine sharing it.
    /// `None` when the engine has none (today's behavior: always compile).
    registry: Option<Arc<ImageRegistry>>,
    /// Set when [`PreparedEngine::snapshot_install`] already found this
    /// content in the registry: [`PreparedEngine::compile_off_checkout`]
    /// then compiles nothing and just returns this `Arc`.
    precompiled: Option<Arc<CompiledProgram>>,
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
        let json_layout = prepared
            .json_layout()
            .map(|layout| (*layout).map(|constructor| constructors[constructor.0 as usize].1));
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
            apply_entry,
            apply_value,
            sites,
            verb_sites,
            types: prepared.types().to_vec(),
            constructors,
            by_identity,
            json_layout,
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

    fn json_layout(&self) -> Option<JsonLayout<DataConId>> {
        self.json_layout
    }

    fn is_json_value_node(&self, node: TypeNodeId) -> bool {
        let Some(layout) = self.json_layout() else {
            return false;
        };
        let Some(TypeNode::Data { rows, .. }) = self.type_node(node) else {
            return false;
        };
        rows.len() == 6
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.object))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.array))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.string))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.number))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.bool_))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.null))
    }

    fn constructor_host_id(
        &self,
        id: tidepool_repr::execution_schema::ConstructorId,
    ) -> Option<DataConId> {
        self.constructors
            .get(id.0 as usize)
            .map(|(_, host_id)| *host_id)
    }

    fn is_json_list_constructor(&self, host_id: DataConId) -> bool {
        self.json_layout()
            .is_some_and(|layout| host_id == layout.cons || host_id == layout.nil)
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
}

const TEXT_MODULE: &str = "Data.Text.Internal";
const INTEGER_MODULE: &str = "GHC.Num.Integer";
const NATURAL_MODULE: &str = "GHC.Num.Natural";

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

#[derive(Clone, Copy)]
enum StructuralExpected {
    Node(TypeNodeId),
    Bytes,
    Scalar(RuntimeRep),
    JsonValue,
    JsonMap,
    JsonList,
    JsonText,
    JsonScientific,
    JsonInteger,
    JsonBool,
    JsonBoxedInt,
}

struct StructuralFrame {
    host_id: DataConId,
    expected: Vec<StructuralExpected>,
    fields: Vec<ManagedField>,
    counts_depth: bool,
}

struct StructuralAnswerVisitor<'facts, 'builder, 'machine, 'code> {
    site: u64,
    root: TypeNodeId,
    facts: &'facts ProgramFacts,
    builder: &'builder mut ManagedBuilder<'machine, 'code>,
    frames: Vec<StructuralFrame>,
    result: Option<ManagedNode>,
    failure: Option<PreparedRuntimeError>,
    depth: usize,
}

impl StructuralAnswerVisitor<'_, '_, '_, '_> {
    fn json_value_shape(
        &mut self,
        host_id: DataConId,
    ) -> Result<Vec<StructuralExpected>, BridgeError> {
        let Some(layout) = self.facts.json_layout() else {
            return Err(self.shape("the program has no authenticated JSON layout"));
        };
        if host_id == layout.object {
            Ok(vec![StructuralExpected::JsonMap])
        } else if host_id == layout.array {
            Ok(vec![StructuralExpected::JsonList])
        } else if host_id == layout.string {
            Ok(vec![StructuralExpected::JsonText])
        } else if host_id == layout.number {
            Ok(vec![StructuralExpected::JsonScientific])
        } else if host_id == layout.bool_ {
            Ok(vec![StructuralExpected::JsonBool])
        } else if host_id == layout.null {
            Ok(Vec::new())
        } else {
            Err(self
                .shape("the JSON value constructor is absent from authenticated layout evidence"))
        }
    }

    fn bridge_abort(&mut self, error: PreparedRuntimeError) -> BridgeError {
        self.failure = Some(error);
        BridgeError::TypeMismatch {
            expected: "the parked site's answer type".into(),
            got: "structural response mismatch".into(),
        }
    }

    fn shape(&mut self, detail: &'static str) -> BridgeError {
        self.bridge_abort(PreparedRuntimeError::AnswerShape {
            site: self.site,
            detail,
        })
    }

    fn expected(&mut self) -> Result<StructuralExpected, BridgeError> {
        if let Some(frame) = self.frames.last() {
            frame
                .expected
                .get(frame.fields.len())
                .copied()
                .ok_or_else(|| self.shape("the response emits too many constructor fields"))
        } else if self.result.is_none() {
            Ok(StructuralExpected::Node(self.root))
        } else {
            Err(self.shape("the response emits more than one root"))
        }
    }

    fn attach(&mut self, field: ManagedField) -> Result<(), BridgeError> {
        if let Some(frame) = self.frames.last_mut() {
            frame.fields.push(field);
            Ok(())
        } else if let ManagedField::Node(root) = field {
            self.result = Some(root);
            Ok(())
        } else {
            Err(self.shape("a host answer root must be a constructor"))
        }
    }

    fn constructor_shape(
        &mut self,
        expected: StructuralExpected,
        host_id: DataConId,
    ) -> Result<Vec<StructuralExpected>, BridgeError> {
        let StructuralExpected::Node(node) = expected else {
            return match expected {
                StructuralExpected::JsonMap => {
                    let Some(layout) = self.facts.json_layout() else {
                        return Err(self.shape("the program has no authenticated JSON layout"));
                    };
                    if host_id == layout.map_tip {
                        Ok(Vec::new())
                    } else if host_id == layout.map_bin {
                        let representation = self
                            .builder
                            .constructor_field_rep(host_id, 0)
                            .map_err(|error| self.bridge_abort(PreparedRuntimeError::Run(error)))?;
                        let size = match representation {
                            Some(RuntimeRep::Int(64)) => {
                                StructuralExpected::Scalar(RuntimeRep::Int(64))
                            }
                            Some(RuntimeRep::LiftedRef) => StructuralExpected::JsonBoxedInt,
                            _ => {
                                return Err(
                                    self.shape("a JSON map size has an invalid representation")
                                )
                            }
                        };
                        Ok(vec![
                            size,
                            StructuralExpected::JsonText,
                            StructuralExpected::JsonValue,
                            StructuralExpected::JsonMap,
                            StructuralExpected::JsonMap,
                        ])
                    } else {
                        Err(self.shape("a JSON object requires Data.Map Bin or Tip"))
                    }
                }
                StructuralExpected::JsonList => {
                    let Some(layout) = self.facts.json_layout() else {
                        return Err(self.shape("the program has no authenticated JSON layout"));
                    };
                    if host_id == layout.nil {
                        Ok(Vec::new())
                    } else if host_id == layout.cons {
                        Ok(vec![
                            StructuralExpected::JsonValue,
                            StructuralExpected::JsonList,
                        ])
                    } else {
                        Err(self.shape("a JSON array requires list constructors"))
                    }
                }
                StructuralExpected::JsonText => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.text)
                    {
                        Ok(vec![
                            StructuralExpected::Bytes,
                            StructuralExpected::Scalar(RuntimeRep::Int(64)),
                            StructuralExpected::Scalar(RuntimeRep::Int(64)),
                        ])
                    } else {
                        Err(self.shape("a JSON string requires the Text constructor"))
                    }
                }
                StructuralExpected::JsonScientific => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.scientific)
                    {
                        Ok(vec![
                            StructuralExpected::JsonInteger,
                            StructuralExpected::Scalar(RuntimeRep::Int(64)),
                        ])
                    } else {
                        Err(self.shape("a JSON number requires the Scientific constructor"))
                    }
                }
                StructuralExpected::JsonInteger => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.integer_small)
                    {
                        Ok(vec![StructuralExpected::Scalar(RuntimeRep::Int(64))])
                    } else if self.facts.json_layout().is_some_and(|layout| {
                        host_id == layout.integer_positive || host_id == layout.integer_negative
                    }) {
                        Ok(vec![StructuralExpected::Bytes])
                    } else {
                        Err(self.shape("a JSON coefficient requires IS, IP or IN"))
                    }
                }
                StructuralExpected::JsonBool => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.true_ || host_id == layout.false_)
                    {
                        Ok(Vec::new())
                    } else {
                        Err(self.shape("a JSON boolean requires True or False"))
                    }
                }
                StructuralExpected::JsonBoxedInt => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.int)
                    {
                        Ok(vec![StructuralExpected::Scalar(RuntimeRep::Int(64))])
                    } else {
                        Err(self.shape("a JSON map size requires I#"))
                    }
                }
                StructuralExpected::JsonValue => self.json_value_shape(host_id),
                StructuralExpected::Bytes | StructuralExpected::Scalar(_) => {
                    Err(self.shape("a constructor was emitted for a scalar or byte field"))
                }
                StructuralExpected::Node(_) => unreachable!(),
            };
        };
        let structural_node = node;
        let Some(node) = self.facts.type_node(structural_node) else {
            return Err(self.shape("the site's type evidence names an undeclared node"));
        };
        match node {
            TypeNode::Data { rows, .. } => {
                if self.facts.is_json_value_node(structural_node) {
                    let admitted = rows.iter().any(|row| {
                        self.facts
                            .constructors
                            .get(row.constructor.0 as usize)
                            .is_some_and(|(_, declared)| *declared == host_id)
                    });
                    if !admitted {
                        return Err(self.bridge_abort(PreparedRuntimeError::AnswerConstructor {
                            site: self.site,
                            host_id,
                        }));
                    }
                    return self.json_value_shape(host_id);
                }
                let row = rows
                    .iter()
                    .find(|row| {
                        self.facts
                            .constructors
                            .get(row.constructor.0 as usize)
                            .is_some_and(|(_, declared)| *declared == host_id)
                    })
                    .ok_or_else(|| {
                        self.bridge_abort(PreparedRuntimeError::AnswerConstructor {
                            site: self.site,
                            host_id,
                        })
                    })?;
                Ok(row
                    .fields
                    .iter()
                    .copied()
                    .map(StructuralExpected::Node)
                    .collect())
            }
            TypeNode::Text => {
                let text = self.facts.constructor_named(TEXT_MODULE, "Text");
                if text != Some(host_id) {
                    return Err(self.shape("a Text answer requires the Text constructor"));
                }
                Ok(vec![
                    StructuralExpected::Bytes,
                    StructuralExpected::Scalar(RuntimeRep::Int(64)),
                    StructuralExpected::Scalar(RuntimeRep::Int(64)),
                ])
            }
            TypeNode::Integer => {
                let is = self.facts.constructor_named(INTEGER_MODULE, "IS");
                let ip = self.facts.constructor_named(INTEGER_MODULE, "IP");
                let in_ = self.facts.constructor_named(INTEGER_MODULE, "IN");
                if is == Some(host_id) {
                    Ok(vec![StructuralExpected::Scalar(RuntimeRep::Int(64))])
                } else if ip == Some(host_id) || in_ == Some(host_id) {
                    Ok(vec![StructuralExpected::Bytes])
                } else {
                    Err(self.shape("an Integer answer requires IS, IP or IN"))
                }
            }
            TypeNode::Natural => {
                let ns = self.facts.constructor_named(NATURAL_MODULE, "NS");
                let nb = self.facts.constructor_named(NATURAL_MODULE, "NB");
                if ns == Some(host_id) {
                    Ok(vec![StructuralExpected::Scalar(RuntimeRep::Word(64))])
                } else if nb == Some(host_id) {
                    Ok(vec![StructuralExpected::Bytes])
                } else {
                    Err(self.shape("a Natural answer requires NS or NB"))
                }
            }
            TypeNode::Scalar(_) => Err(self.shape("a scalar field requires a literal")),
            TypeNode::Unconstructible { reason, .. } => Err(self.bridge_abort(
                PreparedRuntimeError::AnswerUnconstructible {
                    site: self.site,
                    reason: reason.clone(),
                },
            )),
        }
    }
}

impl HaskellVisitor for StructuralAnswerVisitor<'_, '_, '_, '_> {
    fn expected_field_rep(&self) -> Option<RuntimeRep> {
        let frame = self.frames.last()?;
        self.builder
            .constructor_field_rep(frame.host_id, frame.fields.len())
            .ok()
            .flatten()
    }

    fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError> {
        let expected = self.expected()?;
        // Representation spines do not add semantic nesting: a flat 100k
        // element list or a large Map may have that many cons/tree nodes.
        // Their elements and JSON Value wrappers still pass through this
        // bound, as do ordinary nested constructors.
        let counts_depth = !matches!(
            expected,
            StructuralExpected::JsonList
                | StructuralExpected::JsonMap
                | StructuralExpected::JsonText
                | StructuralExpected::JsonScientific
                | StructuralExpected::JsonInteger
                | StructuralExpected::JsonBool
                | StructuralExpected::JsonBoxedInt
        ) && !self.facts.is_json_list_constructor(id);
        if counts_depth && self.depth >= MAX_ANSWER_DEPTH {
            return Err(self.shape("the response exceeds the maximum constructor nesting depth"));
        }
        let shape = self.constructor_shape(expected, id)?;
        if shape.len() != fields {
            return Err(self.shape("the constructor's field count does not match its declaration"));
        }
        self.frames.push(StructuralFrame {
            host_id: id,
            expected: shape,
            fields: Vec::with_capacity(fields),
            counts_depth,
        });
        self.depth += usize::from(counts_depth);
        Ok(())
    }

    fn end_constructor(&mut self) -> Result<(), BridgeError> {
        let frame = self
            .frames
            .pop()
            .ok_or_else(|| self.shape("a constructor ended without a matching begin"))?;
        self.depth -= usize::from(frame.counts_depth);
        if frame.fields.len() != frame.expected.len() {
            return Err(self.shape("a constructor ended before all fields were emitted"));
        }
        let mut fields = frame.fields;
        for field in &mut fields {
            *field = match *field {
                ManagedField::Node(node) => ManagedField::Consume(node),
                field => field,
            };
        }
        let node = self
            .builder
            .constructor(frame.host_id, &fields)
            .map_err(|error| self.bridge_abort(PreparedRuntimeError::Run(error)))?;
        self.attach(ManagedField::Node(node))
    }

    fn literal(&mut self, literal: Literal) -> Result<(), BridgeError> {
        let rep = match self.expected()? {
            StructuralExpected::Node(node) => match self.facts.type_node(node) {
                Some(TypeNode::Scalar(rep)) => *rep,
                _ => return Err(self.shape("a literal was emitted for a constructor field")),
            },
            StructuralExpected::Scalar(rep) => rep,
            StructuralExpected::Bytes => {
                return Err(self.shape("a literal was emitted for a byte-array field"))
            }
            StructuralExpected::JsonValue
            | StructuralExpected::JsonMap
            | StructuralExpected::JsonList
            | StructuralExpected::JsonText
            | StructuralExpected::JsonScientific
            | StructuralExpected::JsonInteger
            | StructuralExpected::JsonBool
            | StructuralExpected::JsonBoxedInt => {
                return Err(self.shape("a literal was emitted for a JSON constructor field"))
            }
        };
        let bits = scalar_bits(rep, &literal).ok_or_else(|| {
            self.shape("the literal does not fit the field's scalar representation")
        })?;
        self.attach(ManagedField::Scalar(bits))
    }

    fn byte_array(&mut self, bytes: Vec<u8>) -> Result<(), BridgeError> {
        if !matches!(self.expected()?, StructuralExpected::Bytes) {
            return Err(self.shape("a byte array was emitted for a non-byte field"));
        }
        let node = self
            .builder
            .bytes(&bytes)
            .map_err(|error| self.bridge_abort(PreparedRuntimeError::Run(error)))?;
        self.attach(ManagedField::Node(node))
    }
}

/// The constructor `response` visits as exactly one field-less constructor,
/// or `None` for any other shape (fields, literals, byte arrays, nesting).
fn nullary_constructor_of(
    response: &dyn tidepool_bridge::ToHaskell,
    table: &DataConTable,
) -> Option<DataConId> {
    #[derive(Default)]
    struct Nullary {
        id: Option<DataConId>,
        open: bool,
    }
    impl Nullary {
        fn reject(got: &str) -> BridgeError {
            BridgeError::TypeMismatch {
                expected: "one field-less constructor".into(),
                got: got.into(),
            }
        }
    }
    impl HaskellVisitor for Nullary {
        fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError> {
            if fields != 0 || self.id.is_some() {
                return Err(Self::reject("a constructor with fields"));
            }
            self.id = Some(id);
            self.open = true;
            Ok(())
        }
        fn end_constructor(&mut self) -> Result<(), BridgeError> {
            if !self.open {
                return Err(Self::reject("an unmatched constructor end"));
            }
            self.open = false;
            Ok(())
        }
        fn literal(&mut self, _: Literal) -> Result<(), BridgeError> {
            Err(Self::reject("a literal"))
        }
        fn byte_array(&mut self, _: Vec<u8>) -> Result<(), BridgeError> {
            Err(Self::reject("a byte array"))
        }
    }
    let mut visitor = Nullary::default();
    response.visit(table, &mut visitor).ok()?;
    (!visitor.open).then_some(visitor.id).flatten()
}

fn build_structural_node(
    response: &dyn tidepool_bridge::ToHaskell,
    table: &DataConTable,
    site: u64,
    root: TypeNodeId,
    facts: &ProgramFacts,
    builder: &mut ManagedBuilder<'_, '_>,
) -> Result<ManagedNode, PreparedRuntimeError> {
    // A structural reply belongs to the parked site's owner, not to the
    // accumulated session table. Attach that owner's admitted JSON roles for
    // this visit only: `serde_json::Value` then cannot borrow another
    // program's layout, and ordinary JSON effect replies work even when the
    // session table was assembled before this program installed.
    let response_table = table.with_json_layout(facts.json_layout());
    let mut visitor = StructuralAnswerVisitor {
        site,
        root,
        facts,
        builder,
        frames: Vec::new(),
        result: None,
        failure: None,
        depth: 0,
    };
    let visited = response.visit(&response_table, &mut visitor);
    if let Some(error) = visitor.failure.take() {
        return Err(error);
    }
    visited.map_err(|source| PreparedRuntimeError::AnswerRejected { site, source })?;
    if !visitor.frames.is_empty() {
        return Err(PreparedRuntimeError::AnswerShape {
            site,
            detail: "the response left a constructor unfinished",
        });
    }
    visitor.result.ok_or(PreparedRuntimeError::AnswerShape {
        site,
        detail: "the response emitted no managed answer root",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal helper with one call site; the parameters are the disjoint framed-answer \
              build context (payload, target constructor, and lookup tables), not a natural struct"
)]
fn build_framed_structural_node(
    prefix: &[Box<dyn tidepool_bridge::ToHaskell + Send>],
    handle: PreparedHandle,
    constructor: DataConId,
    field_nodes: &[TypeNodeId],
    table: &DataConTable,
    site: u64,
    root: TypeNodeId,
    facts: &ProgramFacts,
    builder: &mut ManagedBuilder<'_, '_>,
) -> Result<ManagedNode, PreparedRuntimeError> {
    let response_table = table.with_json_layout(facts.json_layout());
    let mut visitor = StructuralAnswerVisitor {
        site,
        root,
        facts,
        builder,
        frames: vec![StructuralFrame {
            host_id: constructor,
            expected: field_nodes
                .iter()
                .copied()
                .map(StructuralExpected::Node)
                .collect(),
            fields: Vec::with_capacity(field_nodes.len()),
            counts_depth: true,
        }],
        result: None,
        failure: None,
        depth: 1,
    };
    for field in prefix {
        let visited = field.visit(&response_table, &mut visitor);
        if let Some(error) = visitor.failure.take() {
            return Err(error);
        }
        visited.map_err(|source| PreparedRuntimeError::AnswerRejected { site, source })?;
    }
    if visitor.frames.len() != 1 {
        return Err(PreparedRuntimeError::AnswerShape {
            site,
            detail: "a framed prefix left a constructor unfinished",
        });
    }
    #[allow(clippy::expect_used, reason = "checked above: frames.len() == 1")]
    visitor
        .frames
        .last_mut()
        .expect("the outer framed constructor remains open")
        .fields
        .push(ManagedField::Handle(handle));
    let ended = visitor.end_constructor();
    if let Some(error) = visitor.failure.take() {
        return Err(error);
    }
    ended.map_err(|source| PreparedRuntimeError::AnswerRejected { site, source })?;
    visitor.result.ok_or(PreparedRuntimeError::AnswerShape {
        site,
        detail: "the framed response emitted no managed answer root",
    })
}

/// A deliberately small structural sink for values whose Haskell type is
/// fixed by a compiler-produced binding interface. It does not make a
/// `HaskellValue` tree: every completed child is immediately handed to the
/// machine's one managed builder. The builder authenticates constructor
/// descriptors and field representations before it allocates.
struct ManagedMountVisitor<'builder, 'machine, 'code> {
    builder: &'builder mut ManagedBuilder<'machine, 'code>,
    frames: Vec<MountFrame>,
    root: Option<ManagedNode>,
}

struct MountFrame {
    host_id: DataConId,
    expected: usize,
    fields: Vec<ManagedField>,
}

impl ManagedMountVisitor<'_, '_, '_> {
    fn rejected(expected: impl Into<String>, got: impl Into<String>) -> BridgeError {
        BridgeError::TypeMismatch {
            expected: expected.into(),
            got: got.into(),
        }
    }

    fn attach(&mut self, field: ManagedField) -> Result<(), BridgeError> {
        if let Some(frame) = self.frames.last_mut() {
            if frame.fields.len() == frame.expected {
                return Err(Self::rejected(
                    format!("constructor with {} fields", frame.expected),
                    "too many visitor fields",
                ));
            }
            frame.fields.push(field);
            return Ok(());
        }
        let ManagedField::Node(node) = field else {
            return Err(Self::rejected("one managed value root", "a scalar field"));
        };
        if self.root.replace(node).is_some() {
            return Err(Self::rejected("one visitor root", "multiple visitor roots"));
        }
        Ok(())
    }

    fn scalar(literal: Literal) -> Result<[u8; 16], BridgeError> {
        let word = match literal {
            Literal::LitInt(value) => value as u128,
            Literal::LitWord(value) => u128::from(value),
            Literal::LitChar(value) => u128::from(value as u32),
            Literal::LitFloat(bits) | Literal::LitDouble(bits) => u128::from(bits),
            literal => {
                return Err(Self::rejected(
                    "a scalar literal supported by managed construction",
                    format!("{literal:?}"),
                ))
            }
        };
        Ok(word.to_ne_bytes())
    }

    fn finish(self) -> Result<ManagedNode, BridgeError> {
        if !self.frames.is_empty() {
            return Err(Self::rejected(
                "closed visitor constructors",
                "unfinished constructor",
            ));
        }
        self.root
            .ok_or_else(|| Self::rejected("one visitor root", "no visitor root"))
    }
}

impl HaskellVisitor for ManagedMountVisitor<'_, '_, '_> {
    fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError> {
        self.frames.push(MountFrame {
            host_id: id,
            expected: fields,
            fields: Vec::with_capacity(fields),
        });
        Ok(())
    }

    fn end_constructor(&mut self) -> Result<(), BridgeError> {
        let frame = self.frames.pop().ok_or_else(|| {
            Self::rejected(
                "an open visitor constructor",
                "constructor end without begin",
            )
        })?;
        if frame.fields.len() != frame.expected {
            return Err(BridgeError::ArityMismatch {
                con: frame.host_id,
                expected: frame.expected,
                got: frame.fields.len(),
            });
        }
        let fields = frame
            .fields
            .into_iter()
            .map(|field| match field {
                ManagedField::Node(node) => ManagedField::Consume(node),
                field => field,
            })
            .collect::<Vec<_>>();
        let node = self
            .builder
            .constructor(frame.host_id, &fields)
            .map_err(|error| BridgeError::InternalError(error.to_string()))?;
        self.attach(ManagedField::Node(node))
    }

    fn literal(&mut self, literal: Literal) -> Result<(), BridgeError> {
        if let Literal::LitByteArray(bytes) = literal {
            let node = self
                .builder
                .bytes(&bytes)
                .map_err(|error| BridgeError::InternalError(error.to_string()))?;
            return self.attach(ManagedField::Node(node));
        }
        self.attach(ManagedField::Scalar(Self::scalar(literal)?))
    }

    fn byte_array(&mut self, bytes: Vec<u8>) -> Result<(), BridgeError> {
        let node = self
            .builder
            .bytes(&bytes)
            .map_err(|error| BridgeError::InternalError(error.to_string()))?;
        self.attach(ManagedField::Node(node))
    }

    fn expected_field_rep(&self) -> Option<RuntimeRep> {
        let frame = self.frames.last()?;
        self.builder
            .constructor_field_rep(frame.host_id, frame.fields.len())
            .ok()
            .flatten()
    }
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
fn site_field(field: &HaskellValue, table: &DataConTable) -> Option<u64> {
    match field {
        HaskellValue::Lit(Literal::LitInt(n)) => u64::try_from(*n).ok(),
        HaskellValue::Con(id, inner) if table.name_of(*id) == Some("I#") => {
            match inner.as_slice() {
                [HaskellValue::Lit(Literal::LitInt(n))] => u64::try_from(*n).ok(),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The typed site a suspended request names, read from the request's
/// rendered payload: the protocol's sited helpers place the site id under the
/// `typedSite` key of the request's JSON payload object
/// (`tidepool-protocol`'s `ObjectValue::Site`), the same field the harness
/// uses to classify a suspension. `None` for a request that carries no such
/// field: an ordinary handled effect.
fn typed_site_of(request: &HaskellValue, table: &DataConTable) -> Option<u64> {
    /// The `Text`/string-literal content of an aeson `Key`/`HaskellValue` leaf.
    fn value_text(value: &HaskellValue, table: &DataConTable) -> Option<String> {
        match value {
            HaskellValue::Lit(Literal::LitString(bytes)) => {
                std::str::from_utf8(bytes).ok().map(str::to_owned)
            }
            HaskellValue::Con(id, fields) if table.name_of(*id) == Some("Text") => {
                tidepool_bridge::shapes::text_bytes_clamped(fields, table)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            }
            _ => None,
        }
    }
    /// The numeric content of a `typedSite` leaf: an unboxed or boxed
    /// integral literal, or an aeson `Number` wrapping one. This is the one
    /// leaf this walk ever renders through the snapshot JSON renderer — never
    /// the whole request.
    fn value_u64(value: &HaskellValue, table: &DataConTable) -> Option<u64> {
        match value {
            HaskellValue::Lit(Literal::LitInt(n)) => u64::try_from(*n).ok(),
            HaskellValue::Lit(Literal::LitWord(n)) => Some(*n),
            HaskellValue::Con(_, _) => match crate::render::value_to_json(value, table, 0) {
                serde_json::Value::Number(n) => n.as_u64(),
                _ => None,
            },
            _ => None,
        }
    }
    /// Walk the request's `HaskellValue` tree directly (never its JSON rendering)
    /// looking for an aeson `Object` layer with a `typedSite` entry, the same
    /// depth bound (4) the JSON walk used.
    fn search(value: &HaskellValue, table: &DataConTable, depth: usize) -> Option<u64> {
        if depth > 4 {
            return None;
        }
        let HaskellValue::Con(id, fields) = value else {
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
/// scopes and leases stay in `PersistentSession`; this owns only code and heap.
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
    /// `PreparedEngine::heap_stats`'s `gc_count` -- see [`Self::heap_stats`].
    major_collections: u64,
    /// Package-defined tops already installed on this machine, offered to
    /// every later turn as executable imports (see [`CodeExport`] and
    /// [`exportable_code_tops`]). Grows once, on the turns that first reach
    /// a package definition, and is never invalidated: a package's code
    /// cannot change under a live session.
    code_exports: BTreeMap<SymbolIdentity, CodeExport>,
    /// Shared with every other machine of this run when the composition
    /// root wires one up ([`Self::set_image_registry`]); `None` keeps
    /// today's behavior (every install compiles its own image). A registry
    /// hit installs the already-compiled `Arc<CompiledProgram>` through
    /// [`PreparedMachine::install_shared`] instead of compiling again.
    registry: Option<Arc<ImageRegistry>>,
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
/// turn target, the session declaration environment and binding store, the workspace source layer,
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
/// plan: what a suspension is parked with. `pub` alongside
/// [`PreparedEngine::park_suspension`], which takes it.
#[derive(Clone, Copy)]
pub struct ParkPolicy {
    pub principal: PrincipalId,
    pub effect_policy: EffectRunPolicy,
    pub live_payload: LivePayloadPolicy,
}

/// How a settled entry (the scaffold or a resume) is called: nothing is
/// observed by the call itself, and no collection is forced before the
/// layer is read.
const SETTLE_CALL: PreparedCallOptions = PreparedCallOptions {
    observation_budget: 0,
    collect_before_observation: false,
};

/// One suspension parked by [`PreparedEngine::park_suspension`]: the frame's
/// id and the observed request.
pub struct PreparedParked {
    pub id: ContinuationId,
    pub request: HaskellValue,
}

/// A parked frame re-entered by [`PreparedEngine::resume_parked`]: the
/// settled layer the resume produced, under the frame's resource scope, by the runner
/// program whose entry re-entered it (the program a further suspension is
/// parked against).
pub struct PreparedResumed {
    pub settlement: PreparedSettlement,
    pub realm: RealmId,
    pub runner: ProgramId,
}

// SAFETY: the machine is the only non-auto-`Send` field, and `PersistentSession`
// moves the engine
// between exactly one owning thread at a time (stowed XOR running).
unsafe impl Send for PreparedEngine {}

static_assertions::assert_impl_all!(PreparedEngine: Send);

/// One turn's settled computation, read from the `Settled` layer its
/// `__prepared` scaffold produced. Every handle is retained under the run's
/// resource scope until the caller adopts or releases it.
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
    /// Stream a structurally encoded host value into the resident heap. The
    /// caller supplies the compiler-authenticated constructor table for the
    /// typed binding it will mount; no source-text representation is involved.
    pub fn build_host_value(
        &mut self,
        realm: RealmId,
        value: &dyn tidepool_bridge::ToHaskell,
        table: &DataConTable,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = {
            let mut visitor = ManagedMountVisitor {
                builder: &mut builder,
                frames: Vec::new(),
                root: None,
            };
            value
                .visit(table, &mut visitor)
                .map_err(|error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                })?;
            visitor
                .finish()
                .map_err(|error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                })?
        };
        builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Stream a host JSON document into the resident heap. The caller supplies
    /// the compiler-authenticated table for the binding it will mount; this
    /// method deliberately has no source-text representation or parser path.
    pub fn build_host_json(
        &mut self,
        realm: RealmId,
        value: &serde_json::Value,
        layout: &JsonLayout<DataConId>,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = {
            let mut visitor = ManagedMountVisitor {
                builder: &mut builder,
                frames: Vec::new(),
                root: None,
            };
            tidepool_bridge::json_builder::visit_json(value, layout, &mut visitor).map_err(
                |error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                },
            )?;
            visitor
                .finish()
                .map_err(|error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                })?
        };
        builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Stream host UTF-8 text into the worker `Text` representation. A
    /// qualified constructor identity is required when it is present, so a
    /// same-named user constructor cannot silently become host text.
    pub fn build_host_text(
        &mut self,
        realm: RealmId,
        text: &str,
        table: &DataConTable,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let text_id = table
            .get_by_qualified_name("Data.Text.Internal.Text")
            .or_else(|| table.get_by_qualified_name("Data.Text.Text"))
            .filter(|id| table.get(*id).is_some_and(|con| con.rep_arity == 3))
            .ok_or_else(|| PreparedRuntimeError::HostMount {
                detail: "the compiler table does not declare Data.Text.Internal.Text".into(),
            })?;
        let length = i64::try_from(text.len()).map_err(|_| PreparedRuntimeError::HostMount {
            detail: "host Text exceeds the worker Int length range".into(),
        })?;
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let bytes = builder
            .bytes(text.as_bytes())
            .map_err(PreparedRuntimeError::Run)?;
        let mut zero = [0_u8; 16];
        zero[..8].copy_from_slice(&0_i64.to_ne_bytes());
        let mut len = [0_u8; 16];
        len[..8].copy_from_slice(&length.to_ne_bytes());
        let root = builder
            .constructor(
                text_id,
                &[
                    ManagedField::Consume(bytes),
                    ManagedField::Scalar(zero),
                    ManagedField::Scalar(len),
                ],
            )
            .map_err(PreparedRuntimeError::Run)?;
        builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Share `registry` with this engine's machine: every later install
    /// consults it before compiling (see [`Self::install`] and the
    /// off-checkout split), and the compiling install's image is the one
    /// every other machine holding the same `Arc<ImageRegistry>` reuses.
    /// Set once by the composition root that owns a run's machines (e.g. a
    /// `ChildSessionFactory`); `None` (the default) keeps every install
    /// compiling its own image, exactly as before this existed.
    pub fn set_image_registry(&mut self, registry: Arc<ImageRegistry>) {
        self.registry = Some(registry);
    }

    /// The registry this engine shares its installs with, if any.
    #[must_use]
    pub fn image_registry(&self) -> Option<&Arc<ImageRegistry>> {
        self.registry.as_ref()
    }

    /// Compile `linked` (or reuse an already-compiled image) and install it,
    /// consulting [`Self::registry`] when this engine has one. Shared by
    /// [`Self::install`]'s single-checkout path.
    fn compile_and_install(
        &mut self,
        linked: tidepool_repr::execution_schema::LinkedProgram,
        imports: ImportBindings,
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let Some(registry) = self.registry.clone() else {
            let compiled = self
                .machine
                .compile_for_install(&linked)
                .map_err(PreparedRuntimeError::Compile)?;
            return self
                .machine
                .install_program(compiled, imports)
                .map_err(PreparedRuntimeError::Run);
        };
        let image = match registry.lookup(&linked) {
            Some(image) => image,
            None => {
                let compiled = self
                    .machine
                    .compile_for_install(&linked)
                    .map_err(PreparedRuntimeError::Compile)?;
                registry.insert(linked, Arc::new(compiled))
            }
        };
        self.machine
            .install_shared(image, imports)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Create the session's machine from its first turn's program and
    /// install that program. The first turn can import nothing: no prepared
    /// binding exists before the machine does. Uses the default nursery
    /// size; see [`Self::bootstrap_with_nursery_bytes`] for a caller-chosen
    /// size (e.g. a session's configured `nursery_size`, or a one-shot
    /// caller that wants a small nursery to force collections).
    pub fn bootstrap(prepared: PreparedProgram) -> Result<(Self, ProgramId), PreparedRuntimeError> {
        Self::bootstrap_with_nursery_bytes(prepared, RunOptions::default().nursery_bytes)
    }

    /// As [`Self::bootstrap`], with an explicit nursery size instead of the
    /// default.
    pub fn bootstrap_with_nursery_bytes(
        prepared: PreparedProgram,
        nursery_bytes: usize,
    ) -> Result<(Self, ProgramId), PreparedRuntimeError> {
        let facts = ProgramFacts::of(&prepared);
        let exports = exportable_code_tops(&prepared);
        let linked = link_program(prepared, &MachineImports::default())?;
        let compiled = CompiledProgram::compile(&linked).map_err(PreparedRuntimeError::Compile)?;
        let (machine, program) =
            PreparedMachine::new(compiled, PreparedMachineOptions { nursery_bytes })
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
            registry: None,
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
        let (values, imports) = self.resolve_imports(&prepared, bindings, index)?;
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
        // `compile_and_install` consults `self.registry` (when this engine
        // has one) before compiling: a hit installs the already-compiled
        // image through `install_shared` and compiles nothing, so
        // `compile_ms` below also covers a registry lookup on the hit path.
        let program = self.compile_and_install(linked, imports)?;
        let compile_install_ms = lap();
        tracing::info!(
            target: "tidepool_runtime::prepared_install",
            resolve_imports_ms,
            evidence_ms,
            link_ms,
            compile_install_ms,
            imports = import_count,
            compiled_off_checkout = false,
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

    /// Resolve `prepared`'s declared globals against `bindings`/`index` (and
    /// this machine's package-top exports), one live-handle fact snapshot
    /// per import: `MachineImports` for `link_program`, `ImportBindings` for
    /// `install_program`. Shared by the single-checkout [`Self::install`]
    /// and both ends of the off-checkout split
    /// ([`Self::snapshot_install`]/[`Self::revalidate_and_install`]) so a
    /// snapshot and its revalidation resolve imports exactly the same way.
    fn resolve_imports(
        &self,
        prepared: &PreparedProgram,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<(MachineImports, ImportBindings), PreparedRuntimeError> {
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
            let BoundValue { handle, .. } = &entry.value;
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
                    entry_signature: None,
                    evaluated,
                    generation: entry.module.gen().0,
                },
            );
            imports.insert(identity.clone(), handle);
        }
        Ok((values, imports))
    }

    /// Step (a) of the off-checkout split install: resolve `prepared`'s
    /// imports, plan its evidence and take a compile snapshot -- everything
    /// [`Self::compile_off_checkout`] needs -- without compiling. Run this
    /// under the machine checkout; the returned [`InstallSnapshot`] carries
    /// no machine reference and may be compiled after the checkout is
    /// released.
    pub(crate) fn snapshot_install(
        &mut self,
        prepared: PreparedProgram,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<InstallSnapshot, PreparedRuntimeError> {
        let (values, imports) = self.resolve_imports(&prepared, bindings, index)?;
        let exports = exportable_code_tops(&prepared);
        let facts = ProgramFacts::of(&prepared);
        let plan = self.plan_evidence(&facts)?;
        let linked = link_program(prepared, &values)?;
        let precompiled = self
            .registry
            .as_ref()
            .and_then(|registry| registry.lookup(&linked));
        let compile = self.machine.compile_snapshot();
        Ok(InstallSnapshot {
            linked,
            values,
            imports,
            facts,
            plan,
            exports,
            compile,
            registry: self.registry.clone(),
            precompiled,
        })
    }

    /// Step (b): compile `snapshot`'s linked program off any checkout, or
    /// reuse an image the registry already had at snapshot time -- either
    /// way returning an `Arc` so [`Self::revalidate_and_install`] can
    /// install it with [`PreparedMachine::install_shared`]. A fresh compile
    /// with a registry attached is inserted here, off-checkout, so every
    /// other machine sharing that registry sees it as soon as this step
    /// finishes rather than waiting for `revalidate_and_install`. Pure with
    /// respect to the machine; safe to run on a blocking thread while other
    /// turns hold the checkout.
    pub(crate) fn compile_off_checkout(
        snapshot: &mut InstallSnapshot,
    ) -> Result<Arc<CompiledProgram>, CompileError> {
        if let Some(image) = &snapshot.precompiled {
            return Ok(Arc::clone(image));
        }
        let compiled = snapshot.compile.compile(&snapshot.linked)?;
        let image = match &snapshot.registry {
            Some(registry) => registry.insert(snapshot.linked.clone(), Arc::new(compiled)),
            None => Arc::new(compiled),
        };
        Ok(image)
    }

    /// Step (c): under the machine checkout again, re-resolve `snapshot`'s
    /// imports and compare them against the facts the off-checkout compile
    /// ran against. An import that became evaluated, changed representation
    /// or generation, or was retired invalidates the compile: this returns
    /// `Ok(None)` and the caller must recompile (or fall back to the
    /// single-checkout [`Self::install`]) rather than install a program
    /// linked against stale import facts. Otherwise installs, publishes and
    /// pins exactly as [`Self::install`] does.
    pub(crate) fn revalidate_and_install(
        &mut self,
        snapshot: InstallSnapshot,
        compiled: Arc<CompiledProgram>,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<Option<ProgramId>, PreparedRuntimeError> {
        let (fresh_values, fresh_imports) =
            self.resolve_imports(snapshot.linked.prepared(), bindings, index)?;
        if fresh_values != snapshot.values || fresh_imports != snapshot.imports {
            return Ok(None);
        }
        let import_count = snapshot.imports.len();
        let program = self
            .machine
            .install_shared(compiled, snapshot.imports)
            .map_err(PreparedRuntimeError::Run)?;
        tracing::info!(
            target: "tidepool_runtime::prepared_install",
            imports = import_count,
            compiled_off_checkout = true,
            "prepared install"
        );
        self.programs.insert(program, snapshot.facts);
        self.publish_evidence(program, snapshot.plan);
        self.publish_code_exports(program, snapshot.exports);
        // Held live across the install-to-first-run gap; the turn's
        // bind/complete path (`resident.rs`) unpins it once the run's
        // outcome is bound, released or parked.
        self.machine
            .pin(program)
            .map_err(PreparedRuntimeError::Run)?;
        self.installs_since_major += 1;
        Ok(Some(program))
    }

    /// Run `program`'s settled scaffold under `realm` and read its one
    /// constructor layer. The scaffold value itself is released here; the
    /// layer's fields come back as retained handles.
    pub fn run_settled(
        &mut self,
        program: ProgramId,
        realm: RealmId,
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        self.run_settled_with_inputs(program, realm, &[])
    }

    /// Invoke a compiled inspection entry against an already mounted value.
    pub(crate) fn run_settled_with_inputs(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        inputs: &[PreparedInput],
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
            .run_entry_retained(program, entry, inputs, SETTLE_CALL, realm)
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
    /// and retaining its fields under the given resource scope. Initial runs
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
    /// machine observe path (the request the host reports), read
    /// the typed site it names, resolve that site through the machine-owned
    /// index to its evidence owner, and park `continuation` with that
    /// evidence and `program`'s admitted resume entry. Every refusal releases
    /// both handles and parks nothing: a runner without a resume entry, a
    /// suspension under `HandleOrError` (nothing is handled on this route
    /// yet), a request without a typed site (an ordinary handled effect, not
    /// yet answered on this route), or a site no installed program declares.
    ///
    /// `pub`, not `pub(crate)`: this method is self-contained on `Self` —
    /// every input it reads (`self.sites`/`self.verb_sites`/`self.programs`,
    /// all populated by [`Self::bootstrap`]/[`Self::install`]) and mutates
    /// (the machine's own park registry) belongs to the engine itself, with
    /// no session, actor, or lexical-scope bookkeeping folded in. The rest of
    /// the parked-continuation cycle this feeds — [`Self::resume_with_structural_answer`],
    /// [`Self::parked_count`], [`Self::stowed_roots_count`],
    /// [`Self::parked_ids`], [`Self::parked_realm`], [`Self::close_realm`],
    /// [`Self::abort_parked`] — was already `pub`; this was the one private
    /// step in an otherwise-public cycle.
    ///
    /// Per `docs/continuation-parking-contract.md`'s "Consumer obligations":
    /// the returned [`PreparedParked::id`] is the caller's to retain — this
    /// engine enforces no capacity limit (e.g. "one outstanding turn") and no
    /// actor-local grant or principal check; those remain the caller's, same
    /// as on `PreparedEngine`. A parked frame is a registered GC root until
    /// resumed or its resource scope closes; an unresumed park that the caller drops
    /// on the floor leaks a root until [`Self::close_realm`].
    pub fn park_suspension(
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
        // path, producing the value reported for a suspension. This runs
        // AFTER any prior effect in the same turn already committed (the
        // continuation being parked here is its own committed answer's
        // continuation), so an exhausted budget here must not fail the turn
        // outright the way it would for a fresh, uncommitted observation --
        // that would report a hard failure for effects that already ran.
        //
        // Unlike model-facing display, this observation feeds a TYPED
        // decode: `handlers.dispatch` below reconstructs a concrete Rust
        // request type (e.g. `JevAskWith`) from the value, field by field.
        // `BudgetPolicy::Bounded` is the wrong tool here -- a cut can land on
        // ANY field, swapping in `OVERSIZE_SENTINEL` in place of whatever
        // that field's own type was (an `Int`, a `Map` entry, not only a
        // `Text`), which decode then rejects with a confusing type mismatch
        // instead of a clean, budget-attributed failure. `100_000` is a
        // DISPLAY-sized ceiling; the request already exists in full on the
        // heap, so the real constraint on rematerializing it here is memory,
        // not that ceiling. Retry under `Complete` with no such ceiling
        // instead of degrading the shape.
        let observed = match self.machine.observe_handle(
            program,
            payload,
            RunOptions::default().observation_budget,
        ) {
            Ok(request) => Ok(request),
            Err(error) if error.is_observation_budget_exhausted() => {
                self.machine.observe_handle(program, payload, usize::MAX)
            }
            Err(error) => Err(error),
        };
        let request = match observed {
            Ok(request) => request,
            Err(error) => {
                self.machine.release(payload);
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::Run(error));
            }
        };
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
                HaskellValue::Con(host_id, fields) => Ok(self
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
                    // parks unsited and re-enters only by handle.
                    .map_or((UNSITED, program), |(site, witness)| (site, witness.owner))),
                _ => Err(PreparedRuntimeError::UntypedRequest {
                    constructor: "a non-constructor value".to_owned(),
                }),
            },
        };
        let (site, owner) = match classified {
            Ok(classified) => classified,
            Err(error) => {
                self.machine.release(payload);
                self.machine.release(continuation);
                return Err(error);
            }
        };
        // A live-payload policy names one field of THIS request Con (the
        // convention's field 1) as the value crossing the runtime boundary
        // by reference. Classify first so a rejected request cannot strand a
        // newly tenured root without a frame to own it; then mirror the field
        // before releasing `payload`. `PreparedMachine::park` consumes the
        // root on both success and refusal.
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
    /// analogue of `PreparedEngine`'s `tenure_live_payload`, built from the
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
    /// `PreparedEngine`'s own `request_has_field`/
    /// `request_field_carries_closure_sentinel` do. `None` when the
    /// policy names no field, the constructor doesn't have it, or (rare: a
    /// nullary/scalar-only request) the field is not itself managed.
    fn tenure_live_payload(
        &mut self,
        payload: PreparedHandle,
        realm: RealmId,
        policy: LivePayloadPolicy,
        request: &HaskellValue,
    ) -> Result<Option<tidepool_codegen::old_space::RootSlot>, ExecutionError> {
        let field = match policy {
            LivePayloadPolicy::None => None,
            LivePayloadPolicy::ClosureField(field) => {
                matches!(request, HaskellValue::Con(_, fields)
                    if fields.get(field).is_some_and(tidepool_codegen::observation::contains_closure_sentinel))
                .then_some(field)
            }
            LivePayloadPolicy::ValueField(field) => {
                matches!(request, HaskellValue::Con(_, fields) if field < fields.len()).then_some(field)
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

    /// Peek at a parked frame's evidence and resource-scope id without consuming it.
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
    /// parked under `id` (`PreparedMachine::take_live_payload_handle`). The
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

    /// [`Self::live_payload_handle`] with the handle owned by the given resource scope rather
    /// than the frame's own resource scope (see `PreparedMachine::take_live_payload_handle_owned_by`).
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
    /// retained under the frame's resource scope: take the frame, enter the runner's
    /// resume entry with the continuation and the answer, and read the
    /// settled layer through the shared settlement reader. `answer` is consumed on
    /// every path. Every failure before the take (unknown id, an answer from
    /// another resource scope, cancellation) leaves the frame parked and rooted; a
    /// failure after the take is a run failure.
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
    /// live afterward, ownership unchanged — borrowed-handle delivery
    /// (`docs/continuation-parking-contract.md`), which reads a handle's
    /// current heap pointer without releasing its root. No resource-scope check: a
    /// handle is meant to move between parked continuations across resource
    /// scopes (see `ValueHandle`'s own doc), unlike a freshly built answer,
    /// which is always scoped to the frame's resource scope.
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
        // `answer` stays live: the caller owns it before and after.
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
    /// included, the same shape borrowed-handle delivery accepts. `raw`
    /// must be live in this engine's ledger; the only check possible on this
    /// route is its `RuntimeRep` (every handle this engine mints is
    /// `LiftedRef`), so no deeper type check is available on this path. The
    /// handle's root is a BORROW: this call does not release it.
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
    /// field borrows `raw` verbatim. Prefix fields stream through the same
    /// typed structural visitor as ordinary host answers; the borrowed field
    /// is spliced in unvalidated beyond its `RuntimeRep`, under the framed-delivery contract
    /// (`docs/continuation-parking-contract.md`). The built constructor is
    /// released as usual once the resume entry has read it; `raw`'s root is
    /// untouched throughout.
    pub fn resume_with_framed_handle(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
        constructor: DataConId,
        prefix: Vec<HaskellValue>,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let prefix = prefix
            .into_iter()
            .map(|field| Box::new(field) as Box<dyn tidepool_bridge::ToHaskell + Send>)
            .collect::<Vec<_>>();
        self.resume_with_framed_handle_sources(id, raw, constructor, &prefix, table)
    }

    /// [`Self::resume_with_framed_handle`] with each prefix field supplied as
    /// an owned structural source. The borrowed final field remains outside
    /// normal structural construction, under the framed-delivery contract.
    pub fn resume_with_framed_handle_sources(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
        constructor: DataConId,
        prefix: &[Box<dyn tidepool_bridge::ToHaskell + Send>],
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let handle = self
            .machine
            .prepared_handle_of(raw)
            .ok_or(PreparedRuntimeError::UnknownHandle)?;
        let (realm, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let evidence = *evidence;
        let site = evidence.site;
        if site == UNSITED {
            return Err(PreparedRuntimeError::UnsitedAnswer);
        }
        let (programs, machine) = (&self.programs, &mut self.machine);
        let owner = programs
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
                detail: "the framed constructor's declared field count does not match the supplied prefix plus the borrowed handle field",
            });
        }
        if machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut builder = machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = build_framed_structural_node(
            prefix,
            handle,
            constructor,
            &ctor_row.fields,
            table,
            site,
            row.wire,
            owner,
            &mut builder,
        )?;
        let built = builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, built)
    }

    /// The pre-take checks of a resume, then the take: the frame exists,
    /// `answer` is live under its resource scope, the scope is not cancelled.
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

    /// Validate and construct an owned response directly from structural
    /// visitor events. Completed children enter the shared incremental
    /// builder immediately, so no intermediate `HaskellValue` tree exists.
    pub fn resume_with_structural_answer(
        &mut self,
        id: ContinuationId,
        response: &dyn tidepool_bridge::ToHaskell,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let (realm, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let evidence = *evidence;
        if evidence.site == UNSITED {
            return self.resume_unsited_with_nullary(id, realm, response, table);
        }
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let site = evidence.site;
        let (programs, machine) = (&self.programs, &mut self.machine);
        let owner = programs
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
        let mut builder = machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = build_structural_node(response, table, site, row.wire, owner, &mut builder)?;
        let answer = builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, answer)
    }

    /// The one host-built answer an open-reply frame ([`UNSITED`]) accepts:
    /// a field-less constructor. The frame carries no wire evidence, so the
    /// constructor is built from the machine's authenticated descriptors
    /// alone; anything with a field is [`PreparedRuntimeError::UnsitedAnswer`]
    /// and the frame stays parked. `Tidepool.Actor.statefulLoop` parks its
    /// receive this way on purpose (its reply type `Maybe state` is open
    /// until the handler runs) and a drain resumes it with `Nothing`.
    fn resume_unsited_with_nullary(
        &mut self,
        id: ContinuationId,
        realm: RealmId,
        response: &dyn tidepool_bridge::ToHaskell,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let host_id =
            nullary_constructor_of(response, table).ok_or(PreparedRuntimeError::UnsitedAnswer)?;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = builder
            .constructor(host_id, &[])
            .map_err(PreparedRuntimeError::Run)?;
        let answer = builder
            .finish(realm, root)
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

    /// Materialize a retained value as a bridge `HaskellValue`, forcing its lazy
    /// fields through `program`'s force adapter. The handle stays retained.
    pub fn observe(
        &mut self,
        program: ProgramId,
        handle: PreparedHandle,
    ) -> Result<HaskellValue, PreparedRuntimeError> {
        self.machine
            .observe_handle(program, handle, RunOptions::default().observation_budget)
            .map_err(PreparedRuntimeError::Run)
    }

    /// [`Self::observe`] under the same budget, but a budget that runs out
    /// CUTS the walk instead of failing it: the result is a bounded SELECTION
    /// carrying `tidepool_codegen::observation::OVERSIZE_SENTINEL` wherever a
    /// subtree was left unread. Use it where a display-sized limit must not
    /// discard work that already ran; the handle stays retained, so the part
    /// the cut omitted is still reachable through the binding.
    pub fn observe_bounded(
        &mut self,
        program: ProgramId,
        handle: PreparedHandle,
    ) -> Result<HaskellValue, PreparedRuntimeError> {
        self.machine
            .observe_handle_bounded(program, handle, RunOptions::default().observation_budget)
            .map_err(PreparedRuntimeError::Run)
    }

    /// The managed fields of one constructor layer of a retained value,
    /// each retained as its own handle under the given resource scope, without forcing. The
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

    /// Hand a run result to the session binding store: the handle moves into
    /// the machine's ROOT scope (no resource-scope close releases it) and its
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

    /// [`tidepool_codegen::prepared_program::PreparedMachine::inspect_retained`]:
    /// a non-forcing structural read of one constructor layer of an
    /// arbitrary retained root by its bare cross-engine [`ValueHandle`] id
    /// (a `RootCustody`'s, typically) -- no Haskell compiles, no thunk
    /// forces. Managed fields come back as fresh handles owned by the
    /// handle's own resource scope; the caller releases every one it does
    /// not keep.
    pub fn inspect_retained(
        &mut self,
        handle: ValueHandle,
    ) -> Result<CodegenPreparedOuter, PreparedRuntimeError> {
        self.machine
            .inspect_retained(handle)
            .map_err(PreparedRuntimeError::Run)
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

    /// Read-only heap/GC snapshot. Field mapping onto the prepared machine's
    /// own accounting:
    ///
    /// - `fragments` ↔ installed programs ([`Self::residency`]'s
    ///   `programs`) -- bounded by [`Self::quiesce_and_collect_now`]'s
    ///   retirement, so this count can fall as programs retire;
    /// - `live_bytes` ↔ prepared old-space bytes as of the last successful
    ///   major collection ([`Self::old_bytes`]);
    /// - `gc_count` ↔ major collections actually run
    ///   ([`Self::major_collections`]) -- there is no nursery-collection
    ///   counter exposed at this boundary;
    /// - `nursery_bytes` ↔ `0`: `PreparedMachine` does not expose its
    ///   nursery capacity today, and no consumer of this snapshot reads it.
    #[must_use]
    pub fn heap_stats(&self) -> tidepool_codegen::machine::HeapStats {
        tidepool_codegen::machine::HeapStats {
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
    use tidepool_bridge::ToHaskell;
    use tidepool_codegen::host_fns::RuntimeError;
    use tidepool_codegen::machine_state::MachineFailure;
    use tidepool_repr::execution_schema::{
        testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
        GlobalDecl, GlobalId, HeapBinding, ResultContract, ScalarLiteral, Signature, SignatureId,
        StorageLayout, TopBinding, UpdatePolicy, ValueRef,
    };
    use tidepool_repr::DataCon;
    use tidepool_repr::SessionModule;

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
    // contract and `PreparedMachine::install_program` verification.
    //
    // `PreparedEngine` only ever runs a turn's settled
    // scaffold (`run_settled`); it exposes no general-purpose entry call, so
    // it cannot force an arbitrary CAF the way these tests need to flip a
    // binding from unevaluated to evaluated. What is under test here is
    // `link_program`'s contract check itself (stale generation, missing
    // import, `required_evaluated` against live handle state) plus
    // `PreparedMachine::install_program`'s import verification --
    // exactly the mechanism `PreparedEngine::install` owns one layer up (and
    // which `tidepool/runtime/tests/prepared_turn.rs`'s `notebook_turns`
    // exercises end to end on the happy path). Driving `PreparedMachine`
    // directly here isolates the contract from turn orchestration.
    //
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

    /// [`consumer_program`] declares its import with `entry_signature:
    /// Some(_)`, matching how [`install_importing`]'s hand-built
    /// [`ImportedValue`]s model a `code_exports` import. A real session
    /// value resolved through [`resolve_prepared_import`] never carries an
    /// entry signature (see [`PreparedEngine::resolve_imports`]), so a
    /// split-install test exercising that real path needs its own
    /// plain-import consumer, otherwise every install (single-checkout
    /// included) is refused by `link_program`'s `wrong_entry` check.
    fn plain_import_consumer_program(
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
            entry_signature: None,
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
        testing::prepare(wire).expect("plain-import consumer fixture")
    }

    /// A bootstrapped [`PreparedEngine`] whose producer top is bound into a
    /// real `BindingTable`/`BindingIndex` pair under `producer_identity()`
    /// at generation 1 -- the same shape [`PreparedEngine::install`]'s
    /// `resolve_imports` reads through [`resolve_prepared_import`], built
    /// the way `ResidentSession::bind_prepared` builds one (`adopt` then a
    /// `BindingEntry`), so a split install against it exercises the same
    /// import-resolution path a live turn does, not the hand-assembled
    /// `MachineImports` `install_importing` uses.
    fn engine_with_bound_producer(
        force: bool,
    ) -> (
        PreparedEngine,
        ProgramId,
        ValueId,
        BindingTable,
        BindingIndex,
    ) {
        let prepared = producer_program();
        let top = prepared.entry();
        let (mut engine, first) = PreparedEngine::bootstrap(prepared).expect("producer bootstraps");
        if force {
            engine
                .machine
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
        }
        let handle = engine
            .machine
            .retain_top(first, top)
            .expect("producer top binds");
        let root = engine.adopt(handle).expect("retained handle adopts a root");
        let entry = BindingEntry {
            name: tidepool_repr::BindingName("producer".into()),
            id: SessionVarId::from_extract(1),
            module: SessionModule::val(tidepool_repr::Generation(1)),
            value: BoundValue {
                root,
                handle,
                identity: producer_identity(),
            },
            type_display: None,
            defining_expr: None,
            scope: tidepool_codegen::scope::ScopeId::ROOT,
        };
        let mut index = BindingIndex::new();
        index.on_bind(&entry);
        let mut bindings = BindingTable::new();
        bindings.bind(entry);
        (engine, first, top, bindings, index)
    }

    #[test]
    fn split_install_matches_single_checkout_install_with_imports() {
        let (mut single, _, _, bindings, index) = engine_with_bound_producer(true);
        let single_program = single
            .install(
                plain_import_consumer_program(true, Some(1)),
                &bindings,
                &index,
            )
            .expect("single-checkout install links against the bound producer");
        let single_read = read_consumer_import(&mut single, single_program);

        let (mut split, _, _, bindings, index) = engine_with_bound_producer(true);
        let snapshot = split
            .snapshot_install(
                plain_import_consumer_program(true, Some(1)),
                &bindings,
                &index,
            )
            .expect("snapshot step resolves the same imports off no checkout yet");
        let mut snapshot = snapshot;
        let compiled = PreparedEngine::compile_off_checkout(&mut snapshot)
            .expect("off-checkout compile of the linked program succeeds");
        let split_program = split
            .revalidate_and_install(snapshot, compiled, &bindings, &index)
            .expect("revalidation runs")
            .expect("nothing changed the import between snapshot and revalidation");
        let split_read = read_consumer_import(&mut split, split_program);

        // Both engines were bootstrapped and bound identically, so the split
        // install must be observably the same program as the single-checkout
        // one: same value read back through the same import.
        assert_eq!(single_read, split_read);
    }

    /// Run `program`'s one entry (a [`plain_import_consumer_program`],
    /// which just returns its imported global) and decode the constructor
    /// it reads back, releasing the observed value afterward.
    fn read_consumer_import(
        engine: &mut PreparedEngine,
        program: ProgramId,
    ) -> (tidepool_repr::DataConId, Vec<u64>) {
        let read = engine
            .machine
            .run_entry_retained(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("consumer entry runs and reads its import through the slot");
        let mut values = read.values;
        let PreparedResult::Managed(value) = values.remove(0) else {
            panic!("consumer must return the imported managed value");
        };
        let CodegenPreparedOuter::Constructor { identity, fields } = engine
            .machine
            .inspect_outer(value, RealmId::ROOT)
            .expect("imported value inspects through the shared machine");
        let scalars = fields
            .iter()
            .map(|field| match field {
                PreparedResult::Scalar(n) => *n,
                other => panic!("expected a scalar field, got {other:?}"),
            })
            .collect();
        assert!(engine.machine.release(value));
        (identity, scalars)
    }

    #[test]
    fn stale_import_between_snapshot_and_revalidation_falls_back() {
        // Snapshot against the UNFORCED producer top (required_evaluated:
        // false, so linking the snapshot itself does not reject it), then
        // force the CAF -- simulating another actor's turn mutating the
        // same shared import while this compile ran off-checkout -- before
        // revalidating.
        let (mut engine, first, top, bindings, index) = engine_with_bound_producer(false);
        let snapshot = engine
            .snapshot_install(
                plain_import_consumer_program(false, Some(1)),
                &bindings,
                &index,
            )
            .expect("snapshot resolves the still-unforced import");
        let mut snapshot = snapshot;
        let compiled = PreparedEngine::compile_off_checkout(&mut snapshot)
            .expect("off-checkout compile succeeds against the unforced snapshot");

        engine
            .machine
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
            .expect("forcing the CAF between snapshot and revalidation");

        let outcome = engine
            .revalidate_and_install(snapshot, compiled, &bindings, &index)
            .expect("revalidation itself does not error");
        assert!(
            outcome.is_none(),
            "an import that became evaluated between snapshot and revalidation \
             must invalidate the split compile rather than install it"
        );

        // The caller's documented recovery -- recompile fresh, or fall back
        // to the single-checkout path -- still succeeds against the now-
        // forced import.
        engine
            .install(
                plain_import_consumer_program(false, Some(1)),
                &bindings,
                &index,
            )
            .expect("the single-checkout fallback installs against the current import");
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

    #[allow(
        clippy::too_many_arguments,
        reason = "test fixture builder; each argument is an independent field of the constructor \
                  under test, not a natural grouping"
    )]
    fn mount_constructor(
        module: &str,
        occurrence: &str,
        family_module: &str,
        family_occurrence: &str,
        id: u64,
        tag: u32,
        family_size: u32,
        fields: Vec<RuntimeRep>,
    ) -> ConstructorDecl {
        let layout = StorageLayout::for_reps(&testing::target(), &fields)
            .expect("mount fixture field layout");
        let mut identity = testing::identity(module, occurrence);
        identity.namespace = "constructor".into();
        let mut family = testing::identity(family_module, family_occurrence);
        family.namespace = "type".into();
        ConstructorDecl {
            identity,
            family,
            host_id: DataConId(id),
            result_rep: RuntimeRep::LiftedRef,
            tag,
            family_size,
            strict_fields: vec![true; fields.len()],
            field_reps: fields,
            layout: CheckedLayout {
                fields: layout
                    .fields()
                    .iter()
                    .map(|field| FieldLayout {
                        rep: field.rep(),
                        offset: field.offset(),
                    })
                    .collect(),
                alignment: layout.alignment(),
                payload_size: layout.payload_size(),
                root_mask: layout
                    .fields()
                    .iter()
                    .map(|field| {
                        matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)
                    })
                    .collect(),
            },
        }
    }

    fn mount_table_row(id: u64, name: &str, arity: u32, qualified_name: Option<&str>) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.into(),
            tag: 1,
            rep_arity: arity,
            field_bangs: Vec::new(),
            qualified_name: qualified_name.map(str::to_owned),
            type_name: String::new(),
        }
    }

    fn canonical_mount_value(value: &HaskellValue, output: &mut String) {
        match value {
            HaskellValue::Lit(Literal::LitByteArray(bytes)) => {
                output.push_str(&format!("B{bytes:?};"));
            }
            HaskellValue::Lit(literal) => output.push_str(&format!("L{literal:?};")),
            HaskellValue::ByteArray(bytes) => {
                output.push_str(&format!("B{:?};", *bytes.lock().expect("byte array lock")));
            }
            HaskellValue::Con(id, fields) => {
                output.push_str(&format!("C{}(", id.0));
                for field in fields {
                    canonical_mount_value(field, output);
                }
                output.push_str(");");
            }
        }
    }

    /// The whole family the JSON bridge emits, plus `BadText`: it deliberately
    /// has three scalar fields so its table row passes name/arity lookup but
    /// descriptor validation rejects it after the byte array was built.
    fn json_mount_program() -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors = vec![
            mount_constructor(
                "Fixture.Mount",
                "Unit",
                "Fixture.Mount",
                "Unit",
                1,
                1,
                1,
                vec![],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Object",
                "Tidepool.Aeson.Value",
                "Value",
                100,
                1,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Array",
                "Tidepool.Aeson.Value",
                "Value",
                101,
                2,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "String",
                "Tidepool.Aeson.Value",
                "Value",
                102,
                3,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Number",
                "Tidepool.Aeson.Value",
                "Value",
                103,
                4,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Bool",
                "Tidepool.Aeson.Value",
                "Value",
                104,
                5,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Null",
                "Tidepool.Aeson.Value",
                "Value",
                105,
                6,
                6,
                vec![],
            ),
            mount_constructor(
                "Tidepool.Aeson.Scientific",
                "Scientific",
                "Tidepool.Aeson.Scientific",
                "Scientific",
                110,
                1,
                1,
                vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
            ),
            mount_constructor(
                "GHC.Num.Integer",
                "IS",
                "GHC.Num.Integer",
                "Integer",
                120,
                1,
                3,
                vec![RuntimeRep::Int(64)],
            ),
            mount_constructor(
                "GHC.Num.Integer",
                "IP",
                "GHC.Num.Integer",
                "Integer",
                121,
                2,
                3,
                vec![RuntimeRep::UnliftedRef],
            ),
            mount_constructor(
                "GHC.Num.Integer",
                "IN",
                "GHC.Num.Integer",
                "Integer",
                122,
                3,
                3,
                vec![RuntimeRep::UnliftedRef],
            ),
            mount_constructor("GHC.Types", "True", "GHC.Types", "Bool", 130, 2, 2, vec![]),
            mount_constructor("GHC.Types", "False", "GHC.Types", "Bool", 131, 1, 2, vec![]),
            mount_constructor(
                "Data.Map.Internal",
                "Bin",
                "Data.Map.Internal",
                "Map",
                140,
                1,
                2,
                vec![RuntimeRep::LiftedRef; 5],
            ),
            mount_constructor(
                "Data.Map.Internal",
                "Tip",
                "Data.Map.Internal",
                "Map",
                141,
                2,
                2,
                vec![],
            ),
            mount_constructor(
                "GHC.Types",
                "I#",
                "GHC.Types",
                "Int",
                150,
                1,
                1,
                vec![RuntimeRep::Int(64)],
            ),
            mount_constructor(
                "Data.Text.Internal",
                "Text",
                "Data.Text.Internal",
                "Text",
                160,
                1,
                1,
                vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Int(64),
                ],
            ),
            mount_constructor(
                "GHC.Types",
                ":",
                "GHC.Types",
                "List",
                170,
                2,
                2,
                vec![RuntimeRep::LiftedRef; 2],
            ),
            mount_constructor("GHC.Types", "[]", "GHC.Types", "List", 171, 1, 2, vec![]),
            mount_constructor(
                "Fixture.Mount",
                "BadText",
                "Fixture.Mount",
                "BadText",
                900,
                1,
                1,
                vec![RuntimeRep::Int(64); 3],
            ),
            mount_constructor(
                "Tidepool.Internal.Resume",
                "Done",
                "Tidepool.Internal.Resume",
                "Settled",
                901,
                1,
                2,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Internal.Resume",
                "Suspended",
                "Tidepool.Internal.Resume",
                "Settled",
                902,
                2,
                2,
                vec![RuntimeRep::LiftedRef; 2],
            ),
            mount_constructor(
                "Fixture.Mount",
                "Framed",
                "Fixture.Mount",
                "Framed",
                903,
                1,
                1,
                vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef],
            ),
        ];
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef; 2],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.expressions.nodes.push(ExprFrame::Construct {
            constructor: ConstructorId(20),
            fields: vec![Atom::Ref(ValueRef::Local(ValueId(3)))],
        });
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Fixture", PREPARED_RESUME_TARGET),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![ValueId(2), ValueId(3)],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        let mut json_value_family = testing::identity("Tidepool.Aeson.Value", "Value");
        json_value_family.namespace = "type".into();
        wire.types = vec![
            TypeNode::Data {
                family: json_value_family,
                arguments: vec![],
                rows: vec![
                    CtorRow {
                        constructor: ConstructorId(1),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(2),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(3),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(4),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(5),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(6),
                        fields: vec![],
                    },
                ],
            },
            TypeNode::Scalar(RuntimeRep::Int(64)),
            TypeNode::Data {
                family: {
                    let mut family = testing::identity("Fixture.Mount", "Framed");
                    family.namespace = "type".into();
                    family
                },
                arguments: vec![],
                rows: vec![CtorRow {
                    constructor: ConstructorId(22),
                    fields: vec![TypeNodeId(1), TypeNodeId(0)],
                }],
            },
        ];
        wire.sites = vec![
            SiteRow {
                site: 7,
                origin: "Fixture.Mount.json_reply".into(),
                ordinal: 0,
                delivery: SiteDelivery::HostAnswer,
                wire: TypeNodeId(0),
                inputs: vec![],
            },
            SiteRow {
                site: 8,
                origin: "Fixture.Mount.framed_reply".into(),
                ordinal: 1,
                delivery: SiteDelivery::HostAnswer,
                wire: TypeNodeId(2),
                inputs: vec![],
            },
        ];
        wire.json_layout = Some(JsonLayout {
            object: ConstructorId(1),
            array: ConstructorId(2),
            string: ConstructorId(3),
            number: ConstructorId(4),
            bool_: ConstructorId(5),
            null: ConstructorId(6),
            map_bin: ConstructorId(13),
            map_tip: ConstructorId(14),
            true_: ConstructorId(11),
            false_: ConstructorId(12),
            cons: ConstructorId(17),
            nil: ConstructorId(18),
            scientific: ConstructorId(7),
            integer_small: ConstructorId(8),
            integer_positive: ConstructorId(9),
            integer_negative: ConstructorId(10),
            text: ConstructorId(16),
            int: ConstructorId(15),
        });
        testing::prepare(wire).expect("JSON mount fixture")
    }

    fn json_mount_table() -> DataConTable {
        let mut table = DataConTable::new();
        for (id, name, arity, qualified_name) in [
            (100, "Object", 1, Some("Tidepool.Aeson.Value.Object")),
            (101, "Array", 1, Some("Tidepool.Aeson.Value.Array")),
            (102, "String", 1, Some("Tidepool.Aeson.Value.String")),
            (103, "Number", 1, Some("Tidepool.Aeson.Value.Number")),
            (104, "Bool", 1, Some("Tidepool.Aeson.Value.Bool")),
            (105, "Null", 0, Some("Tidepool.Aeson.Value.Null")),
            (
                110,
                "Scientific",
                2,
                Some("Tidepool.Aeson.Scientific.Scientific"),
            ),
            (120, "IS", 1, Some("GHC.Num.Integer.IS")),
            (121, "IP", 1, Some("GHC.Num.Integer.IP")),
            (122, "IN", 1, Some("GHC.Num.Integer.IN")),
            (130, "True", 0, Some("GHC.Types.True")),
            (131, "False", 0, Some("GHC.Types.False")),
            (140, "Bin", 5, Some("Data.Map.Internal.Bin")),
            (141, "Tip", 0, Some("Data.Map.Internal.Tip")),
            (150, "I#", 1, Some("GHC.Types.I#")),
            (160, "Text", 3, Some("Data.Text.Internal.Text")),
            (170, ":", 2, Some("GHC.Types.:")),
            (171, "[]", 0, Some("GHC.Types.[]")),
            (903, "Framed", 2, Some("Fixture.Mount.Framed")),
        ] {
            table
                .insert_checked(mount_table_row(id, name, arity, qualified_name))
                .unwrap();
        }
        table
    }

    #[test]
    fn host_json_and_text_mount_stream_through_tiny_nursery_and_recover_after_rejection() {
        let prepared = json_mount_program();
        let layout = prepared
            .json_layout()
            .expect("JSON mount program carries layout")
            .try_map(|constructor| {
                prepared
                    .constructors()
                    .get(constructor.0 as usize)
                    .map(|declaration| declaration.host_id)
                    .ok_or(())
            })
            .expect("JSON layout IDs are declared");
        let (mut engine, program) = PreparedEngine::bootstrap_with_nursery_bytes(prepared, 64)
            .expect("bootstrap JSON mount fixture");
        let table = json_mount_table();
        let table = table.with_json_layout(Some(layout));
        let payload = serde_json::json!({
            "nested": [[{"key": "value", "n": serde_json::Value::Number("1000000000000000000000000000001".parse().expect("large JSON number"))}], [true, null]],
            "large": (0..128).map(|index| serde_json::json!({"index": index, "text": "x".repeat(32)})).collect::<Vec<_>>(),
        });

        let initial_handles = engine.handle_count();
        let initial_roots = engine.persistent_roots_count();
        let json = engine
            .build_host_json(RealmId::ROOT, &payload, &layout)
            .expect("large nested JSON mounts under a tiny nursery");
        let observed = engine.observe(program, json).expect("observe mounted JSON");
        let expected = payload.to_value(&table).expect("reference JSON value");
        let mut observed_shape = String::new();
        canonical_mount_value(&observed, &mut observed_shape);
        let mut expected_shape = String::new();
        canonical_mount_value(&expected, &mut expected_shape);
        assert_eq!(observed_shape, expected_shape);
        assert!(engine.release(json));

        let mut wrong_text = DataConTable::new();
        wrong_text
            .insert_checked(mount_table_row(900, "Text", 3, None))
            .unwrap();
        let error = engine
            .build_host_text(RealmId::ROOT, "must not publish", &wrong_text)
            .expect_err("wrong Text descriptor is rejected after byte construction");
        assert!(matches!(error, PreparedRuntimeError::HostMount { .. }));
        assert_eq!(engine.handle_count(), initial_handles);
        assert_eq!(engine.persistent_roots_count(), initial_roots);

        let text = engine
            .build_host_text(RealmId::ROOT, "reusable after rejection", &table)
            .expect("a later Text mount succeeds");
        assert!(matches!(
            engine.observe(program, text).expect("observe mounted Text"),
            HaskellValue::Con(id, ref fields)
                if id == DataConId(160)
                    && matches!(fields.as_slice(), [HaskellValue::Lit(Literal::LitByteArray(bytes)), HaskellValue::Lit(Literal::LitInt(0)), HaskellValue::Lit(Literal::LitInt(24))] if bytes == b"reusable after rejection")
        ));
        assert!(engine.release(text));
        assert_eq!(engine.handle_count(), initial_handles);
        assert_eq!(engine.persistent_roots_count(), initial_roots);
    }

    #[test]
    fn structural_json_effect_reply_uses_parked_owner_layout() {
        let (mut engine, program) =
            PreparedEngine::bootstrap(json_mount_program()).expect("bootstrap JSON reply fixture");
        let table = json_mount_table();
        assert!(
            table.json_layout().is_none(),
            "the accumulated session table deliberately has no JSON authority"
        );

        let continuation = {
            let mut builder = engine
                .machine
                .managed_builder()
                .expect("JSON reply fixture opens a managed builder");
            let root = builder
                .constructor(DataConId(105), &[])
                .expect("fixture Null constructor is declared");
            builder
                .finish(RealmId::ROOT, root)
                .expect("fixture continuation is retained")
        };
        let id = engine
            .machine
            .park(
                continuation,
                RealmId::ROOT,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence: PreparedFrameEvidence {
                        owner: program,
                        site: 7,
                        runner: program,
                        resume_entry: ValueId(1),
                        continuation_rep: RuntimeRep::LiftedRef,
                    },
                },
            )
            .expect("fixture continuation parks");

        let response = serde_json::json!({"worked": [true, 3]});
        let resumed = engine
            .resume_with_structural_answer(id, &response, &table)
            .expect("JSON response derives its layout from the parked site owner");
        let PreparedSettlement::Done { value } = resumed.settlement else {
            panic!("resume fixture returns a settled Done value")
        };
        assert!(matches!(
            engine
                .observe(program, value)
                .expect("observe resumed JSON"),
            HaskellValue::Con(DataConId(100), _)
        ));
        assert!(engine.release(value));
        assert_eq!(engine.parked_count(), 0);
    }

    #[test]
    fn framed_structural_prefix_rejects_without_consuming_then_retries() {
        struct RawInt(i64);
        impl tidepool_bridge::sealed::ToHaskellSealed for RawInt {}
        impl ToHaskell for RawInt {
            fn visit(
                &self,
                _table: &DataConTable,
                visitor: &mut dyn tidepool_bridge::HaskellVisitor,
            ) -> Result<(), BridgeError> {
                visitor.literal(Literal::LitInt(self.0))
            }
        }

        let (mut engine, program) =
            PreparedEngine::bootstrap(json_mount_program()).expect("bootstrap framed fixture");
        let table = json_mount_table();
        let make_null = |engine: &mut PreparedEngine| {
            let mut builder = engine
                .machine
                .managed_builder()
                .expect("framed fixture opens a managed builder");
            let root = builder
                .constructor(DataConId(105), &[])
                .expect("fixture Null constructor is declared");
            builder
                .finish(RealmId::ROOT, root)
                .expect("fixture value is retained")
        };
        let continuation = make_null(&mut engine);
        let held = make_null(&mut engine);
        let id = engine
            .machine
            .park(
                continuation,
                RealmId::ROOT,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence: PreparedFrameEvidence {
                        owner: program,
                        site: 8,
                        runner: program,
                        resume_entry: ValueId(1),
                        continuation_rep: RuntimeRep::LiftedRef,
                    },
                },
            )
            .expect("fixture continuation parks");
        let roots_before = engine.persistent_roots_count();
        let handles_before = engine.handle_count();

        let error = match engine.resume_with_framed_handle_sources(
            id,
            held.raw(),
            DataConId(903),
            &[],
            &table,
        ) {
            Err(error) => error,
            Ok(_) => panic!("missing prefix must be rejected"),
        };
        assert!(matches!(error, PreparedRuntimeError::AnswerShape { .. }));
        assert_eq!(engine.parked_count(), 1);
        assert_eq!(engine.persistent_roots_count(), roots_before);
        assert_eq!(engine.handle_count(), handles_before);

        let wrong: Vec<Box<dyn ToHaskell + Send>> = vec![Box::new("not an Int".to_owned())];
        let error = match engine.resume_with_framed_handle_sources(
            id,
            held.raw(),
            DataConId(903),
            &wrong,
            &table,
        ) {
            Err(error) => error,
            Ok(_) => panic!("wrong prefix shape must be rejected"),
        };
        assert!(matches!(error, PreparedRuntimeError::AnswerRejected { .. }));
        assert_eq!(engine.parked_count(), 1);
        assert_eq!(engine.persistent_roots_count(), roots_before);
        assert_eq!(engine.handle_count(), handles_before);

        let prefix: Vec<Box<dyn ToHaskell + Send>> = vec![Box::new(RawInt(37))];
        let resumed = engine
            .resume_with_framed_handle_sources(id, held.raw(), DataConId(903), &prefix, &table)
            .expect("valid prefix retries the exact parked frame");
        let PreparedSettlement::Done { value } = resumed.settlement else {
            panic!("framed fixture returns a settled Done value")
        };
        assert!(matches!(
            engine.observe(program, value).expect("observe framed reply"),
            HaskellValue::Con(DataConId(903), ref fields)
                if matches!(fields.as_slice(), [HaskellValue::Lit(Literal::LitInt(37)), HaskellValue::Con(DataConId(105), nested)] if nested.is_empty())
        ));
        assert!(engine.release(value));
        assert!(engine.release(held));
        assert_eq!(engine.parked_count(), 0);
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

    #[test]
    fn alpha_stable_polymorphic_reply_evidence_installs_without_weakening_conflicts() {
        use tidepool_repr::execution_schema::SYNTHETIC_SITE_BIT;
        let site = SYNTHETIC_SITE_BIT | 7;
        let stable = TypeNode::Unconstructible {
            reason: "polymorphic".into(),
            rendered: "a".into(),
        };
        let (mut engine, first) =
            PreparedEngine::bootstrap(verb_program(site, stable.clone())).expect("bootstrap");
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let second = engine
            .install(verb_program(site, stable), &bindings, &index)
            .expect("alpha-stable polymorphic evidence is equivalent");
        assert_ne!(first, second);

        let different = TypeNode::Unconstructible {
            reason: "polymorphic".into(),
            rendered: "a_unique".into(),
        };
        let error = engine
            .install(verb_program(site, different), &bindings, &index)
            .expect_err("different type evidence must remain a conflict");
        assert!(
            matches!(error, PreparedRuntimeError::SiteConflict { owner, .. } if owner == first),
            "expected SiteConflict, got {error:?}"
        );
        assert_eq!(engine.programs.len(), 2);
    }

    #[test]
    fn two_engines_sharing_one_registry_the_second_install_is_a_registry_hit() {
        use tidepool_repr::execution_schema::SYNTHETIC_SITE_BIT;
        let registry = Arc::new(ImageRegistry::new());
        let bootstrap_site = SYNTHETIC_SITE_BIT | 30;
        let (mut engine_a, _) =
            PreparedEngine::bootstrap(verb_program(bootstrap_site, TypeNode::Text))
                .expect("engine a bootstraps");
        let (mut engine_b, _) =
            PreparedEngine::bootstrap(verb_program(bootstrap_site, TypeNode::Text))
                .expect("engine b bootstraps its own, independent machine");
        engine_a.set_image_registry(Arc::clone(&registry));
        engine_b.set_image_registry(Arc::clone(&registry));

        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let shared_site = SYNTHETIC_SITE_BIT | 31;

        engine_a
            .install(verb_program(shared_site, TypeNode::Text), &bindings, &index)
            .expect("engine a compiles and registers the image");
        assert_eq!(
            registry.misses(),
            1,
            "engine a's install is a registry miss"
        );
        assert_eq!(registry.hits(), 0);

        engine_b
            .install(verb_program(shared_site, TypeNode::Text), &bindings, &index)
            .expect("engine b installs the same content against its own machine");
        assert_eq!(
            registry.hits(),
            1,
            "engine b's install of the same linked program content is a registry hit"
        );
        assert_eq!(registry.misses(), 1, "no second compile happened");
    }
}
