//! Immutable declaration evidence shared by installations of one native image.

use std::collections::BTreeMap;
use tidepool_repr::execution_schema::{
    DefinitionsView, Group, HeapRhs, JsonLayout, Signature, SiteRow, SymbolIdentity, TypeNode,
    ValueId,
};
use tidepool_repr::DataConId;

/// Declaration metadata from validated definitions. This owns no machine
/// handles, CAF state, authority decisions, or runtime entry selection.
pub struct DefinitionFacts {
    pub tops: BTreeMap<ValueId, (SymbolIdentity, Option<Signature>)>,
    pub sites: Vec<SiteRow>,
    pub types: Vec<TypeNode>,
    /// Bridge request constructor IDs paired with indexes into `sites`.
    pub verb_sites: Vec<(DataConId, usize)>,
    /// Identity, bridge ID and family, indexed by the local `ConstructorId`.
    pub constructors: Vec<(SymbolIdentity, DataConId, SymbolIdentity)>,
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
        // Validation guarantees every entry names a declared constructor and
        // an admitted row.
        let verb_sites = prepared
            .verb_sites()
            .iter()
            .filter_map(|(constructor, site)| {
                let (_, host_id, _) = constructors.get(constructor.0 as usize)?;
                let row = sites.iter().position(|row| row.site == *site)?;
                Some((*host_id, row))
            })
            .collect();
        Self {
            tops,
            sites,
            types: prepared.types().to_vec(),
            verb_sites,
            constructors,
            json_layout,
            by_identity,
        }
    }
}
