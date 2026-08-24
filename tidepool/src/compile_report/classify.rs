//! The GHC-error-text classifier: maps one compile-failure error string onto
//! a desire-path bucket plus the identifiers named in it.
//!
//! This is a small pure function over GHC's stable error phrasing — the
//! literal prefixes GHC has used for these diagnostics across every captured
//! sample in this codebase's own dogfood logs (`tests` below cites the exact
//! source of every fixture). No `regex` dependency: every pattern here is a
//! fixed literal prefix followed by a delimited token, which plain
//! `str::find`/slicing handles without pulling in a new crate for a workspace
//! that does not already depend on `regex` from this crate.
//!
//! Order matters: [`classify_error`] checks the most specific phrasing first
//! (e.g. "type constructor or class" before the bare "not in scope" it is a
//! superstring of) so a single error text is never misrouted to a broader
//! bucket that happens to share a substring.

/// One "most-reached-for-but-unsupported construct" bucket a GHC error text
/// falls into. Named per `docs/GLOSSARY.md`'s composition-over-coinage rule —
/// every name here is the plain description of the GHC/JIT failure shape it
/// covers, not an invented label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DesireBucket {
    VariableNotInScope,
    TypeConstructorNotInScope,
    ModuleNotFound,
    MissingInstance,
    UnsupportedPrimopJitGap,
    ImportGrammarRejection,
    WrapperOrigin,
    Other,
}

impl DesireBucket {
    /// Stable lowercase-hyphen tag — safe to print in a human table or a
    /// JSON key, and to grep across report runs.
    pub fn tag(self) -> &'static str {
        match self {
            DesireBucket::VariableNotInScope => "variable-not-in-scope",
            DesireBucket::TypeConstructorNotInScope => "type-constructor-not-in-scope",
            DesireBucket::ModuleNotFound => "module-not-found",
            DesireBucket::MissingInstance => "missing-instance",
            DesireBucket::UnsupportedPrimopJitGap => "unsupported-primop-jit-gap",
            DesireBucket::ImportGrammarRejection => "import-grammar-rejection",
            DesireBucket::WrapperOrigin => "wrapper-origin",
            DesireBucket::Other => "other",
        }
    }

    /// Every bucket, in the fixed reporting order (not alphabetical — the
    /// order the spec named them in, most-actionable first).
    pub fn all() -> [DesireBucket; 8] {
        [
            DesireBucket::VariableNotInScope,
            DesireBucket::TypeConstructorNotInScope,
            DesireBucket::ModuleNotFound,
            DesireBucket::MissingInstance,
            DesireBucket::UnsupportedPrimopJitGap,
            DesireBucket::ImportGrammarRejection,
            DesireBucket::WrapperOrigin,
            DesireBucket::Other,
        ]
    }
}

/// One error text's classification: which bucket it fell into, and every
/// named identifier the matching pattern found (a multi-diagnostic error
/// naming the same missing type three times contributes three occurrences,
/// so a report's "top identifiers" ranking reflects how often each one was
/// actually reached for, not just how many rounds mentioned it at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub bucket: DesireBucket,
    pub identifiers: Vec<String>,
}

/// Find every occurrence of `needle` in `text` and, for each, the identifier
/// token immediately following it: leading whitespace/newlines are skipped
/// (GHC sometimes wraps the identifier onto the next line, e.g. `"Variable
/// not in scope:\n  forkAll"`), then a run of identifier characters
/// (`[A-Za-z0-9_']`) is read — stopping at whitespace, `::`, or punctuation.
fn identifiers_after(text: &str, needle: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find(needle) {
        let after = &rest[idx + needle.len()..];
        let trimmed = after.trim_start();
        let tok: String = trimmed
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '\'')
            .collect();
        if !tok.is_empty() {
            out.push(tok);
        }
        // Advance past this occurrence so a repeated needle later in the
        // text is still found.
        rest = after;
    }
    out
}

/// Find every quoted name after `needle` — GHC quotes a module/constructor
/// name either the old way (`` `Name' ``, ASCII backtick + apostrophe) or the
/// unicode-quotes way (`'Name'`/`Name`Left single quote+right single quote,
/// U+2018/U+2019) depending on terminal capability detection; both are
/// captured here since either can appear in a durable log written from a
/// different environment than this reader runs in.
fn quoted_after(text: &str, needle: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find(needle) {
        let after = &rest[idx + needle.len()..];
        let trimmed = after.trim_start();
        let opens: &[char] = &['`', '\u{2018}', '\''];
        if let Some(first) = trimmed.chars().next() {
            if opens.contains(&first) {
                let body = &trimmed[first.len_utf8()..];
                let closes: &[char] = &['\'', '\u{2019}'];
                if let Some(end) = body.find(closes) {
                    out.push(body[..end].to_string());
                }
            }
        }
        rest = after;
    }
    out
}

/// THE classifier: one error text in, one bucket + its named identifiers out.
/// A text with no recognized GHC/JIT phrasing lands in [`DesireBucket::Other`]
/// with no identifiers — the catch-all is a real signal (it is exactly what
/// a ranked report surfaces as "recurring, not yet named" desire paths), not
/// a failure of the classifier.
pub fn classify_error(text: &str) -> Classification {
    // Most specific / least ambiguous phrasing first, so a broader pattern
    // never shadows a narrower one it happens to contain as a substring.

    // Tidepool's own wrapper-attribution note — never a raw GHC message, so
    // check it before anything GHC-shaped.
    if text.contains("arose in the harness's result-display wrapper") {
        return Classification {
            bucket: DesireBucket::WrapperOrigin,
            identifiers: Vec::new(),
        };
    }

    // The JIT's own "can't lower this yet" report (`tidepool-codegen`'s
    // `EmitError::NotYetImplemented`), never GHC's.
    if let Some(idx) = text.find("not yet implemented:") {
        let rest = text[idx + "not yet implemented:".len()..].trim();
        let ident = rest.lines().next().unwrap_or("").trim().to_string();
        return Classification {
            bucket: DesireBucket::UnsupportedPrimopJitGap,
            identifiers: if ident.is_empty() {
                Vec::new()
            } else {
                vec![ident]
            },
        };
    }

    if text.contains("Not in scope: type constructor or class") {
        let idents = quoted_after(text, "Not in scope: type constructor or class");
        return Classification {
            bucket: DesireBucket::TypeConstructorNotInScope,
            identifiers: idents,
        };
    }

    if text.contains("Could not find module") || text.contains("Could not load module") {
        let mut idents = quoted_after(text, "Could not find module");
        idents.extend(quoted_after(text, "Could not load module"));
        return Classification {
            bucket: DesireBucket::ModuleNotFound,
            identifiers: idents,
        };
    }

    if let Some(idx) = text.find("No instance for") {
        let rest = &text["No instance for".len() + idx..];
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let mut head = rest[..line_end].trim().to_string();
        if let Some(stripped) = head.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            head = stripped.to_string();
        }
        return Classification {
            bucket: DesireBucket::MissingInstance,
            identifiers: if head.is_empty() {
                Vec::new()
            } else {
                vec![head]
            },
        };
    }

    if text.contains("Variable not in scope") {
        let idents = identifiers_after(text, "Variable not in scope:");
        return Classification {
            bucket: DesireBucket::VariableNotInScope,
            identifiers: idents,
        };
    }
    // A bare "Not in scope:" (no "Variable"/"type constructor or class"
    // qualifier) is still term-level in every GHC version this project has
    // observed — treat it the same as the "Variable not in scope" phrasing.
    if text.contains("Not in scope:") {
        let idents = identifiers_after(text, "Not in scope:");
        return Classification {
            bucket: DesireBucket::VariableNotInScope,
            identifiers: idents,
        };
    }

    if text.contains("parse error on input `import'")
        || text.contains("parse error on input \u{2018}import\u{2019}")
    {
        return Classification {
            bucket: DesireBucket::ImportGrammarRejection,
            identifiers: vec!["import".to_string()],
        };
    }

    Classification {
        bucket: DesireBucket::Other,
        identifiers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every fixture below is either a verbatim quote from a real captured
    // GHC/JIT error text (source noted per case) or GHC's own well-known
    // stable phrasing for a shape this codebase documents hitting but that
    // did not happen to occur in the two harvested evidence sources during
    // this pass (noted explicitly where that applies).

    // --- variable-not-in-scope -------------------------------------------
    // Both real, captured verbatim from a live dogfood run:
    // ~/.cache/tidepool-dogfood/tidepool/selfharness/transcript.jsonl
    // (`answerer_round` events), 2026-08-24.

    #[test]
    fn variable_not_in_scope_multiline() {
        let c = classify_error(
            "GHC error (4 diagnostic(s)):\n\
             /tmp/.tmpbVCh6n/Expr.hs:39:27: error: Variable not in scope:\n  forkAll",
        );
        assert_eq!(c.bucket, DesireBucket::VariableNotInScope);
        assert_eq!(c.identifiers, vec!["forkAll".to_string()]);
    }

    #[test]
    fn variable_not_in_scope_same_line_with_type_sig() {
        let c = classify_error(
            "/tmp/.tmpbVCh6n/Expr.hs:56:21: error: Variable not in scope: fork :: t2 -> ",
        );
        assert_eq!(c.bucket, DesireBucket::VariableNotInScope);
        assert_eq!(c.identifiers, vec!["fork".to_string()]);
    }

    // --- type-constructor-not-in-scope ------------------------------------
    // Real, captured verbatim: same transcript.jsonl run (two distinct
    // rounds/holes, "KyotoTrack" and "DelegateResult").

    #[test]
    fn type_constructor_not_in_scope_kyototrack() {
        let c = classify_error(
            "<turn>:6:25-35: error:\n    Not in scope: type constructor or class `KyotoTrack'\n   |\n42 | \n   |                         ^^^^^^^^^^",
        );
        assert_eq!(c.bucket, DesireBucket::TypeConstructorNotInScope);
        assert_eq!(c.identifiers, vec!["KyotoTrack".to_string()]);
    }

    #[test]
    fn type_constructor_not_in_scope_multiple_occurrences_all_counted() {
        let text = "\
            <turn>:6:25-35: error:\n    Not in scope: type constructor or class `KyotoTrack'\n\
            <turn>:4:25-35: error:\n    Not in scope: type constructor or class `KyotoTrack'\n\
            <turn>:2:28-38: error:\n    Not in scope: type constructor or class `KyotoTrack'\n";
        let c = classify_error(text);
        assert_eq!(c.bucket, DesireBucket::TypeConstructorNotInScope);
        assert_eq!(c.identifiers.len(), 3);
        assert!(c.identifiers.iter().all(|i| i == "KyotoTrack"));
    }

    #[test]
    fn type_constructor_not_in_scope_delegateresult() {
        // Real, captured verbatim: transcript.jsonl declaration-turn failure.
        let c = classify_error(
            "declaration type-check failed: <decl>:1:38-52: error:\n    Not in scope: type constructor or class `DelegateResult'\n   |\n25 | researchText :: Ei",
        );
        assert_eq!(c.bucket, DesireBucket::TypeConstructorNotInScope);
        assert_eq!(c.identifiers, vec!["DelegateResult".to_string()]);
    }

    // --- module-not-found --------------------------------------------------
    // Real GHC message text this codebase documents having observed verbatim
    // (quoted in its own source comments, not fabricated wording):
    // tidepool-runtime/src/session/mod.rs:224 and
    // tidepool-repl/tests/text_bind.rs:7.

    #[test]
    fn module_not_found_effects() {
        let c = classify_error("Could not find module `Tidepool.Effects'");
        assert_eq!(c.bucket, DesireBucket::ModuleNotFound);
        assert_eq!(c.identifiers, vec!["Tidepool.Effects".to_string()]);
    }

    #[test]
    fn module_not_found_session_val() {
        let c = classify_error("Could not find module 'Tidepool.Session.Val.G1'");
        assert_eq!(c.bucket, DesireBucket::ModuleNotFound);
        assert_eq!(c.identifiers, vec!["Tidepool.Session.Val.G1".to_string()]);
    }

    // --- missing-instance ----------------------------------------------
    // Real GHC message shapes exercised (byte-for-byte) by this codebase's
    // own diagnostics-rendering tests: tidepool-runtime/src/diag.rs:696,795
    // ("No instance for HasField"/"No instance for ToJSON"), and GHC's own
    // message quoted verbatim in haskell/lib/Tidepool/Form/Check.hs:109
    // ("No instance for (Generic Environment)").

    #[test]
    fn missing_instance_hasfield() {
        let c = classify_error("Expr.hs:3:1: error:\n    No instance for HasField \"foo\" T Int");
        assert_eq!(c.bucket, DesireBucket::MissingInstance);
        assert_eq!(c.identifiers, vec!["HasField \"foo\" T Int".to_string()]);
    }

    #[test]
    fn missing_instance_generic_environment_parens_stripped() {
        let c = classify_error("No instance for (Generic Environment)");
        assert_eq!(c.bucket, DesireBucket::MissingInstance);
        assert_eq!(c.identifiers, vec!["Generic Environment".to_string()]);
    }

    // --- unsupported-primop/JIT-gap ----------------------------------------
    // Real, verbatim from the JIT's own `EmitError::NotYetImplemented`
    // construction sites: tidepool-codegen/src/emit/case.rs:544 and
    // tidepool-codegen/src/emit/expr.rs:2859.

    #[test]
    fn jit_gap_litstring_in_case() {
        let c = classify_error("not yet implemented: LitString in Case");
        assert_eq!(c.bucket, DesireBucket::UnsupportedPrimopJitGap);
        assert_eq!(c.identifiers, vec!["LitString in Case".to_string()]);
    }

    #[test]
    fn jit_gap_litstring() {
        let c = classify_error("not yet implemented: LitString");
        assert_eq!(c.bucket, DesireBucket::UnsupportedPrimopJitGap);
        assert_eq!(c.identifiers, vec!["LitString".to_string()]);
    }

    // --- import-grammar rejection -------------------------------------
    // Not observed live in either harvested evidence source during this
    // pass (`tidepool-runtime/src/session/render.rs:1225` documents the
    // failure mode — a body-position `import` — this codebase actively
    // avoids by hoisting, which is exactly why it wasn't caught live). Both
    // fixtures use GHC's own well-known, stable phrasing for a body-position
    // token GHC's parser did not expect (`parse error on input `X''` is the
    // same family already captured live for `}'` — see the `other` tests
    // below — just with `import` as the offending token).

    #[test]
    fn import_grammar_rejection_backtick_quotes() {
        let c = classify_error("<turn>:3:1: error: parse error on input `import'");
        assert_eq!(c.bucket, DesireBucket::ImportGrammarRejection);
        assert_eq!(c.identifiers, vec!["import".to_string()]);
    }

    #[test]
    fn import_grammar_rejection_unicode_quotes() {
        let c = classify_error("Expr.hs:12:1: error: parse error on input \u{2018}import\u{2019}");
        assert_eq!(c.bucket, DesireBucket::ImportGrammarRejection);
        assert_eq!(c.identifiers, vec!["import".to_string()]);
    }

    // --- wrapper-origin ------------------------------------------------
    // Real, captured verbatim: transcript.jsonl / round-errors.txt (the
    // scratchpad dogfood evidence directory named in the spec) — the
    // harness's own attribution note, generated by
    // `tidepool-runtime/src/diag.rs`'s wrapper-blame path.

    #[test]
    fn wrapper_origin_result_display() {
        let c = classify_error(
            "GHC error (1 diagnostic(s)):\n1 error(s) arose in the harness's result-display wrapper, not in your block's own code — this usually means the block's result type doesn't satisfy the wrapper (e.g. no Show/ToJSON instance, or an unresolved type at the result position).",
        );
        assert_eq!(c.bucket, DesireBucket::WrapperOrigin);
    }

    #[test]
    fn wrapper_origin_takes_priority_over_diagnostic_count_prefix() {
        let c = classify_error(
            "GHC error (3 diagnostic(s)):\n3 error(s) arose in the harness's result-display wrapper, not in your block's own code.",
        );
        assert_eq!(c.bucket, DesireBucket::WrapperOrigin);
    }

    // --- other -----------------------------------------------------------
    // Real, captured verbatim: transcript.jsonl — a plain parse error (not
    // import-related) and a type-mismatch report; neither fits any named
    // bucket, which is the point of the catch-all.

    #[test]
    fn other_parse_error_on_brace() {
        let c = classify_error("GHC error (1 diagnostic(s)):\n/tmp/.tmpAymbMk/Expr.hs:37:2: error: parse error on input `}'");
        assert_eq!(c.bucket, DesireBucket::Other);
        assert!(c.identifiers.is_empty());
    }

    #[test]
    fn other_couldnt_match_type() {
        let c = classify_error(
            "/tmp/.tmpch8lCn/Expr.hs:32:18: error: Couldn't match type `Text' with `Void'\nExpected: Int",
        );
        assert_eq!(c.bucket, DesireBucket::Other);
    }

    // --- ordering: a superstring pattern must not shadow a narrower one ---

    #[test]
    fn type_constructor_message_not_misrouted_to_variable_bucket() {
        let c = classify_error("Not in scope: type constructor or class `Foo'");
        assert_eq!(c.bucket, DesireBucket::TypeConstructorNotInScope);
    }

    #[test]
    fn bare_not_in_scope_without_qualifier_is_variable_bucket() {
        let c = classify_error("<turn>:1:1: error: Not in scope: `bar'");
        assert_eq!(c.bucket, DesireBucket::VariableNotInScope);
    }
}
