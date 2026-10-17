//! Shared semantic inventory tables with a bound on total index references.
use super::{
    identity_value, rep_value, signature_value, ProjectedGroup, RuntimeRep, Signature,
    SymbolIdentity, Value, MANIFEST_LIMIT,
};
use std::collections::BTreeMap;
use std::io::{self, Write};

const TABLE_LIMIT: usize = 65536;
const LIST_LIMIT: usize = 65536;
const SIGNATURE_LIMIT: usize = 256;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::module_candidates) struct GlobalKey {
    pub(in crate::module_candidates) identity: SymbolIdentity,
    pub(in crate::module_candidates) rep: RuntimeRep,
    pub(in crate::module_candidates) signature: Option<Signature>,
    pub(in crate::module_candidates) evaluated: bool,
    pub(in crate::module_candidates) generation: Option<u64>,
}

/// Structural codec input; this contains no executable definitions or admission proof.
#[cfg(test)]
pub(in crate::module_candidates) struct StructuralGroup {
    pub(in crate::module_candidates) ordinal: u32,
    pub(in crate::module_candidates) binders: Vec<SymbolIdentity>,
    pub(in crate::module_candidates) globals: Vec<GlobalKey>,
}

#[derive(Clone, Copy)]
enum GroupView<'a> {
    Projected(&'a ProjectedGroup),
    #[cfg(test)]
    Structural(&'a StructuralGroup),
}
impl<'a> GroupView<'a> {
    fn ordinal(self) -> u32 {
        match self {
            Self::Projected(group) => group.original_ordinal(),
            #[cfg(test)]
            Self::Structural(group) => group.ordinal,
        }
    }
    fn binders(self) -> &'a [SymbolIdentity] {
        match self {
            Self::Projected(group) => group.binders(),
            #[cfg(test)]
            Self::Structural(group) => &group.binders,
        }
    }
    fn globals_len(self) -> usize {
        match self {
            Self::Projected(group) => group.globals().len(),
            #[cfg(test)]
            Self::Structural(group) => group.globals.len(),
        }
    }
    fn keys(self) -> impl Iterator<Item = Option<GlobalKey>> + 'a {
        (0..self.globals_len()).map(move |index| match self {
            Self::Projected(group) => global_key(group, &group.globals()[index]),
            #[cfg(test)]
            Self::Structural(group) => {
                let key = &group.globals[index];
                validate_key(key)?;
                Some(key.clone())
            }
        })
    }
    fn check(self) -> Option<()> {
        (self.binders().len() <= LIST_LIMIT && self.globals_len() <= LIST_LIMIT).then_some(())
    }
}

pub(super) struct InventoryTables {
    symbols: BTreeMap<SymbolIdentity, usize>,
    globals: BTreeMap<GlobalKey, usize>,
    symbol_rows: Vec<Value>,
    global_rows: Vec<Value>,
    references_emitted: usize,
}

impl InventoryTables {
    pub(super) fn new<'a>(groups: impl IntoIterator<Item = &'a ProjectedGroup>) -> Option<Self> {
        Self::from_views(groups.into_iter().map(GroupView::Projected)).or_else(|| {
            tracing::warn!(target: "tidepool_toolchain::module_candidates", "candidate inventory construction refused");
            None
        })
    }

    #[cfg(test)]
    pub(in crate::module_candidates) fn structural(groups: &[StructuralGroup]) -> Option<Self> {
        Self::from_views(groups.iter().map(GroupView::Structural))
    }

    fn from_views<'a>(groups: impl IntoIterator<Item = GroupView<'a>>) -> Option<Self> {
        let mut tables = Self {
            symbols: BTreeMap::new(),
            globals: BTreeMap::new(),
            symbol_rows: Vec::new(),
            global_rows: Vec::new(),
            references_emitted: 0,
        };
        let mut references = 0usize;
        for group in groups {
            group.check()?;
            charge_references(&mut references, group.binders().len(), group.globals_len())?;
            for binder in group.binders() {
                tables.intern_symbol(binder)?;
            }
            for key in group.keys() {
                tables.intern_global(key?)?;
            }
        }
        Some(tables)
    }

    /// Each index needs at least one byte in the bounded manifest. Charge the
    /// aggregate reference budget across candidates before allocating lists.
    pub(super) fn groups(&mut self, groups: &[ProjectedGroup]) -> Option<Value> {
        self.encode_groups(groups.iter().map(GroupView::Projected))
    }

    #[cfg(test)]
    pub(in crate::module_candidates) fn structural_groups(
        &mut self,
        groups: &[StructuralGroup],
    ) -> Option<Value> {
        self.encode_groups(groups.iter().map(GroupView::Structural))
    }

    fn encode_groups<'a>(
        &mut self,
        groups: impl Iterator<Item = GroupView<'a>> + Clone + ExactSizeIterator,
    ) -> Option<Value> {
        if groups.len() > LIST_LIMIT {
            return None;
        }
        let mut references = self.references_emitted;
        for group in groups.clone() {
            group.check()?;
            charge_references(&mut references, group.binders().len(), group.globals_len())?;
            for binder in group.binders() {
                self.symbols.get(binder)?;
            }
            for key in group.keys() {
                self.globals.get(&key?)?;
            }
        }
        let mut rows = Vec::with_capacity(groups.len());
        for group in groups {
            let binders = group
                .binders()
                .iter()
                .map(|binder| Some(Value::Integer((*self.symbols.get(binder)? as u64).into())))
                .collect::<Option<Vec<_>>>()?;
            let globals = group
                .keys()
                .map(|key| Some(Value::Integer((*self.globals.get(&key?)? as u64).into())))
                .collect::<Option<Vec<_>>>()?;
            rows.push(Value::Array(vec![
                Value::Integer(group.ordinal().into()),
                Value::Array(binders),
                Value::Array(globals),
            ]));
        }
        self.references_emitted = references;
        Some(Value::Array(rows))
    }

    pub(super) fn into_wire_tables(self) -> (Value, Value) {
        (
            Value::Array(self.symbol_rows),
            Value::Array(self.global_rows),
        )
    }

    fn intern_symbol(&mut self, identity: &SymbolIdentity) -> Option<usize> {
        if let Some(entry) = self.symbols.get(identity) {
            return Some(*entry);
        }
        if self.symbol_rows.len() == TABLE_LIMIT {
            tracing::warn!(target: "tidepool_toolchain::module_candidates", limit = TABLE_LIMIT, "candidate symbol table limit reached");
            return None;
        }
        identity_size(identity)?;
        let value = identity_value(identity);
        let index = self.symbol_rows.len();
        self.symbol_rows.push(value);
        self.symbols.insert(identity.clone(), index);
        Some(index)
    }

    fn intern_global(&mut self, key: GlobalKey) -> Option<usize> {
        if let Some(entry) = self.globals.get(&key) {
            return Some(*entry);
        }
        if self.global_rows.len() == TABLE_LIMIT {
            tracing::warn!(target: "tidepool_toolchain::module_candidates", limit = TABLE_LIMIT, "candidate global table limit reached");
            return None;
        }
        let symbol = self.intern_symbol(&key.identity)?;
        let identity_bytes = identity_size(&key.identity)?;
        let tail = vec![
            rep_value(key.rep),
            key.signature.as_ref().map_or(Value::Null, signature_value),
            Value::Bool(key.evaluated),
            key.generation
                .map_or(Value::Null, |generation| Value::Integer(generation.into())),
        ];
        let mut bytes = array_size(5).checked_add(identity_bytes)?;
        for value in &tail {
            charge_bytes(&mut bytes, encoded_size(value)?)?;
        }
        let index = self.global_rows.len();
        let mut row = Vec::with_capacity(5);
        row.push(Value::Integer((symbol as u64).into()));
        row.extend(tail);
        self.global_rows.push(Value::Array(row));
        self.globals.insert(key, index);
        Some(index)
    }
}

fn global_key(
    group: &ProjectedGroup,
    global: &tidepool_repr::execution_schema::GlobalDecl,
) -> Option<GlobalKey> {
    identity_size(&global.identity)?;
    let signature = match global.entry_signature {
        None => None,
        Some(id) => {
            let definitions = group.definitions();
            let signature = definitions.signatures().get(id.0 as usize)?;
            if signature.arguments.len() > SIGNATURE_LIMIT
                || matches!(&signature.results, super::ResultContract::Returns(results) if results.len() > SIGNATURE_LIMIT)
            {
                return None;
            }
            Some(signature.clone())
        }
    };
    Some(GlobalKey {
        identity: global.identity.clone(),
        rep: global.rep,
        signature,
        evaluated: global.required_evaluated,
        generation: global.required_generation,
    })
}

#[cfg(test)]
fn validate_key(key: &GlobalKey) -> Option<()> {
    identity_size(&key.identity)?;
    if let Some(signature) = &key.signature {
        if signature.arguments.len() > SIGNATURE_LIMIT
            || matches!(&signature.results, super::ResultContract::Returns(results) if results.len() > SIGNATURE_LIMIT)
        {
            return None;
        }
    }
    Some(())
}
/// Reference count is an allocation safeguard derived from the actual encoded
/// manifest bound: every binder/global index occupies at least one CBOR byte.
fn charge_references(total: &mut usize, binders: usize, globals: usize) -> Option<()> {
    let observed = total.checked_add(binders)?.checked_add(globals)?;
    if observed > MANIFEST_LIMIT {
        tracing::warn!(target: "tidepool_toolchain::module_candidates", observed, limit = MANIFEST_LIMIT, "candidate inventory reference limit exceeded");
        return None;
    }
    *total = observed;
    Some(())
}

#[track_caller]
fn charge_bytes(total: &mut usize, bytes: usize) -> Option<()> {
    *total = total.checked_add(bytes)?;
    if *total > MANIFEST_LIMIT {
        tracing::warn!(target: "tidepool_toolchain::module_candidates", observed = *total, limit = MANIFEST_LIMIT, location = %std::panic::Location::caller(), "candidate unique inventory entry byte limit exceeded");
        return None;
    }
    Some(())
}
fn identity_size(identity: &SymbolIdentity) -> Option<usize> {
    let mut bytes = array_size(5);
    for text in [
        &identity.unit,
        &identity.module,
        &identity.namespace,
        &identity.occurrence,
    ] {
        charge_bytes(
            &mut bytes,
            uint_size(text.len() as u64).checked_add(text.len())?,
        )?;
    }
    let parent = match &identity.record_parent {
        None => 1,
        Some(text) => uint_size(text.len() as u64).checked_add(text.len())?,
    };
    charge_bytes(&mut bytes, parent)?;
    Some(bytes)
}
fn array_size(items: usize) -> usize {
    uint_size(items as u64)
}
fn uint_size(value: u64) -> usize {
    match value {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=4294967295 => 5,
        _ => 9,
    }
}
struct SizeWriter(usize);
impl Write for SizeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        charge_bytes(&mut self.0, bytes.len())
            .ok_or_else(|| io::Error::other("candidate inventory entry exceeds four MiB"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded_size(value: &Value) -> Option<usize> {
    let mut writer = SizeWriter(0);
    ciborium::ser::into_writer(value, &mut writer).ok()?;
    Some(writer.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{testing, GlobalDecl, ResultContract, SignatureId};

    fn group(
        signature: Option<Signature>,
        evaluated: bool,
        generation: Option<u64>,
        ordinal: u32,
    ) -> ProjectedGroup {
        let mut wire = testing::wire_program();
        let entry_signature = signature.map(|signature| {
            wire.signatures.push(signature);
            SignatureId((wire.signatures.len() - 1) as u32)
        });
        wire.globals.push(GlobalDecl {
            identity: testing::identity("External", "same"),
            rep: RuntimeRep::LiftedRef,
            entry_signature,
            required_evaluated: evaluated,
            required_generation: generation,
        });
        testing::projected_group(wire, ordinal).unwrap()
    }
    fn expand(tables: &InventoryTables, value: &Value) -> Value {
        Value::Array(
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|group| {
                    let fields = group.as_array().unwrap();
                    let symbols = fields[1]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|index| {
                            tables.symbol_rows
                                [usize::try_from(index.as_integer().unwrap()).unwrap()]
                            .clone()
                        })
                        .collect();
                    let globals = fields[2]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|index| {
                            let mut row = tables.global_rows
                                [usize::try_from(index.as_integer().unwrap()).unwrap()]
                            .as_array()
                            .unwrap()
                            .clone();
                            row[0] = tables.symbol_rows
                                [usize::try_from(row[0].as_integer().unwrap()).unwrap()]
                            .clone();
                            Value::Array(row)
                        })
                        .collect();
                    Value::Array(vec![
                        fields[0].clone(),
                        Value::Array(symbols),
                        Value::Array(globals),
                    ])
                })
                .collect(),
        )
    }
    #[test]
    fn interned_inventory_reconstructs_full_groups_in_order() {
        let groups = vec![group(None, false, None, 7), group(None, false, None, 2)];
        let mut tables = InventoryTables::new(groups.iter()).unwrap();
        assert_eq!(tables.symbol_rows.len(), 2);
        assert_eq!(tables.global_rows.len(), 1);
        let indexed = tables.groups(&groups).unwrap();
        let expanded = Value::Array(groups.iter().map(super::super::group_inventory).collect());
        assert_eq!(expand(&tables, &indexed), expanded);
        assert_eq!(
            tables.references_emitted,
            groups
                .iter()
                .map(|group| group.binders().len() + group.globals().len())
                .sum::<usize>()
        );
        let other = InventoryTables::new(groups.iter()).unwrap();
        assert_eq!(tables.into_wire_tables(), other.into_wire_tables());
    }
    #[test]
    fn global_interning_keeps_complete_signature_and_requirements() {
        let signature = |results| {
            Some(Signature {
                arguments: vec![RuntimeRep::LiftedRef],
                results,
            })
        };
        let groups = vec![
            group(None, false, None, 0),
            group(signature(ResultContract::Returns(vec![])), false, None, 1),
            group(signature(ResultContract::NoSuccess), false, None, 2),
            group(signature(ResultContract::CallerResult), false, None, 3),
            group(signature(ResultContract::Returns(vec![])), true, None, 4),
            group(
                signature(ResultContract::Returns(vec![])),
                false,
                Some(9),
                5,
            ),
        ];
        let mut tables = InventoryTables::new(groups.iter()).unwrap();
        assert_eq!(tables.global_rows.len(), groups.len());
        let indexed = tables.groups(&groups).unwrap();
        assert_eq!(
            expand(&tables, &indexed),
            Value::Array(groups.iter().map(super::super::group_inventory).collect())
        );
    }
    #[test]
    fn symbol_interning_keeps_record_parent_and_owner() {
        let mut tables = InventoryTables::new(std::iter::empty()).unwrap();
        let identity = testing::identity("Module", "same");
        let mut record = identity.clone();
        record.record_parent = Some("Record".into());
        let mut unit = identity.clone();
        unit.unit = "other-unit".into();
        let mut namespace = identity.clone();
        namespace.namespace = "other-namespace".into();
        for value in [&identity, &record, &unit, &namespace] {
            tables.intern_symbol(value).unwrap();
        }
        assert_eq!(tables.symbol_rows.len(), 4);
        assert_eq!(tables.intern_symbol(&identity).unwrap(), 0);
    }
    #[test]
    fn reference_budget_counts_all_lists_at_limit_and_checks_overflow() {
        let groups = vec![group(None, false, None, 0)];
        let references = groups[0].binders().len() + groups[0].globals().len();
        let mut tables = InventoryTables::new(groups.iter()).unwrap();
        tables.references_emitted = MANIFEST_LIMIT - references;
        assert!(tables.groups(&groups).is_some());
        assert_eq!(tables.references_emitted, MANIFEST_LIMIT);
        assert!(tables.groups(&groups).is_none());
        assert_eq!(tables.references_emitted, MANIFEST_LIMIT);
        let repeated = std::iter::repeat(&groups[0]).take(MANIFEST_LIMIT / references + 1);
        assert!(InventoryTables::new(repeated).is_none());
        let mut maximum = usize::MAX;
        assert!(charge_references(&mut maximum, 1, 0).is_none());
        assert!(charge_references(&mut 0, usize::MAX, 1).is_none());
        let mut exact = 0;
        assert!(charge_references(&mut exact, MANIFEST_LIMIT / 2, MANIFEST_LIMIT / 2).is_some());
        assert_eq!(exact, MANIFEST_LIMIT);
        assert!(charge_references(&mut exact, 0, 1).is_none());
    }

    #[test]
    fn compact_shared_inventory_does_not_charge_repeated_semantic_bytes() {
        let mut identity = testing::identity("Fixture", "entry");
        identity.occurrence = "x".repeat(2048);
        let key = GlobalKey {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            signature: Some(Signature {
                arguments: vec![RuntimeRep::LiftedRef; SIGNATURE_LIMIT],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64); SIGNATURE_LIMIT]),
            }),
            evaluated: false,
            generation: None,
        };
        let groups = vec![StructuralGroup {
            ordinal: 0,
            binders: vec![identity.clone(); 1536],
            globals: vec![key; 1536],
        }];
        // Repeated identity bytes alone exceeded the obsolete semantic-expansion
        // limit; the wire format stores one identity and bounded index lists.
        assert!(identity_size(&identity).unwrap() * 1536 * 2 > MANIFEST_LIMIT);
        let mut tables = InventoryTables::structural(&groups).unwrap();
        assert_eq!(tables.symbol_rows.len(), 1);
        assert_eq!(tables.global_rows.len(), 1);
        let indexed = tables.structural_groups(&groups).unwrap();
        assert_eq!(tables.references_emitted, 3072);
        let (symbols, globals) = tables.into_wire_tables();
        assert!(
            encoded_size(&Value::Array(vec![symbols, globals, indexed])).unwrap() < MANIFEST_LIMIT
        );
    }

    #[test]
    fn unique_inventory_entry_byte_bound_is_preserved() {
        let mut identity = testing::identity("Fixture", "entry");
        identity.occurrence = "x".repeat(MANIFEST_LIMIT);
        let mut tables = InventoryTables::structural(&[]).unwrap();
        assert!(tables.intern_symbol(&identity).is_none());
        assert!(tables.symbol_rows.is_empty());
        let mut maximum = usize::MAX;
        assert!(charge_bytes(&mut maximum, 1).is_none());
    }
}
