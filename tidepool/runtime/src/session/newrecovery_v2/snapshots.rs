//! Immutable recovery rows, shared by exact publication snapshots.
use super::*;
use rpds::RedBlackTreeMapSync;
use serde::ser::{SerializeSeq, SerializeStruct};

type EdgeKey = (ArtifactId, ArtifactId, ArtifactDependency);

/// A successfully checked v8 record. Cloning copies persistent tree roots;
/// historical node, artifact, edge and surface payloads are not cloned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryGraph {
    pub(super) version: u32,
    pub(super) public_schema: String,
    pub(super) source_session: u64,
    pub(super) lineage: u64,
    pub(super) high_water: Generation,
    pub(super) public_surfaces: RedBlackTreeMapSync<RecoveryPublicOwner, RecoveryPublicSurface>,
    pub(super) nodes: RedBlackTreeMapSync<Generation, RecoveryNode>,
    pub(super) artifacts: RedBlackTreeMapSync<ArtifactId, RecoveryArtifactClosure>,
    pub(super) artifact_dependencies: RedBlackTreeMapSync<EdgeKey, RecoveryArtifactDependency>,
    /// The authenticated raw-wire checksum remains the baseline revision even
    /// when a valid input's array order differs from canonical tree order.
    pub(super) checksum: String,
    wire_order_matches_maps: bool,
}

pub(crate) struct RecoveryGraphCandidate(RecoveryGraph);

/// Shared read contract lets raw wire validation inspect the original arrays
/// before any map conversion can erase duplicates or change checksum order.
pub(super) trait GraphRead {
    fn version(&self) -> u32;
    fn public_schema(&self) -> &str;
    fn source_session(&self) -> u64;
    fn lineage(&self) -> u64;
    fn high_water(&self) -> Generation;
    fn nodes(&self) -> impl Iterator<Item = &RecoveryNode>;
    fn artifacts(&self) -> impl Iterator<Item = &RecoveryArtifactClosure>;
    fn public_surfaces(&self) -> impl Iterator<Item = &RecoveryPublicSurface>;
    fn artifact_dependencies(&self) -> impl Iterator<Item = &RecoveryArtifactDependency>;
}

macro_rules! scalar_reads {
    () => {
        fn version(&self) -> u32 {
            self.version
        }
        fn public_schema(&self) -> &str {
            &self.public_schema
        }
        fn source_session(&self) -> u64 {
            self.source_session
        }
        fn lineage(&self) -> u64 {
            self.lineage
        }
        fn high_water(&self) -> Generation {
            self.high_water
        }
    };
}
impl GraphRead for RecoveryGraphWire {
    scalar_reads!();
    fn nodes(&self) -> impl Iterator<Item = &RecoveryNode> {
        self.nodes.iter()
    }
    fn artifacts(&self) -> impl Iterator<Item = &RecoveryArtifactClosure> {
        self.artifacts.iter()
    }
    fn public_surfaces(&self) -> impl Iterator<Item = &RecoveryPublicSurface> {
        self.public_surfaces.iter()
    }
    fn artifact_dependencies(&self) -> impl Iterator<Item = &RecoveryArtifactDependency> {
        self.artifact_dependencies.iter()
    }
}
impl GraphRead for RecoveryGraph {
    fn version(&self) -> u32 {
        self.version
    }
    fn public_schema(&self) -> &str {
        &self.public_schema
    }
    fn source_session(&self) -> u64 {
        self.source_session()
    }
    fn lineage(&self) -> u64 {
        self.lineage()
    }
    fn high_water(&self) -> Generation {
        self.high_water()
    }
    fn nodes(&self) -> impl Iterator<Item = &RecoveryNode> {
        self.nodes.values()
    }
    fn artifacts(&self) -> impl Iterator<Item = &RecoveryArtifactClosure> {
        self.artifacts.values()
    }
    fn public_surfaces(&self) -> impl Iterator<Item = &RecoveryPublicSurface> {
        self.public_surfaces.values()
    }
    fn artifact_dependencies(&self) -> impl Iterator<Item = &RecoveryArtifactDependency> {
        self.artifact_dependencies.values()
    }
}

// These serializers borrow rows directly. They neither build historical Vecs
// nor clone row payloads. Each v8 node includes its exact native selection.
macro_rules! row_serializer {
    ($name:ident, $method:ident) => {
        struct $name<'a, T: GraphRead>(&'a T);
        impl<T: GraphRead> Serialize for $name<'_, T> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(None)?;
                for row in self.0.$method() {
                    sequence.serialize_element(row)?;
                }
                sequence.end()
            }
        }
    };
}
row_serializer!(Nodes, nodes);
row_serializer!(Artifacts, artifacts);
row_serializer!(Surfaces, public_surfaces);
row_serializer!(Edges, artifact_dependencies);

pub(super) struct GraphEncoding<'a, T: GraphRead> {
    pub graph: &'a T,
    pub checksum: &'a str,
}
impl<T: GraphRead> Serialize for GraphEncoding<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let graph = self.graph;
        let mut fields = serializer.serialize_struct("RecoveryGraph", 10)?;
        fields.serialize_field("version", &graph.version())?;
        fields.serialize_field("public_schema", graph.public_schema())?;
        fields.serialize_field("source_session", &graph.source_session())?;
        fields.serialize_field("lineage", &graph.lineage())?;
        fields.serialize_field("high_water", &graph.high_water().0)?;
        fields.serialize_field("public_surfaces", &Surfaces(graph))?;
        fields.serialize_field("nodes", &Nodes(graph))?;
        fields.serialize_field("artifacts", &Artifacts(graph))?;
        fields.serialize_field("artifact_dependencies", &Edges(graph))?;
        fields.serialize_field("checksum", self.checksum)?;
        fields.end()
    }
}
impl Serialize for RecoveryGraph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !self.wire_order_matches_maps {
            return Err(serde::ser::Error::custom(
                "raw recovery revision requires a sealed publication candidate",
            ));
        }
        GraphEncoding {
            graph: self,
            checksum: &self.checksum,
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for RecoveryGraph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_wire(RecoveryGraphWire::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl RecoveryGraph {
    pub(crate) fn from_wire(wire: RecoveryGraphWire) -> Result<Self, RecoveryError> {
        // Authenticate the original order and refuse duplicates before moving
        // any row into a keyed map. The checksum is an opaque revision token.
        wire.validate()?;
        let mut graph = Self {
            version: wire.version,
            public_schema: wire.public_schema,
            source_session: wire.source_session,
            lineage: wire.lineage,
            high_water: wire.high_water,
            checksum: wire.checksum,
            wire_order_matches_maps: true,
            nodes: RedBlackTreeMapSync::new_sync(),
            artifacts: RedBlackTreeMapSync::new_sync(),
            public_surfaces: RedBlackTreeMapSync::new_sync(),
            artifact_dependencies: RedBlackTreeMapSync::new_sync(),
        };
        let mut previous = None;
        for row in wire.nodes {
            let key = row.id;
            if previous.as_ref().is_some_and(|old| old >= &key) {
                graph.wire_order_matches_maps = false;
            }
            previous = Some(key.clone());
            graph.nodes.insert_mut(key, row);
        }
        let mut previous = None;
        for mut row in wire.artifacts {
            if let RecoveryArtifactClosure::ValueInterface(reference) = &mut row {
                if reference
                    .requirements
                    .windows(2)
                    .any(|pair| pair[0] > pair[1])
                {
                    // Authenticate the original ordering first, then normalize
                    // this newly owned admission row exactly once. Keep the
                    // raw revision token until a successor is sealed.
                    reference.requirements.sort();
                    graph.wire_order_matches_maps = false;
                }
            }
            let key = row.artifact_id();
            if previous.as_ref().is_some_and(|old| old >= &key) {
                graph.wire_order_matches_maps = false;
            }
            previous = Some(key.clone());
            graph.artifacts.insert_mut(key, row);
        }
        let mut previous = None;
        for row in wire.public_surfaces {
            let key = row.owner.clone();
            if previous.as_ref().is_some_and(|old| old >= &key) {
                graph.wire_order_matches_maps = false;
            }
            previous = Some(key.clone());
            graph.public_surfaces.insert_mut(key, row);
        }
        let mut previous = None;
        for row in wire.artifact_dependencies {
            let key = edge_key(&row);
            if previous.as_ref().is_some_and(|old| old >= &key) {
                graph.wire_order_matches_maps = false;
            }
            previous = Some(key.clone());
            graph.artifact_dependencies.insert_mut(key, row);
        }
        Ok(graph)
    }
    pub(super) fn into_staging_snapshot_with_encoded_bytes(
        self,
    ) -> Result<(Self, u64), RecoveryError> {
        if self.wire_order_matches_maps {
            Ok((self, 0))
        } else {
            self.candidate().seal_with_encoded_bytes()
        }
    }
    pub(crate) fn checksum(&self) -> &str {
        &self.checksum
    }
    pub(crate) fn high_water(&self) -> Generation {
        self.high_water
    }
    pub(crate) fn source_session(&self) -> u64 {
        self.source_session
    }
    pub(crate) fn lineage(&self) -> u64 {
        self.lineage
    }
    pub(crate) fn nodes(&self) -> impl Iterator<Item = &RecoveryNode> {
        self.nodes.values()
    }
    pub(crate) fn artifacts(&self) -> impl Iterator<Item = &RecoveryArtifactClosure> {
        self.artifacts.values()
    }
    pub(crate) fn public_surfaces(&self) -> impl Iterator<Item = &RecoveryPublicSurface> {
        self.public_surfaces.values()
    }
    pub(crate) fn artifact_dependencies(
        &self,
    ) -> impl Iterator<Item = &RecoveryArtifactDependency> {
        self.artifact_dependencies.values()
    }
    pub(crate) fn candidate(&self) -> RecoveryGraphCandidate {
        RecoveryGraphCandidate(self.clone())
    }
    pub(crate) fn node(&self, id: Generation) -> Option<&RecoveryNode> {
        self.nodes.get(&id)
    }
    pub(crate) fn surface(&self, owner: &RecoveryPublicOwner) -> Option<&RecoveryPublicSurface> {
        self.public_surfaces.get(owner)
    }
    #[cfg(test)]
    pub(crate) fn wire_for_test(&self) -> RecoveryGraphWire {
        RecoveryGraphWire {
            version: self.version,
            public_schema: self.public_schema.clone(),
            source_session: self.source_session,
            lineage: self.lineage,
            high_water: self.high_water,
            checksum: self.checksum.clone(),
            nodes: self.nodes().cloned().collect(),
            artifacts: self.artifacts().cloned().collect(),
            public_surfaces: self.public_surfaces().cloned().collect(),
            artifact_dependencies: self.artifact_dependencies().cloned().collect(),
        }
    }
}
fn edge_key(row: &RecoveryArtifactDependency) -> EdgeKey {
    (row.source, row.target, row.dependency.clone())
}
impl RecoveryGraphCandidate {
    pub(crate) fn set_high_water(&mut self, next: Generation) -> Result<(), RecoveryError> {
        if next < self.0.high_water {
            return Err(error("recovery high-water cannot move backwards"));
        }
        self.0.high_water = next;
        Ok(())
    }
    pub(crate) fn insert_node(&mut self, mut row: RecoveryNode) -> Result<(), RecoveryError> {
        if self.0.nodes.contains_key(&row.id) {
            return Err(error("duplicate recovery node"));
        }
        normalize_node(&mut row);
        self.0.nodes.insert_mut(row.id, row);
        Ok(())
    }
    pub(crate) fn insert_artifact(
        &mut self,
        mut row: RecoveryArtifactClosure,
    ) -> Result<(), RecoveryError> {
        normalize_artifact(&mut row);
        let key = row.artifact_id();
        if let Some(existing) = self.0.artifacts.get(&key) {
            if existing != &row {
                return Err(error(
                    "recovery artifact identity has conflicting references",
                ));
            }
        } else {
            self.0.artifacts.insert_mut(key, row);
        }
        Ok(())
    }
    pub(crate) fn insert_interface_edge(
        &mut self,
        row: RecoveryArtifactDependency,
    ) -> Result<(), RecoveryError> {
        if !matches!(row.dependency, ArtifactDependency::Interface) {
            return Err(error(
                "native recovery edges must derive from original certification",
            ));
        }
        let key = edge_key(&row);
        if !self.0.artifact_dependencies.contains_key(&key) {
            self.0.artifact_dependencies.insert_mut(key, row);
        }
        Ok(())
    }
    pub(crate) fn replace_surface(&mut self, mut row: RecoveryPublicSurface) {
        normalize_surface(&mut row);
        self.0.public_surfaces.insert_mut(row.owner.clone(), row);
    }
    pub(crate) fn remove_surface(&mut self, owner: &RecoveryPublicOwner) {
        self.0.public_surfaces.remove_mut(owner);
    }
    pub(crate) fn seal(self) -> Result<RecoveryGraph, RecoveryError> {
        self.seal_with_encoded_bytes().map(|(graph, _)| graph)
    }
    pub(crate) fn seal_with_encoded_bytes(mut self) -> Result<(RecoveryGraph, u64), RecoveryError> {
        validate_shape(&self.0)?;
        let (checksum, encoded_bytes) = checksum_with_encoded_bytes(&self.0)?;
        self.0.checksum = checksum;
        self.0.wire_order_matches_maps = true;
        Ok((self.0, encoded_bytes))
    }
}
