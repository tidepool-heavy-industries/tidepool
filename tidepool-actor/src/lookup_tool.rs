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
Type search is Hoogle-like and needs a complete type: use `_` to wildcard an \
unknown part and qualify types as they are imported, e.g. \
`:: Cmd.Command -> _` finds functions from `Cmd.Command` to anything. \
A dotted capitalized query, e.g. `Cmd.RunResult` or `Project.Investigate`, is \
resolved first as a qualified name and, only when no such name is in scope, \
browsed as a module's exports; see `doc topics` for the workspace's own modules. \
Callable results show current-row availability: `polymorphic` fits your row and \
its remaining constraint is decided by the call site, so it is usable; `unknown` \
needs more type information. Resource grants are checked when an operation \
executes. \
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
    /// Dotted and capitalized throughout: `Cmd.RunResult` is a qualified type
    /// or constructor, `Project.Investigate` is a module, and the spelling
    /// alone does not say which. Resolved as a name first and browsed as a
    /// module only when no such name is in scope.
    Qualified(String),
    Type(String),
    Doc(String),
    Rejected(String),
}

/// Dotted with every segment capitalized. That is the spelling a module has,
/// and equally the spelling a qualified type or constructor has (`Cmd.RunResult`,
/// `Maybe.Just`), so it cannot be classified here — only GHC knows which names
/// the turn's scope carries. A lowercase-led final segment (`Cmd.run`) is a
/// plain `Name` query, and so is a bare capitalized word (`Maybe`).
fn is_dotted_capitalized(candidate: &str) -> bool {
    let mut segments = 0;
    for segment in candidate.split('.') {
        segments += 1;
        let mut chars = segment.chars();
        match chars.next() {
            Some(first) if first.is_ascii_uppercase() => {}
            _ => return false,
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'') {
            return false;
        }
    }
    segments >= 2
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
                } else if body.ends_with("->") {
                    PreparedLookupKind::Rejected(
                        "type search needs a complete type; use `_` for unknown parts, \
                         e.g. `:: T -> _`"
                            .into(),
                    )
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
            } else if is_dotted_capitalized(trimmed) {
                PreparedLookupKind::Qualified(trimmed.into())
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
        InspectionAvailability::Polymorphic => 1,
        InspectionAvailability::Unknown => 2,
        InspectionAvailability::Unavailable => 3,
    }
}

/// A type query is compiled as a generated `Expr.hs` under a temporary
/// directory, and GHC's diagnostic arrives carrying that path. A model reading
/// `/tmp/nix-shell.abc/.tmpXYZ/query-5/Expr.hs:60:33: Not in scope: …` cannot
/// tell the file is not its own, and the coordinates describe generated source
/// it never wrote. Keep the message and drop the location.
///
/// `tidepool-toolchain`'s `render_diagnostics` does this properly for ordinary
/// compiles, but it needs structured spans and this path is a flat string by the
/// time it leaves the worker, so the same anchor rule is applied here.
fn strip_generated_query_locations(diagnostic: &str) -> String {
    diagnostic
        .lines()
        .map(strip_generated_query_location)
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_generated_query_location(line: &str) -> &str {
    const ANCHOR: &str = "Expr.hs";
    let Some((head, message)) = line.split_once(": ") else {
        return line;
    };
    let mut segments = head.trim_start().rsplitn(3, ':');
    let (Some(column), Some(row), Some(path)) =
        (segments.next(), segments.next(), segments.next())
    else {
        return line;
    };
    if !is_span_coordinate(column) || !is_span_coordinate(row) {
        return line;
    }
    // The anchor must end a path component, never be an embedded suffix, so
    // a real `SomeExpr.hs` in the workspace keeps its location.
    let matches_anchor = path == ANCHOR
        || (path.ends_with(ANCHOR)
            && path.len() > ANCHOR.len()
            && matches!(path.as_bytes()[path.len() - ANCHOR.len() - 1], b'/' | b'\\'));
    if matches_anchor {
        message
    } else {
        line
    }
}

/// A GHC span coordinate: `60`, or `33-41` for a range.
fn is_span_coordinate(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
        && text.bytes().any(|byte| byte.is_ascii_digit())
}

fn availability_label(availability: InspectionAvailability) -> &'static str {
    match availability {
        InspectionAvailability::Available => "available",
        InspectionAvailability::Polymorphic => "polymorphic",
        InspectionAvailability::Unknown => "unknown",
        InspectionAvailability::Unavailable => "unavailable",
    }
}

/// How a query was put to GHC. A miss reports these so the reader can tell
/// "this does not exist" from "this was never asked that way".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LookupInterpretation {
    Name,
    Module,
    TypeSearch,
}

impl LookupInterpretation {
    /// What this interpretation reports when it finds nothing.
    fn miss(self) -> &'static str {
        match self {
            Self::Name => "not in scope as a name",
            Self::Module => "no module of that name",
            Self::TypeSearch => "no value with that type",
        }
    }
}

/// A bare `no match` hides which question was asked. A live lead read one for
/// `Cmd.CommandResult`, which had been classified as a module and never tried
/// as a name, and spent a turn investigating whether the type existed at all.
/// Every miss names the interpretations that produced it.
fn describe_misses(attempted: &[LookupInterpretation]) -> String {
    let misses = attempted
        .iter()
        .map(|interpretation| interpretation.miss())
        .collect::<Vec<_>>();
    match misses.as_slice() {
        [] => "nothing was looked up".into(),
        [only] => (*only).into(),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum LookupOutcome {
    Found {
        matches: Vec<LookupEntry>,
        truncated: bool,
    },
    NotFound {
        attempted: Vec<LookupInterpretation>,
    },
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
                    LookupOutcome::NotFound { attempted } => {
                        format!("  no match: {}", describe_misses(attempted))
                    }
                    LookupOutcome::Rejected { diagnostic } => {
                        format!(
                            "  error: {}",
                            strip_generated_query_locations(diagnostic).trim()
                        )
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
    fn dotted_capitalized_query_is_a_qualified_lookup_and_other_names_are_unchanged() {
        let prepared = prepare(serde_json::json!({
            "queries": [
                "Project.Investigate",
                "Tidepool.Actor.Record",
                "Cmd.run",
                "awaitSettled",
                "Maybe",
            ]
        }))
        .unwrap();
        assert_eq!(
            prepared[0].kind,
            PreparedLookupKind::Qualified("Project.Investigate".into())
        );
        assert_eq!(
            prepared[1].kind,
            PreparedLookupKind::Qualified("Tidepool.Actor.Record".into())
        );
        // A qualified value (lowercase-led final segment) is a plain Name
        // query: it cannot be a module, so nothing is browsed for it.
        assert_eq!(prepared[2].kind, PreparedLookupKind::Name("Cmd.run".into()));
        assert_eq!(
            prepared[3].kind,
            PreparedLookupKind::Name("awaitSettled".into())
        );
        // A bare capitalized word with no dot stays a Name query too — that is
        // how a type or constructor is already found today.
        assert_eq!(prepared[4].kind, PreparedLookupKind::Name("Maybe".into()));
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
    fn incomplete_type_query_is_rejected_with_a_wildcard_hint() {
        let prepared = prepare(serde_json::json!({
            "queries": [":: Cmd.Command ->", ":: Cmd.Command -> _"]
        }))
        .unwrap();
        match &prepared[0].kind {
            PreparedLookupKind::Rejected(diagnostic) => {
                assert!(diagnostic.contains('_'));
                assert!(diagnostic.contains("::"));
            }
            other => panic!("expected rejection, got {other:?}"),
        }
        assert_eq!(
            prepared[1].kind,
            PreparedLookupKind::Type("Cmd.Command -> _".into())
        );
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
        // Usable, with a constraint the call site decides: it must outrank
        // `unknown`, because a model that skips it skips a working name.
        let mut polymorphic = entry("exact-polymorphic", MatchQuality::Exact);
        polymorphic.availability = InspectionAvailability::Polymorphic;
        let available = entry("usable-available", MatchQuality::Usable);
        let result = LookupResult::found(
            ":: Row".into(),
            vec![unavailable, unknown, polymorphic, available],
            2,
        );
        let LookupOutcome::Found { matches, truncated } = &result.outcome else {
            panic!("expected found");
        };
        assert!(*truncated);
        assert_eq!(
            matches
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["usable-available", "exact-polymorphic"]
        );
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["outcome"]["matches"][0]["availability"], "available");
        assert_eq!(json["outcome"]["matches"][1]["availability"], "polymorphic");
        let text = LookupResponse {
            results: vec![result],
        }
        .render_text();
        assert!(text.contains("[available] usable-available :: Int"));
        assert!(text.contains("[polymorphic] exact-polymorphic :: Int"));
    }

    #[test]
    fn a_rejection_keeps_its_message_and_drops_the_generated_query_location() {
        // The exact string a live lead was shown. The path is the lookup tool's
        // own scratch module; nothing about it is actionable, and a reader
        // cannot tell the file is not theirs.
        let response = LookupResponse {
            results: vec![LookupResult {
                query: ":: _ -> Command".into(),
                outcome: LookupOutcome::Rejected {
                    diagnostic: "/tmp/nix-shell.1bCMmT/.tmpQMOXBc/query-5/Expr.hs:60:33: \
                                 Not in scope: type constructor or class `Command'"
                        .into(),
                },
            }],
        };
        let rendered = response.render_text();
        assert_eq!(
            rendered,
            ":: _ -> Command\n  error: Not in scope: type constructor or class `Command'"
        );
        assert!(!rendered.contains("nix-shell"), "{rendered}");
        assert!(!rendered.contains("Expr.hs"), "{rendered}");

        // A column range is still a span, and every line of a multi-line
        // diagnostic is cleaned.
        assert_eq!(
            strip_generated_query_locations(
                "/tmp/q/Expr.hs:12:1-9: first\n/tmp/q/Expr.hs:13:2: second"
            ),
            "first\nsecond"
        );

        // A real file in the workspace keeps its location, including one whose
        // name merely ends in the anchor.
        for untouched in [
            "src/store.rs:14:2: Not in scope: thing",
            "/home/me/SomeExpr.hs:3:4: Not in scope: thing",
            "Not in scope: thing",
        ] {
            assert_eq!(strip_generated_query_locations(untouched), untouched);
        }
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
                    outcome: LookupOutcome::NotFound {
                        attempted: vec![LookupInterpretation::Name],
                    },
                },
            ],
        };
        let rendered = response.render_text();
        assert!(rendered.find("awaitSettled").unwrap() < rendered.find("NotInScope").unwrap());
        assert!(rendered.contains("awaitSettled :: Int"));
        assert!(rendered.contains("error: Not in scope"));
        assert!(rendered.ends_with("missing\n  no match: not in scope as a name"));
    }

    #[test]
    fn a_missing_qualified_query_names_both_interpretations_it_was_given() {
        // The exact shape that cost a live lead a turn: it read `no match` for
        // `Cmd.CommandResult`, could not tell the query had been browsed as a
        // module and never tried as a name, and went looking for the type.
        let response = LookupResponse {
            results: vec![LookupResult {
                query: "Cmd.CommandResult".into(),
                outcome: LookupOutcome::NotFound {
                    attempted: vec![LookupInterpretation::Name, LookupInterpretation::Module],
                },
            }],
        };
        assert_eq!(
            response.render_text(),
            "Cmd.CommandResult\n  no match: not in scope as a name, and no module of that name"
        );
        assert_eq!(
            serde_json::to_value(&response).unwrap()["results"][0]["outcome"],
            serde_json::json!({"status": "not_found", "attempted": ["name", "module"]})
        );
        // One interpretation reports only its own miss; a type query says which
        // search came back empty.
        assert_eq!(
            LookupResponse {
                results: vec![LookupResult {
                    query: ":: Int -> Cmd.RunResult".into(),
                    outcome: LookupOutcome::NotFound {
                        attempted: vec![LookupInterpretation::TypeSearch],
                    },
                }],
            }
            .render_text(),
            ":: Int -> Cmd.RunResult\n  no match: no value with that type"
        );
    }
}
