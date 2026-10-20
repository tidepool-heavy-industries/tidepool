//! Dependency-neutral structured inspection answers emitted by the worker.
//! Decoding these values grants no source, compiler-world or runtime authority.

use ciborium::value::Value as CborValue;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
#[error("inspection receipt: {0}")]
pub struct InspectionDecodeError(String);

fn invalid(detail: impl Into<String>) -> InspectionDecodeError {
    InspectionDecodeError(detail.into())
}

/// Whether a callable's required effects fit the inspecting actor's row.
/// Runtime resource grants remain a separate authority check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionAvailability {
    Available,
    /// The row fits and every closed constraint solves; a constraint stays open
    /// only because the call site decides it. Usable, not uncertain.
    Polymorphic,
    Unavailable,
    Unknown,
}

impl InspectionAvailability {
    pub fn decode(value: &CborValue, what: &str) -> Result<Self, InspectionDecodeError> {
        match text(value, what)? {
            "Available" => Ok(Self::Available),
            "Polymorphic" => Ok(Self::Polymorphic),
            "Unavailable" => Ok(Self::Unavailable),
            "Unknown" => Ok(Self::Unknown),
            other => Err(invalid(format!("unknown {what} {other:?}"))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeMatchQuality {
    Exact,
    Usable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeMatch {
    pub name: String,
    pub module: Option<String>,
    pub signature: String,
    pub quality: TypeMatchQuality,
    pub availability: InspectionAvailability,
    pub references: Vec<IdentifierRef>,
    /// The matched value's canonical resolved identity. `references` are
    /// dependencies, not the matched declaration itself.
    pub identity: IdentifierRef,
}

pub fn decode_type_match(value: &CborValue) -> Result<TypeMatch, InspectionDecodeError> {
    let fields = array_len(value, 7, "TypeMatch")?;
    let module = match &fields[1] {
        CborValue::Null => None,
        value => Some(text(value, "TypeMatch module")?.into()),
    };
    let quality = match text(&fields[3], "TypeMatch quality")? {
        "Exact" => TypeMatchQuality::Exact,
        "Usable" => TypeMatchQuality::Usable,
        other => return Err(invalid(format!("unknown TypeMatch quality {other:?}"))),
    };
    Ok(TypeMatch {
        name: text(&fields[0], "TypeMatch name")?.into(),
        module,
        signature: text(&fields[2], "TypeMatch signature")?.into(),
        quality,
        availability: InspectionAvailability::decode(&fields[4], "TypeMatch availability")?,
        references: array(&fields[5], "TypeMatch references")?
            .iter()
            .map(decode_identifier_ref)
            .collect::<Result<_, _>>()?,
        identity: decode_identifier_ref(&fields[6])?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameScope {
    Current,
    PublicModule(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameNamespace {
    Any,
    Value,
    Type,
    Constructor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameQuery {
    pub scope: NameScope,
    pub namespace: NameNamespace,
    pub name: String,
}

/// Full provenance for diagnostics and wire answers. Deliberately not
/// `PartialEq`: staleness is decided by `same_view`, and a structural
/// comparison would silently reintroduce the scope-label bug.
#[derive(Clone, Debug)]
pub struct ScopeProvenance {
    pub scope: NameScope,
    pub generation: u64,
    pub fingerprint: String,
}

impl ScopeProvenance {
    /// Staleness compares only the view identity. Keep the full provenance for
    /// diagnostics such as `ScopeChanged { before, after }`.
    pub fn same_view(&self, other: &Self) -> bool {
        self.generation == other.generation && self.fingerprint == other.fingerprint
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentifierNamespace {
    Value,
    Type,
    Constructor,
    Field,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifierRef {
    pub module: String,
    pub name: String,
    pub namespace: IdentifierNamespace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeExpression {
    pub canonical: String,
    pub variables: Vec<String>,
    pub constraints: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct TypeInfo {
    pub identifier: IdentifierRef,
    pub expression: TypeExpression,
    pub provenance: ScopeProvenance,
}

impl PartialEq for TypeInfo {
    fn eq(&self, other: &Self) -> bool {
        self.identifier == other.identifier
            && self.expression == other.expression
            && self.provenance.same_view(&other.provenance)
    }
}

impl Eq for TypeInfo {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub ty: TypeExpression,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstructorInfo {
    pub identifier: IdentifierRef,
    pub ty: TypeExpression,
    pub arguments: Vec<TypeExpression>,
    pub fields: Vec<FieldInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassMethodInfo {
    pub identifier: IdentifierRef,
    pub ty: TypeExpression,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclarationInfo {
    Value(TypeExpression),
    Data {
        parameters: Vec<String>,
        constructors: Vec<ConstructorInfo>,
    },
    Newtype {
        parameters: Vec<String>,
        constructor: ConstructorInfo,
    },
    TypeSynonym {
        parameters: Vec<String>,
        body: TypeExpression,
    },
    Class {
        parameters: Vec<String>,
        superclasses: Vec<TypeExpression>,
        methods: Vec<ClassMethodInfo>,
    },
    Constructor {
        parent: IdentifierRef,
        constructor: ConstructorInfo,
    },
    RecordSelector {
        parent: IdentifierRef,
        ty: TypeExpression,
    },
}

#[derive(Clone, Debug)]
pub struct IdentifierInfo {
    pub identifier: IdentifierRef,
    pub declaration: DeclarationInfo,
    pub parent: Option<IdentifierRef>,
    pub provenance: ScopeProvenance,
}

impl PartialEq for IdentifierInfo {
    fn eq(&self, other: &Self) -> bool {
        self.identifier == other.identifier
            && self.declaration == other.declaration
            && self.parent == other.parent
            && self.provenance.same_view(&other.provenance)
    }
}

impl Eq for IdentifierInfo {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryError {
    Unknown(NameQuery),
    Ambiguous(NameQuery, Vec<IdentifierRef>),
    UnknownModule(String),
    Unsupported(String),
}

/// A complete structured answer; error payloads do not pretend to be Info answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructuredReply {
    Info(Box<IdentifierInfo>),
    Type(TypeInfo),
    Error(QueryError),
}

pub fn decode_structured_result(
    value: &CborValue,
) -> Result<StructuredReply, InspectionDecodeError> {
    let fields = array_len(value, 2, "structured result")?;
    match text(&fields[0], "structured result tag")? {
        "StructuredInfoOk" => Ok(StructuredReply::Info(Box::new(decode_identifier_info(
            &fields[1],
        )?))),
        "StructuredTypeOk" => Ok(StructuredReply::Type(decode_type_info(&fields[1])?)),
        "StructuredError" => Ok(StructuredReply::Error(decode_query_error(&fields[1])?)),
        tag => Err(invalid(format!("unexpected structured result {tag:?}"))),
    }
}

/// Decode the one complete structured answer carried by a world response.
pub fn decode_structured_output(bytes: &[u8]) -> Result<StructuredReply, InspectionDecodeError> {
    let value = parse_output(bytes)?;
    decode_structured_result(single_result(&value)?)
}

fn parse_output(bytes: &[u8]) -> Result<CborValue, InspectionDecodeError> {
    let mut reader = std::io::Cursor::new(bytes);
    let value: CborValue = ciborium::de::from_reader(&mut reader)
        .map_err(|error| invalid(format!("malformed CBOR: {error}")))?;
    if reader.position() != bytes.len() as u64 {
        return Err(invalid("trailing CBOR data"));
    }
    Ok(value)
}

fn single_result(value: &CborValue) -> Result<&CborValue, InspectionDecodeError> {
    let top = array_len(value, 2, "inspection receipt")?;
    if text(&top[0], "inspection version")? != "TPINSP008" {
        return Err(invalid("unsupported inspection version"));
    }
    let complete = array_len(&top[1], 2, "complete output")?;
    if text(&complete[0], "output kind")? != "Complete" {
        return Err(invalid("expected complete output"));
    }
    let results = array_len(&complete[1], 1, "world inspection results")?;
    Ok(&results[0])
}

/// One scoped compiler answer. It remains untrusted data until the owning
/// World and runtime transaction authenticate its envelope and settlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopedReply {
    Info(Box<IdentifierInfo>),
    Type(TypeInfo),
    RowFit {
        identifier: IdentifierRef,
        availability: InspectionAvailability,
    },
    Expression {
        expression: String,
        display: String,
        availability: InspectionAvailability,
    },
    TypeSearch {
        pattern: String,
        matches: Vec<TypeMatch>,
    },
    Error(QueryError),
}

pub fn decode_scoped_output(bytes: &[u8]) -> Result<ScopedReply, InspectionDecodeError> {
    let value = parse_output(bytes)?;
    let result = single_result(&value)?;
    let body = array(result, "scoped result")?;
    let tag = body.first().ok_or_else(|| invalid("empty scoped result"))?;
    match text(tag, "scoped result tag")? {
        "StructuredInfoOk" | "StructuredTypeOk" | "StructuredError" => {
            Ok(match decode_structured_result(result)? {
                StructuredReply::Info(info) => ScopedReply::Info(info),
                StructuredReply::Type(info) => ScopedReply::Type(info),
                StructuredReply::Error(error) => ScopedReply::Error(error),
            })
        }
        "RowFit" => {
            let fields = array_len(result, 3, "row fit")?;
            Ok(ScopedReply::RowFit {
                identifier: decode_identifier_ref(&fields[1])?,
                availability: InspectionAvailability::decode(&fields[2], "row availability")?,
            })
        }
        "Type" => {
            let fields = array_len(result, 4, "expression type")?;
            Ok(ScopedReply::Expression {
                expression: text(&fields[1], "expression")?.into(),
                display: text(&fields[2], "expression type")?.into(),
                availability: InspectionAvailability::decode(
                    &fields[3],
                    "expression availability",
                )?,
            })
        }
        "TypeMatches" => {
            let fields = array_len(result, 3, "type search")?;
            Ok(ScopedReply::TypeSearch {
                pattern: text(&fields[1], "type pattern")?.into(),
                matches: array(&fields[2], "type matches")?
                    .iter()
                    .map(decode_type_match)
                    .collect::<Result<_, _>>()?,
            })
        }
        other => Err(invalid(format!("unexpected scoped result {other:?}"))),
    }
}

fn decode_identifier_info(value: &CborValue) -> Result<IdentifierInfo, InspectionDecodeError> {
    let fields = array_len(value, 4, "IdentifierInfo")?;
    Ok(IdentifierInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        declaration: decode_declaration(&fields[1])?,
        parent: match &fields[2] {
            CborValue::Null => None,
            value => Some(decode_identifier_ref(value)?),
        },
        provenance: decode_provenance(&fields[3])?,
    })
}

fn decode_type_info(value: &CborValue) -> Result<TypeInfo, InspectionDecodeError> {
    let fields = array_len(value, 3, "TypeInfo")?;
    Ok(TypeInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        expression: decode_type_expression(&fields[1])?,
        provenance: decode_provenance(&fields[2])?,
    })
}

pub fn decode_identifier_ref(value: &CborValue) -> Result<IdentifierRef, InspectionDecodeError> {
    let fields = array_len(value, 3, "IdentifierRef")?;
    let namespace = match text(&fields[2], "IdentifierRef namespace")? {
        "Value" => IdentifierNamespace::Value,
        "Type" => IdentifierNamespace::Type,
        "Constructor" => IdentifierNamespace::Constructor,
        "Field" => IdentifierNamespace::Field,
        other => {
            return Err(invalid(format!(
                "unknown IdentifierRef namespace {other:?}"
            )));
        }
    };
    Ok(IdentifierRef {
        module: text(&fields[0], "IdentifierRef module")?.into(),
        name: text(&fields[1], "IdentifierRef name")?.into(),
        namespace,
    })
}

fn decode_type_expression(value: &CborValue) -> Result<TypeExpression, InspectionDecodeError> {
    let fields = array_len(value, 3, "TypeExpression")?;
    Ok(TypeExpression {
        canonical: text(&fields[0], "TypeExpression canonical")?.into(),
        variables: decode_texts(&fields[1], "TypeExpression variables")?,
        constraints: decode_texts(&fields[2], "TypeExpression constraints")?,
    })
}

fn decode_provenance(value: &CborValue) -> Result<ScopeProvenance, InspectionDecodeError> {
    let fields = array_len(value, 3, "ScopeProvenance")?;
    Ok(ScopeProvenance {
        scope: decode_scope(&fields[0])?,
        generation: unsigned(&fields[1], "ScopeProvenance generation")?,
        fingerprint: text(&fields[2], "ScopeProvenance fingerprint")?.into(),
    })
}

fn decode_query_error(value: &CborValue) -> Result<QueryError, InspectionDecodeError> {
    let fields = array(value, "StructuredError")?;
    let tag = fields
        .first()
        .ok_or_else(|| invalid("empty StructuredError"))
        .and_then(|value| text(value, "StructuredError tag"))?;
    match tag {
        "Unknown" => {
            let fields = array_len(value, 2, "Unknown error")?;
            Ok(QueryError::Unknown(decode_query(&fields[1])?))
        }
        "Ambiguous" => {
            let fields = array_len(value, 3, "Ambiguous error")?;
            Ok(QueryError::Ambiguous(
                decode_query(&fields[1])?,
                array(&fields[2], "Ambiguous candidates")?
                    .iter()
                    .map(decode_identifier_ref)
                    .collect::<Result<_, _>>()?,
            ))
        }
        "UnknownModule" => {
            let fields = array_len(value, 2, "UnknownModule error")?;
            Ok(QueryError::UnknownModule(
                text(&fields[1], "UnknownModule module")?.into(),
            ))
        }
        "Unsupported" => {
            let fields = array_len(value, 2, "Unsupported error")?;
            Ok(QueryError::Unsupported(
                text(&fields[1], "Unsupported detail")?.into(),
            ))
        }
        other => Err(invalid(format!("unknown StructuredError tag {other:?}"))),
    }
}

fn decode_query(value: &CborValue) -> Result<NameQuery, InspectionDecodeError> {
    let fields = array_len(value, 3, "Structured query")?;
    let namespace = match text(&fields[1], "Structured query namespace")? {
        "Any" => NameNamespace::Any,
        "Value" => NameNamespace::Value,
        "Type" => NameNamespace::Type,
        "Constructor" => NameNamespace::Constructor,
        other => {
            return Err(invalid(format!(
                "unknown structured query namespace {other:?}"
            )));
        }
    };
    Ok(NameQuery {
        scope: decode_scope(&fields[0])?,
        namespace,
        name: text(&fields[2], "Structured query name")?.into(),
    })
}

pub fn decode_scope(value: &CborValue) -> Result<NameScope, InspectionDecodeError> {
    let fields = array(value, "scope")?;
    match fields
        .first()
        .and_then(|value| text(value, "scope tag").ok())
    {
        Some("Current") if fields.len() == 1 => Ok(NameScope::Current),
        Some("PublicModule") if fields.len() == 2 => Ok(NameScope::PublicModule(
            text(&fields[1], "PublicModule name")?.into(),
        )),
        Some(tag) => Err(invalid(format!("invalid scope {tag:?}"))),
        None => Err(invalid("scope tag must be text")),
    }
}

fn decode_declaration(value: &CborValue) -> Result<DeclarationInfo, InspectionDecodeError> {
    let fields = array(value, "DeclarationInfo")?;
    let tag = fields
        .first()
        .ok_or_else(|| invalid("empty DeclarationInfo"))
        .and_then(|value| text(value, "DeclarationInfo tag"))?;
    match tag {
        "Value" => Ok(DeclarationInfo::Value(decode_type_expression(
            &array_len(value, 2, "Value declaration")?[1],
        )?)),
        "Data" => {
            let f = array_len(value, 3, "Data declaration")?;
            Ok(DeclarationInfo::Data {
                parameters: decode_texts(&f[1], "Data parameters")?,
                constructors: array(&f[2], "Data constructors")?
                    .iter()
                    .map(decode_constructor)
                    .collect::<Result<_, _>>()?,
            })
        }
        "Newtype" => {
            let f = array_len(value, 3, "Newtype declaration")?;
            Ok(DeclarationInfo::Newtype {
                parameters: decode_texts(&f[1], "Newtype parameters")?,
                constructor: decode_constructor(&f[2])?,
            })
        }
        "TypeSynonym" => {
            let f = array_len(value, 3, "TypeSynonym declaration")?;
            Ok(DeclarationInfo::TypeSynonym {
                parameters: decode_texts(&f[1], "TypeSynonym parameters")?,
                body: decode_type_expression(&f[2])?,
            })
        }
        "Class" => {
            let f = array_len(value, 4, "Class declaration")?;
            Ok(DeclarationInfo::Class {
                parameters: decode_texts(&f[1], "Class parameters")?,
                superclasses: array(&f[2], "Class superclasses")?
                    .iter()
                    .map(decode_type_expression)
                    .collect::<Result<_, _>>()?,
                methods: array(&f[3], "Class methods")?
                    .iter()
                    .map(decode_class_method)
                    .collect::<Result<_, _>>()?,
            })
        }
        "Constructor" => {
            let f = array_len(value, 3, "Constructor declaration")?;
            Ok(DeclarationInfo::Constructor {
                parent: decode_identifier_ref(&f[1])?,
                constructor: decode_constructor(&f[2])?,
            })
        }
        "RecordSelector" => {
            let f = array_len(value, 3, "RecordSelector declaration")?;
            Ok(DeclarationInfo::RecordSelector {
                parent: decode_identifier_ref(&f[1])?,
                ty: decode_type_expression(&f[2])?,
            })
        }
        other => Err(invalid(format!("unknown DeclarationInfo tag {other:?}"))),
    }
}

fn decode_constructor(value: &CborValue) -> Result<ConstructorInfo, InspectionDecodeError> {
    let fields = array_len(value, 4, "ConstructorInfo")?;
    Ok(ConstructorInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        ty: decode_type_expression(&fields[1])?,
        arguments: array(&fields[2], "ConstructorInfo arguments")?
            .iter()
            .map(decode_type_expression)
            .collect::<Result<_, _>>()?,
        fields: array(&fields[3], "ConstructorInfo fields")?
            .iter()
            .map(decode_field)
            .collect::<Result<_, _>>()?,
    })
}

fn decode_field(value: &CborValue) -> Result<FieldInfo, InspectionDecodeError> {
    let fields = array_len(value, 2, "FieldInfo")?;
    Ok(FieldInfo {
        name: text(&fields[0], "FieldInfo name")?.into(),
        ty: decode_type_expression(&fields[1])?,
    })
}

fn decode_class_method(value: &CborValue) -> Result<ClassMethodInfo, InspectionDecodeError> {
    let fields = array_len(value, 2, "ClassMethodInfo")?;
    Ok(ClassMethodInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        ty: decode_type_expression(&fields[1])?,
    })
}

fn decode_texts(value: &CborValue, what: &str) -> Result<Vec<String>, InspectionDecodeError> {
    array(value, what)?
        .iter()
        .map(|value| Ok(text(value, what)?.into()))
        .collect()
}

fn unsigned(value: &CborValue, what: &str) -> Result<u64, InspectionDecodeError> {
    match value {
        CborValue::Integer(value) => {
            u64::try_from(*value).map_err(|_| invalid(format!("{what} must be u64")))
        }
        _ => Err(invalid(format!("{what} must be u64"))),
    }
}

fn array<'a>(value: &'a CborValue, what: &str) -> Result<&'a [CborValue], InspectionDecodeError> {
    match value {
        CborValue::Array(values) => Ok(values),
        _ => Err(invalid(format!("{what} must be an array"))),
    }
}

fn array_len<'a>(
    value: &'a CborValue,
    expected: usize,
    what: &str,
) -> Result<&'a [CborValue], InspectionDecodeError> {
    let values = array(value, what)?;
    if values.len() != expected {
        return Err(invalid(format!(
            "{what} must contain {expected} items, got {}",
            values.len()
        )));
    }
    Ok(values)
}

fn text<'a>(value: &'a CborValue, what: &str) -> Result<&'a str, InspectionDecodeError> {
    match value {
        CborValue::Text(value) => Ok(value),
        _ => Err(invalid(format!("{what} must be text"))),
    }
}
