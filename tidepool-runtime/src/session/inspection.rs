//! GHC-authoritative inspection of the exact source environment used by a
//! resident workbench turn.

use std::path::Path;

use ciborium::value::Value as CborValue;
use tempfile::TempDir;
use tidepool_extract_cmd::{ExtractCmd, SpawnError};

use crate::{timing, CompileError};

use super::assemble_inspection_module;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionQuery {
    TypeOf(String),
    Info(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoEntry {
    pub name: String,
    pub module: Option<String>,
    pub kind: String,
    pub display: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionResult {
    Type {
        expression: String,
        display: String,
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
}

impl InspectionResult {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Type {
                expression,
                display,
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
        }
    }
}

pub struct InspectionRequest<'a> {
    pub preamble: &'a str,
    pub imports: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
    pub query: InspectionQuery,
}

/// Inspect one expression or name without evaluating it or mutating the
/// resident session. The compiler sees the same preamble, imports, session
/// modules, and injected value interfaces as the next ordinary turn.
pub fn run_inspection(request: InspectionRequest<'_>) -> Result<InspectionResult, CompileError> {
    let temp = TempDir::new()?;
    let source_path = temp.path().join("Expr.hs");
    let output_path = temp.path().join("inspection.cbor");
    let expression = match &request.query {
        InspectionQuery::TypeOf(expression) => Some(expression.as_str()),
        InspectionQuery::Info(_) => None,
    };
    let source = assemble_inspection_module(request.preamble, request.imports, expression);
    std::fs::write(&source_path, source)?;

    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .input(&source_path)
        .output_dir(temp.path())
        .inspect_out(&output_path)
        .includes(request.include)
        .session_root(request.session_root)
        .inject_vals(request.inject_modules);
    match &request.query {
        InspectionQuery::TypeOf(expression) => {
            command.inspect_type(expression);
        }
        InspectionQuery::Info(name) => {
            command.inspect_info(name);
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
    decode_inspection(&bytes)
}

fn map_spawn(error: SpawnError) -> CompileError {
    CompileError::Io(crate::extract_spawn_error(error.source))
}

fn decode_inspection(bytes: &[u8]) -> Result<InspectionResult, CompileError> {
    let mut reader = std::io::Cursor::new(bytes);
    let value: CborValue = ciborium::de::from_reader(&mut reader)
        .map_err(|error| invalid(format!("malformed CBOR: {error}")))?;
    if reader.position() != bytes.len() as u64 {
        return Err(invalid("trailing CBOR data"));
    }
    let root = array_len(&value, 2, "receipt")?;
    if text(&root[0], "version")? != "TPINSP001" {
        return Err(invalid("unsupported receipt version"));
    }
    let body = array(&root[1], "body")?;
    let tag = body
        .first()
        .ok_or_else(|| invalid("empty result body"))
        .and_then(|value| text(value, "result tag"))?;
    match tag {
        "Type" => {
            let body = array_len(&root[1], 3, "Type result")?;
            Ok(InspectionResult::Type {
                expression: text(&body[1], "Type expression")?.into(),
                display: text(&body[2], "Type display")?.into(),
            })
        }
        "Info" => {
            let body = array_len(&root[1], 3, "Info result")?;
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
            let body = array_len(&root[1], 3, "Ambiguous result")?;
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
            let body = array_len(&root[1], 2, "NotFound result")?;
            Ok(InspectionResult::NotFound {
                query: text(&body[1], "NotFound query")?.into(),
            })
        }
        other => Err(invalid(format!("unknown result tag {other:?}"))),
    }
}

fn decode_info_entry(value: &CborValue) -> Result<InfoEntry, CompileError> {
    let fields = array_len(value, 4, "Info entry")?;
    let module = match &fields[1] {
        CborValue::Null => None,
        value => Some(text(value, "Info module")?.into()),
    };
    Ok(InfoEntry {
        name: text(&fields[0], "Info name")?.into(),
        module,
        kind: text(&fields[2], "Info kind")?.into(),
        display: text(&fields[3], "Info display")?.into(),
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

    fn encoded(value: CborValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn decodes_every_result_shape() {
        let ty = CborValue::Array(vec![
            CborValue::Text("TPINSP001".into()),
            CborValue::Array(vec![
                CborValue::Text("Type".into()),
                CborValue::Text("fmap".into()),
                CborValue::Text("Functor f => (a -> b) -> f a -> f b".into()),
            ]),
        ]);
        assert!(matches!(
            decode_inspection(&encoded(ty)).unwrap(),
            InspectionResult::Type { .. }
        ));

        let info = CborValue::Array(vec![
            CborValue::Text("TPINSP001".into()),
            CborValue::Array(vec![
                CborValue::Text("Info".into()),
                CborValue::Text("Maybe".into()),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("Maybe".into()),
                    CborValue::Text("GHC.Internal.Maybe".into()),
                    CborValue::Text("type".into()),
                    CborValue::Text("data Maybe a = Nothing | Just a".into()),
                ])]),
            ]),
        ]);
        assert!(matches!(
            decode_inspection(&encoded(info)).unwrap(),
            InspectionResult::Info { .. }
        ));

        let ambiguous = CborValue::Array(vec![
            CborValue::Text("TPINSP001".into()),
            CborValue::Array(vec![
                CborValue::Text("Ambiguous".into()),
                CborValue::Text("Result".into()),
                CborValue::Array(vec![]),
            ]),
        ]);
        assert_eq!(
            decode_inspection(&encoded(ambiguous)).unwrap(),
            InspectionResult::Ambiguous {
                query: "Result".into(),
                entries: vec![],
            }
        );

        let missing = CborValue::Array(vec![
            CborValue::Text("TPINSP001".into()),
            CborValue::Array(vec![
                CborValue::Text("NotFound".into()),
                CborValue::Text("nope".into()),
            ]),
        ]);
        assert_eq!(
            decode_inspection(&encoded(missing)).unwrap(),
            InspectionResult::NotFound {
                query: "nope".into()
            }
        );
    }

    #[test]
    fn rejects_version_shape_and_unknown_tag() {
        for value in [
            CborValue::Array(vec![
                CborValue::Text("TPINSP000".into()),
                CborValue::Array(vec![]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("TPINSP001".into()),
                CborValue::Array(vec![CborValue::Text("Other".into())]),
            ]),
            CborValue::Array(vec![CborValue::Text("TPINSP001".into())]),
        ] {
            assert!(decode_inspection(&encoded(value)).is_err());
        }

        let mut trailing = encoded(CborValue::Array(vec![
            CborValue::Text("TPINSP001".into()),
            CborValue::Array(vec![
                CborValue::Text("NotFound".into()),
                CborValue::Text("x".into()),
            ]),
        ]));
        trailing.push(0);
        assert!(decode_inspection(&trailing).is_err());
    }
}
