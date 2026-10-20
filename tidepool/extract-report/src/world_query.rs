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

/// Exact operands of a current-scope observation. These are descriptions,
/// never World/source/runtime admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldObservationQuestion {
    Info(NameQuery),
    Type(NameQuery),
    RowFit(IdentifierRef),
    Expression(String),
    TypeSearch { pattern: String, limit: u32 },
}

pub struct WorldObservationExpectation<'a> {
    pub world: &'a [u8; 32],
    pub question: &'a WorldObservationQuestion,
    pub generation: u64,
    pub fingerprint: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldObservationOutcome {
    Answer(inspection::ScopedReply),
    Rejected(WorldQueryRejection),
}

/// Decoded correlation facts remain separate from the authenticating issuer.
#[derive(Clone, Debug)]
pub struct DecodedWorldObservation {
    pub provenance: inspection::ScopeProvenance,
    pub outcome: WorldObservationOutcome,
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

fn decode_world_envelope(bytes: &[u8]) -> Result<Value, WorldAnswerDecodeError> {
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
    Ok(value)
}

/// Decode the outer world envelope and its single structured result once.
/// Source-domain/profile authentication remains with the toolchain world issuer.
pub fn decode_world_answer(
    bytes: &[u8],
    expected: &WorldQueryExpectation<'_>,
) -> Result<WorldAnswer, WorldAnswerDecodeError> {
    let value = decode_world_envelope(bytes)?;
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
                    return Err(mismatch("resolved module paired with absence"));
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

pub fn decode_world_observation(
    bytes: &[u8],
    expected: &WorldObservationExpectation<'_>,
) -> Result<DecodedWorldObservation, WorldAnswerDecodeError> {
    use inspection::{ScopeProvenance, ScopedReply};
    let value = decode_world_envelope(bytes)?;
    let fields = array(&value, 5)?;
    let pin = array(&fields[2], 2)?;
    if number(&fields[0])? != 2
        || hash(&fields[1])? != *expected.world
        || number(&pin[0])? != expected.generation
        || text(&pin[1])? != expected.fingerprint
    {
        return Err(mismatch("observation envelope"));
    }
    let namespace = |value: NameNamespace| match value {
        NameNamespace::Any => 0,
        NameNamespace::Value => 1,
        NameNamespace::Type => 2,
        NameNamespace::Constructor => 3,
    };
    let identifier_namespace = |value: IdentifierNamespace| match value {
        IdentifierNamespace::Value => 1,
        IdentifierNamespace::Type => 2,
        IdentifierNamespace::Constructor => 3,
        IdentifierNamespace::Field => 4,
    };
    let echo = match expected.question {
        WorldObservationQuestion::Info(query) | WorldObservationQuestion::Type(query) => {
            if query.scope != NameScope::Current {
                return Err(mismatch("observation question scope"));
            }
            let echo = array(&fields[3], 3)?;
            let tag = if matches!(expected.question, WorldObservationQuestion::Info(_)) {
                2
            } else {
                3
            };
            number(&echo[0])? == tag
                && number(&echo[1])? == namespace(query.namespace)
                && text(&echo[2])? == query.name
        }
        WorldObservationQuestion::RowFit(reference) => {
            let echo = array(&fields[3], 4)?;
            number(&echo[0])? == 4
                && number(&echo[1])? == identifier_namespace(reference.namespace)
                && text(&echo[2])? == reference.name
                && text(&echo[3])? == reference.module
        }
        WorldObservationQuestion::Expression(expression) => {
            let echo = array(&fields[3], 2)?;
            number(&echo[0])? == 5 && text(&echo[1])? == expression
        }
        WorldObservationQuestion::TypeSearch { pattern, limit } => {
            let echo = array(&fields[3], 3)?;
            number(&echo[0])? == 6
                && text(&echo[1])? == pattern
                && number(&echo[2])? == u64::from(*limit)
                && *limit > 0
        }
    };
    if !echo {
        return Err(mismatch("observation operands"));
    }
    let provenance = ScopeProvenance {
        scope: NameScope::Current,
        generation: number(&pin[0])?,
        fingerprint: text(&pin[1])?.into(),
    };
    let result = array(&fields[4], 2)?;
    let outcome = match text(&result[0])? {
        "rejected" => WorldObservationOutcome::Rejected(match number(&result[1])? {
            0 => WorldQueryRejection::Unsupported,
            1 => WorldQueryRejection::Unavailable,
            2 => WorldQueryRejection::Corrupt,
            _ => return Err(malformed("unknown observation refusal")),
        }),
        "answer" => {
            let reply = inspection::decode_scoped_output(blob(&result[1])?)
                .map_err(|error| malformed(error.to_string()))?;
            let same_pin = |actual: &ScopeProvenance| {
                actual.scope == NameScope::Current && actual.same_view(&provenance)
            };
            let name_matches = |reference: &IdentifierRef, query: &NameQuery| {
                !reference.module.is_empty()
                    && !reference.name.is_empty()
                    && (query.namespace == NameNamespace::Any
                        || namespace(query.namespace) == identifier_namespace(reference.namespace)
                        || query.namespace == NameNamespace::Value
                            && reference.namespace == IdentifierNamespace::Field)
            };
            match (expected.question, &reply) {
                (WorldObservationQuestion::Info(query), ScopedReply::Info(info))
                    if same_pin(&info.provenance) && name_matches(&info.identifier, query) => {}
                (WorldObservationQuestion::Type(query), ScopedReply::Type(info))
                    if same_pin(&info.provenance) && name_matches(&info.identifier, query) => {}
                (
                    WorldObservationQuestion::RowFit(expected),
                    ScopedReply::RowFit { identifier, .. },
                ) if expected == identifier => {}
                (
                    WorldObservationQuestion::Expression(expected),
                    ScopedReply::Expression { expression, .. },
                ) if expected == expression => {}
                (
                    WorldObservationQuestion::TypeSearch {
                        pattern: expected,
                        limit,
                    },
                    ScopedReply::TypeSearch { pattern, matches },
                ) if pattern == expected && matches.len() <= *limit as usize => {}
                (question, ScopedReply::Error(error)) => match (question, error) {
                    (
                        WorldObservationQuestion::Info(expected)
                        | WorldObservationQuestion::Type(expected),
                        QueryError::Unknown(actual) | QueryError::Ambiguous(actual, _),
                    ) if actual == expected => {}
                    (
                        WorldObservationQuestion::RowFit(expected),
                        QueryError::Unknown(actual) | QueryError::Ambiguous(actual, _),
                    ) if actual.scope == NameScope::Current
                        && actual.name == expected.name
                        && namespace(actual.namespace)
                            == identifier_namespace(expected.namespace) => {}
                    (_, QueryError::Unsupported(_)) => {}
                    _ => return Err(mismatch("observation name refusal")),
                },
                _ => return Err(mismatch("observation answer kind or provenance")),
            }
            WorldObservationOutcome::Answer(reply)
        }
        _ => return Err(malformed("unknown observation outcome")),
    };
    Ok(DecodedWorldObservation {
        provenance,
        outcome,
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
    fn scoped_packet(echo: Value, result: Value) -> Value {
        let mut payload = Vec::new();
        ciborium::ser::into_writer(
            &arr(vec![
                txt("TPINSP008"),
                arr(vec![txt("Complete"), arr(vec![result])]),
            ]),
            &mut payload,
        )
        .unwrap();
        arr(vec![
            num(2),
            Value::Bytes(vec![7; 32]),
            arr(vec![num(3), txt("pin")]),
            echo,
            arr(vec![txt("answer"), Value::Bytes(payload)]),
        ])
    }
    fn scoped_cases() -> Vec<(WorldObservationQuestion, Value, Value)> {
        let name = NameQuery {
            scope: NameScope::Current,
            namespace: NameNamespace::Value,
            name: "x".into(),
        };
        let reference = IdentifierRef {
            module: "M".into(),
            name: "x".into(),
            namespace: IdentifierNamespace::Value,
        };
        let identity = arr(vec![txt("M"), txt("x"), txt("Value")]);
        let ty = arr(vec![txt("Int"), arr(vec![]), arr(vec![])]);
        let pin = arr(vec![arr(vec![txt("Current")]), num(3), txt("pin")]);
        vec![
            (
                WorldObservationQuestion::Info(name.clone()),
                arr(vec![num(2), num(1), txt("x")]),
                arr(vec![
                    txt("StructuredInfoOk"),
                    arr(vec![
                        identity.clone(),
                        arr(vec![txt("Value"), ty.clone()]),
                        Value::Null,
                        pin.clone(),
                    ]),
                ]),
            ),
            (
                WorldObservationQuestion::Type(name),
                arr(vec![num(3), num(1), txt("x")]),
                arr(vec![
                    txt("StructuredTypeOk"),
                    arr(vec![identity.clone(), ty, pin]),
                ]),
            ),
            (
                WorldObservationQuestion::RowFit(reference),
                arr(vec![num(4), num(1), txt("x"), txt("M")]),
                arr(vec![txt("RowFit"), identity.clone(), txt("Available")]),
            ),
            (
                WorldObservationQuestion::Expression("x".into()),
                arr(vec![num(5), txt("x")]),
                arr(vec![txt("Type"), txt("x"), txt("Int"), txt("Available")]),
            ),
            (
                WorldObservationQuestion::TypeSearch {
                    pattern: "Int".into(),
                    limit: 1,
                },
                arr(vec![num(6), txt("Int"), num(1)]),
                arr(vec![
                    txt("TypeMatches"),
                    txt("Int"),
                    arr(vec![arr(vec![
                        txt("x"),
                        txt("M"),
                        txt("Int"),
                        txt("Exact"),
                        txt("Available"),
                        arr(vec![]),
                        identity,
                    ])]),
                ]),
            ),
        ]
    }
    #[test]
    fn scoped_answers_decode_all_closed_shapes_once() {
        for (question, echo, result) in scoped_cases() {
            let expected = WorldObservationExpectation {
                world: &[7; 32],
                question: &question,
                generation: 3,
                fingerprint: "pin",
            };
            let decoded =
                decode_world_observation(&encoded(&scoped_packet(echo, result)), &expected)
                    .unwrap();
            assert!(matches!(
                decoded.outcome,
                WorldObservationOutcome::Answer(_)
            ));
            assert_eq!(decoded.provenance.scope, NameScope::Current);
            assert_eq!(decoded.provenance.fingerprint, "pin");
        }
    }
    #[test]
    fn scoped_answers_reject_wrong_pin_question_kind_and_candidate() {
        let mut cases = scoped_cases();
        let (question, echo, result) = cases.remove(2);
        let expected = WorldObservationExpectation {
            world: &[7; 32],
            question: &question,
            generation: 3,
            fingerprint: "pin",
        };
        let packet = scoped_packet(echo, result);
        for index in [1, 2, 3] {
            let mut wrong = packet.clone();
            wrong.as_array_mut().unwrap()[index] = match index {
                1 => Value::Bytes(vec![9; 32]),
                2 => arr(vec![num(4), txt("pin")]),
                _ => arr(vec![num(4), num(1), txt("other"), txt("M")]),
            };
            assert!(matches!(
                decode_world_observation(&encoded(&wrong), &expected),
                Err(WorldAnswerDecodeError::ProvenanceMismatch(_))
            ));
        }
        let wrong_ref = scoped_packet(
            arr(vec![num(4), num(1), txt("x"), txt("M")]),
            arr(vec![
                txt("RowFit"),
                arr(vec![txt("Other"), txt("x"), txt("Value")]),
                txt("Available"),
            ]),
        );
        assert!(decode_world_observation(&encoded(&wrong_ref), &expected).is_err());
        let wrong_kind = scoped_packet(
            arr(vec![num(4), num(1), txt("x"), txt("M")]),
            arr(vec![txt("Type"), txt("x"), txt("Int"), txt("Available")]),
        );
        assert!(decode_world_observation(&encoded(&wrong_kind), &expected).is_err());
        let mut trailing = encoded(&packet);
        trailing.push(0);
        assert!(decode_world_observation(&trailing, &expected).is_err());
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
