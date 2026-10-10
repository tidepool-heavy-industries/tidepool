//! Full nominal metadata histories against a list-based logical tuple model.
use proptest::prelude::*;
use tidepool_repr::execution_schema::SymbolIdentity;
use tidepool_repr::serial::{read_metadata, write_metadata, MetaWarnings};
use tidepool_repr::{DataCon, DataConCollision, DataConId, DataConTable};

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicalRow { id: u8, unit: u8, shape: u8 }

fn issued(row: &LogicalRow) -> DataCon {
    DataCon {
        identity: SymbolIdentity { unit: format!("unit-{}", row.unit), module: "Homonymous".into(),
            namespace: "constructor".into(), occurrence: "Ticket".into(), record_parent: None },
        id: DataConId(row.id.into()), name: "Ticket".into(), tag: 1,
        rep_arity: row.shape.into(), field_bangs: vec![], qualified_name: Some("Homonymous.Ticket".into()),
        type_name: "Ticket".into(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal { HostId, NominalId, Shape }

fn model_insert(model: &mut Vec<LogicalRow>, incoming: &LogicalRow) -> Result<(), Refusal> {
    if let Some(old) = model.iter().find(|old| old.id == incoming.id) {
        if old.unit != incoming.unit { return Err(Refusal::HostId); }
        if old.shape != incoming.shape { return Err(Refusal::Shape); }
        return Ok(());
    }
    if model.iter().any(|old| old.unit == incoming.unit) { return Err(Refusal::NominalId); }
    model.push(incoming.clone());
    Ok(())
}

fn observed(result: Result<(), DataConCollision>) -> Result<(), Refusal> {
    result.map_err(|error| match error {
        DataConCollision::Id { .. } => Refusal::HostId,
        DataConCollision::Identity { .. } => Refusal::NominalId,
        DataConCollision::Shape { .. } => Refusal::Shape,
        error => panic!("valid generated constructor refused: {error:?}"),
    })
}

fn assert_model(table: &DataConTable, model: &[LogicalRow]) {
    assert_eq!(table.len(), model.len());
    for row in model {
        let expected = issued(row);
        assert_eq!(table.get(expected.id), Some(&expected));
        assert_eq!(table.get_by_identity(&expected.identity), Some(expected.id));
    }
    for arity in 0..=2 {
        let matches: Vec<_> = model.iter().filter(|row| u32::from(row.shape) == arity).collect();
        match matches.as_slice() {
            [] => assert_eq!(table.get_by_qualified_name_checked("Homonymous.Ticket", arity), Ok(None)),
            [one] => assert_eq!(table.get_by_qualified_name_checked("Homonymous.Ticket", arity), Ok(Some(DataConId(one.id.into())))),
            _ => assert!(table.get_by_qualified_name_checked("Homonymous.Ticket", arity).is_err()),
        }
    }
    assert_eq!(table.get_by_qualified_name("Homonymous.Ticket"),
        if model.len() == 1 { Some(DataConId(model[0].id.into())) } else { None });
}

#[derive(Clone, Debug)]
enum Step { Batch(Vec<LogicalRow>), Recover, Retain, Release }

fn row() -> impl Strategy<Value=LogicalRow> {
    (0u8..4, 0u8..4, 0u8..3).prop_map(|(id,unit,shape)| LogicalRow { id, unit, shape })
}
fn step() -> impl Strategy<Value=Step> {
    prop_oneof![4 => prop::collection::vec(row(), 0..6).prop_map(Step::Batch),
        1 => Just(Step::Recover), 1 => Just(Step::Retain), 1 => Just(Step::Release)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn nominal_histories_preserve_owners_across_merge_and_recovery(history in prop::collection::vec(step(), 0..48)) {
        let mut table = DataConTable::new();
        let mut model = Vec::new();
        let mut retained = Vec::new();
        // This guided same-ID history changes only unit, the original erasure boundary.
        let first = LogicalRow { id: 0, unit: 0, shape: 0 };
        let second = LogicalRow { unit: 1, ..first.clone() };
        model_insert(&mut model, &first).unwrap();
        table.insert_checked(issued(&first)).unwrap();
        assert_eq!(observed(table.insert_checked(issued(&second))), model_insert(&mut model, &second));
        for operation in history {
            match operation {
                Step::Batch(rows) => {
                    let mut expected = Ok(());
                    for row in &rows {
                        if let Err(error) = model_insert(&mut model, row) { expected = Err(error); break; }
                    }
                    assert_eq!(observed(table.extend_checked(rows.iter().map(issued))), expected);
                }
                Step::Recover => {
                    let bytes = write_metadata(&table, &MetaWarnings::default()).unwrap();
                    let recovered = read_metadata(&bytes).unwrap().0;
                    assert_eq!(recovered, table);
                    table = recovered;
                }
                Step::Retain => retained.push((table.clone(), model.clone())),
                Step::Release => { retained.pop(); }
            }
            assert_model(&table, &model);
            for (snapshot, model) in &retained { assert_model(snapshot, model); }
        }
    }

    #[test]
    fn batched_ingestion_matches_sequential_refusals(rows in prop::collection::vec(row(), 0..64)) {
        let mut sequential = DataConTable::new();
        let mut result = Ok(());
        for row in &rows {
            if let Err(error) = sequential.insert_checked(issued(row)) { result = Err(error); break; }
        }
        let mut batch = DataConTable::new();
        assert_eq!(batch.extend_checked(rows.iter().map(issued)), result);
        assert_eq!(batch, sequential);
    }
}
