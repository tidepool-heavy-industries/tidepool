//! Pure decoding and correlation of a worker world-query response.
//! The caller must authenticate module receipts against its captured source owner.

use crate::inspection::{
    self, IdentifierInfo, IdentifierNamespace, IdentifierRef, NameNamespace, NameQuery, NameScope,
    QueryError, StructuredReply, TypeInfo,
};
use ciborium::value::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldModuleOrigin {
    Home {
        root: u32,
        path: String,
        source_sha256: [u8; 32],
    },
    Package {
        profile: [u8; 32],
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldModuleReceipt {
    pub unit: String,
    pub module: String,
    pub origin: WorldModuleOrigin,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedModuleAbsence {
    pub module: String,
    pub source_domain: [u8; 32],
    pub package_profile: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldQueryRejection {
    Unsupported,
    Unavailable,
    Corrupt,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldModuleResolution {
    Resolved(WorldModuleReceipt),
    Absent(ClosedModuleAbsence),
    Rejected(WorldQueryRejection),
}

/// A resolved module can refuse a name without claiming module absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameRefusal {
    Unknown(NameQuery),
    Ambiguous(NameQuery, Vec<IdentifierRef>),
    Unsupported(String),
}
impl From<NameRefusal> for QueryError {
    fn from(value: NameRefusal) -> Self {
        match value {
            NameRefusal::Unknown(query) => Self::Unknown(query),
            NameRefusal::Ambiguous(query, candidates) => Self::Ambiguous(query, candidates),
            NameRefusal::Unsupported(detail) => Self::Unsupported(detail),
        }
    }
}

/// Decoded data, not an authenticated compiler/runtime capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldAnswer {
    Info {
        requested: WorldModuleReceipt,
        defining: WorldModuleReceipt,
        info: Box<IdentifierInfo>,
    },
    Type {
        requested: WorldModuleReceipt,
        defining: WorldModuleReceipt,
        info: TypeInfo,
    },
    NameRefused {
        requested: WorldModuleReceipt,
        refusal: NameRefusal,
    },
    Absent(ClosedModuleAbsence),
    Rejected(WorldQueryRejection),
}
impl WorldAnswer {
    pub fn resolution(&self) -> WorldModuleResolution {
        match self {
            Self::Info { requested, .. }
            | Self::Type { requested, .. }
            | Self::NameRefused { requested, .. } => {
                WorldModuleResolution::Resolved(requested.clone())
            }
            Self::Absent(value) => WorldModuleResolution::Absent(value.clone()),
            Self::Rejected(value) => WorldModuleResolution::Rejected(*value),
        }
    }
    pub fn defining_module(&self) -> Option<&WorldModuleReceipt> {
        match self {
            Self::Info { defining, .. } | Self::Type { defining, .. } => Some(defining),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldQueryKind {
    Info,
    Type,
}

/// Expected request data; supplying it does not authenticate a world.
pub struct WorldQueryExpectation<'a> {
    pub world: &'a [u8; 32],
    pub kind: WorldQueryKind,
    pub module: &'a str,
    pub namespace: Option<IdentifierNamespace>,
    pub name: &'a str,
    pub generation: u64,
    pub fingerprint: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum WorldAnswerDecodeError {
    #[error("malformed world answer: {0}")]
    Malformed(String),
    #[error("world answer differs from its request: {0}")]
    ProvenanceMismatch(&'static str),
}
fn malformed(detail: impl Into<String>) -> WorldAnswerDecodeError {
    WorldAnswerDecodeError::Malformed(detail.into())
}
fn mismatch(detail: &'static str) -> WorldAnswerDecodeError {
    WorldAnswerDecodeError::ProvenanceMismatch(detail)
}
fn array(value: &Value, size: usize) -> Result<&[Value], WorldAnswerDecodeError> {
    match value {
        Value::Array(values) if values.len() == size => Ok(values),
        _ => Err(malformed(format!("expected {size} fields"))),
    }
}
fn text(value: &Value) -> Result<&str, WorldAnswerDecodeError> {
    value.as_text().ok_or_else(|| malformed("expected text"))
}
fn number(value: &Value) -> Result<u64, WorldAnswerDecodeError> {
    match value {
        Value::Integer(value) => u64::try_from(*value).map_err(|_| malformed("expected u64")),
        _ => Err(malformed("expected integer")),
    }
}
fn blob(value: &Value) -> Result<&[u8], WorldAnswerDecodeError> {
    value
        .as_bytes()
        .map(Vec::as_slice)
        .ok_or_else(|| malformed("expected bytes"))
}
fn hash(value: &Value) -> Result<[u8; 32], WorldAnswerDecodeError> {
    blob(value)?
        .try_into()
        .map_err(|_| malformed("expected 32 byte hash"))
}
fn receipt(value: &Value) -> Result<WorldModuleReceipt, WorldAnswerDecodeError> {
    let fields = array(value, 4)?;
    if text(&fields[0])? != "resolved" {
        return Err(malformed("expected resolved module receipt"));
    }
    let unit = text(&fields[1])?.to_owned();
    let module = text(&fields[2])?.to_owned();
    if unit.is_empty() || module.is_empty() || unit.contains('\0') || module.contains('\0') {
        return Err(mismatch("empty or invalid module identity"));
    }
    let origin = match &fields[3] {
        Value::Array(origin) if origin.len() == 2 && text(&origin[0])? == "package" => {
            WorldModuleOrigin::Package {
                profile: hash(&origin[1])?,
            }
        }
        Value::Array(origin) if origin.len() == 4 && text(&origin[0])? == "home" => {
            WorldModuleOrigin::Home {
                root: u32::try_from(number(&origin[1])?)
                    .map_err(|_| malformed("home root exceeds u32"))?,
                path: text(&origin[2])?.to_owned(),
                source_sha256: hash(&origin[3])?,
            }
        }
        _ => return Err(malformed("invalid module origin")),
    };
    Ok(WorldModuleReceipt {
        unit,
        module,
        origin,
    })
}
fn namespace_tag(value: Option<IdentifierNamespace>) -> u64 {
    match value {
        None => 0,
        Some(IdentifierNamespace::Value) => 1,
        Some(IdentifierNamespace::Type) => 2,
        Some(IdentifierNamespace::Constructor) => 3,
        Some(IdentifierNamespace::Field) => 4,
    }
}
fn check_query(
    query: &NameQuery,
    expected: &WorldQueryExpectation<'_>,
) -> Result<(), WorldAnswerDecodeError> {
    let namespace = match query.namespace {
        NameNamespace::Any => None,
        NameNamespace::Value => Some(IdentifierNamespace::Value),
        NameNamespace::Type => Some(IdentifierNamespace::Type),
        NameNamespace::Constructor => Some(IdentifierNamespace::Constructor),
    };
    if query.scope != NameScope::PublicModule(expected.module.into())
        || query.name != expected.name
        || namespace != expected.namespace
    {
        return Err(mismatch("structured refusal query"));
    }
    Ok(())
}

/// Decode the outer world envelope and its single structured result once.
/// Source-domain/profile authentication remains with the toolchain world issuer.
pub fn decode_world_answer(
    bytes: &[u8],
    expected: &WorldQueryExpectation<'_>,
) -> Result<WorldAnswer, WorldAnswerDecodeError> {
    if bytes.get(..8) != Some(b"TPWQ0002") || bytes.len() < 12 {
        return Err(malformed("world header"));
    }
    let size = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|_| malformed("world length"))?,
    ) as usize;
    if size != bytes.len() - 12 {
        return Err(malformed("world length mismatch"));
    }
    let mut reader = std::io::Cursor::new(&bytes[12..]);
    let value: Value =
        ciborium::de::from_reader(&mut reader).map_err(|error| malformed(error.to_string()))?;
    if reader.position() != size as u64 {
        return Err(malformed("trailing world CBOR"));
    }
    let fields = array(&value, 8)?;
    let echo = array(&fields[4], 4)?;
    if number(&fields[0])? != 1
        || hash(&fields[1])? != *expected.world
        || number(&fields[2])?
            != if expected.kind == WorldQueryKind::Info {
                0
            } else {
                1
            }
        || text(&fields[3])? != expected.module
        || number(&echo[0])? != namespace_tag(expected.namespace)
        || text(&echo[1])? != expected.name
        || number(&echo[2])? != expected.generation
        || text(&echo[3])? != expected.fingerprint
    {
        return Err(mismatch("world/question echo"));
    }
    let defining = if matches!(fields[6], Value::Null) {
        None
    } else {
        Some(receipt(&fields[6])?)
    };
    let payload = blob(&fields[7])?;
    let resolution = fields[5]
        .as_array()
        .ok_or_else(|| malformed("resolution must be array"))?;
    let tag = resolution
        .first()
        .ok_or_else(|| malformed("empty resolution"))?;
    if text(tag)? == "rejected" {
        let resolution = array(&fields[5], 2)?;
        if defining.is_some() || !payload.is_empty() {
            return Err(malformed("rejection contains an answer"));
        }
        return Ok(WorldAnswer::Rejected(match number(&resolution[1])? {
            0 => WorldQueryRejection::Unsupported,
            1 => WorldQueryRejection::Unavailable,
            2 => WorldQueryRejection::Corrupt,
            _ => return Err(malformed("unknown rejection")),
        }));
    }
    let reply = inspection::decode_structured_output(payload)
        .map_err(|error| malformed(error.to_string()))?;
    if text(tag)? == "absent" {
        let resolution = array(&fields[5], 4)?;
        let absence = ClosedModuleAbsence {
            module: text(&resolution[1])?.into(),
            source_domain: hash(&resolution[2])?,
            package_profile: hash(&resolution[3])?,
        };
        if defining.is_some()
            || absence.module != expected.module
            || !matches!(reply, StructuredReply::Error(QueryError::UnknownModule(ref module)) if module == expected.module)
        {
            return Err(mismatch("absence is not paired with UnknownModule"));
        }
        return Ok(WorldAnswer::Absent(absence));
    }
    let requested = receipt(&fields[5])?;
    if requested.module != expected.module {
        return Err(mismatch("requested module"));
    }
    let (identifier, provenance) = match &reply {
        StructuredReply::Info(info) if expected.kind == WorldQueryKind::Info => {
            (&info.identifier, &info.provenance)
        }
        StructuredReply::Type(info) if expected.kind == WorldQueryKind::Type => {
            (&info.identifier, &info.provenance)
        }
        StructuredReply::Error(error) => {
            if defining.is_some() {
                return Err(mismatch("name refusal has defining receipt"));
            }
            let refusal = match error {
                QueryError::Unknown(query) => {
                    check_query(query, expected)?;
                    NameRefusal::Unknown(query.clone())
                }
                QueryError::Ambiguous(query, candidates) => {
                    check_query(query, expected)?;
                    NameRefusal::Ambiguous(query.clone(), candidates.clone())
                }
                QueryError::Unsupported(detail) => NameRefusal::Unsupported(detail.clone()),
                QueryError::UnknownModule(_) => {
                    return Err(mismatch("resolved module paired with absence"))
                }
            };
            return Ok(WorldAnswer::NameRefused { requested, refusal });
        }
        _ => return Err(mismatch("structured answer kind")),
    };
    let defining = defining.ok_or_else(|| mismatch("success lacks defining receipt"))?;
    let fits_namespace = expected.namespace.is_none()
        || expected.namespace == Some(identifier.namespace)
        || (expected.namespace == Some(IdentifierNamespace::Value)
            && identifier.namespace == IdentifierNamespace::Field);
    if !fits_namespace
        || identifier.module != defining.module
        || !(expected.name == identifier.name
            || expected.name == format!("{}.{}", defining.module, identifier.name))
    {
        return Err(mismatch("canonical identifier"));
    }
    if provenance.scope != NameScope::PublicModule(expected.module.into())
        || provenance.generation != expected.generation
        || provenance.fingerprint != expected.fingerprint
    {
        return Err(mismatch("presentation provenance"));
    }
    Ok(match reply {
        StructuredReply::Info(info) => WorldAnswer::Info {
            requested,
            defining,
            info,
        },
        StructuredReply::Type(info) => WorldAnswer::Type {
            requested,
            defining,
            info,
        },
        StructuredReply::Error(_) => unreachable!("name refusal returned above"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn txt(value: &str) -> Value {
        Value::Text(value.into())
    }
    fn arr(values: Vec<Value>) -> Value {
        Value::Array(values)
    }
    fn num(value: u64) -> Value {
        value.into()
    }
    fn receipt_value(module: &str) -> Value {
        arr(vec![
            txt("resolved"),
            txt("main"),
            txt(module),
            arr(vec![txt("package"), Value::Bytes(vec![8; 32])]),
        ])
    }
    fn query_value() -> Value {
        arr(vec![
            arr(vec![txt("PublicModule"), txt("M")]),
            txt("Value"),
            txt("x"),
        ])
    }
    fn error_value(tag: &str, value: Value) -> Value {
        arr(vec![txt("StructuredError"), arr(vec![txt(tag), value])])
    }
    fn success(kind: WorldQueryKind) -> Value {
        let identity = arr(vec![txt("Origin"), txt("x"), txt("Value")]);
        let ty = arr(vec![txt("Int"), arr(vec![]), arr(vec![])]);
        let provenance = arr(vec![
            arr(vec![txt("PublicModule"), txt("M")]),
            num(0),
            txt(""),
        ]);
        match kind {
            WorldQueryKind::Info => arr(vec![
                txt("StructuredInfoOk"),
                arr(vec![
                    identity,
                    arr(vec![txt("Value"), ty]),
                    Value::Null,
                    provenance,
                ]),
            ]),
            WorldQueryKind::Type => arr(vec![
                txt("StructuredTypeOk"),
                arr(vec![identity, ty, provenance]),
            ]),
        }
    }
    fn envelope(
        kind: WorldQueryKind,
        resolution: Value,
        defining: Value,
        result: Option<Value>,
    ) -> Value {
        let payload = result
            .map(|result| {
                let mut bytes = Vec::new();
                ciborium::ser::into_writer(
                    &arr(vec![
                        txt("TPINSP008"),
                        arr(vec![txt("Complete"), arr(vec![result])]),
                    ]),
                    &mut bytes,
                )
                .unwrap();
                bytes
            })
            .unwrap_or_default();
        arr(vec![
            num(1),
            Value::Bytes(vec![7; 32]),
            num(if kind == WorldQueryKind::Info { 0 } else { 1 }),
            txt("M"),
            arr(vec![num(1), txt("x"), num(0), txt("")]),
            resolution,
            defining,
            Value::Bytes(payload),
        ])
    }
    fn encoded(value: &Value) -> Vec<u8> {
        let mut payload = Vec::new();
        ciborium::ser::into_writer(value, &mut payload).unwrap();
        let mut bytes = b"TPWQ0002".to_vec();
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend(payload);
        bytes
    }
    fn expected(kind: WorldQueryKind) -> WorldQueryExpectation<'static> {
        WorldQueryExpectation {
            world: &[7; 32],
            kind,
            module: "M",
            namespace: Some(IdentifierNamespace::Value),
            name: "x",
            generation: 0,
            fingerprint: "",
        }
    }
    #[test]
    fn found_answers_keep_requested_and_canonical_defining_identity() {
        for kind in [WorldQueryKind::Info, WorldQueryKind::Type] {
            let packet = envelope(
                kind,
                receipt_value("M"),
                receipt_value("Origin"),
                Some(success(kind)),
            );
            let answer = decode_world_answer(&encoded(&packet), &expected(kind)).unwrap();
            assert_eq!(answer.defining_module().unwrap().module, "Origin");
            match answer {
                WorldAnswer::Info { info, .. } => assert!(
                    matches!(info.declaration, inspection::DeclarationInfo::Value(ref ty) if ty.canonical == "Int")
                ),
                WorldAnswer::Type { info, .. } => assert_eq!(info.expression.canonical, "Int"),
                _ => panic!("decoded a different answer"),
            }
        }
    }
    #[test]
    fn name_refusal_is_independent_of_info_or_type_question_kind() {
        for kind in [WorldQueryKind::Info, WorldQueryKind::Type] {
            let packet = envelope(
                kind,
                receipt_value("M"),
                Value::Null,
                Some(error_value("Unknown", query_value())),
            );
            assert!(
                matches!(decode_world_answer(&encoded(&packet), &expected(kind)).unwrap(), WorldAnswer::NameRefused { refusal: NameRefusal::Unknown(query), .. } if query.name == "x")
            );
        }
    }
    #[test]
    fn absence_and_service_refusal_have_distinct_payload_contracts() {
        let kind = WorldQueryKind::Info;
        let absent = arr(vec![
            txt("absent"),
            txt("M"),
            Value::Bytes(vec![9; 32]),
            Value::Bytes(vec![8; 32]),
        ]);
        let valid = envelope(
            kind,
            absent.clone(),
            Value::Null,
            Some(error_value("UnknownModule", txt("M"))),
        );
        assert!(
            matches!(decode_world_answer(&encoded(&valid), &expected(kind)).unwrap(), WorldAnswer::Absent(value) if value.source_domain == [9;32])
        );
        let invalid = envelope(
            kind,
            absent,
            Value::Null,
            Some(error_value("Unsupported", txt("unsupported"))),
        );
        assert!(matches!(
            decode_world_answer(&encoded(&invalid), &expected(kind)),
            Err(WorldAnswerDecodeError::ProvenanceMismatch(_))
        ));
        let rejected = arr(vec![txt("rejected"), num(1)]);
        let packet = envelope(kind, rejected.clone(), Value::Null, None);
        assert_eq!(
            decode_world_answer(&encoded(&packet), &expected(kind)).unwrap(),
            WorldAnswer::Rejected(WorldQueryRejection::Unavailable)
        );
        let invalid = envelope(kind, rejected, Value::Null, Some(success(kind)));
        assert!(decode_world_answer(&encoded(&invalid), &expected(kind)).is_err());
    }
    #[test]
    fn rejects_crossed_echo_identity_namespace_kind_and_malformed_payload() {
        let kind = WorldQueryKind::Info;
        let packet = envelope(
            kind,
            receipt_value("M"),
            receipt_value("Origin"),
            Some(success(kind)),
        );
        for index in [1, 2, 3, 4, 5, 6, 7] {
            let mut changed = packet.clone();
            let fields = changed.as_array_mut().unwrap();
            fields[index] = match index {
                1 => Value::Bytes(vec![0; 32]),
                2 => num(1),
                3 => txt("Other"),
                4 => arr(vec![num(2), txt("x"), num(0), txt("")]),
                5 => receipt_value("Other"),
                6 => receipt_value("Wrong"),
                _ => Value::Bytes(vec![0xff]),
            };
            assert!(
                decode_world_answer(&encoded(&changed), &expected(kind)).is_err(),
                "mutation {index}"
            );
        }
        let wrong_kind = envelope(
            kind,
            receipt_value("M"),
            receipt_value("Origin"),
            Some(success(WorldQueryKind::Type)),
        );
        assert!(decode_world_answer(&encoded(&wrong_kind), &expected(kind)).is_err());
        let mut trailing = encoded(&packet);
        trailing.push(0);
        assert!(decode_world_answer(&trailing, &expected(kind)).is_err());
    }
    #[test]
    fn canonical_namespace_is_checked_independently_of_the_echo() {
        for requested in [
            None,
            Some(IdentifierNamespace::Value),
            Some(IdentifierNamespace::Type),
            Some(IdentifierNamespace::Constructor),
            Some(IdentifierNamespace::Field),
        ] {
            for (actual, spelling) in [
                (IdentifierNamespace::Value, "Value"),
                (IdentifierNamespace::Type, "Type"),
                (IdentifierNamespace::Constructor, "Constructor"),
                (IdentifierNamespace::Field, "Field"),
            ] {
                let mut value = success(WorldQueryKind::Info);
                value.as_array_mut().unwrap()[1].as_array_mut().unwrap()[0]
                    .as_array_mut()
                    .unwrap()[2] = txt(spelling);
                let mut packet = envelope(
                    WorldQueryKind::Info,
                    receipt_value("M"),
                    receipt_value("Origin"),
                    Some(value),
                );
                packet.as_array_mut().unwrap()[4].as_array_mut().unwrap()[0] =
                    num(namespace_tag(requested));
                let mut expectation = expected(WorldQueryKind::Info);
                expectation.namespace = requested;
                let valid = requested.is_none()
                    || requested == Some(actual)
                    || (requested == Some(IdentifierNamespace::Value)
                        && actual == IdentifierNamespace::Field);
                assert_eq!(
                    decode_world_answer(&encoded(&packet), &expectation).is_ok(),
                    valid,
                    "{requested:?}/{actual:?}"
                );
            }
        }
    }
}
