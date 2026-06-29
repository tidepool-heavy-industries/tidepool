//! The `tidepool-repl` session surface — the `SessionCommand` sum (domain §5)
//! and the per-turn outcome it produces.
//!
//! The tool NAME classifies a turn (no decl-vs-expr heuristic, plan §5.0): each
//! MCP tool maps to exactly one [`SessionCommand`] variant. Stringly-typed
//! dispatch is replaced by this sum so a turn's kind is a closed set.

use serde_json::Value as Json;

/// A user-written declaration (the payload of `session_def`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclText(pub String);

/// A user-written expression of type `M a` (the payload of `session_eval`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprText(pub String);

/// A `session_cmd` meta-command (`:bindings` / `:reset` / `:t` / `:i` / `:vocab`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaCommand {
    /// `:t <expr>` — show the inferred type of an expression.
    Type(ExprText),
    /// `:i <name>` — show info for a bound name.
    Info(String),
    /// `:bindings` — list the session's value bindings.
    Bindings,
    /// `:reset` — clear the declaration log and drop the resident machine.
    Reset,
    /// `:vocab` — list verb signatures from the user library dirs.
    Vocab,
}

impl MetaCommand {
    /// Parse a `session_cmd` argument string (`":reset"`, `"reset"`, `":t foo"`,
    /// …) into a [`MetaCommand`]. The leading colon is optional.
    pub fn parse(raw: &str) -> Result<MetaCommand, String> {
        let s = raw.trim();
        let s = s.strip_prefix(':').unwrap_or(s).trim();
        let (head, rest) = match s.split_once(char::is_whitespace) {
            Some((h, r)) => (h, r.trim()),
            None => (s, ""),
        };
        match head {
            "bindings" | "b" => Ok(MetaCommand::Bindings),
            "reset" => Ok(MetaCommand::Reset),
            "t" | "type" => Ok(MetaCommand::Type(ExprText(rest.to_string()))),
            "i" | "info" => Ok(MetaCommand::Info(rest.to_string())),
            "vocab" => Ok(MetaCommand::Vocab),
            other => Err(format!(
                "unknown session command ':{other}' (known: :bindings, :reset, :t, :i, :vocab)"
            )),
        }
    }
}

/// The `tidepool-repl` tool surface as a closed sum (domain model §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    /// `session_def`: append a declaration to the Lane-A decl log and
    /// regenerate `Tidepool.Session.Lib.G<g>`.
    Def(DeclText),
    /// `session_eval`: compile an `M a` expression against the current session
    /// include and run it on the resident machine.
    Eval(ExprText),
    /// `session_cmd`: a meta-command.
    Cmd(MetaCommand),
    /// `session_close`: drop the resident machine and free the session heap.
    Close,
}

/// The result of running a non-`Close` turn (domain §5, adapted for Wave 2 —
/// value binding / `Suspended` are Wave 3b; an in-turn `ask` suspends through
/// the channel layer, not here).
#[derive(Clone, Debug)]
pub enum TurnOutcome {
    /// `session_eval` produced this JSON-rendered value alongside its inferred
    /// Haskell type (GHC `ppr` of the `__user` binding). `type_display` is
    /// `None` only when the extractor is older than the feature or the compiled
    /// module had no `__user` binding (e.g. a pure-reference fallback).
    Value { value: Json, type_display: Option<String> },
    /// `session_eval` bound a value to the live heap (`x <- e` / `let x = e`);
    /// `name` is now referenceable by later turns, with the captured `type_display`.
    Bound { name: String, type_display: String },
    /// `session_eval` bound multiple values from a flat-tuple pattern
    /// (`(a, b) <- e` / `let (x, y) = e`). Each component is independently
    /// referenceable and GC-rooted. `components` is `[(name, type_display)]`.
    MultiBound { components: Vec<(String, String)> },
    /// `session_def` accumulated a declaration; the session advanced to
    /// `generation` and `Tidepool.Session.Lib.G<generation>` now in scope.
    Defined { generation: u64, module: String },
    /// `session_cmd` produced this structured result.
    Meta(Json),
    /// The turn failed (compile error, GHC error, runtime yield, …).
    Error(String),
}

impl TurnOutcome {
    /// Render to a result string for the MCP response. `Error` is kept distinct
    /// (so the caller can flag `is_error`); see [`TurnOutcome::is_error`].
    pub fn render(&self) -> String {
        match self {
            TurnOutcome::Value { value, type_display } => serde_json::json!({
                "type": type_display,
                "value": value,
            })
            .to_string(),
            TurnOutcome::Bound { name, type_display } => serde_json::json!({
                "bound": name,
                "type": type_display,
            })
            .to_string(),
            TurnOutcome::MultiBound { components } => serde_json::json!({
                "bound": components.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                "types": components.iter().map(|(_, t)| t).collect::<Vec<_>>(),
            })
            .to_string(),
            TurnOutcome::Defined { generation, module } => serde_json::json!({
                "defined": true,
                "generation": generation,
                "module": module,
            })
            .to_string(),
            TurnOutcome::Meta(v) => {
                serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
            }
            TurnOutcome::Error(e) => e.clone(),
        }
    }

    /// Whether this outcome should surface as an MCP error result.
    pub fn is_error(&self) -> bool {
        matches!(self, TurnOutcome::Error(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_parse_colon_optional() {
        assert_eq!(MetaCommand::parse(":reset").unwrap(), MetaCommand::Reset);
        assert_eq!(MetaCommand::parse("reset").unwrap(), MetaCommand::Reset);
        assert_eq!(
            MetaCommand::parse(" :bindings ").unwrap(),
            MetaCommand::Bindings
        );
        assert_eq!(
            MetaCommand::parse(":t slug \"a b\"").unwrap(),
            MetaCommand::Type(ExprText("slug \"a b\"".into()))
        );
        assert!(MetaCommand::parse(":nope").is_err());
        assert_eq!(MetaCommand::parse(":vocab").unwrap(), MetaCommand::Vocab);
    }

    #[test]
    fn outcome_render_and_error_flag() {
        let v = TurnOutcome::Value {
            value: serde_json::json!("a-b"),
            type_display: Some("Text".into()),
        };
        assert!(v.render().contains("a-b"));
        assert!(!v.is_error());
        let e = TurnOutcome::Error("boom".into());
        assert_eq!(e.render(), "boom");
        assert!(e.is_error());
    }

    #[test]
    fn value_outcome_render_includes_type_and_value() {
        let v = TurnOutcome::Value {
            value: serde_json::json!(42),
            type_display: Some("M Int".into()),
        };
        let rendered = v.render();
        assert!(rendered.contains("M Int"), "type field missing: {rendered}");
        assert!(rendered.contains("42"), "value field missing: {rendered}");

        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("render is valid JSON");
        assert_eq!(parsed["type"], "M Int");
        assert_eq!(parsed["value"], 42);
    }

    #[test]
    fn value_outcome_render_null_type_when_absent() {
        let v = TurnOutcome::Value { value: serde_json::json!(true), type_display: None };
        let rendered = v.render();
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("render is valid JSON");
        assert!(parsed["type"].is_null(), "expected null type: {rendered}");
        assert_eq!(parsed["value"], true);
    }
}
