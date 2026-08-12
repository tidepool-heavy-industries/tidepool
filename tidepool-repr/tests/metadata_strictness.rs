//! Strictness pins for `read_metadata`'s field-level and warnings-level
//! decoding. Each malformed-shape test asserts the SPECIFIC `ReadError`
//! variant a conforming reader must produce — not merely `is_err()`.

use ciborium::value::Value as Cbor;
use tidepool_repr::serial::{
    read_metadata, write_metadata, HEADER_MAGIC, VERSION_MAJOR, VERSION_MINOR,
};
use tidepool_repr::serial::{MetaWarnings, ReadError};

fn header() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&HEADER_MAGIC);
    bytes.extend_from_slice(&VERSION_MAJOR.to_be_bytes());
    bytes.extend_from_slice(&VERSION_MINOR.to_be_bytes());
    bytes
}

/// A writer-conforming 9-element metadata entry: id, name, tag 1, arity 0,
/// no bangs, and the given (possibly corrupted) qualified-name/field-labels
/// elements — the two fields these tests target. Field types (9th element)
/// is always the empty array here — not one of this file's targets.
fn entry(dcid: u64, name: &str, qualified_name: Cbor, field_labels: Cbor) -> Cbor {
    Cbor::Array(vec![
        Cbor::Integer(dcid.into()),
        Cbor::Text(name.to_string()),
        Cbor::Integer(1u64.into()),
        Cbor::Integer(0u64.into()),
        Cbor::Array(vec![]),
        qualified_name,
        field_labels,
        Cbor::Text(name.to_string()),
        Cbor::Array(vec![]),
    ])
}

fn plain_entry(dcid: u64, name: &str) -> Cbor {
    entry(dcid, name, Cbor::Text(String::new()), Cbor::Array(vec![]))
}

fn meta_bytes(entries: Vec<Cbor>, warnings_map: Vec<(Cbor, Cbor)>) -> Vec<u8> {
    let root = Cbor::Array(vec![Cbor::Array(entries), Cbor::Map(warnings_map)]);
    let mut bytes = header();
    ciborium::ser::into_writer(&root, &mut bytes).unwrap();
    bytes
}

fn has_io_false() -> (Cbor, Cbor) {
    (Cbor::Text("has_io".to_string()), Cbor::Bool(false))
}

fn assert_malformed(
    result: Result<(tidepool_repr::DataConTable, MetaWarnings), ReadError>,
    expected_field: &str,
) {
    match result {
        Err(ReadError::MalformedMetadataField { field, .. }) => {
            assert_eq!(field, expected_field, "wrong field named in error")
        }
        other => panic!("expected MalformedMetadataField({expected_field}), got {other:?}"),
    }
}

// ---- entry-level: qualified name (6th element) ----

#[test]
fn non_text_qualified_name_is_malformed() {
    let e = entry(
        1,
        "Foo",
        Cbor::Integer(7.into()), // corrupt: not text
        Cbor::Array(vec![]),
    );
    let bytes = meta_bytes(vec![e], vec![has_io_false()]);
    assert_malformed(read_metadata(&bytes), "qualified_name");
}

// ---- entry-level: field labels (7th element) ----

#[test]
fn non_array_field_labels_is_malformed() {
    let e = entry(
        1,
        "Foo",
        Cbor::Text(String::new()),
        Cbor::Text("not-an-array".to_string()), // corrupt: not array
    );
    let bytes = meta_bytes(vec![e], vec![has_io_false()]);
    assert_malformed(read_metadata(&bytes), "field_labels");
}

#[test]
fn non_text_field_label_element_is_malformed() {
    let e = entry(
        1,
        "Foo",
        Cbor::Text(String::new()),
        Cbor::Array(vec![Cbor::Integer(3.into())]), // corrupt: element not text
    );
    let bytes = meta_bytes(vec![e], vec![has_io_false()]);
    assert_malformed(read_metadata(&bytes), "field_labels");
}

// ---- entry-level: field types (9th element) ----

/// A 9-element entry whose 9th (field-types) element is corrupted; the first
/// 8 elements are the plain-entry shape.
fn entry_with_field_types(dcid: u64, name: &str, field_types: Cbor) -> Cbor {
    Cbor::Array(vec![
        Cbor::Integer(dcid.into()),
        Cbor::Text(name.to_string()),
        Cbor::Integer(1u64.into()),
        Cbor::Integer(0u64.into()),
        Cbor::Array(vec![]),
        Cbor::Text(String::new()),
        Cbor::Array(vec![]),
        Cbor::Text(name.to_string()),
        field_types,
    ])
}

#[test]
fn non_array_field_types_is_malformed() {
    let e = entry_with_field_types(1, "Foo", Cbor::Text("not-an-array".to_string()));
    let bytes = meta_bytes(vec![e], vec![has_io_false()]);
    assert_malformed(read_metadata(&bytes), "field_types");
}

#[test]
fn non_text_field_type_element_is_malformed() {
    let e = entry_with_field_types(1, "Foo", Cbor::Array(vec![Cbor::Integer(3.into())]));
    let bytes = meta_bytes(vec![e], vec![has_io_false()]);
    assert_malformed(read_metadata(&bytes), "field_types");
}

#[test]
fn eight_element_entry_is_rejected() {
    // The pre-3.0 shape (8 elements, no field-types) must be a hard reject —
    // no tolerated short form.
    let e = Cbor::Array(vec![
        Cbor::Integer(1u64.into()),
        Cbor::Text("Foo".to_string()),
        Cbor::Integer(1u64.into()),
        Cbor::Integer(0u64.into()),
        Cbor::Array(vec![]),
        Cbor::Text(String::new()),
        Cbor::Array(vec![]),
        Cbor::Text("Foo".to_string()),
    ]);
    let bytes = meta_bytes(vec![e], vec![has_io_false()]);
    match read_metadata(&bytes) {
        Err(ReadError::InvalidStructure(_)) => {}
        other => panic!("expected InvalidStructure for an 8-element entry, got {other:?}"),
    }
}

// ---- warnings-level: has_io ----

#[test]
fn malformed_has_io_is_malformed() {
    let bytes = meta_bytes(
        vec![plain_entry(1, "Foo")],
        vec![(
            Cbor::Text("has_io".to_string()),
            Cbor::Text("nope".to_string()),
        )],
    );
    assert_malformed(read_metadata(&bytes), "has_io");
}

// ---- warnings-level: captured_type ----

#[test]
fn malformed_captured_type_is_malformed() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (
                Cbor::Text("captured_type".to_string()),
                Cbor::Integer(1.into()),
            ),
        ],
    );
    assert_malformed(read_metadata(&bytes), "captured_type");
}

// ---- warnings-level: warnings array ----

#[test]
fn non_array_warnings_is_malformed() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (
                Cbor::Text("warnings".to_string()),
                Cbor::Text("not-an-array".to_string()),
            ),
        ],
    );
    assert_malformed(read_metadata(&bytes), "warnings");
}

#[test]
fn non_text_warnings_item_is_malformed() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (
                Cbor::Text("warnings".to_string()),
                Cbor::Array(vec![Cbor::Integer(1.into())]),
            ),
        ],
    );
    assert_malformed(read_metadata(&bytes), "warnings");
}

// ---- warnings-level: var_names, each malformed sub-shape ----

#[test]
fn non_array_var_names_is_malformed() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (
                Cbor::Text("var_names".to_string()),
                Cbor::Text("not-an-array".to_string()),
            ),
        ],
    );
    assert_malformed(read_metadata(&bytes), "var_names");
}

#[test]
fn non_array_var_names_item_is_malformed() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (
                Cbor::Text("var_names".to_string()),
                Cbor::Array(vec![Cbor::Integer(1.into())]), // item not [id, name]
            ),
        ],
    );
    assert_malformed(read_metadata(&bytes), "var_names");
}

#[test]
fn var_names_item_not_id_name_pair_is_malformed() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (
                Cbor::Text("var_names".to_string()),
                Cbor::Array(vec![Cbor::Array(vec![Cbor::Integer(1.into())])]), // only 1 element
            ),
        ],
    );
    assert_malformed(read_metadata(&bytes), "var_names");
}

#[test]
fn var_names_id_exceeding_u64_is_malformed() {
    // ciborium represents CBOR integers with more range than u64 (negative
    // included); an id that does not fit u64 must be rejected, not wrapped.
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (
                Cbor::Text("var_names".to_string()),
                Cbor::Array(vec![Cbor::Array(vec![
                    Cbor::Integer((-1i64).into()),
                    Cbor::Text("neg".to_string()),
                ])]),
            ),
        ],
    );
    assert_malformed(read_metadata(&bytes), "var_names");
}

// ---- warnings-level: map keys ----

#[test]
fn non_text_map_key_is_malformed() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (Cbor::Integer(1.into()), Cbor::Bool(true)), // corrupt: non-text key
        ],
    );
    assert_malformed(read_metadata(&bytes), "warnings_map");
}

#[test]
fn duplicate_key_is_rejected() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (Cbor::Text("has_io".to_string()), Cbor::Bool(true)),
        ],
    );
    match read_metadata(&bytes) {
        Err(ReadError::DuplicateMetadataKey(k)) => assert_eq!(k, "has_io"),
        other => panic!("expected DuplicateMetadataKey, got {other:?}"),
    }
}

#[test]
fn unknown_key_is_rejected() {
    let bytes = meta_bytes(
        vec![],
        vec![
            has_io_false(),
            (Cbor::Text("totally_unknown".to_string()), Cbor::Bool(true)),
        ],
    );
    match read_metadata(&bytes) {
        Err(ReadError::UnknownMetadataKey(k)) => assert_eq!(k, "totally_unknown"),
        other => panic!("expected UnknownMetadataKey, got {other:?}"),
    }
}

// ---- positive: a writer-conforming payload round-trips unchanged ----

#[test]
fn writer_conforming_payload_round_trips_through_strict_reader() {
    use tidepool_repr::datacon::DataCon;
    use tidepool_repr::datacon_table::DataConTable;
    use tidepool_repr::types::DataConId;

    let mut table = DataConTable::new();
    // No qualified name (writer emits "" -> reader None) and no field labels
    // (writer emits [] -> reader empty Vec) — the exact placeholder shapes
    // strictness must still accept.
    table.insert(DataCon {
        id: DataConId(1),
        name: "Nothing".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: None,
        type_name: "Maybe".to_string(),
    });
    // With a qualified name, field labels, and field types.
    table.insert(DataCon {
        id: DataConId(2),
        name: "Hit".to_string(),
        tag: 1,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: Some("Tidepool.Records.Hit".to_string()),
        type_name: "Hit".to_string(),
    });
    table.set_field_labels(DataConId(2), vec!["path".to_string(), "line".to_string()]);
    table.set_field_types(DataConId(2), vec!["Text".to_string(), "Int".to_string()]);

    let warnings = MetaWarnings {
        has_io: true,
        var_names: vec![(0xfe00_0000_0000_0001_u64, "foo".to_string())],
        captured_type: Some("[Int]".to_string()),
        warnings: vec!["Wincomplete-patterns".to_string()],
        poisoned: vec![(1u64, "Dep.helper".to_string())],
    };

    let bytes = write_metadata(&table, &warnings).expect("write_metadata failed");
    let (recovered_table, recovered_warnings) =
        read_metadata(&bytes).expect("writer-conforming payload must round-trip");

    assert_eq!(table, recovered_table);
    assert_eq!(recovered_warnings.has_io, warnings.has_io);
    assert_eq!(recovered_warnings.var_names, warnings.var_names);
    assert_eq!(recovered_warnings.captured_type, warnings.captured_type);
    assert_eq!(recovered_warnings.warnings, warnings.warnings);
    assert_eq!(recovered_warnings.poisoned, warnings.poisoned);
}
