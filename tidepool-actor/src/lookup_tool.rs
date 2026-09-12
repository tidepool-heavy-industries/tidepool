//! Model-facing contract for the actor-local `lookup` tool.
//!
//! Scope assembly and GHC inspection remain owned by the resident workbench.
//! This module validates the hosted arguments and presents typed inspection
//! results; it never parses Haskell type syntax.

use serde::{Deserialize, Serialize};
use tidepool_runtime::session::InspectionAvailability;
use tidepool_tool::{HostedTool, ToolDeclaration, ToolKind};

pub(crate) const LOOKUP_TOOL: &str = "lookup";

const LOOKUP_DESCRIPTION: &str = "Look up names, Haskell types, or Shoal documentation. \
Batch example: {\"queries\":[\"awaitSettled\",\":: Int -> Int\",\"doc workbench\"]}. \
Prefix a type query with `::`; use `doc` for topics or `doc <topic>` for a topic. \
Callable results show current-row availability; `unknown` needs more type \
information. Resource grants are checked when an operation executes. \
A bare string is also accepted as one query. \
Each query reports independently in deterministic text, so one bad query does \
not hide other results.";

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
    Doc(String),
    Rejected(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum LookupInputError {
    #[error(
        "lookup arguments must be a string or an object containing only `queries: [string]`: {0}"
    )]
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
        output_schema: None,
        kind: ToolKind::Call,
    })
}

/// Accept a bare string as one query or validate the canonical batch object,
/// then classify each query independently.
///
/// Empty strings are retained as per-query rejections rather than rejecting
/// the whole batch. Haskell after `::` remains opaque for GHC.
pub(crate) fn prepare(
    arguments: serde_json::Value,
) -> Result<Vec<PreparedLookup>, LookupInputError> {
    let arguments = match arguments {
        serde_json::Value::String(query) => LookupArguments {
            queries: vec![query],
        },
        value => serde_json::from_value(value)
            .map_err(|error| LookupInputError::InvalidArguments(error.to_string()))?,
    };
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
            } else if trimmed == "doc" {
                PreparedLookupKind::Doc("topics".into())
            } else if let Some(body) = trimmed
                .strip_prefix("doc")
                .filter(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
            {
                let mut words = body.split_whitespace();
                match (words.next(), words.next()) {
                    (Some(topic), None) => PreparedLookupKind::Doc(topic.into()),
                    (None, _) => PreparedLookupKind::Doc("topics".into()),
                    _ => {
                        PreparedLookupKind::Rejected("documentation query accepts one topic".into())
                    }
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
    Documentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LookupOrigin {
    ModuleExport,
    LiveBinding,
    Documentation,
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
    pub(crate) availability: InspectionAvailability,
}

fn availability_rank(availability: InspectionAvailability) -> u8 {
    match availability {
        InspectionAvailability::Available => 0,
        InspectionAvailability::Unknown => 1,
        InspectionAvailability::Unavailable => 2,
    }
}

fn availability_label(availability: InspectionAvailability) -> &'static str {
    match availability {
        InspectionAvailability::Available => "available",
        InspectionAvailability::Unknown => "unknown",
        InspectionAvailability::Unavailable => "unavailable",
    }
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
            availability_rank(left.availability),
            left.quality.rank(),
            &left.name,
            &left.defining_module,
            &left.signature_or_declaration,
        )
            .cmp(&(
                availability_rank(right.availability),
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
                            .map(|entry| {
                                let signature = entry.signature_or_declaration.trim();
                                match entry.kind {
                                    LookupEntryKind::Value
                                    | LookupEntryKind::ClassMethod
                                    | LookupEntryKind::RecordSelector => format!(
                                        "  [{}] {signature}",
                                        availability_label(entry.availability)
                                    ),
                                    _ => format!("  {signature}"),
                                }
                            })
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
            availability: InspectionAvailability::Available,
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
        assert_eq!(function.output_schema, None);
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
    fn bare_string_is_one_query_with_the_same_classification() {
        assert_eq!(
            prepare(serde_json::json!(" :: Int -> Int ")).unwrap(),
            vec![PreparedLookup {
                query: " :: Int -> Int ".into(),
                kind: PreparedLookupKind::Type("Int -> Int".into()),
            }]
        );
        assert!(matches!(
            prepare(serde_json::json!("  ")).unwrap()[0].kind,
            PreparedLookupKind::Rejected(_)
        ));
    }

    #[test]
    fn documentation_queries_are_local_and_keep_names_distinct() {
        let prepared = prepare(serde_json::json!({
            "queries": ["doc", "doc workbench", "doc unknown", "doc workbench extra", "doctor"]
        }))
        .unwrap();
        assert_eq!(prepared[0].kind, PreparedLookupKind::Doc("topics".into()));
        assert_eq!(
            prepared[1].kind,
            PreparedLookupKind::Doc("workbench".into())
        );
        assert_eq!(prepared[2].kind, PreparedLookupKind::Doc("unknown".into()));
        assert!(matches!(prepared[3].kind, PreparedLookupKind::Rejected(_)));
        assert_eq!(prepared[4].kind, PreparedLookupKind::Name("doctor".into()));
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
        assert!(matches!(
            prepare(serde_json::json!(["awaitSettled"])),
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
    fn row_availability_precedes_match_quality_and_survives_json() {
        let mut unavailable = entry("exact-unavailable", MatchQuality::Exact);
        unavailable.availability = InspectionAvailability::Unavailable;
        let mut unknown = entry("exact-unknown", MatchQuality::Exact);
        unknown.availability = InspectionAvailability::Unknown;
        let available = entry("usable-available", MatchQuality::Usable);
        let result = LookupResult::found(":: Row".into(), vec![unavailable, unknown, available], 2);
        let LookupOutcome::Found { matches, truncated } = &result.outcome else {
            panic!("expected found");
        };
        assert!(*truncated);
        assert_eq!(
            matches
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["usable-available", "exact-unknown"]
        );
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["outcome"]["matches"][0]["availability"], "available");
        assert_eq!(json["outcome"]["matches"][1]["availability"], "unknown");
        let text = LookupResponse {
            results: vec![result],
        }
        .render_text();
        assert!(text.contains("[available] usable-available :: Int"));
        assert!(text.contains("[unknown] exact-unknown :: Int"));
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
