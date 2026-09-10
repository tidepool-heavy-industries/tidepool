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
    Browse { module: String, expanded: bool },
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
        }
    }
}

pub struct InspectionRequest<'a> {
    pub preamble: &'a str,
    pub imports: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
    pub queries: &'a [InspectionQuery],
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
    for (index, query) in request.queries.iter().enumerate() {
        let query_dir = temp.path().join(format!("query-{index}"));
        std::fs::create_dir(&query_dir)?;
        let source_path = query_dir.join("Expr.hs");
        let expressions = match query {
            InspectionQuery::TypeOf(expression) => std::slice::from_ref(expression),
            InspectionQuery::Info(_) | InspectionQuery::Browse { .. } => &[],
        };
        let source = assemble_inspection_module(request.preamble, request.imports, expressions);
        std::fs::write(&source_path, source)?;
        command.input(&source_path);
        match query {
            InspectionQuery::TypeOf(expression) => {
                command.inspect_type(expression);
            }
            InspectionQuery::Info(name) => {
                command.inspect_info(name);
            }
            InspectionQuery::Browse { module, expanded } => {
                command.inspect_browse(module, *expanded);
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
    decode_inspections(&bytes)
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
    if text(&root[0], "version")? != "TPINSP002" {
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
            let body = array_len(value, 3, "Type result")?;
            Ok(InspectionResult::Type {
                expression: text(&body[1], "Type expression")?.into(),
                display: text(&body[2], "Type display")?.into(),
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
        other => Err(invalid(format!("unknown result tag {other:?}"))),
    }
}

fn boolean(value: &CborValue, what: &str) -> Result<bool, CompileError> {
    match value {
        CborValue::Bool(value) => Ok(*value),
        _ => Err(invalid(format!("{what} must be boolean"))),
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
    use tidepool_testing::eval_harness;

    fn encoded(value: CborValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn decodes_every_result_shape() {
        let receipt = CborValue::Array(vec![
            CborValue::Text("TPINSP002".into()),
            CborValue::Array(vec![
                CborValue::Array(vec![
                    CborValue::Text("Type".into()),
                    CborValue::Text("fmap".into()),
                    CborValue::Text("Functor f => (a -> b) -> f a -> f b".into()),
                ]),
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
        assert!(matches!(decoded[0], InspectionResult::Type { .. }));
        assert!(matches!(decoded[1], InspectionResult::Info { .. }));
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
    fn rejects_version_shape_and_unknown_tag() {
        for value in [
            CborValue::Array(vec![
                CborValue::Text("TPINSP000".into()),
                CborValue::Array(vec![]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("TPINSP002".into()),
                CborValue::Array(vec![CborValue::Text("Other".into())]),
            ]),
            CborValue::Array(vec![CborValue::Text("TPINSP001".into())]),
        ] {
            assert!(decode_inspections(&encoded(value)).is_err());
        }

        let mut trailing = encoded(CborValue::Array(vec![
            CborValue::Text("TPINSP002".into()),
            CborValue::Array(vec![CborValue::Array(vec![
                CborValue::Text("NotFound".into()),
                CborValue::Text("x".into()),
            ])]),
        ]));
        trailing.push(0);
        assert!(decode_inspections(&trailing).is_err());
    }

    #[test]
    fn one_inspection_compile_answers_type_info_and_browse_queries() {
        eval_harness::require_extract();
        let include = tempfile::tempdir().unwrap();
        let session = tempfile::tempdir().unwrap();
        std::fs::write(
            include.path().join("BrowseFixture.hs"),
            concat!(
                "module BrowseFixture (Public(..), Service(..), exportedValue, Maybe(..)) where\n",
                "import Prelude\n",
                "import Data.Maybe (Maybe(..))\n",
                "data Public = First | Second\n",
                "class Service a where service :: a -> Int\n",
                "exportedValue :: Int\n",
                "exportedValue = 42\n",
            ),
        )
        .unwrap();
        let preamble = concat!(
            "{-# LANGUAGE NoImplicitPrelude, NoMonomorphismRestriction #-}\n",
            "module Expr where\n",
            "import BrowseFixture\n",
            "import qualified BrowseFixture as Alias\n",
        );
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
        ];
        queries.push(InspectionQuery::Info("Alias.Public".into()));
        queries.push(InspectionQuery::Info("Alias.exportedValue".into()));
        queries.push(InspectionQuery::Info("Missing.Public".into()));
        let results = run_inspections(InspectionRequest {
            preamble,
            imports: "",
            include: &[include.path()],
            session_root: session.path(),
            inject_modules: &[],
            queries: &queries,
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
    }
}
