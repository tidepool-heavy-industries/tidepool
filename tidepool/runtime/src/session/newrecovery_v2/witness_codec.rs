//! One durable dictionary carries complete issuer witness facts. Indices only
//! encode exact typed nodes and edges; they never select compiler authority.
use super::*;
use serde::ser::{SerializeSeq, SerializeStruct};

type BoundEdge = (ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency);
const MAX_EXPANDED_WITNESS_BYTES: u64 = 4 << 30;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedGraph {
    version: u32,
    public_schema: String,
    source_session: u64,
    lineage: u64,
    high_water: u64,
    public_surfaces: Vec<RecoveryPublicSurface<SelectionRecord, RoleRecord>>,
    nodes: Vec<RecoveryNode<SelectionRecord, RoleRecord>>,
    artifacts: Vec<RecoveryArtifactClosure>,
    artifact_bindings: Vec<ArtifactBinding>,
    artifact_dependencies: Vec<RecoveryArtifactDependency>,
    witness_nodes: Vec<ArtifactBindingNode>,
    witness_edges: Vec<EdgeRecord>,
    selection_plans: Vec<PlanRecord>,
    checksum: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionRecord {
    selections: Vec<ArtifactSelectionId>,
    bindings: Vec<ArtifactBinding>,
    namespace: Vec<ArtifactBinding>,
    roots: Vec<ArtifactBindingNode>,
    native_groups: Vec<NativeGroupBinding>,
    native_custody: Vec<ArtifactBinding>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactSelectionRecord {
    graph: SelectionRecord,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RoleRecord {
    InterfaceOnly {
        interface: ArtifactId,
    },
    ReusableOriginal {
        interface: ArtifactId,
        original: ArtifactId,
    },
    PublishedSourceOriginal {
        interface: ArtifactId,
        original: ArtifactId,
        source_revision: String,
        original_input_identity: String,
        selection: ExactSelectionRecord,
        selection_sha256: [u8; 32],
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EdgeRecord {
    source: u32,
    target: u32,
    dependency: ArtifactDependency,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanRecord {
    selection: ArtifactSelectionId,
    nodes: Vec<u32>,
    dependencies: Vec<u32>,
}

struct SelectionRef<'a>(&'a ArtifactGraphSelection);
struct PlanIds<'a>(&'a [Arc<ArtifactSelectionPlan>]);
impl Serialize for PlanIds<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for plan in self.0 {
            seq.serialize_element(&plan.selection)?;
        }
        seq.end()
    }
}
impl Serialize for SelectionRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let row = self.0;
        let mut fields = serializer.serialize_struct("SelectionRecord", 6)?;
        fields.serialize_field("selections", &PlanIds(&row.selections))?;
        fields.serialize_field("bindings", &row.bindings)?;
        fields.serialize_field("namespace", &row.namespace)?;
        fields.serialize_field("roots", &row.roots)?;
        fields.serialize_field("native_groups", &row.native_groups)?;
        fields.serialize_field("native_custody", &row.native_custody)?;
        fields.end()
    }
}
#[derive(Serialize)]
struct ExactSelectionRef<'a> {
    graph: SelectionRef<'a>,
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RoleRef<'a> {
    InterfaceOnly {
        interface: &'a ArtifactId,
    },
    ReusableOriginal {
        interface: &'a ArtifactId,
        original: &'a ArtifactId,
    },
    PublishedSourceOriginal {
        interface: &'a ArtifactId,
        original: &'a ArtifactId,
        source_revision: &'a str,
        original_input_identity: &'a str,
        selection: ExactSelectionRef<'a>,
        selection_sha256: &'a [u8; 32],
    },
}
impl<'a> From<&'a CompilerInputRole> for RoleRef<'a> {
    fn from(role: &'a CompilerInputRole) -> Self {
        match role {
            CompilerInputRole::InterfaceOnly { interface } => Self::InterfaceOnly { interface },
            CompilerInputRole::ReusableOriginal {
                interface,
                original,
            } => Self::ReusableOriginal {
                interface,
                original,
            },
            CompilerInputRole::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection,
                selection_sha256,
            } => Self::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection: ExactSelectionRef {
                    graph: SelectionRef(selection.graph_selection()),
                },
                selection_sha256,
            },
        }
    }
}
struct RolesRef<'a>(&'a [CompilerInputRole]);
impl Serialize for RolesRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for role in self.0 {
            seq.serialize_element(&RoleRef::from(role))?;
        }
        seq.end()
    }
}
struct ContextRef<'a>(&'a RecoveryCompilerContext);
impl Serialize for ContextRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let row = self.0;
        let mut fields = serializer.serialize_struct("RecoveryCompilerContext", 5)?;
        fields.serialize_field("graph_selection", &SelectionRef(&row.graph_selection))?;
        fields.serialize_field("artifact_refs", &row.artifact_refs)?;
        fields.serialize_field("native_groups", &row.native_groups)?;
        fields.serialize_field("compiler_roles", &RolesRef(&row.compiler_roles))?;
        fields.serialize_field("lexical", &row.lexical)?;
        fields.end()
    }
}
struct SurfaceRef<'a>(&'a RecoveryPublicSurface);
impl Serialize for SurfaceRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let row = self.0;
        let mut fields = serializer.serialize_struct("RecoveryPublicSurface", 6)?;
        fields.serialize_field("owner", &row.owner)?;
        fields.serialize_field("declaration_root", &row.declaration_root.map(|root| root.0))?;
        fields.serialize_field("epoch", &row.epoch)?;
        fields.serialize_field("bindings", &row.bindings)?;
        fields.serialize_field("source_instances", &row.source_instances)?;
        fields.serialize_field("compiler_context", &ContextRef(&row.compiler_context))?;
        fields.end()
    }
}
struct GenerationRows<'a>(&'a [Generation]);
impl Serialize for GenerationRows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for generation in self.0 {
            seq.serialize_element(&generation.0)?;
        }
        seq.end()
    }
}
struct NodeRef<'a>(&'a RecoveryNode);
impl Serialize for NodeRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let row = self.0;
        let mut fields = serializer.serialize_struct("RecoveryNode", 16)?;
        fields.serialize_field("graph_selection", &SelectionRef(&row.graph_selection))?;
        fields.serialize_field("id", &row.id.0)?;
        fields.serialize_field("parent", &row.parent.map(|root| root.0))?;
        fields.serialize_field("kind", &row.kind)?;
        fields.serialize_field(
            "implementation_refs",
            &GenerationRows(&row.implementation_refs),
        )?;
        fields.serialize_field("lexical_roots", &row.lexical_roots)?;
        fields.serialize_field("lexical", &row.lexical)?;
        fields.serialize_field("artifact_refs", &row.artifact_refs)?;
        fields.serialize_field("native_groups", &row.native_groups)?;
        fields.serialize_field("compiler_roles", &RolesRef(&row.compiler_roles))?;
        fields.serialize_field("exports", &row.exports)?;
        fields.serialize_field("retracts", &row.retracts)?;
        fields.serialize_field("workbench_imports", &row.workbench_imports)?;
        fields.serialize_field("instances", &row.instances)?;
        fields.serialize_field("live_dependencies", &row.live_dependencies)?;
        fields.serialize_field("state", &row.state)?;
        fields.end()
    }
}

#[derive(Serialize)]
struct EncodedGraphRef<'a> {
    version: u32,
    public_schema: &'a str,
    source_session: u64,
    lineage: u64,
    high_water: u64,
    public_surfaces: Vec<SurfaceRef<'a>>,
    nodes: Vec<NodeRef<'a>>,
    artifacts: Vec<&'a RecoveryArtifactClosure>,
    artifact_bindings: Vec<&'a ArtifactBinding>,
    artifact_dependencies: Vec<&'a RecoveryArtifactDependency>,
    witness_nodes: Vec<ArtifactBindingNode>,
    witness_edges: Vec<EdgeRef<'a>>,
    selection_plans: Vec<PlanRef<'a>>,
    checksum: &'a str,
}
#[derive(Serialize)]
struct EdgeRef<'a> {
    source: u32,
    target: u32,
    dependency: &'a ArtifactDependency,
}
#[derive(Serialize)]
struct PlanRef<'a> {
    selection: ArtifactSelectionId,
    nodes: Vec<u32>,
    dependencies: Vec<u32>,
    #[serde(skip)]
    _owner: std::marker::PhantomData<&'a ArtifactSelectionPlan>,
}

fn collect_selection<'a>(
    selection: &'a ArtifactGraphSelection,
    plans: &mut BTreeMap<ArtifactSelectionId, &'a ArtifactSelectionPlan>,
) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for plan in &selection.selections {
        if !unique.insert(plan.selection) {
            return Err("duplicate selection witness reference".into());
        }
        if let Some(previous) = plans.get(&plan.selection) {
            if !std::ptr::eq(*previous, plan.as_ref()) && *previous != plan.as_ref() {
                return Err("one selection ID has differing complete witness facts".into());
            }
        } else {
            if !sorted_unique(&plan.nodes) || !sorted_unique(&plan.dependencies) {
                return Err("complete witness facts must be sorted and unique".into());
            }
            plans.insert(plan.selection, plan.as_ref());
        }
    }
    Ok(())
}
fn collect_roles<'a>(
    roles: &'a [CompilerInputRole],
    plans: &mut BTreeMap<ArtifactSelectionId, &'a ArtifactSelectionPlan>,
) -> Result<(), String> {
    for role in roles {
        if let CompilerInputRole::PublishedSourceOriginal { selection, .. } = role {
            collect_selection(selection.graph_selection(), plans)?;
        }
    }
    Ok(())
}
pub(super) fn encode<S: serde::Serializer>(
    graph: &impl GraphRead,
    checksum: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut plans = BTreeMap::new();
    for node in graph.nodes() {
        collect_selection(&node.graph_selection, &mut plans).map_err(serde::ser::Error::custom)?;
        collect_roles(&node.compiler_roles, &mut plans).map_err(serde::ser::Error::custom)?;
    }
    for surface in graph.public_surfaces() {
        collect_selection(&surface.compiler_context.graph_selection, &mut plans)
            .map_err(serde::ser::Error::custom)?;
        collect_roles(&surface.compiler_context.compiler_roles, &mut plans)
            .map_err(serde::ser::Error::custom)?;
    }
    let mut nodes = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for plan in plans.values() {
        nodes.extend(plan.nodes.iter().copied());
        for (source, target, dependency) in &plan.dependencies {
            nodes.extend([*source, *target]);
            edges.insert((*source, *target, dependency));
        }
    }
    let node_indices = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| u32::try_from(index).map(|index| (*node, index)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(serde::ser::Error::custom)?;
    let edge_indices = edges
        .iter()
        .enumerate()
        .map(|(index, edge)| u32::try_from(index).map(|index| (*edge, index)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(serde::ser::Error::custom)?;
    let witness_edges = edges
        .iter()
        .map(|(source, target, dependency)| EdgeRef {
            source: node_indices[source],
            target: node_indices[target],
            dependency,
        })
        .collect();
    let selection_plans = plans
        .values()
        .map(|plan| PlanRef {
            selection: plan.selection,
            nodes: plan.nodes.iter().map(|node| node_indices[node]).collect(),
            dependencies: plan
                .dependencies
                .iter()
                .map(|(source, target, dependency)| edge_indices[&(*source, *target, dependency)])
                .collect(),
            _owner: std::marker::PhantomData,
        })
        .collect();
    EncodedGraphRef {
        version: graph.version(),
        public_schema: graph.public_schema(),
        source_session: graph.source_session(),
        lineage: graph.lineage(),
        high_water: graph.high_water().0,
        public_surfaces: graph.public_surfaces().map(SurfaceRef).collect(),
        nodes: graph.nodes().map(NodeRef).collect(),
        artifacts: graph.artifacts().collect(),
        artifact_bindings: graph.artifact_bindings().collect(),
        artifact_dependencies: graph.artifact_dependencies().collect(),
        witness_nodes: nodes.into_iter().collect(),
        witness_edges,
        selection_plans,
        checksum,
    }
    .serialize(serializer)
}

struct DecodedPlans {
    plans: BTreeMap<ArtifactSelectionId, Arc<ArtifactSelectionPlan>>,
    used: BTreeSet<ArtifactSelectionId>,
}
impl SelectionRecord {
    fn restore(self, table: &mut DecodedPlans) -> Result<ArtifactGraphSelection, String> {
        let mut unique = BTreeSet::new();
        let selections = self
            .selections
            .into_iter()
            .map(|id| {
                if !unique.insert(id) {
                    return Err("duplicate selection witness reference".into());
                }
                table.used.insert(id);
                table
                    .plans
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| "missing selection witness reference".into())
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(ArtifactGraphSelection {
            selections,
            bindings: self.bindings,
            namespace: self.namespace,
            roots: self.roots,
            native_groups: self.native_groups,
            native_custody: self.native_custody,
        })
    }
}
impl RoleRecord {
    fn restore(self, table: &mut DecodedPlans) -> Result<CompilerInputRole, String> {
        Ok(match self {
            Self::InterfaceOnly { interface } => CompilerInputRole::InterfaceOnly { interface },
            Self::ReusableOriginal {
                interface,
                original,
            } => CompilerInputRole::ReusableOriginal {
                interface,
                original,
            },
            Self::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection,
                selection_sha256,
            } => CompilerInputRole::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection: ExactArtifactSelection::from_recovered_graph_selection(
                    selection.graph.restore(table)?,
                ),
                selection_sha256,
            },
        })
    }
}
impl RecoveryCompilerContext<SelectionRecord, RoleRecord> {
    fn restore(self, table: &mut DecodedPlans) -> Result<RecoveryCompilerContext, String> {
        Ok(RecoveryCompilerContext {
            graph_selection: self.graph_selection.restore(table)?,
            artifact_refs: self.artifact_refs,
            native_groups: self.native_groups,
            compiler_roles: self
                .compiler_roles
                .into_iter()
                .map(|role| role.restore(table))
                .collect::<Result<Vec<_>, _>>()?,
            lexical: self.lexical,
        })
    }
}
impl RecoveryPublicSurface<SelectionRecord, RoleRecord> {
    fn restore(self, table: &mut DecodedPlans) -> Result<RecoveryPublicSurface, String> {
        Ok(RecoveryPublicSurface {
            owner: self.owner,
            declaration_root: self.declaration_root,
            epoch: self.epoch,
            bindings: self.bindings,
            source_instances: self.source_instances,
            compiler_context: self.compiler_context.restore(table)?,
        })
    }
}
impl RecoveryNode<SelectionRecord, RoleRecord> {
    fn restore(self, table: &mut DecodedPlans) -> Result<RecoveryNode, String> {
        Ok(RecoveryNode {
            graph_selection: self.graph_selection.restore(table)?,
            id: self.id,
            parent: self.parent,
            kind: self.kind,
            implementation_refs: self.implementation_refs,
            lexical_roots: self.lexical_roots,
            lexical: self.lexical,
            artifact_refs: self.artifact_refs,
            native_groups: self.native_groups,
            compiler_roles: self
                .compiler_roles
                .into_iter()
                .map(|role| role.restore(table))
                .collect::<Result<Vec<_>, _>>()?,
            exports: self.exports,
            retracts: self.retracts,
            workbench_imports: self.workbench_imports,
            instances: self.instances,
            live_dependencies: self.live_dependencies,
            state: self.state,
        })
    }
}
fn sorted_unique<T: Ord>(rows: &[T]) -> bool {
    rows.windows(2).all(|pair| pair[0] < pair[1])
}
fn dependency_heap_bytes(dependency: &ArtifactDependency) -> u64 {
    match dependency {
        ArtifactDependency::NativeBinding {
            namespace,
            occurrence,
            record_parent,
            ..
        } => {
            namespace.len() as u64
                + occurrence.len() as u64
                + record_parent
                    .as_ref()
                    .map_or(0, |parent| parent.len() as u64)
        }
        ArtifactDependency::Interface | ArtifactDependency::NativeGroup { .. } => 0,
    }
}
impl EncodedGraph {
    fn restore(mut self, expansion_limit: u64) -> Result<RecoveryGraphWire, String> {
        if self.version != VERSION || self.public_schema != PAIRED_PUBLIC_SCHEMA {
            return Err("unsupported shared-witness recovery schema".into());
        }
        // Authenticate the typed raw arrays before maps or expansion erase any
        // order, duplicate field, reference, or checksum evidence.
        let checksum = std::mem::take(&mut self.checksum);
        let unsigned = serde_json::to_vec(&self).map_err(|error| error.to_string())?;
        let mut domain = b"tidepool-recovery-graph-v11\0".to_vec();
        domain.extend_from_slice(&unsigned);
        if blake3::hash(&domain).to_hex().as_str() != checksum {
            return Err("recovery graph checksum mismatch".into());
        }
        self.checksum = checksum;
        if !sorted_unique(&self.witness_nodes)
            || !sorted_unique(&self.witness_edges)
            || self
                .selection_plans
                .windows(2)
                .any(|pair| pair[0].selection >= pair[1].selection)
        {
            return Err("witness dictionaries must be sorted and unique".into());
        }
        // References are cheap transport facts. Refuse unused, duplicate, or
        // unknown plans before allocating any expanded complete witness.
        let known = self
            .selection_plans
            .iter()
            .map(|plan| plan.selection)
            .collect::<BTreeSet<_>>();
        let mut referenced = BTreeSet::new();
        let mut check_selection = |selection: &SelectionRecord| -> Result<(), String> {
            let mut unique = BTreeSet::new();
            for id in &selection.selections {
                if !unique.insert(*id) {
                    return Err("duplicate selection witness reference".into());
                }
                if !known.contains(id) {
                    return Err("missing selection witness reference".into());
                }
                referenced.insert(*id);
            }
            Ok(())
        };
        for node in &self.nodes {
            check_selection(&node.graph_selection)?;
            for role in &node.compiler_roles {
                if let RoleRecord::PublishedSourceOriginal { selection, .. } = role {
                    check_selection(&selection.graph)?;
                }
            }
        }
        for surface in &self.public_surfaces {
            check_selection(&surface.compiler_context.graph_selection)?;
            for role in &surface.compiler_context.compiler_roles {
                if let RoleRecord::PublishedSourceOriginal { selection, .. } = role {
                    check_selection(&selection.graph)?;
                }
            }
        }
        if referenced != known {
            return Err("unreferenced selection witness table row".into());
        }
        let mut used_nodes = BTreeSet::new();
        let mut used_edges = BTreeSet::new();
        for edge in &self.witness_edges {
            for index in [edge.source, edge.target] {
                if index as usize >= self.witness_nodes.len() {
                    return Err("missing witness edge endpoint index".into());
                }
                used_nodes.insert(index);
            }
        }
        let mut expanded = (self.selection_plans.len() as u64)
            .checked_mul(
                (std::mem::size_of::<ArtifactSelectionPlan>() + 2 * std::mem::size_of::<usize>())
                    as u64,
            )
            .ok_or("expanded witness byte count overflow")?;
        for plan in &self.selection_plans {
            if !sorted_unique(&plan.nodes) || !sorted_unique(&plan.dependencies) {
                return Err(
                    "complete witness node and edge references must be sorted and unique".into(),
                );
            }
            for index in &plan.nodes {
                if *index as usize >= self.witness_nodes.len() {
                    return Err("missing witness node index".into());
                }
                used_nodes.insert(*index);
                expanded = expanded
                    .checked_add(std::mem::size_of::<ArtifactBindingNode>() as u64)
                    .ok_or("expanded witness byte count overflow")?;
            }
            for index in &plan.dependencies {
                let edge = self
                    .witness_edges
                    .get(*index as usize)
                    .ok_or("missing witness edge index")?;
                used_edges.insert(*index);
                expanded = expanded
                    .checked_add(
                        std::mem::size_of::<BoundEdge>() as u64
                            + dependency_heap_bytes(&edge.dependency),
                    )
                    .ok_or("expanded witness byte count overflow")?;
            }
        }
        if used_nodes.len() != self.witness_nodes.len()
            || used_edges.len() != self.witness_edges.len()
        {
            return Err("unused witness dictionary node or edge".into());
        }
        if expanded > expansion_limit {
            return Err(format!("expanded recovery witness facts exceed bound: bytes={expanded}, limit={expansion_limit}"));
        }
        let mut table = DecodedPlans {
            plans: BTreeMap::new(),
            used: BTreeSet::new(),
        };
        for plan in self.selection_plans {
            let nodes = plan
                .nodes
                .into_iter()
                .map(|index| self.witness_nodes[index as usize])
                .collect();
            let dependencies = plan
                .dependencies
                .into_iter()
                .map(|index| {
                    let edge = &self.witness_edges[index as usize];
                    (
                        self.witness_nodes[edge.source as usize],
                        self.witness_nodes[edge.target as usize],
                        edge.dependency.clone(),
                    )
                })
                .collect();
            table.plans.insert(
                plan.selection,
                Arc::new(ArtifactSelectionPlan {
                    selection: plan.selection,
                    nodes,
                    dependencies,
                }),
            );
        }
        let public_surfaces = self
            .public_surfaces
            .into_iter()
            .map(|surface| surface.restore(&mut table))
            .collect::<Result<Vec<_>, _>>()?;
        let nodes = self
            .nodes
            .into_iter()
            .map(|node| node.restore(&mut table))
            .collect::<Result<Vec<_>, _>>()?;
        if table.used.len() != table.plans.len() {
            return Err("unreferenced selection witness table row".into());
        }
        Ok(RecoveryGraphWire {
            version: self.version,
            public_schema: self.public_schema,
            source_session: self.source_session,
            lineage: self.lineage,
            high_water: Generation(self.high_water),
            public_surfaces,
            nodes,
            artifacts: self.artifacts,
            artifact_bindings: self.artifact_bindings,
            artifact_dependencies: self.artifact_dependencies,
            checksum: self.checksum,
        })
    }
}
impl Serialize for RecoveryGraphWire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        encode(self, &self.checksum, serializer)
    }
}
impl<'de> Deserialize<'de> for RecoveryGraphWire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        EncodedGraph::deserialize(deserializer)?
            .restore(MAX_EXPANDED_WITNESS_BYTES)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[path = "witness_codec/census_tests.rs"]
mod census_tests;
#[cfg(test)]
#[path = "witness_codec/refusal_tests.rs"]
mod refusal_tests;

#[cfg(test)]
#[path = "witness_codec/history_tests.rs"]
mod history_tests;
