//! Model-facing contract for the actor-local `lookup` tool.
//!
//! Scope assembly and GHC inspection remain owned by the resident workbench.
//! This module validates the hosted arguments and presents typed inspection
//! results; it never parses Haskell type syntax.

#![allow(
    dead_code,
    reason = "coordinator-owned resident dispatch consumes this checked adapter in the integration join"
)]

use serde::{Deserialize, Serialize};
use tidepool_tool::{HostedTool, ToolDeclaration, ToolKind};

pub(crate) const LOOKUP_TOOL: &str = "lookup";

const LOOKUP_DESCRIPTION: &str = "Look up names or Haskell types in the current \
actor scope. Pass several queries together; prefix a type query with `::`. \
Each query reports independently, so one bad query does not hide other results.";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupArguments {
    queries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLookup {
    pub(crate) query: String,
    pub(crate) kind: PreparedLookupKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedLookupKind {
    Name(String),
    Type(String),
    Rejected(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum LookupInputError {
    #[error("lookup arguments must be an object containing only `queries: [string]`: {0}")]
    InvalidArguments(String),
    #[error("lookup requires at least one query")]
    EmptyBatch,
}

/// The declaration is kept beside argument validation so the advertised and
/// accepted shapes have one owner. Registration remains the host's job.
pub(crate) fn declaration() -> HostedTool {
    HostedTool::Function(ToolDeclaration {
        name: LOOKUP_TOOL.into(),
        description: LOOKUP_DESCRIPTION.into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "queries": {
                    "type": "array",
                    "minItems": 1,
                    "items": {"type": "string"}
                }
            },
            "required": ["queries"],
            "additionalProperties": false
        }),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "results": {"type": "array"}
            },
            "required": ["results"],
            "additionalProperties": false
        })),
        kind: ToolKind::Call,
    })
}

/// Validate the canonical object once, then classify each query independently.
///
/// Empty strings are retained as per-query rejections rather than rejecting
/// the whole batch. Haskell after `::` remains opaque for GHC.
pub(crate) fn prepare(
    arguments: serde_json::Value,
) -> Result<Vec<PreparedLookup>, LookupInputError> {
    let arguments: LookupArguments = serde_json::from_value(arguments)
        .map_err(|error| LookupInputError::InvalidArguments(error.to_string()))?;
    if arguments.queries.is_empty() {
        return Err(LookupInputError::EmptyBatch);
    }
    Ok(arguments
        .queries
        .into_iter()
        .map(|query| {
            let trimmed = query.trim();
            let kind = if trimmed.is_empty() {
                PreparedLookupKind::Rejected("empty lookup query".into())
            } else if let Some(body) = trimmed.strip_prefix("::") {
                let body = body.trim();
                if body.is_empty() {
                    PreparedLookupKind::Rejected("type query after `::` is empty".into())
                } else {
                    PreparedLookupKind::Type(body.into())
                }
            } else {
                PreparedLookupKind::Name(trimmed.into())
            };
            PreparedLookup { query, kind }
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LookupEntryKind {
    Value,
    ClassMethod,
    RecordSelector,
    Constructor,
    Type,
    Coercion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LookupOrigin {
    ModuleExport,
    LiveBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MatchQuality {
    Exact,
    Usable,
}

impl MatchQuality {
    fn rank(self) -> u8 {
        match self {
            Self::Exact => 0,
            Self::Usable => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LookupEntry {
    pub(crate) name: String,
    pub(crate) defining_module: Option<String>,
    pub(crate) kind: LookupEntryKind,
    pub(crate) signature_or_declaration: String,
    pub(crate) origin: LookupOrigin,
    pub(crate) quality: MatchQuality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum LookupOutcome {
    Found {
        matches: Vec<LookupEntry>,
        truncated: bool,
    },
    NotFound,
    Ambiguous {
        matches: Vec<LookupEntry>,
        truncated: bool,
    },
    Rejected {
        diagnostic: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LookupResult {
    pub(crate) query: String,
    pub(crate) outcome: LookupOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LookupResponse {
    pub(crate) results: Vec<LookupResult>,
}

impl LookupResult {
    pub(crate) fn found(query: String, matches: Vec<LookupEntry>, limit: usize) -> Self {
        Self {
            query,
            outcome: bounded_matches(matches, limit, |matches, truncated| LookupOutcome::Found {
                matches,
                truncated,
            }),
        }
    }

    pub(crate) fn ambiguous(query: String, matches: Vec<LookupEntry>, limit: usize) -> Self {
        Self {
            query,
            outcome: bounded_matches(matches, limit, |matches, truncated| {
                LookupOutcome::Ambiguous { matches, truncated }
            }),
        }
    }
}

fn bounded_matches(
    mut matches: Vec<LookupEntry>,
    limit: usize,
    outcome: impl FnOnce(Vec<LookupEntry>, bool) -> LookupOutcome,
) -> LookupOutcome {
    matches.sort_by(|left, right| {
        (
            left.quality.rank(),
            &left.name,
            &left.defining_module,
            &left.signature_or_declaration,
        )
            .cmp(&(
                right.quality.rank(),
                &right.name,
                &right.defining_module,
                &right.signature_or_declaration,
            ))
    });
    let truncated = matches.len() > limit;
    matches.truncate(limit);
    outcome(matches, truncated)
}

impl LookupResponse {
    /// Compact deterministic text for the hosted result. The structured value
    /// remains authoritative; this rendering never drives lookup behavior.
    pub(crate) fn render_text(&self) -> String {
        self.results
            .iter()
            .map(|result| {
                let body = match &result.outcome {
                    LookupOutcome::Found { matches, truncated }
                    | LookupOutcome::Ambiguous { matches, truncated } => {
                        let mut lines = matches
                            .iter()
                            .map(|entry| format!("  {}", entry.signature_or_declaration.trim()))
                            .collect::<Vec<_>>();
                        if *truncated {
                            lines.push("  … more matches omitted".into());
                        }
                        lines.join("\n")
                    }
                    LookupOutcome::NotFound => "  no match".into(),
                    LookupOutcome::Rejected { diagnostic } => {
                        format!("  error: {}", diagnostic.trim())
                    }
                };
                format!("{}\n{body}", result.query)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_tool::ToolArguments;

    fn entry(name: &str, quality: MatchQuality) -> LookupEntry {
        LookupEntry {
            name: name.into(),
            defining_module: Some("Tidepool.Actors".into()),
            kind: LookupEntryKind::Value,
            signature_or_declaration: format!("{name} :: Int"),
            origin: LookupOrigin::ModuleExport,
            quality,
        }
    }

    #[test]
    fn declaration_advertises_only_the_structured_batch() {
        let tool = declaration();
        assert_eq!(tool.name(), "lookup");
        assert!(tool.accepts(&ToolArguments::Structured(serde_json::json!({
            "queries": ["awaitSettled"]
        }))));
        assert!(!tool.accepts(&ToolArguments::Raw("awaitSettled".into())));
        let HostedTool::Function(function) = tool else {
            panic!("lookup must be a function tool");
        };
        assert_eq!(
            function.input_schema["required"],
            serde_json::json!(["queries"])
        );
        assert_eq!(function.input_schema["additionalProperties"], false);
        assert_eq!(
            function.input_schema["properties"]["queries"]["minItems"],
            1
        );
    }

    #[test]
    fn preparation_preserves_order_duplicates_and_per_query_rejections() {
        let prepared = prepare(serde_json::json!({
            "queries": [
                " awaitSettled ",
                ":: Response result -> Await (Settlement result)",
                "",
                ":: ",
                "awaitSettled"
            ]
        }))
        .unwrap();
        assert_eq!(prepared.len(), 5);
        assert_eq!(
            prepared[0].kind,
            PreparedLookupKind::Name("awaitSettled".into())
        );
        assert_eq!(
            prepared[1].kind,
            PreparedLookupKind::Type("Response result -> Await (Settlement result)".into())
        );
        assert!(matches!(prepared[2].kind, PreparedLookupKind::Rejected(_)));
        assert!(matches!(prepared[3].kind, PreparedLookupKind::Rejected(_)));
        assert_eq!(
            prepared[4].kind,
            PreparedLookupKind::Name("awaitSettled".into())
        );
    }

    #[test]
    fn outer_shape_errors_do_not_become_query_results() {
        assert!(matches!(
            prepare(serde_json::json!({"queries": []})),
            Err(LookupInputError::EmptyBatch)
        ));
        assert!(matches!(
            prepare(serde_json::json!({"query": "awaitSettled"})),
            Err(LookupInputError::InvalidArguments(_))
        ));
        assert!(matches!(
            prepare(serde_json::json!({"queries": [1]})),
            Err(LookupInputError::InvalidArguments(_))
        ));
    }

    #[test]
    fn bounded_results_are_exact_first_stable_and_explicitly_truncated() {
        let result = LookupResult::found(
            ":: Int".into(),
            vec![
                entry("zeta", MatchQuality::Usable),
                entry("beta", MatchQuality::Exact),
                entry("alpha", MatchQuality::Exact),
            ],
            2,
        );
        let LookupOutcome::Found { matches, truncated } = result.outcome else {
            panic!("expected found");
        };
        assert!(truncated);
        assert_eq!(
            matches
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }

    #[test]
    fn mixed_results_render_independently_in_input_order() {
        let response = LookupResponse {
            results: vec![
                LookupResult::found(
                    "awaitSettled".into(),
                    vec![entry("awaitSettled", MatchQuality::Exact)],
                    8,
                ),
                LookupResult {
                    query: ":: NotInScope".into(),
                    outcome: LookupOutcome::Rejected {
                        diagnostic: "Not in scope: type constructor `NotInScope`".into(),
                    },
                },
                LookupResult {
                    query: "missing".into(),
                    outcome: LookupOutcome::NotFound,
                },
            ],
        };
        let rendered = response.render_text();
        assert!(rendered.find("awaitSettled").unwrap() < rendered.find("NotInScope").unwrap());
        assert!(rendered.contains("awaitSettled :: Int"));
        assert!(rendered.contains("error: Not in scope"));
        assert!(rendered.ends_with("missing\n  no match"));
    }
}
