//! The `tidepool-repl` session surface — the `SessionCommand` sum (domain §5)
//! and the per-turn outcome it produces.
//!
//! `session_run` classifies each item in a block into a [`BlockItem`] (decl /
//! stmt / meta) and runs them via the existing `run_def`/`run_eval`/`run_meta`
//! handlers. `SessionCommand::{Def,Eval,Cmd}` are kept as the per-item handler
//! targets; `Block` is the new composite variant.

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
    /// `:i <name>` — show info for a bound name or type/constructor definition.
    Info(String),
    /// `:bindings` — list the session's value bindings.
    Bindings,
    /// `:reset` — clear the declaration log and drop the resident machine.
    Reset,
    /// `:vocab` — list verb signatures from the user library dirs.
    Vocab,
}

/// One item in a `session_run` block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockItem {
    /// A top-level Haskell declaration (keyword-initiated, unambiguous) —
    /// routed directly to `run_def` with no cascade.
    Decl(DeclText),
    /// A bind statement (`x <- e` / `let x = e`) or bare expression — routed
    /// to `run_eval`, which classifies bind vs expr internally via `classify_turn`.
    Stmt(ExprText),
    /// An ambiguous item that needs the try-cascade: `run_block` attempts it
    /// as a declaration via `run_def` first; on a GHC parse error it falls
    /// back to `run_eval`. A non-parse error (type error, scope error, …) is
    /// returned as-is — the item IS a declaration, just a broken one.
    Auto(ExprText),
    /// A meta-command (`:reset`, `:t`, …) — routed to `run_meta`.
    Meta(MetaCommand),
}

/// The outcome of one item in a `session_run` block (serialised into the
/// aggregate `TurnOutcome::Block.items` array).
#[derive(Clone, Debug)]
pub struct BlockItemResult {
    /// Zero-based position in the block.
    pub index: usize,
    /// Item kind string: `"decl"`, `"stmt"`, or `"meta"`.
    pub kind: String,
    /// Whether the item succeeded.
    pub ok: bool,
    /// Verbatim `TurnOutcome::render()` of this item's outcome — identical to
    /// the legacy per-tool render so a 1-item block unwraps to the old shape.
    pub result: String,
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
    /// `session_run`: run a list of classified items in sequence on the resident
    /// machine. Each item is dispatched to `run_def`/`run_eval`/`run_meta`.
    Block(Vec<BlockItem>),
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
    Value {
        value: Json,
        type_display: Option<String>,
    },
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
    /// `session_run` block result: per-item outcomes + the last expression value.
    Block {
        items: Vec<BlockItemResult>,
        /// The value produced by the last value-yielding `Stmt` in the block
        /// (`None` if no expression was evaluated or the block errored before
        /// any expression ran).
        value: Option<Json>,
        /// Declaration generation at block completion.
        generation: u64,
        /// Value-binding generation at block completion.
        val_gen: u64,
    },
    /// The turn failed (compile error, GHC error, runtime yield, …).
    Error(String),
}

impl TurnOutcome {
    /// Render to a result string for the MCP response. `Error` is kept distinct
    /// (so the caller can flag `is_error`); see [`TurnOutcome::is_error`].
    pub fn render(&self) -> String {
        match self {
            TurnOutcome::Value {
                value,
                type_display,
            } => serde_json::json!({
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
            TurnOutcome::Block {
                items,
                value,
                generation,
                val_gen,
            } => {
                let items_json: Vec<serde_json::Value> = items
                    .iter()
                    .map(|i| {
                        serde_json::json!({
                            "index": i.index,
                            "kind": i.kind,
                            "ok": i.ok,
                            "result": i.result,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "items": items_json,
                    "value": value,
                    "generation": generation,
                    "valGeneration": val_gen,
                })
                .to_string()
            }
            TurnOutcome::Error(e) => e.clone(),
        }
    }

    /// Whether this outcome should surface as an MCP error result.
    pub fn is_error(&self) -> bool {
        match self {
            TurnOutcome::Error(_) => true,
            // A block is an error when the last recorded item failed (we stop
            // on first error and include the failing item in `items`).
            TurnOutcome::Block { items, .. } => items.last().is_some_and(|i| !i.ok),
            _ => false,
        }
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
        let v = TurnOutcome::Value {
            value: serde_json::json!(true),
            type_display: None,
        };
        let rendered = v.render();
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("render is valid JSON");
        assert!(parsed["type"].is_null(), "expected null type: {rendered}");
        assert_eq!(parsed["value"], true);
    }
}
