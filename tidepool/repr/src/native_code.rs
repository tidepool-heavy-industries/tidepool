//! Immutable demanded linkage. Structural references grant no compiler, source,
//! live-value or machine authority; acquisition owners keep those permissions.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use petgraph::{algo::kosaraju_scc, graph::DiGraph};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::execution_schema::{
    encoding::hash_projected_group, CertifiedGroupCode, DefinitionsView, InventoryOperation,
    PackageGroupCode, ParseError, SymbolIdentity, ValueId,
};

/// A content observation, never permission to acquire or install code.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NativeCodeId([u8; 32]);
impl NativeCodeId {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeCodeBody {
    Home(CertifiedGroupCode),
    Package(PackageGroupCode),
}
impl NativeCodeBody {
    pub fn definitions(&self) -> DefinitionsView<'_> {
        match self {
            Self::Home(code) => code.definitions(),
            Self::Package(code) => code.definitions(),
        }
    }
    pub fn exported_values(&self) -> impl Iterator<Item = (&SymbolIdentity, ValueId)> {
        match self {
            Self::Home(code) => code.projected_group(),
            Self::Package(code) => code.projected_group(),
        }
        .exported_values()
    }
    fn key(&self, operation: &InventoryOperation) -> Result<NativeCodeBodyKey, NativeCodeError> {
        let (nominal, group) = match self {
            Self::Home(code) => {
                let owner = code.owner();
                operation.charge(owner.unit.len().saturating_add(owner.module.len()))?;
                (
                    NativeCodeNominalKey::Home {
                        owner: NativeHomeOwnerKey {
                            unit: owner.unit.clone(),
                            module: owner.module.clone(),
                            module_version: owner.module_version.0,
                            skinny_iface_sha256: owner.skinny_iface_sha256,
                            product_sha256: owner.product_sha256,
                        },
                        original_ordinal: code.original_ordinal(),
                    },
                    code.projected_group(),
                )
            }
            Self::Package(code) => {
                let owner = code.owner();
                operation.reserve::<NativeSymbolKey>(code.binders().len())?;
                operation.charge(owner.unit.len().saturating_add(owner.module.len()))?;
                let mut definitions = Vec::new();
                for symbol in code.binders() {
                    operation.charge(
                        symbol.unit.len()
                            + symbol.module.len()
                            + symbol.namespace.len()
                            + symbol.occurrence.len()
                            + symbol.record_parent.as_ref().map_or(0, String::len),
                    )?;
                    definitions.push(NativeSymbolKey {
                        unit: symbol.unit.clone(),
                        module: symbol.module.clone(),
                        namespace: symbol.namespace.clone(),
                        occurrence: symbol.occurrence.clone(),
                        record_parent: symbol.record_parent.clone(),
                    });
                }
                definitions.sort();
                (
                    NativeCodeNominalKey::Package {
                        owner: NativePackageOwnerKey {
                            unit: owner.unit.clone(),
                            module: owner.module.clone(),
                            compiler_sha256: owner.compiler_sha256,
                            profile_sha256: owner.profile_sha256,
                            interface_sha256: owner.interface_sha256,
                            defining_version_sha256: owner.defining_version_sha256,
                        },
                        definitions,
                    },
                    code.projected_group(),
                )
            }
        };
        Ok(NativeCodeBodyKey {
            nominal,
            definitions_sha256: hash_projected_group(group, operation)?,
        })
    }
}

/// A symbolic immutable leaf, not a captured runtime value. The acquired caller
/// must still validate its actual context, representation and generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRetainedRequirement {
    identity: SymbolIdentity,
    generation: u64,
    type_artifact: Option<[u8; 32]>,
}
impl NativeRetainedRequirement {
    pub fn new(identity: SymbolIdentity, generation: u64, type_artifact: Option<[u8; 32]>) -> Self {
        Self {
            identity,
            generation,
            type_artifact,
        }
    }
    pub fn identity(&self) -> &SymbolIdentity {
        &self.identity
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn type_artifact(&self) -> Option<&[u8; 32]> {
        self.type_artifact.as_ref()
    }
    fn facts(&self) -> NativeRetainedFacts {
        NativeRetainedFacts {
            symbol: (
                self.identity.unit.clone(),
                self.identity.module.clone(),
                self.identity.namespace.clone(),
                self.identity.occurrence.clone(),
                self.identity.record_parent.clone(),
            ),
            generation: self.generation,
            type_artifact: self.type_artifact,
        }
    }
}

/// Traversal identity is an exact occurrence index, never an ArtifactId/name.
#[derive(Clone, Debug)]
pub enum NativeCodePlanImport {
    Local { occurrence: usize, value: ValueId },
    External { code: NativeCodeRef, value: ValueId },
    Retained(NativeRetainedRequirement),
}
#[derive(Clone, Debug)]
pub struct NativeCodePlan {
    pub body: NativeCodeBody,
    pub imports: Vec<NativeCodePlanImport>,
}
#[derive(Clone, Debug)]
pub enum NativeCodeImport {
    Code { code: NativeCodeRef, value: ValueId },
    Retained(NativeRetainedRequirement),
}
#[derive(Clone, Debug)]
enum ComponentImport {
    Local { member: u32, value: ValueId },
    External { code: NativeCodeRef, value: ValueId },
    Retained(NativeRetainedRequirement),
}
#[derive(Debug)]
struct NativeCodeNode {
    body: NativeCodeBody,
    imports: Arc<[ComponentImport]>,
}

/// One maximal SCC. Internal edges are local indices, so ownership has no Arc
/// cycle. External edges retain exact child components.
#[derive(Debug)]
pub struct NativeCodeComponent {
    identity: [u8; 32],
    facts: Arc<NativeCodeComponentFacts>,
    record_bytes: Arc<[u8]>,
    nodes: Arc<[NativeCodeNode]>,
}
#[derive(Clone)]
pub struct NativeCodeRef {
    component: Arc<NativeCodeComponent>,
    member: u32,
    id: NativeCodeId,
}
impl std::fmt::Debug for NativeCodeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeCodeRef")
            .field("id", &self.id)
            .field("member", &self.member)
            .finish()
    }
}
impl PartialEq for NativeCodeRef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for NativeCodeRef {}
impl NativeCodeRef {
    fn new(component: Arc<NativeCodeComponent>, member: u32) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"Tidepool linked native member v1\0");
        hash.update(component.identity);
        hash.update(member.to_be_bytes());
        Self {
            component,
            member,
            id: NativeCodeId(hash.finalize().into()),
        }
    }
    pub fn id(&self) -> NativeCodeId {
        self.id
    }
    pub fn component(&self) -> &Arc<NativeCodeComponent> {
        &self.component
    }
    pub fn member(&self) -> u32 {
        self.member
    }
    pub fn location(&self) -> NativeCodeLocation {
        NativeCodeLocation {
            component: self.component.identity,
            member: self.member,
        }
    }
    pub fn body(&self) -> &NativeCodeBody {
        &self.component.nodes[self.member as usize].body
    }
    pub fn imports(&self) -> impl ExactSizeIterator<Item = NativeCodeImport> + '_ {
        self.component.nodes[self.member as usize]
            .imports
            .iter()
            .map(|edge| match edge {
                ComponentImport::Local { member, value } => NativeCodeImport::Code {
                    code: Self::new(Arc::clone(&self.component), *member),
                    value: *value,
                },
                ComponentImport::External { code, value } => NativeCodeImport::Code {
                    code: code.clone(),
                    value: *value,
                },
                ComponentImport::Retained(requirement) => {
                    NativeCodeImport::Retained(requirement.clone())
                }
            })
    }
}

/// Pure transport observations. Decoding cannot create a usable reference.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCodeBodyKey {
    pub nominal: NativeCodeNominalKey,
    pub definitions_sha256: [u8; 32],
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum NativeCodeNominalKey {
    Home {
        owner: NativeHomeOwnerKey,
        original_ordinal: u32,
    },
    Package {
        owner: NativePackageOwnerKey,
        definitions: Vec<NativeSymbolKey>,
    },
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHomeOwnerKey {
    pub unit: String,
    pub module: String,
    pub module_version: [u8; 32],
    pub skinny_iface_sha256: [u8; 32],
    pub product_sha256: [u8; 32],
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePackageOwnerKey {
    pub unit: String,
    pub module: String,
    pub compiler_sha256: [u8; 32],
    pub profile_sha256: [u8; 32],
    pub interface_sha256: [u8; 32],
    pub defining_version_sha256: [u8; 32],
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSymbolKey {
    pub unit: String,
    pub module: String,
    pub namespace: String,
    pub occurrence: String,
    pub record_parent: Option<String>,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCodeLocation {
    pub component: [u8; 32],
    pub member: u32,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRetainedFacts {
    pub symbol: (String, String, String, String, Option<String>),
    pub generation: u64,
    pub type_artifact: Option<[u8; 32]>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NativeCodeEdgeFacts {
    Local {
        member: u32,
        value: u32,
    },
    External {
        code: NativeCodeLocation,
        value: u32,
    },
    Retained(NativeRetainedFacts),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCodeNodeFacts {
    pub body: NativeCodeBodyKey,
    pub imports: Vec<NativeCodeEdgeFacts>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCodeComponentFacts {
    pub version: u32,
    pub nodes: Vec<NativeCodeNodeFacts>,
}

#[derive(Debug, thiserror::Error)]
pub enum NativeCodeError {
    #[error(transparent)]
    Budget(#[from] ParseError),
    #[error("native linked graph references a missing occurrence")]
    MissingOccurrence,
    #[error("native linked graph has incompatible duplicate nominal definitions")]
    ConflictingDefinition,
    #[error("native linked graph slot count differs from its definitions")]
    ImportCount,
    #[error("native linked graph has an invalid export or retained contract")]
    ImportContract,
    #[error("native linked component facts are not canonical or maximal")]
    NoncanonicalComponent,
    #[error("native linked component commitment collision")]
    CommitmentCollision,
    #[error("native linked graph encoding failed: {0}")]
    Encoding(String),
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>, NativeCodeError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|error| NativeCodeError::Encoding(error.to_string()))?;
    Ok(bytes)
}
impl NativeCodeComponent {
    pub fn identity(&self) -> &[u8; 32] {
        &self.identity
    }
    pub fn facts(&self) -> &NativeCodeComponentFacts {
        &self.facts
    }
    pub fn snapshot_record(&self) -> &Arc<NativeCodeComponentFacts> {
        &self.facts
    }
    pub fn record_bytes(&self) -> &Arc<[u8]> {
        &self.record_bytes
    }
    pub fn member(self: &Arc<Self>, member: u32) -> Option<NativeCodeRef> {
        ((member as usize) < self.nodes.len()).then(|| NativeCodeRef::new(Arc::clone(self), member))
    }
    pub fn members(self: &Arc<Self>) -> impl ExactSizeIterator<Item = NativeCodeRef> + '_ {
        (0..self.nodes.len()).map(|member| NativeCodeRef::new(Arc::clone(self), member as u32))
    }

    /// Link selected occurrences without an inventory search or held-candidate
    /// reconciliation. Unreachable available products are never linked.
    pub fn link(
        plans: &[NativeCodePlan],
        roots: &[usize],
        operation: &InventoryOperation,
    ) -> Result<Vec<NativeCodeRef>, NativeCodeError> {
        operation.reserve::<Option<petgraph::graph::NodeIndex>>(plans.len())?;
        operation.reserve::<Option<NativeCodeRef>>(plans.len())?;
        let mut indices = vec![None; plans.len()];
        let mut graph = DiGraph::<usize, ()>::new();
        let mut pending = roots.to_vec();
        while let Some(index) = pending.pop() {
            operation.charge(1)?;
            let plan = plans.get(index).ok_or(NativeCodeError::MissingOccurrence)?;
            if indices[index].is_some() {
                continue;
            }
            operation.reserve::<(usize, usize, usize)>(1)?;
            indices[index] = Some(graph.add_node(index));
            if plan.imports.len() != plan.body.definitions().globals().len() {
                return Err(NativeCodeError::ImportCount);
            }
            for import in &plan.imports {
                if let NativeCodePlanImport::Local { occurrence, .. } = import {
                    pending.push(*occurrence);
                }
            }
        }
        for node in graph.node_indices().collect::<Vec<_>>() {
            for import in &plans[graph[node]].imports {
                operation.charge(1)?;
                if let NativeCodePlanImport::Local { occurrence, .. } = import {
                    let target = indices
                        .get(*occurrence)
                        .copied()
                        .flatten()
                        .ok_or(NativeCodeError::MissingOccurrence)?;
                    operation.reserve::<(usize, usize, usize)>(1)?;
                    graph.add_edge(node, target, ());
                }
            }
        }
        // Both DFS passes are iterative; account for their temporary storage.
        operation.reserve::<usize>(
            graph
                .node_count()
                .saturating_mul(8)
                .saturating_add(graph.edge_count().saturating_mul(2)),
        )?;
        let components = kosaraju_scc(&graph);
        let mut component_of = vec![usize::MAX; plans.len()];
        for (component, nodes) in components.iter().enumerate() {
            for node in nodes {
                component_of[graph[*node]] = component;
            }
        }
        let mut dependencies = vec![BTreeSet::new(); components.len()];
        let mut dependents = vec![Vec::new(); components.len()];
        for (component, nodes) in components.iter().enumerate() {
            for node in nodes {
                for import in &plans[graph[*node]].imports {
                    if let NativeCodePlanImport::Local { occurrence, .. } = import {
                        let target = component_of[*occurrence];
                        if target != component {
                            dependencies[component].insert(target);
                        }
                    }
                }
            }
            for dependency in &dependencies[component] {
                dependents[*dependency].push(component);
            }
        }
        let mut ready = dependencies
            .iter()
            .enumerate()
            .filter_map(|(index, deps)| deps.is_empty().then_some(index))
            .collect::<BTreeSet<_>>();
        let mut linked: Vec<Option<NativeCodeRef>> = vec![None; plans.len()];
        let mut count = 0;
        while let Some(component_index) = ready.pop_first() {
            let mut ordered: BTreeMap<NativeCodeBodyKey, Vec<usize>> = BTreeMap::new();
            for node in &components[component_index] {
                let index = graph[*node];
                ordered
                    .entry(plans[index].body.key(operation)?)
                    .or_default()
                    .push(index);
            }
            if ordered.len() > u32::MAX as usize {
                return Err(NativeCodeError::NoncanonicalComponent);
            }
            let mut nominal = BTreeMap::new();
            for key in ordered.keys() {
                if nominal
                    .insert(key.nominal.clone(), key.definitions_sha256)
                    .is_some_and(|previous| previous != key.definitions_sha256)
                {
                    return Err(NativeCodeError::ConflictingDefinition);
                }
            }
            let mut local = BTreeMap::new();
            for (member, indices) in ordered.values().enumerate() {
                for index in indices {
                    local.insert(*index, member as u32);
                }
            }
            let mut members = Vec::new();
            let mut records = Vec::new();
            for (key, indices) in ordered {
                let first = indices[0];
                let mut chosen: Option<(Vec<ComponentImport>, Vec<NativeCodeEdgeFacts>)> = None;
                for index in indices {
                    if plans[first].body != plans[index].body {
                        return Err(NativeCodeError::CommitmentCollision);
                    }
                    let mut imports = Vec::new();
                    let mut facts = Vec::new();
                    for (slot, import) in plans[index].imports.iter().enumerate() {
                        let declaration = &plans[index].body.definitions().globals()[slot];
                        let (edge, fact) = match import {
                            NativeCodePlanImport::Local { occurrence, value } => {
                                validate_export(
                                    &plans[*occurrence].body,
                                    *value,
                                    declaration,
                                    plans[index].body.definitions(),
                                    operation,
                                )?;
                                if let Some(member) = local.get(occurrence) {
                                    (
                                        ComponentImport::Local {
                                            member: *member,
                                            value: *value,
                                        },
                                        NativeCodeEdgeFacts::Local {
                                            member: *member,
                                            value: value.0,
                                        },
                                    )
                                } else {
                                    let code = linked[*occurrence]
                                        .clone()
                                        .ok_or(NativeCodeError::NoncanonicalComponent)?;
                                    let fact = NativeCodeEdgeFacts::External {
                                        code: code.location(),
                                        value: value.0,
                                    };
                                    (
                                        ComponentImport::External {
                                            code,
                                            value: *value,
                                        },
                                        fact,
                                    )
                                }
                            }
                            NativeCodePlanImport::External { code, value } => {
                                validate_export(
                                    code.body(),
                                    *value,
                                    declaration,
                                    plans[index].body.definitions(),
                                    operation,
                                )?;
                                (
                                    ComponentImport::External {
                                        code: code.clone(),
                                        value: *value,
                                    },
                                    NativeCodeEdgeFacts::External {
                                        code: code.location(),
                                        value: value.0,
                                    },
                                )
                            }
                            NativeCodePlanImport::Retained(requirement) => {
                                if requirement.identity != declaration.identity
                                    || declaration.required_generation
                                        != Some(requirement.generation)
                                {
                                    return Err(NativeCodeError::ImportContract);
                                }
                                (
                                    ComponentImport::Retained(requirement.clone()),
                                    NativeCodeEdgeFacts::Retained(requirement.facts()),
                                )
                            }
                        };
                        imports.push(edge);
                        facts.push(fact);
                    }
                    if let Some((_, previous)) = &chosen {
                        if *previous != facts {
                            return Err(NativeCodeError::ConflictingDefinition);
                        }
                    } else {
                        chosen = Some((imports, facts));
                    }
                }
                let (imports, facts) = chosen.ok_or(NativeCodeError::NoncanonicalComponent)?;
                records.push(NativeCodeNodeFacts {
                    body: key,
                    imports: facts,
                });
                members.push(NativeCodeNode {
                    body: plans[first].body.clone(),
                    imports: imports.into(),
                });
            }
            let facts = NativeCodeComponentFacts {
                version: 1,
                nodes: records,
            };
            let bytes = encode(&facts)?;
            operation.charge(bytes.len())?;
            let mut hash = Sha256::new();
            hash.update(b"Tidepool linked native component v1\0");
            hash.update(&bytes);
            let owner = Arc::new(Self {
                identity: hash.finalize().into(),
                facts: Arc::new(facts),
                record_bytes: bytes.into(),
                nodes: members.into(),
            });
            for (index, member) in local {
                linked[index] = Some(NativeCodeRef::new(Arc::clone(&owner), member));
            }
            count += 1;
            for dependent in &dependents[component_index] {
                dependencies[*dependent].remove(&component_index);
                if dependencies[*dependent].is_empty() {
                    ready.insert(*dependent);
                }
            }
        }
        if count != components.len() {
            return Err(NativeCodeError::NoncanonicalComponent);
        }
        roots
            .iter()
            .map(|root| {
                linked
                    .get(*root)
                    .and_then(Clone::clone)
                    .ok_or(NativeCodeError::MissingOccurrence)
            })
            .collect()
    }

    /// Reissue exact acquired bodies and decoded references through the same
    /// constructor. Arbitrary partitions and nonmaximal components refuse.
    pub fn restore(
        facts: &NativeCodeComponentFacts,
        bodies: Vec<NativeCodeBody>,
        external: &BTreeMap<NativeCodeLocation, NativeCodeRef>,
        operation: &InventoryOperation,
    ) -> Result<Arc<Self>, NativeCodeError> {
        if facts.version != 1
            || facts.nodes.is_empty()
            || bodies.len() != facts.nodes.len()
            || facts
                .nodes
                .windows(2)
                .any(|pair| pair[0].body >= pair[1].body)
        {
            return Err(NativeCodeError::NoncanonicalComponent);
        }
        let mut plans = Vec::new();
        for (record, body) in facts.nodes.iter().zip(bodies) {
            if body.key(operation)? != record.body {
                return Err(NativeCodeError::CommitmentCollision);
            }
            let imports = record
                .imports
                .iter()
                .map(|edge| match edge {
                    NativeCodeEdgeFacts::Local { member, value } => {
                        Ok(NativeCodePlanImport::Local {
                            occurrence: *member as usize,
                            value: ValueId(*value),
                        })
                    }
                    NativeCodeEdgeFacts::External { code, value } => {
                        let reference = external
                            .get(code)
                            .filter(|reference| reference.location() == *code)
                            .ok_or(NativeCodeError::MissingOccurrence)?;
                        Ok(NativeCodePlanImport::External {
                            code: reference.clone(),
                            value: ValueId(*value),
                        })
                    }
                    NativeCodeEdgeFacts::Retained(requirement) => {
                        let (unit, module, namespace, occurrence, record_parent) =
                            &requirement.symbol;
                        Ok(NativeCodePlanImport::Retained(
                            NativeRetainedRequirement::new(
                                SymbolIdentity {
                                    unit: unit.clone(),
                                    module: module.clone(),
                                    namespace: namespace.clone(),
                                    occurrence: occurrence.clone(),
                                    record_parent: record_parent.clone(),
                                },
                                requirement.generation,
                                requirement.type_artifact,
                            ),
                        ))
                    }
                })
                .collect::<Result<Vec<_>, NativeCodeError>>()?;
            plans.push(NativeCodePlan { body, imports });
        }
        let roots = (0..plans.len()).collect::<Vec<_>>();
        let linked = Self::link(&plans, &roots, operation)?;
        let first = linked
            .first()
            .ok_or(NativeCodeError::NoncanonicalComponent)?;
        if linked
            .iter()
            .any(|code| !Arc::ptr_eq(code.component(), first.component()))
            || first.component.facts.as_ref() != facts
        {
            return Err(NativeCodeError::NoncanonicalComponent);
        }
        Ok(Arc::clone(first.component()))
    }
}

fn validate_export(
    body: &NativeCodeBody,
    value: ValueId,
    declaration: &crate::execution_schema::GlobalDecl,
    importer: DefinitionsView<'_>,
    operation: &InventoryOperation,
) -> Result<(), NativeCodeError> {
    if body.definitions().envelope() != importer.envelope() {
        return Err(NativeCodeError::ImportContract);
    }
    let contracts = match body {
        NativeCodeBody::Home(code) => code.projected_group(),
        NativeCodeBody::Package(code) => code.projected_group(),
    };
    let mut selected = None;
    for contract in contracts.export_contracts() {
        operation.charge(1)?;
        let contract = contract?;
        if contract.value == value {
            selected = Some(contract);
            break;
        }
    }
    let exported = selected.ok_or(NativeCodeError::ImportContract)?;
    let signature = declaration
        .entry_signature
        .map(|id| {
            importer
                .signatures()
                .get(id.0 as usize)
                .ok_or(NativeCodeError::ImportContract)
        })
        .transpose()?;
    if exported.identity != &declaration.identity
        || exported.rep != declaration.rep
        || signature.is_some_and(|required| exported.entry_signature != Some(required))
        || declaration.required_evaluated && !exported.evaluated
        || declaration.required_generation.is_some()
    {
        return Err(NativeCodeError::ImportContract);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_schema::{
        testing, Atom, ExprFrame, GlobalDecl, Group, HeapRhs, PackageCodeOwner, RuntimeRep,
        ScalarLiteral,
    };

    fn plan(module: &str, children: &[(&str, usize)], result: i64) -> NativeCodePlan {
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = testing::identity(module, "entry");
        assert!(matches!(top.binding.rhs, HeapRhs::Function { .. }));
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: result.to_be_bytes().to_vec(),
        })]);
        wire.globals = children
            .iter()
            .map(|(module, _)| GlobalDecl {
                identity: testing::identity(module, "entry"),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            })
            .collect();
        let body = PackageGroupCode::admit(
            PackageCodeOwner {
                unit: "fixture".into(),
                module: module.into(),
                compiler_sha256: [1; 32],
                profile_sha256: [2; 32],
                interface_sha256: [3; 32],
                defining_version_sha256: [4; 32],
            },
            testing::projected_group(wire, 7).unwrap(),
        )
        .unwrap();
        NativeCodePlan {
            body: NativeCodeBody::Package(body),
            imports: children
                .iter()
                .map(|(_, occurrence)| NativeCodePlanImport::Local {
                    occurrence: *occurrence,
                    value: ValueId(0),
                })
                .collect(),
        }
    }
    fn operation() -> InventoryOperation {
        InventoryOperation::new(Default::default())
    }

    #[test]
    fn unrelated_roots_and_order_do_not_change_linked_code() {
        let plans = vec![
            plan("A", &[("B", 1)], 1),
            plan("B", &[], 2),
            plan("Unused", &[], 3),
        ];
        let one = NativeCodeComponent::link(&plans, &[0], &operation()).unwrap();
        let many = NativeCodeComponent::link(&plans, &[2, 0, 0], &operation()).unwrap();
        assert_eq!(one[0].id(), many[1].id());
        assert_eq!(many[1].id(), many[2].id());
        assert_ne!(many[0].id(), many[1].id());
        let changed = vec![
            plan("A", &[("B", 1)], 1),
            plan("B", &[], 9),
            plan("Unused", &[], 3),
        ];
        let next = NativeCodeComponent::link(&changed, &[0, 2], &operation()).unwrap();
        assert_ne!(next[0].id(), one[0].id());
        assert_eq!(next[1].id(), many[0].id());
    }

    #[test]
    fn cyclic_code_is_canonical_and_restores_through_same_linker() {
        let first = NativeCodeComponent::link(
            &[plan("A", &[("B", 1)], 1), plan("B", &[("A", 0)], 2)],
            &[0, 1],
            &operation(),
        )
        .unwrap();
        let reversed = NativeCodeComponent::link(
            &[plan("B", &[("A", 1)], 2), plan("A", &[("B", 0)], 1)],
            &[0, 1],
            &operation(),
        )
        .unwrap();
        assert_eq!(first[0].id(), reversed[1].id());
        assert_eq!(first[1].id(), reversed[0].id());
        assert!(Arc::ptr_eq(first[0].component(), first[1].component()));
        let component = first[0].component();
        let bodies = component
            .members()
            .map(|member| member.body().clone())
            .collect();
        let restored =
            NativeCodeComponent::restore(component.facts(), bodies, &BTreeMap::new(), &operation())
                .unwrap();
        assert_eq!(restored.identity(), component.identity());
        let disconnected = NativeCodeComponent::link(
            &[plan("A", &[], 1), plan("B", &[], 2)],
            &[0, 1],
            &operation(),
        )
        .unwrap();
        let facts = NativeCodeComponentFacts {
            version: 1,
            nodes: disconnected
                .iter()
                .map(|code| code.component().facts().nodes[0].clone())
                .collect(),
        };
        assert!(matches!(
            NativeCodeComponent::restore(
                &facts,
                disconnected
                    .iter()
                    .map(|code| code.body().clone())
                    .collect(),
                &BTreeMap::new(),
                &operation()
            ),
            Err(NativeCodeError::NoncanonicalComponent)
        ));
    }

    #[test]
    fn incompatible_nominal_definition_inside_a_cycle_refuses() {
        let plans = [
            plan("Same", &[("Same", 1)], 1),
            plan("Same", &[("Same", 0)], 2),
        ];
        assert!(matches!(
            NativeCodeComponent::link(&plans, &[0], &operation()),
            Err(NativeCodeError::ConflictingDefinition)
        ));
    }

    #[test]
    fn distinct_groups_in_one_package_module_keep_distinct_nominal_keys() {
        let make = |name: &str, child: &str, index| {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.identity = testing::identity("Shared", name);
            wire.globals.push(GlobalDecl {
                identity: testing::identity("Shared", child),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            });
            NativeCodePlan {
                body: NativeCodeBody::Package(
                    PackageGroupCode::admit(
                        PackageCodeOwner {
                            unit: "fixture".into(),
                            module: "Shared".into(),
                            compiler_sha256: [1; 32],
                            profile_sha256: [2; 32],
                            interface_sha256: [3; 32],
                            defining_version_sha256: [4; 32],
                        },
                        testing::projected_group(wire, 9).unwrap(),
                    )
                    .unwrap(),
                ),
                imports: vec![NativeCodePlanImport::Local {
                    occurrence: index,
                    value: ValueId(0),
                }],
            }
        };
        let linked = NativeCodeComponent::link(
            &[make("left", "right", 1), make("right", "left", 0)],
            &[0, 1],
            &operation(),
        )
        .unwrap();
        assert_ne!(linked[0].id(), linked[1].id());
        assert!(Arc::ptr_eq(linked[0].component(), linked[1].component()));
    }

    #[test]
    fn a_private_helper_is_not_a_selectable_export() {
        use crate::execution_schema::{
            encoding::encode_projected_group, parse_projected_group, DecodeLimits,
            ProgramRequirements,
        };
        let mut wire = testing::wire_program();
        let Group::NonRecursive(mut helper) = wire.bindings[0].clone() else {
            unreachable!()
        };
        helper.identity = testing::identity("Fixture", "helper");
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!()
        };
        entry.binding.id = ValueId(1);
        wire.bindings.insert(0, Group::NonRecursive(helper));
        wire.entry = ValueId(1);
        let requirements = ProgramRequirements {
            schema_version: wire.envelope.schema_version,
            projection_profile: wire.envelope.projection_profile.clone(),
            toolchain: wire.envelope.toolchain.clone(),
            execution_abi_version: wire.envelope.execution_abi_version,
            target: wire.envelope.target.clone(),
        };
        let encoded = encode_projected_group(&testing::projected_group(wire, 4).unwrap());
        let mut value: ciborium::value::Value =
            ciborium::de::from_reader(encoded.as_slice()).unwrap();
        let ciborium::value::Value::Array(fields) = &mut value else {
            unreachable!()
        };
        let ciborium::value::Value::Array(binders) = &mut fields[3] else {
            unreachable!()
        };
        binders.remove(0);
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&value, &mut encoded).unwrap();
        let group =
            parse_projected_group(&encoded, &requirements, DecodeLimits::default()).unwrap();
        let body = NativeCodeBody::Package(
            PackageGroupCode::admit(
                PackageCodeOwner {
                    unit: "fixture".into(),
                    module: "Fixture".into(),
                    compiler_sha256: [1; 32],
                    profile_sha256: [2; 32],
                    interface_sha256: [3; 32],
                    defining_version_sha256: [4; 32],
                },
                group,
            )
            .unwrap(),
        );
        let mut importer = plan("Importer", &[("Fixture", 1)], 0);
        let NativeCodeBody::Package(code) = &importer.body else {
            unreachable!()
        };
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = testing::identity("Importer", "entry");
        wire.globals.push(GlobalDecl {
            identity: testing::identity("Fixture", "helper"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        importer.body = NativeCodeBody::Package(
            PackageGroupCode::admit(
                code.owner().clone(),
                testing::projected_group(wire, 0).unwrap(),
            )
            .unwrap(),
        );
        assert!(matches!(
            NativeCodeComponent::link(
                &[
                    importer,
                    NativeCodePlan {
                        body,
                        imports: vec![]
                    }
                ],
                &[0],
                &operation()
            ),
            Err(NativeCodeError::ImportContract)
        ));
    }
}
