//! Immutable declaration evidence shared by installations of one native image.

use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_repr::execution_schema::{
    ConstructorReply, DefinitionsView, Group, HeapRhs, JsonLayout, Signature, SiteRow,
    SymbolIdentity, ValueId,
};
use tidepool_repr::type_graph::TypeGraph;
use tidepool_repr::DataConId;

/// Declaration metadata from validated definitions. This owns no machine
/// handles, CAF state, authority decisions, or runtime entry selection.
pub struct DefinitionFacts {
    pub tops: BTreeMap<ValueId, (SymbolIdentity, Option<Signature>)>,
    pub sites: Vec<SiteRow>,
    pub types: Arc<TypeGraph>,
    /// Exact bridge request constructor IDs paired with reply evidence.
    pub constructor_replies: Vec<(DataConId, ConstructorReply)>,
    /// Identity, bridge ID and family, indexed by the local `ConstructorId`.
    pub constructors: Arc<[(SymbolIdentity, DataConId, SymbolIdentity)]>,
    pub json_layout: Option<JsonLayout<DataConId>>,
    /// Constructor row indexes by module and occurrence. Preserve every row
    /// so runtime authority checks can reject conflicting declarations.
    pub by_identity: BTreeMap<String, BTreeMap<String, Vec<usize>>>,
}

impl DefinitionFacts {
    /// Extract immutable evidence for native compilation or an install's
    /// preflight. Live ownership must still be checked by the installer.
    pub fn new(prepared: DefinitionsView<'_>) -> Self {
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
        let constructors: Vec<(SymbolIdentity, DataConId, SymbolIdentity)> = prepared
            .constructors()
            .iter()
            .map(|declaration| {
                (
                    declaration.identity.clone(),
                    declaration.host_id,
                    declaration.family.clone(),
                )
            })
            .collect();
        let mut by_identity = BTreeMap::<_, BTreeMap<_, Vec<usize>>>::new();
        for (index, (identity, _, _)) in constructors.iter().enumerate() {
            by_identity
                .entry(identity.module.clone())
                .or_default()
                .entry(identity.occurrence.clone())
                .or_default()
                .push(index);
        }
        let json_layout = prepared
            .json_layout()
            .map(|layout| (*layout).map(|constructor| constructors[constructor.0 as usize].1));
        let sites = prepared.sites().to_vec();
        // Validation guarantees exact constructor and type-node references.
        let constructor_replies = prepared
            .constructor_replies()
            .iter()
            .map(|(constructor, reply)| (constructors[constructor.0 as usize].1, *reply))
            .collect();
        Self {
            tops,
            sites,
            types: Arc::clone(prepared.types()),
            constructor_replies,
            constructors: constructors.into(),
            json_layout,
            by_identity,
        }
    }
}
