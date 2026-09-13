//! GHC-authoritative inspection of the exact source environment used by a
//! resident workbench turn.

use std::path::Path;

use ciborium::value::Value as CborValue;
use serde::Serialize;
use tempfile::TempDir;
use tidepool_extract_cmd::{ExtractCmd, SpawnError};

use crate::{timing, CompileError};

use super::assemble_inspection_module;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionQuery {
    TypeOf(String),
    Info(String),
    TypeSearch(String),
    Browse {
        module: String,
        expanded: bool,
    },
    StructuredInfo {
        query: NameQuery,
        provenance: ScopeProvenance,
    },
    StructuredType {
        query: NameQuery,
        provenance: ScopeProvenance,
    },
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeProvenance {
    pub scope: NameScope,
    pub generation: u64,
    pub fingerprint: String,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeInfo {
    pub identifier: IdentifierRef,
    pub expression: TypeExpression,
    pub provenance: ScopeProvenance,
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifierInfo {
    pub identifier: IdentifierRef,
    pub declaration: DeclarationInfo,
    pub parent: Option<IdentifierRef>,
    pub provenance: ScopeProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryError {
    Unknown(NameQuery),
    Ambiguous(NameQuery, Vec<IdentifierRef>),
    UnknownModule(String),
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoEntry {
    pub name: String,
    pub module: Option<String>,
    pub kind: String,
    pub display: String,
    pub availability: InspectionAvailability,
}

/// Whether a callable's required effects fit the inspecting actor's row.
/// Runtime resource grants remain a separate authority check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionAvailability {
    Available,
    Unavailable,
    Unknown,
}

impl InspectionAvailability {
    fn decode(value: &CborValue, what: &str) -> Result<Self, CompileError> {
        match text(value, what)? {
            "Available" => Ok(Self::Available),
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionResult {
    Type {
        expression: String,
        display: String,
        availability: InspectionAvailability,
    },
    Info {
        query: String,
        entries: Vec<InfoEntry>,
    },
    Ambiguous {
        query: String,
        entries: Vec<InfoEntry>,
    },
    NotFound {
        query: String,
    },
    ModuleNotFound {
        module: String,
    },
    Rejected {
        diagnostic: String,
    },
    Browse {
        module: String,
        expanded: bool,
        entries: Vec<InfoEntry>,
    },
    TypeMatches {
        query: String,
        matches: Vec<TypeMatch>,
    },
    StructuredInfo(Result<IdentifierInfo, QueryError>),
    StructuredType(Result<TypeInfo, QueryError>),
}

impl InspectionResult {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Type {
                expression,
                display,
                ..
            } => format!("{expression} :: {display}"),
            Self::Info { entries, .. } => entries
                .iter()
                .map(|entry| entry.display.trim())
                .collect::<Vec<_>>()
                .join("\n\n"),
            Self::Ambiguous { query, entries } => {
                let candidates = entries
                    .iter()
                    .map(|entry| match &entry.module {
                        Some(module) => format!("{module}.{}", entry.name),
                        None => entry.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("ambiguous name `{query}`; candidates: {candidates}")
            }
            Self::NotFound { query } => format!("unknown name `{query}`"),
            Self::ModuleNotFound { module } => format!("unknown module `{module}`"),
            Self::Rejected { diagnostic } => diagnostic.clone(),
            Self::Browse {
                module, entries, ..
            } => {
                let declarations = entries
                    .iter()
                    .map(|entry| entry.display.trim())
                    .collect::<Vec<_>>()
                    .join("\n");
                if declarations.is_empty() {
                    format!("-- {module}")
                } else {
                    format!("-- {module}\n{declarations}")
                }
            }
            Self::StructuredInfo(Ok(info)) => format!(
                "{} -- {}.{}",
                render_declaration(&info.declaration),
                info.identifier.module,
                info.identifier.name
            ),
            Self::StructuredType(Ok(info)) => format!(
                "{}.{} :: {}",
                info.identifier.module, info.identifier.name, info.expression.canonical
            ),
            Self::StructuredInfo(Err(error)) | Self::StructuredType(Err(error)) => {
                render_query_error(error)
            }
            Self::TypeMatches { matches, .. } => matches
                .iter()
                .map(|entry| format!("{} :: {}", entry.name, entry.signature))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

fn render_declaration(declaration: &DeclarationInfo) -> String {
    match declaration {
        DeclarationInfo::Value(ty) => ty.canonical.clone(),
        DeclarationInfo::Data { constructors, .. } => {
            format!("data ({} constructors)", constructors.len())
        }
        DeclarationInfo::Newtype { constructor, .. } => {
            format!("newtype ({})", constructor.identifier.name)
        }
        DeclarationInfo::TypeSynonym { body, .. } => format!("type = {}", body.canonical),
        DeclarationInfo::Class { methods, .. } => format!("class ({} methods)", methods.len()),
        DeclarationInfo::Constructor { constructor, .. } => constructor.ty.canonical.clone(),
        DeclarationInfo::RecordSelector { ty, .. } => ty.canonical.clone(),
    }
}

fn render_query_error(error: &QueryError) -> String {
    match error {
        QueryError::Unknown(query) => format!("unknown name `{}`", query.name),
        QueryError::Ambiguous(query, candidates) => format!(
            "ambiguous name `{}`; candidates: {}",
            query.name,
            candidates
                .iter()
                .map(|candidate| format!("{}.{}", candidate.module, candidate.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        QueryError::UnknownModule(module) => format!("unknown module `{module}`"),
        QueryError::Unsupported(detail) => detail.clone(),
    }
}

pub struct InspectionRequest<'a> {
    pub preamble: &'a str,
    pub imports: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
    pub queries: &'a [InspectionQuery],
    /// The actor's GHC effects type alias. Standalone inspections leave it absent.
    pub effects: Option<&'a str>,
}

/// Inspect an ordered batch without evaluating it or mutating the resident
/// session. Every query sees the same preamble, imports, session modules, and
/// injected value interfaces as the next ordinary turn. The worker serves the
/// batch through one request while isolating GHC rejection per query.
pub fn run_inspections(
    request: InspectionRequest<'_>,
) -> Result<Vec<InspectionResult>, CompileError> {
    if request.queries.is_empty() {
        return Ok(Vec::new());
    }
    let temp = TempDir::new()?;
    let output_path = temp.path().join("inspection.cbor");

    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .output_dir(temp.path())
        .inspect_out(&output_path)
        .includes(request.include)
        .session_root(request.session_root)
        .inject_vals(request.inject_modules);
    let imports = match request.effects {
        Some(_) => format!("{}\nqualified Data.Proxy\n", request.imports),
        None => request.imports.to_owned(),
    };
    for (index, query) in request.queries.iter().enumerate() {
        let query_dir = temp.path().join(format!("query-{index}"));
        std::fs::create_dir(&query_dir)?;
        let source_path = query_dir.join("Expr.hs");
        let expressions = match query {
            InspectionQuery::TypeOf(expression) => std::slice::from_ref(expression),
            InspectionQuery::Info(_)
            | InspectionQuery::TypeSearch(_)
            | InspectionQuery::Browse { .. }
            | InspectionQuery::StructuredInfo { .. }
            | InspectionQuery::StructuredType { .. } => &[],
        };
        let mut source = assemble_inspection_module(request.preamble, &imports, expressions);
        if let Some(effects) = request.effects {
            source.push_str("\n__tidepool_lookup_row :: Data.Proxy.Proxy (");
            source.push_str(effects);
            source.push_str(")\n__tidepool_lookup_row = Data.Proxy.Proxy\n");
        }
        if let InspectionQuery::TypeSearch(query) = query {
            source.push_str("\n__tidepool_lookup_query :: ");
            source.push_str(query);
            source.push_str("\n__tidepool_lookup_query = __tidepool_lookup_query\n");
        }
        std::fs::write(&source_path, source)?;
        command.input(&source_path);
        match query {
            InspectionQuery::TypeOf(expression) => {
                command.inspect_type(expression);
            }
            InspectionQuery::Info(name) => {
                command.inspect_info(name);
            }
            InspectionQuery::TypeSearch(query) => {
                command.inspect_search(query);
            }
            InspectionQuery::Browse { module, expanded } => {
                command.inspect_browse(module, *expanded);
            }
            InspectionQuery::StructuredInfo { query, provenance } => {
                command.inspect_structured_info(structured_request(query, provenance));
            }
            InspectionQuery::StructuredType { query, provenance } => {
                command.inspect_structured_type(structured_request(query, provenance));
            }
        }
    }

    let endpoint = command.bind().map_err(map_spawn)?;
    crate::paths::apply_build_products_dir(&mut command, &endpoint);
    let run = endpoint.execute(&command).map_err(map_spawn)?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_EXTRACT_SPAWN,
        run.elapsed,
        0,
    );
    crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;
    let bytes = std::fs::read(&output_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CompileError::MissingOutput(output_path.clone())
        } else {
            CompileError::Io(error)
        }
    })?;
    let results = decode_inspections(&bytes)?;
    if results.len() != request.queries.len() {
        return Err(invalid(format!(
            "worker returned {} results for {} queries",
            results.len(),
            request.queries.len()
        )));
    }
    Ok(request
        .queries
        .iter()
        .zip(results)
        .map(|(query, result)| match (query, result) {
            (
                InspectionQuery::StructuredType { .. },
                InspectionResult::StructuredInfo(Err(error)),
            ) => InspectionResult::StructuredType(Err(error)),
            (_, result) => result,
        })
        .collect())
}

fn structured_request(
    query: &NameQuery,
    provenance: &ScopeProvenance,
) -> tidepool_extract_cmd::StructuredInspection {
    tidepool_extract_cmd::StructuredInspection {
        scope: match &query.scope {
            NameScope::Current => tidepool_extract_cmd::InspectionScope::Current,
            NameScope::PublicModule(module) => {
                tidepool_extract_cmd::InspectionScope::PublicModule(module.clone())
            }
        },
        namespace: match query.namespace {
            NameNamespace::Any => tidepool_extract_cmd::InspectionNamespace::Any,
            NameNamespace::Value => tidepool_extract_cmd::InspectionNamespace::Value,
            NameNamespace::Type => tidepool_extract_cmd::InspectionNamespace::Type,
            NameNamespace::Constructor => tidepool_extract_cmd::InspectionNamespace::Constructor,
        },
        name: query.name.clone(),
        generation: provenance.generation,
        fingerprint: provenance.fingerprint.clone(),
    }
}

fn map_spawn(error: SpawnError) -> CompileError {
    CompileError::Io(crate::extract_spawn_error(error.source))
}

fn decode_inspections(bytes: &[u8]) -> Result<Vec<InspectionResult>, CompileError> {
    let mut reader = std::io::Cursor::new(bytes);
    let value: CborValue = ciborium::de::from_reader(&mut reader)
        .map_err(|error| invalid(format!("malformed CBOR: {error}")))?;
    if reader.position() != bytes.len() as u64 {
        return Err(invalid("trailing CBOR data"));
    }
    let root = array_len(&value, 2, "receipt")?;
    if text(&root[0], "version")? != "TPINSP005" {
        return Err(invalid("unsupported receipt version"));
    }
    array(&root[1], "results")?
        .iter()
        .map(decode_inspection_result)
        .collect()
}

fn decode_inspection_result(value: &CborValue) -> Result<InspectionResult, CompileError> {
    let body = array(value, "body")?;
    let tag = body
        .first()
        .ok_or_else(|| invalid("empty result body"))
        .and_then(|value| text(value, "result tag"))?;
    match tag {
        "Type" => {
            let body = array_len(value, 4, "Type result")?;
            Ok(InspectionResult::Type {
                expression: text(&body[1], "Type expression")?.into(),
                display: text(&body[2], "Type display")?.into(),
                availability: InspectionAvailability::decode(&body[3], "Type availability")?,
            })
        }
        "Info" => {
            let body = array_len(value, 3, "Info result")?;
            let entries = array(&body[2], "Info entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Info {
                query: text(&body[1], "Info query")?.into(),
                entries,
            })
        }
        "Ambiguous" => {
            let body = array_len(value, 3, "Ambiguous result")?;
            let entries = array(&body[2], "Ambiguous entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Ambiguous {
                query: text(&body[1], "Ambiguous query")?.into(),
                entries,
            })
        }
        "NotFound" => {
            let body = array_len(value, 2, "NotFound result")?;
            Ok(InspectionResult::NotFound {
                query: text(&body[1], "NotFound query")?.into(),
            })
        }
        "ModuleNotFound" => {
            let body = array_len(value, 2, "ModuleNotFound result")?;
            Ok(InspectionResult::ModuleNotFound {
                module: text(&body[1], "ModuleNotFound module")?.into(),
            })
        }
        "Rejected" => {
            let body = array_len(value, 2, "Rejected result")?;
            Ok(InspectionResult::Rejected {
                diagnostic: text(&body[1], "Rejected diagnostic")?.into(),
            })
        }
        "Browse" => {
            let body = array_len(value, 4, "Browse result")?;
            let entries = array(&body[3], "Browse entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Browse {
                module: text(&body[1], "Browse module")?.into(),
                expanded: boolean(&body[2], "Browse expanded")?,
                entries,
            })
        }
        "TypeMatches" => {
            let body = array_len(value, 3, "TypeMatches result")?;
            let matches = array(&body[2], "TypeMatches matches")?
                .iter()
                .map(decode_type_match)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::TypeMatches {
                query: text(&body[1], "TypeMatches query")?.into(),
                matches,
            })
        }
        "StructuredInfoOk" => {
            let body = array_len(value, 2, "StructuredInfoOk result")?;
            Ok(InspectionResult::StructuredInfo(Ok(
                decode_identifier_info(&body[1])?,
            )))
        }
        "StructuredTypeOk" => {
            let body = array_len(value, 2, "StructuredTypeOk result")?;
            Ok(InspectionResult::StructuredType(Ok(decode_type_info(
                &body[1],
            )?)))
        }
        "StructuredError" => {
            let body = array_len(value, 2, "StructuredError result")?;
            Ok(InspectionResult::StructuredInfo(Err(decode_query_error(
                &body[1],
            )?)))
        }
        other => Err(invalid(format!("unknown result tag {other:?}"))),
    }
}

fn decode_identifier_info(value: &CborValue) -> Result<IdentifierInfo, CompileError> {
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

fn decode_type_info(value: &CborValue) -> Result<TypeInfo, CompileError> {
    let fields = array_len(value, 3, "TypeInfo")?;
    Ok(TypeInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        expression: decode_type_expression(&fields[1])?,
        provenance: decode_provenance(&fields[2])?,
    })
}

fn decode_identifier_ref(value: &CborValue) -> Result<IdentifierRef, CompileError> {
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

fn decode_type_expression(value: &CborValue) -> Result<TypeExpression, CompileError> {
    let fields = array_len(value, 3, "TypeExpression")?;
    Ok(TypeExpression {
        canonical: text(&fields[0], "TypeExpression canonical")?.into(),
        variables: decode_texts(&fields[1], "TypeExpression variables")?,
        constraints: decode_texts(&fields[2], "TypeExpression constraints")?,
    })
}

fn decode_provenance(value: &CborValue) -> Result<ScopeProvenance, CompileError> {
    let fields = array_len(value, 3, "ScopeProvenance")?;
    Ok(ScopeProvenance {
        scope: decode_scope(&fields[0])?,
        generation: unsigned(&fields[1], "ScopeProvenance generation")?,
        fingerprint: text(&fields[2], "ScopeProvenance fingerprint")?.into(),
    })
}

fn decode_query_error(value: &CborValue) -> Result<QueryError, CompileError> {
    let fields = array(value, "StructuredError")?;
    let tag = fields
        .first()
        .ok_or_else(|| invalid("empty StructuredError"))
        .and_then(|value| text(value, "StructuredError tag"))?;
    match tag {
        "Unknown" => Ok(QueryError::Unknown(decode_query(
            array_len(value, 2, "Unknown error")?.get(1).unwrap(),
        )?)),
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
        "UnknownModule" => Ok(QueryError::UnknownModule(
            text(
                array_len(value, 2, "UnknownModule error")?.get(1).unwrap(),
                "UnknownModule module",
            )?
            .into(),
        )),
        "Unsupported" => Ok(QueryError::Unsupported(
            text(
                array_len(value, 2, "Unsupported error")?.get(1).unwrap(),
                "Unsupported detail",
            )?
            .into(),
        )),
        other => Err(invalid(format!("unknown StructuredError tag {other:?}"))),
    }
}

fn decode_query(value: &CborValue) -> Result<NameQuery, CompileError> {
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

fn decode_scope(value: &CborValue) -> Result<NameScope, CompileError> {
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

fn decode_declaration(value: &CborValue) -> Result<DeclarationInfo, CompileError> {
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

fn decode_constructor(value: &CborValue) -> Result<ConstructorInfo, CompileError> {
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

fn decode_field(value: &CborValue) -> Result<FieldInfo, CompileError> {
    let fields = array_len(value, 2, "FieldInfo")?;
    Ok(FieldInfo {
        name: text(&fields[0], "FieldInfo name")?.into(),
        ty: decode_type_expression(&fields[1])?,
    })
}

fn decode_class_method(value: &CborValue) -> Result<ClassMethodInfo, CompileError> {
    let fields = array_len(value, 2, "ClassMethodInfo")?;
    Ok(ClassMethodInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        ty: decode_type_expression(&fields[1])?,
    })
}

fn decode_texts(value: &CborValue, what: &str) -> Result<Vec<String>, CompileError> {
    array(value, what)?
        .iter()
        .map(|value| Ok(text(value, what)?.into()))
        .collect()
}

fn unsigned(value: &CborValue, what: &str) -> Result<u64, CompileError> {
    match value {
        CborValue::Integer(value) => {
            u64::try_from(*value).map_err(|_| invalid(format!("{what} must be u64")))
        }
        _ => Err(invalid(format!("{what} must be u64"))),
    }
}

fn decode_type_match(value: &CborValue) -> Result<TypeMatch, CompileError> {
    let fields = array_len(value, 5, "TypeMatch")?;
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
    })
}

fn boolean(value: &CborValue, what: &str) -> Result<bool, CompileError> {
    match value {
        CborValue::Bool(value) => Ok(*value),
        _ => Err(invalid(format!("{what} must be boolean"))),
    }
}

fn decode_info_entry(value: &CborValue) -> Result<InfoEntry, CompileError> {
    let fields = array_len(value, 5, "Info entry")?;
    let module = match &fields[1] {
        CborValue::Null => None,
        value => Some(text(value, "Info module")?.into()),
    };
    Ok(InfoEntry {
        name: text(&fields[0], "Info name")?.into(),
        module,
        kind: text(&fields[2], "Info kind")?.into(),
        display: text(&fields[3], "Info display")?.into(),
        availability: InspectionAvailability::decode(&fields[4], "Info availability")?,
    })
}

fn array<'a>(value: &'a CborValue, what: &str) -> Result<&'a [CborValue], CompileError> {
    match value {
        CborValue::Array(values) => Ok(values),
        _ => Err(invalid(format!("{what} must be an array"))),
    }
}

fn array_len<'a>(
    value: &'a CborValue,
    expected: usize,
    what: &str,
) -> Result<&'a [CborValue], CompileError> {
    let values = array(value, what)?;
    if values.len() != expected {
        return Err(invalid(format!(
            "{what} must contain {expected} items, got {}",
            values.len()
        )));
    }
    Ok(values)
}

fn text<'a>(value: &'a CborValue, what: &str) -> Result<&'a str, CompileError> {
    match value {
        CborValue::Text(value) => Ok(value),
        _ => Err(invalid(format!("{what} must be text"))),
    }
}

fn invalid(detail: impl Into<String>) -> CompileError {
    CompileError::ExtractFailed(format!("inspection receipt: {}", detail.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_testing::eval_harness;

    fn encoded(value: CborValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn decodes_every_result_shape() {
        let receipt = CborValue::Array(vec![
            CborValue::Text("TPINSP005".into()),
            CborValue::Array(vec![
                CborValue::Array(vec![
                    CborValue::Text("Type".into()),
                    CborValue::Text("fmap".into()),
                    CborValue::Text("Functor f => (a -> b) -> f a -> f b".into()),
                    CborValue::Text("Unknown".into()),
                ]),
                CborValue::Array(vec![
                    CborValue::Text("Info".into()),
                    CborValue::Text("Maybe".into()),
                    CborValue::Array(vec![CborValue::Array(vec![
                        CborValue::Text("Maybe".into()),
                        CborValue::Text("GHC.Internal.Maybe".into()),
                        CborValue::Text("type".into()),
                        CborValue::Text("data Maybe a = Nothing | Just a".into()),
                        CborValue::Text("Unknown".into()),
                    ])]),
                ]),
                CborValue::Array(vec![
                    CborValue::Text("Ambiguous".into()),
                    CborValue::Text("Result".into()),
                    CborValue::Array(vec![]),
                ]),
                CborValue::Array(vec![
                    CborValue::Text("NotFound".into()),
                    CborValue::Text("nope".into()),
                ]),
                CborValue::Array(vec![
                    CborValue::Text("Browse".into()),
                    CborValue::Text("Tidepool.Actors.Shoal".into()),
                    CborValue::Bool(true),
                    CborValue::Array(vec![]),
                ]),
            ]),
        ]);
        let decoded = decode_inspections(&encoded(receipt)).unwrap();
        assert!(matches!(
            decoded[0],
            InspectionResult::Type {
                availability: InspectionAvailability::Unknown,
                ..
            }
        ));
        assert!(
            matches!(decoded[1], InspectionResult::Info { ref entries, .. } if entries[0].availability == InspectionAvailability::Unknown)
        );
        assert_eq!(
            decoded[2],
            InspectionResult::Ambiguous {
                query: "Result".into(),
                entries: vec![],
            }
        );
        assert_eq!(
            decoded[3],
            InspectionResult::NotFound {
                query: "nope".into()
            }
        );
        assert!(matches!(
            decoded[4],
            InspectionResult::Browse { expanded: true, .. }
        ));
    }

    #[test]
    fn decodes_structured_type_and_typed_query_error() {
        let scope = CborValue::Array(vec![
            CborValue::Text("PublicModule".into()),
            CborValue::Text("Project.Work".into()),
        ]);
        let query = CborValue::Array(vec![
            scope.clone(),
            CborValue::Text("Value".into()),
            CborValue::Text("work".into()),
        ]);
        let identifier = CborValue::Array(vec![
            CborValue::Text("Project.Work".into()),
            CborValue::Text("work".into()),
            CborValue::Text("Value".into()),
        ]);
        let expression = CborValue::Array(vec![
            CborValue::Text("Eq a => a -> Bool".into()),
            CborValue::Array(vec![CborValue::Text("a".into())]),
            CborValue::Array(vec![CborValue::Text("Eq a".into())]),
        ]);
        let provenance = CborValue::Array(vec![
            scope,
            CborValue::Integer(7.into()),
            CborValue::Text("scope-abc".into()),
        ]);
        let receipt = CborValue::Array(vec![
            CborValue::Text("TPINSP005".into()),
            CborValue::Array(vec![
                CborValue::Array(vec![
                    CborValue::Text("StructuredTypeOk".into()),
                    CborValue::Array(vec![identifier, expression, provenance]),
                ]),
                CborValue::Array(vec![
                    CborValue::Text("StructuredError".into()),
                    CborValue::Array(vec![CborValue::Text("Unknown".into()), query]),
                ]),
            ]),
        ]);

        let decoded = decode_inspections(&encoded(receipt)).unwrap();
        let InspectionResult::StructuredType(Ok(info)) = &decoded[0] else {
            panic!("expected structured type: {:?}", decoded[0]);
        };
        assert_eq!(info.identifier.module, "Project.Work");
        assert_eq!(info.expression.variables, ["a"]);
        assert_eq!(info.expression.constraints, ["Eq a"]);
        assert_eq!(info.provenance.generation, 7);
        assert!(matches!(
            &decoded[1],
            InspectionResult::StructuredInfo(Err(QueryError::Unknown(NameQuery {
                scope: NameScope::PublicModule(module),
                namespace: NameNamespace::Value,
                name,
            }))) if module == "Project.Work" && name == "work"
        ));
    }

    #[test]
    fn rejects_version_shape_and_unknown_tag() {
        for value in [
            CborValue::Array(vec![
                CborValue::Text("TPINSP000".into()),
                CborValue::Array(vec![]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("TPINSP004".into()),
                CborValue::Array(vec![CborValue::Text("Other".into())]),
            ]),
            CborValue::Array(vec![CborValue::Text("TPINSP001".into())]),
        ] {
            assert!(decode_inspections(&encoded(value)).is_err());
        }

        let mut trailing = encoded(CborValue::Array(vec![
            CborValue::Text("TPINSP005".into()),
            CborValue::Array(vec![CborValue::Array(vec![
                CborValue::Text("NotFound".into()),
                CborValue::Text("x".into()),
            ])]),
        ]));
        trailing.push(0);
        assert!(decode_inspections(&trailing).is_err());
    }

    #[test]
    fn decodes_row_availability_and_rejects_unknown_values() {
        let receipt = |availability: &str| {
            CborValue::Array(vec![
                CborValue::Text("TPINSP005".into()),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("TypeMatches".into()),
                    CborValue::Text("Eff effects ()".into()),
                    CborValue::Array(vec![CborValue::Array(vec![
                        CborValue::Text("run".into()),
                        CborValue::Null,
                        CborValue::Text("Eff effects ()".into()),
                        CborValue::Text("Usable".into()),
                        CborValue::Text(availability.into()),
                    ])]),
                ])]),
            ])
        };
        for (encoded_name, expected) in [
            ("Available", InspectionAvailability::Available),
            ("Unavailable", InspectionAvailability::Unavailable),
            ("Unknown", InspectionAvailability::Unknown),
        ] {
            let decoded = decode_inspections(&encoded(receipt(encoded_name))).unwrap();
            assert!(
                matches!(&decoded[0], InspectionResult::TypeMatches { matches, .. }
                if matches[0].availability == expected)
            );
        }
        assert!(decode_inspections(&encoded(receipt("Guess"))).is_err());
    }

    #[test]
    fn one_inspection_compile_answers_type_info_and_browse_queries() {
        eval_harness::require_extract();
        let include = tempfile::tempdir().unwrap();
        let session = tempfile::tempdir().unwrap();
        std::fs::write(
            include.path().join("BrowseFixture.hs"),
            concat!(
                "{-# LANGUAGE TypeFamilies #-}\n",
                "module BrowseFixture (Public(..), Record(..), Abstract, Opaque, Solo(..), Alias, Family, Service(..), constrained, exportedValue, Maybe(..)) where\n",
                "import Prelude\n",
                "import Data.Maybe (Maybe(..))\n",
                "data Public = First | Second\n",
                "data Record = Record { recordField :: Int }\n",
                "data Abstract = Hidden\n",
                "newtype Opaque = Opaque Int\n",
                "data Solo = Solo\n",
                "type Alias a = Maybe a\n",
                "type family Family a\n",
                "class Service a where service :: a -> Int\n",
                "constrained :: Eq a => a -> Bool\n",
                "constrained x = x == x\n",
                "exportedValue :: Int\n",
                "exportedValue = 42\n",
            ),
        )
        .unwrap();
        let preamble = concat!(
            "{-# LANGUAGE NoImplicitPrelude, NoMonomorphismRestriction, PartialTypeSignatures #-}\n",
            "module Expr where\n",
            "import BrowseFixture\n",
            "import qualified BrowseFixture as Alias\n",
        );
        let current = ScopeProvenance {
            scope: NameScope::Current,
            generation: 11,
            fingerprint: "current-scope".into(),
        };
        let public = ScopeProvenance {
            scope: NameScope::PublicModule("BrowseFixture".into()),
            generation: 11,
            fingerprint: "public-module".into(),
        };
        let current_query = |namespace, name: &str| NameQuery {
            scope: NameScope::Current,
            namespace,
            name: name.into(),
        };
        let public_query = |namespace, name: &str| NameQuery {
            scope: NameScope::PublicModule("BrowseFixture".into()),
            namespace,
            name: name.into(),
        };
        let mut queries = vec![
            InspectionQuery::TypeOf("exportedValue".into()),
            InspectionQuery::Info("Public".into()),
            InspectionQuery::TypeOf("missing + 1".into()),
            InspectionQuery::Browse {
                module: "No.Such.Module".into(),
                expanded: false,
            },
            InspectionQuery::Browse {
                module: "BrowseFixture".into(),
                expanded: false,
            },
            InspectionQuery::Browse {
                module: "BrowseFixture".into(),
                expanded: true,
            },
            InspectionQuery::StructuredType {
                query: current_query(NameNamespace::Value, "constrained"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Record"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Abstract"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Alias"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Any, "Solo"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Constructor, "Solo"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Value, "missingName"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: NameQuery {
                    scope: NameScope::PublicModule("No.Such.Module".into()),
                    namespace: NameNamespace::Type,
                    name: "Missing".into(),
                },
                provenance: ScopeProvenance {
                    scope: NameScope::PublicModule("No.Such.Module".into()),
                    generation: 11,
                    fingerprint: "missing-module".into(),
                },
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Family"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Opaque"),
                provenance: public,
            },
        ];
        queries.push(InspectionQuery::Info("Alias.Public".into()));
        queries.push(InspectionQuery::Info("Alias.exportedValue".into()));
        queries.push(InspectionQuery::Info("Missing.Public".into()));
        queries.push(InspectionQuery::TypeSearch("Public".into()));
        queries.push(InspectionQuery::TypeSearch("Public ->".into()));
        queries.push(InspectionQuery::TypeSearch("Public -> _".into()));
        let results = run_inspections(InspectionRequest {
            preamble,
            imports: "",
            include: &[include.path()],
            session_root: session.path(),
            inject_modules: &[],
            queries: &queries,
            effects: None,
        })
        .unwrap();

        assert_eq!(results.len(), queries.len());
        assert!(results[0].render().contains("exportedValue :: Int"));
        assert!(results[1].render().contains("data Public"));
        assert!(matches!(results[2], InspectionResult::Rejected { .. }));
        assert_eq!(
            results[3],
            InspectionResult::ModuleNotFound {
                module: "No.Such.Module".into()
            }
        );
        assert!(results[6].render().contains("data Public"));
        assert!(results[7].render().contains("exportedValue :: Int"));
        assert!(matches!(results[8], InspectionResult::NotFound { .. }));
        assert!(
            matches!(results[9], InspectionResult::TypeMatches { .. }),
            "{:?}",
            results[9]
        );
        assert!(matches!(results[10], InspectionResult::Rejected { .. }));
        assert!(
            matches!(results[11], InspectionResult::TypeMatches { .. }),
            "{:?}",
            results[11]
        );
        let grouped = results[4].render();
        assert!(grouped.starts_with("-- BrowseFixture\n"));
        assert!(grouped.contains("data Public"));
        assert!(grouped.contains("class Service"));
        assert!(grouped.contains("exportedValue :: Int"));
        assert!(!grouped.lines().any(|line| line.starts_with("First ::")));
        let expanded = results[5].render();
        assert!(expanded.contains("First :: Public"), "{expanded}");
        assert!(expanded.contains("service ::"), "{expanded}");
        assert!(expanded.contains("data Maybe"), "{expanded}");

        let InspectionResult::StructuredType(Ok(constrained)) = &results[6] else {
            panic!("expected constrained function type: {:?}", results[6]);
        };
        assert_eq!(constrained.identifier.name, "constrained");
        assert!(
            constrained
                .expression
                .constraints
                .iter()
                .any(|constraint| constraint.contains("Eq")),
            "{:?}",
            constrained.expression
        );
        assert_eq!(constrained.provenance.fingerprint, "current-scope");

        let InspectionResult::StructuredInfo(Ok(record)) = &results[7] else {
            panic!("expected record declaration: {:?}", results[7]);
        };
        let DeclarationInfo::Data { constructors, .. } = &record.declaration else {
            panic!("expected data declaration: {:?}", record.declaration);
        };
        assert_eq!(constructors.len(), 1);
        assert_eq!(constructors[0].fields[0].name, "recordField");

        let InspectionResult::StructuredInfo(Ok(abstract_type)) = &results[8] else {
            panic!("expected abstract type declaration: {:?}", results[8]);
        };
        assert!(matches!(
            &abstract_type.declaration,
            DeclarationInfo::Data { constructors, .. } if constructors.is_empty()
        ));

        let InspectionResult::StructuredInfo(Ok(alias)) = &results[9] else {
            panic!("expected type synonym: {:?}", results[9]);
        };
        assert!(
            matches!(
                &alias.declaration,
                DeclarationInfo::TypeSynonym { parameters, body }
                    if parameters.len() == 1 && body.canonical.contains("Maybe")
            ),
            "{:?}",
            alias.declaration
        );
        assert!(matches!(
            &results[10],
            InspectionResult::StructuredInfo(Err(QueryError::Ambiguous(query, candidates)))
                if query.name == "Solo" && candidates.len() == 2
        ));

        let InspectionResult::StructuredInfo(Ok(constructor)) = &results[11] else {
            panic!("expected constructor declaration: {:?}", results[11]);
        };
        assert!(matches!(
            (&constructor.declaration, &constructor.parent),
            (DeclarationInfo::Constructor { parent, .. }, Some(recorded_parent))
                if parent.name == "Solo" && recorded_parent == parent
        ));
        assert!(matches!(
            &results[12],
            InspectionResult::StructuredInfo(Err(QueryError::Unknown(query)))
                if query.name == "missingName"
        ));
        assert!(matches!(
            &results[13],
            InspectionResult::StructuredInfo(Err(QueryError::UnknownModule(module)))
                if module == "No.Such.Module"
        ));
        assert!(matches!(
            &results[14],
            InspectionResult::StructuredInfo(Err(QueryError::Unsupported(detail)))
                if detail.contains("unsupported")
        ));
        assert!(matches!(
            &results[15],
            InspectionResult::StructuredInfo(Err(QueryError::Unsupported(detail)))
                if detail.contains("abstract newtype constructor")
        ));
    }
}
