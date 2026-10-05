//! Full semantic inventory interning with a bound on its normalized expansion.
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
    symbols: BTreeMap<SymbolIdentity, (usize, usize)>,
    globals: BTreeMap<GlobalKey, (usize, usize)>,
    symbol_rows: Vec<Value>,
    global_rows: Vec<Value>,
    expanded_bytes: usize,
}

impl InventoryTables {
    pub(super) fn new<'a>(groups: impl IntoIterator<Item = &'a ProjectedGroup>) -> Option<Self> {
        Self::from_views(groups.into_iter().map(GroupView::Projected))
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
            expanded_bytes: 0,
        };
        let mut expanded = 0usize;
        for group in groups {
            group.check()?;
            charge(
                &mut expanded,
                array_size(3).checked_add(uint_size(group.ordinal().into()))?,
            )?;
            charge(&mut expanded, array_size(group.binders().len()))?;
            charge(&mut expanded, array_size(group.globals_len()))?;
            for binder in group.binders() {
                let (_, bytes) = tables.intern_symbol(binder)?;
                charge(&mut expanded, bytes)?;
            }
            for key in group.keys() {
                let key = key?;
                let (_, bytes) = tables.intern_global(key)?;
                charge(&mut expanded, bytes)?;
            }
        }
        Some(tables)
    }

    /// Each call consumes the offer-wide expanded budget, including its array header.
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
        let mut bytes = array_size(groups.len());
        // Charge the complete normalized expansion before allocating index lists.
        for group in groups.clone() {
            group.check()?;
            charge(
                &mut bytes,
                array_size(3).checked_add(uint_size(group.ordinal().into()))?,
            )?;
            charge(&mut bytes, array_size(group.binders().len()))?;
            charge(&mut bytes, array_size(group.globals_len()))?;
            for binder in group.binders() {
                charge(&mut bytes, self.symbols.get(binder)?.1)?;
            }
            for key in group.keys() {
                charge(&mut bytes, self.globals.get(&key?)?.1)?;
            }
        }
        let mut expanded = self.expanded_bytes;
        charge(&mut expanded, bytes)?;
        let mut rows = Vec::with_capacity(groups.len());
        for group in groups {
            let binders = group
                .binders()
                .iter()
                .map(|binder| Some(Value::Integer((self.symbols.get(binder)?.0 as u64).into())))
                .collect::<Option<Vec<_>>>()?;
            let globals = group
                .keys()
                .map(|key| Some(Value::Integer((self.globals.get(&key?)?.0 as u64).into())))
                .collect::<Option<Vec<_>>>()?;
            rows.push(Value::Array(vec![
                Value::Integer(group.ordinal().into()),
                Value::Array(binders),
                Value::Array(globals),
            ]));
        }
        self.expanded_bytes = expanded;
        Some(Value::Array(rows))
    }

    pub(super) fn into_wire_tables(self) -> (Value, Value) {
        (
            Value::Array(self.symbol_rows),
            Value::Array(self.global_rows),
        )
    }

    fn intern_symbol(&mut self, identity: &SymbolIdentity) -> Option<(usize, usize)> {
        if let Some(entry) = self.symbols.get(identity) {
            return Some(*entry);
        }
        if self.symbol_rows.len() == TABLE_LIMIT {
            return None;
        }
        let bytes = identity_size(identity)?;
        let value = identity_value(identity);
        let index = self.symbol_rows.len();
        self.symbol_rows.push(value);
        self.symbols.insert(identity.clone(), (index, bytes));
        Some((index, bytes))
    }

    fn intern_global(&mut self, key: GlobalKey) -> Option<(usize, usize)> {
        if let Some(entry) = self.globals.get(&key) {
            return Some(*entry);
        }
        if self.global_rows.len() == TABLE_LIMIT {
            return None;
        }
        let (symbol, identity_bytes) = self.intern_symbol(&key.identity)?;
        let tail = vec![
            rep_value(key.rep),
            key.signature.as_ref().map_or(Value::Null, signature_value),
            Value::Bool(key.evaluated),
            key.generation
                .map_or(Value::Null, |generation| Value::Integer(generation.into())),
        ];
        let mut bytes = array_size(5).checked_add(identity_bytes)?;
        for value in &tail {
            charge(&mut bytes, encoded_size(value)?)?;
        }
        let index = self.global_rows.len();
        let mut row = Vec::with_capacity(5);
        row.push(Value::Integer((symbol as u64).into()));
        row.extend(tail);
        self.global_rows.push(Value::Array(row));
        self.globals.insert(key, (index, bytes));
        Some((index, bytes))
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
fn charge(total: &mut usize, bytes: usize) -> Option<()> {
    *total = total.checked_add(bytes)?;
    (*total <= MANIFEST_LIMIT).then_some(())
}
fn identity_size(identity: &SymbolIdentity) -> Option<usize> {
    let mut bytes = array_size(5);
    for text in [
        &identity.unit,
        &identity.module,
        &identity.namespace,
        &identity.occurrence,
    ] {
        charge(
            &mut bytes,
            uint_size(text.len() as u64).checked_add(text.len())?,
        )?;
    }
    let parent = match &identity.record_parent {
        None => 1,
        Some(text) => uint_size(text.len() as u64).checked_add(text.len())?,
    };
    charge(&mut bytes, parent)?;
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
        charge(&mut self.0, bytes.len())
            .ok_or_else(|| io::Error::other("candidate expanded inventory exceeds four MiB"))?;
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
        assert_eq!(tables.expanded_bytes, encoded_size(&expanded).unwrap());
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
        assert_eq!(tables.intern_symbol(&identity).unwrap().0, 0);
    }
    #[test]
    fn expanded_inventory_bound_counts_repeated_reference_lists_and_candidate_headers() {
        let groups = vec![group(None, false, None, 0)];
        let mut tables = InventoryTables::new(groups.iter()).unwrap();
        let legacy = Value::Array(groups.iter().map(super::super::group_inventory).collect());
        let bytes = encoded_size(&legacy).unwrap();
        tables.expanded_bytes = MANIFEST_LIMIT - bytes;
        assert!(tables.groups(&groups).is_some());
        assert_eq!(tables.expanded_bytes, MANIFEST_LIMIT);
        assert!(tables.groups(&groups).is_none());
        let repeated = std::iter::repeat(&groups[0]).take(MANIFEST_LIMIT / (bytes - 1) + 1);
        assert!(InventoryTables::new(repeated).is_none());
        let mut maximum = usize::MAX;
        assert!(charge(&mut maximum, 1).is_none());
    }
}
