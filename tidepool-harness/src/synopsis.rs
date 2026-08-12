//! A type synopsis/document derived from a compiled artifact's
//! [`DataConTable`] — no Haskell Generic machinery, no fluency tax.
//!
//! The table carries both field LABELS and, since the `meta.cbor` 9-element
//! wire bump, rendered field TYPES (`Tidepool.Translate.dcFieldTypes`), so
//! [`type_document`] can render a full GHC-style `data` declaration for a
//! hole's answer type instead of a names-only synopsis — a harness author
//! never hand-embeds an answer type's declaration in a prompt. Consumed by a
//! hole card's shape section ([`crate::engine::hole_card`]/
//! [`crate::engine::answerer_hole_card`], via [`crate::engine::type_shape_line`])
//! and nothing more — a mechanical FORM derived from this same table would
//! have to render every field as a blind text box, which is why no such form
//! exists here; `FormShape` (`Tidepool.Form.GForm`,
//! `selfharness::operator::FormShape`) is the one operator-presentation
//! algebra, derived from a type's own `Generic` metadata instead.

use std::collections::{HashSet, VecDeque};

use tidepool_repr::{DataConId, DataConTable};

/// Prelude/builtin type names never transitively expanded into their own
/// `data` line — everyone already knows their shape, and expanding them (e.g.
/// `Maybe`, whose constructors are often present in a real table) would add
/// noise no author wants in a hole card. Lists (`[T]`) and tuples (`(A, B)`)
/// are excluded from expansion structurally (they are never a single
/// UPPERCASE identifier token), so they need no entry here.
const BUILTIN_TYPES: &[&str] = &[
    "Text", "String", "Int", "Integer", "Word", "Double", "Float", "Bool", "Char", "Maybe",
    "Either", "IO", "Ordering",
];

fn is_builtin(name: &str) -> bool {
    BUILTIN_TYPES.contains(&name)
}

/// Split a rendered type string into maximal Haskell-identifier tokens
/// (`[A-Za-z_][A-Za-z0-9_']*`), dropping every non-identifier character
/// (brackets, parens, spaces, arrows, commas). Good enough for classifying
/// tyvars vs. type-constructor names in a `ppr`-rendered type — it never
/// needs to reparse the type's full grammar, only find identifier-shaped
/// substrings.
fn identifier_tokens(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_alphabetic() || chars[i] == '_' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '\'')
            {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            i += 1;
        }
    }
    out
}

fn is_uppercase_start(tok: &str) -> bool {
    tok.chars().next().is_some_and(|c| c.is_uppercase())
}

fn is_lowercase_start(tok: &str) -> bool {
    tok.chars().next().is_some_and(|c| c.is_lowercase())
}

/// Already fully wrapped in `[...]` or `(...)` — wrapping again would be
/// redundant, not merely harmless (`((Int, Text))` reads as a mistake).
fn already_wrapped(t: &str) -> bool {
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')'))
}

/// Parenthesize a rendered type when placing it POSITIONALLY next to
/// sibling fields requires it — any type containing a space that isn't
/// already bracket/paren-wrapped (`Tree Int` -> `(Tree Int)`, `[Text]`
/// unchanged, `Int` unchanged). Record fields never need this: the `::`
/// already delimits the field's type unambiguously.
fn parenthesize_positional(t: &str) -> String {
    if t.contains(' ') && !already_wrapped(t) {
        format!("({t})")
    } else {
        t.to_string()
    }
}

/// Render one constructor's fragment of a `data` declaration body:
/// - nullary (`rep_arity == 0`) -> the bare constructor name.
/// - record (field labels present, arity-matched) -> `Name { l1 :: T1, ... }`.
/// - positional (no field labels) -> `Name T1 T2 ...`, parenthesizing
///   positionally where required.
///
/// `None` on ANY unusable shape for this constructor (missing field types,
/// a field-type/rep-arity mismatch, or field labels present but
/// arity-mismatched) — the caller degrades the WHOLE type to its bare name
/// rather than rendering a partial/invented shape.
fn render_constructor(table: &DataConTable, id: DataConId) -> Option<String> {
    let dc = table.get(id)?;
    let name = table.name_of(id)?;
    if dc.rep_arity == 0 {
        return Some(name.to_string());
    }
    let types = table.field_types_of(id)?;
    if types.len() != dc.rep_arity as usize {
        return None;
    }
    match table.field_labels_of(id) {
        Some(labels) if labels.len() == dc.rep_arity as usize => {
            let fields: Vec<String> = labels
                .iter()
                .zip(types)
                .map(|(l, t)| format!("{l} :: {t}"))
                .collect();
            Some(format!("{name} {{ {} }}", fields.join(", ")))
        }
        Some(_) => None,
        None => {
            let fields: Vec<String> = types.iter().map(|t| parenthesize_positional(t)).collect();
            Some(format!("{name} {}", fields.join(" ")))
        }
    }
}

/// Render `ty`'s one-line `data` declaration BODY (the part after `= `), in
/// `dataConTag` order — every shape a table can carry: a nullary sum
/// (`Advance | Hold | Abort`), a record (`Contribution { addedIdeas :: [Text],
/// ... }`), a positional sum (`Circle Double | Rect Double Double`), or a
/// mixed sum (`Tick | Burst { count :: Int }`).
///
/// `None` when `ty` has no constructors in `table` (unknown type), OR any one
/// of its constructors fails to render (see [`render_constructor`]) — the
/// WHOLE type degrades, never a partial shape.
fn render_type_body(table: &DataConTable, ty: &str) -> Option<String> {
    let ids = table.constructors_of_type(ty);
    if ids.is_empty() {
        return None;
    }
    let fragments: Option<Vec<String>> = ids
        .into_iter()
        .map(|id| render_constructor(table, id))
        .collect();
    fragments.map(|fs| fs.join(" | "))
}

/// Render a names-only synopsis of `ty`'s shape from `table`: `ty`'s `data`
/// declaration BODY (see [`render_type_body`]) when every constructor
/// renders cleanly, or the bare type name `ty` itself on any degrade —
/// never a partial/invented shape.
#[must_use]
pub fn type_synopsis(table: &DataConTable, ty: &str) -> String {
    render_type_body(table, ty).unwrap_or_else(|| ty.to_string())
}

/// Lowercase identifier tokens found across `ty`'s own field types, in
/// first-appearance (constructor-tag, then field) order, deduplicated — the
/// tyvar header for `ty`'s `data <Ty> <tyvars> = ...` line. Empty for a
/// monomorphic type.
fn tyvar_header(table: &DataConTable, ty: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in table.constructors_of_type(ty) {
        let Some(types) = table.field_types_of(id) else {
            continue;
        };
        for t in types {
            for tok in identifier_tokens(t) {
                if is_lowercase_start(&tok) && seen.insert(tok.clone()) {
                    out.push(tok);
                }
            }
        }
    }
    out
}

/// UPPERCASE identifier tokens across `ty`'s own field types that name a
/// user type reachable for transitive expansion: not a [`BUILTIN_TYPES`]
/// entry, and `table` actually carries constructors for it. Order is
/// first-appearance (constructor-tag, then field, then token-in-string);
/// deduplicated within this call (the caller's global `visited` set
/// dedups across the whole document).
fn referenced_type_names(table: &DataConTable, ty: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in table.constructors_of_type(ty) {
        let Some(types) = table.field_types_of(id) else {
            continue;
        };
        for t in types {
            for tok in identifier_tokens(t) {
                if is_uppercase_start(&tok)
                    && !is_builtin(&tok)
                    && seen.insert(tok.clone())
                    && !table.constructors_of_type(&tok).is_empty()
                {
                    out.push(tok);
                }
            }
        }
    }
    out
}

fn render_data_decl(table: &DataConTable, ty: &str, body: &str) -> String {
    let tyvars = tyvar_header(table, ty);
    if tyvars.is_empty() {
        format!("data {ty} = {body}")
    } else {
        format!("data {ty} {} = {body}", tyvars.join(" "))
    }
}

/// Cap on BFS depth from the root type during transitive expansion — a
/// belt-and-suspenders bound alongside the `visited`-set dedup (which alone
/// already prevents infinite loops on a cyclic type); guards against an
/// unreasonably long expansion CHAIN through many distinct types.
const MAX_EXPANSION_DEPTH: usize = 8;

/// Render a full GHC-style multi-type `data` DOCUMENT for `ty`: `ty`'s own
/// declaration first, then every user type transitively reachable through
/// its (and each subsequently-added type's) field types — a type-name token
/// occurring in a field type, resolved against `table`, not already visited,
/// not a builtin, within [`MAX_EXPANSION_DEPTH`] hops of the root. One line
/// per type, in BFS discovery order, each `data <Ty>[ <tyvars>] = <body>`.
///
/// A type that fails to render (see [`render_type_body`]) is simply not
/// expanded into — no line is added for it, and no further tokens are
/// scanned from it (its shape is unknown, so nothing further can be found
/// honestly). This applies to `ty` itself too: when `ty`'s OWN body can't
/// render, [`type_document`] degrades to the bare type name, exactly like
/// [`type_synopsis`] — never a document containing only broken transitive
/// entries with no root.
#[must_use]
pub fn type_document(table: &DataConTable, ty: &str) -> String {
    let Some(root_body) = render_type_body(table, ty) else {
        return ty.to_string();
    };

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(ty.to_string());
    let mut lines = vec![render_data_decl(table, ty, &root_body)];

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    for candidate in referenced_type_names(table, ty) {
        if visited.insert(candidate.clone()) {
            queue.push_back((candidate, 1));
        }
    }

    while let Some((candidate, depth)) = queue.pop_front() {
        let Some(body) = render_type_body(table, &candidate) else {
            continue;
        };
        lines.push(render_data_decl(table, &candidate, &body));
        if depth < MAX_EXPANSION_DEPTH {
            for next in referenced_type_names(table, &candidate) {
                if visited.insert(next.clone()) {
                    queue.push_back((next, depth + 1));
                }
            }
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{DataCon, DataConId};

    fn nullary(id: u64, name: &str, tag: u32, type_name: &str) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some(format!("{type_name}.{name}")),
            type_name: type_name.to_string(),
        }
    }

    fn with_fields(id: u64, name: &str, tag: u32, rep_arity: u32, type_name: &str) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity,
            field_bangs: vec![],
            qualified_name: Some(format!("{type_name}.{name}")),
            type_name: type_name.to_string(),
        }
    }

    fn verdict_table() -> DataConTable {
        let mut table = DataConTable::new();
        table.insert(nullary(3, "Abort", 3, "Verdict"));
        table.insert(nullary(1, "Advance", 1, "Verdict"));
        table.insert(nullary(2, "Hold", 2, "Verdict"));
        table
    }

    #[test]
    fn nullary_sum_body_in_declaration_order() {
        let table = verdict_table();
        assert_eq!(type_synopsis(&table, "Verdict"), "Advance | Hold | Abort");
        assert_eq!(
            type_document(&table, "Verdict"),
            "data Verdict = Advance | Hold | Abort"
        );
    }

    #[test]
    fn typed_record_body_lists_selector_names_and_types() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Contribution", 1, 3, "Contribution");
        table.insert(dc.clone());
        table.set_field_labels(
            dc.id,
            vec![
                "addedIdeas".to_string(),
                "draftDelta".to_string(),
                "advance".to_string(),
            ],
        );
        table.set_field_types(
            dc.id,
            vec!["[Text]".to_string(), "Text".to_string(), "Bool".to_string()],
        );
        assert_eq!(
            type_synopsis(&table, "Contribution"),
            "Contribution { addedIdeas :: [Text], draftDelta :: Text, advance :: Bool }"
        );
        assert_eq!(
            type_document(&table, "Contribution"),
            "data Contribution = Contribution { addedIdeas :: [Text], draftDelta :: Text, advance :: Bool }"
        );
    }

    #[test]
    fn positional_sum_body_renders_each_constructor() {
        let mut table = DataConTable::new();
        let circle = with_fields(1, "Circle", 1, 1, "Shape");
        table.insert(circle.clone());
        table.set_field_types(circle.id, vec!["Double".to_string()]);
        let rect = with_fields(2, "Rect", 2, 2, "Shape");
        table.insert(rect.clone());
        table.set_field_types(rect.id, vec!["Double".to_string(), "Double".to_string()]);

        assert_eq!(
            type_synopsis(&table, "Shape"),
            "Circle Double | Rect Double Double"
        );
    }

    #[test]
    fn mixed_sum_with_record_constructor_renders() {
        let mut table = DataConTable::new();
        table.insert(nullary(1, "Tick", 1, "Event"));
        let burst = with_fields(2, "Burst", 2, 1, "Event");
        table.insert(burst.clone());
        table.set_field_labels(burst.id, vec!["count".to_string()]);
        table.set_field_types(burst.id, vec!["Int".to_string()]);

        assert_eq!(
            type_synopsis(&table, "Event"),
            "Tick | Burst { count :: Int }"
        );
    }

    #[test]
    fn nested_type_parenthesizes_positional_multiword_fields() {
        let mut table = DataConTable::new();
        let node = with_fields(1, "Node", 1, 2, "Tree");
        table.insert(node.clone());
        table.set_field_types(
            node.id,
            vec!["Tree Int".to_string(), "Tree Int".to_string()],
        );

        assert_eq!(type_synopsis(&table, "Tree"), "Node (Tree Int) (Tree Int)");
    }

    #[test]
    fn already_bracketed_or_parenthesized_types_are_not_double_wrapped() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Wrap", 1, 2, "Wrap");
        table.insert(dc.clone());
        table.set_field_types(
            dc.id,
            vec!["[Maybe Int]".to_string(), "(Int, Text)".to_string()],
        );

        assert_eq!(
            type_synopsis(&table, "Wrap"),
            "Wrap [Maybe Int] (Int, Text)"
        );
    }

    #[test]
    fn tyvar_header_lists_lowercase_tokens_in_first_appearance_order() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Box", 1, 2, "Box");
        table.insert(dc.clone());
        table.set_field_types(dc.id, vec!["b".to_string(), "a".to_string()]);

        assert_eq!(type_document(&table, "Box"), "data Box b a = Box b a");
    }

    /// Transitive expansion: a record type referencing a second user type
    /// (itself a record) AND a nullary sum, both expanded in first-
    /// appearance order after the root.
    #[test]
    fn transitive_expansion_reaches_referenced_user_types() {
        let mut table = DataConTable::new();

        let outer = with_fields(1, "Outer", 1, 2, "Outer");
        table.insert(outer.clone());
        table.set_field_labels(outer.id, vec!["inner".to_string(), "verdict".to_string()]);
        table.set_field_types(outer.id, vec!["Inner".to_string(), "Verdict".to_string()]);

        let inner = with_fields(2, "Inner", 1, 1, "Inner");
        table.insert(inner.clone());
        table.set_field_labels(inner.id, vec!["n".to_string()]);
        table.set_field_types(inner.id, vec!["Int".to_string()]);

        table.insert(nullary(3, "Advance", 1, "Verdict"));
        table.insert(nullary(4, "Hold", 2, "Verdict"));

        let doc = type_document(&table, "Outer");
        assert_eq!(
            doc,
            "data Outer = Outer { inner :: Inner, verdict :: Verdict }\n\
             data Inner = Inner { n :: Int }\n\
             data Verdict = Advance | Hold"
        );
    }

    /// A self-recursive type must terminate — the root type appearing again
    /// inside its own field types (via `Maybe`, a builtin the scan skips
    /// anyway, PLUS the direct self-reference) does not requeue itself.
    #[test]
    fn cycle_self_recursive_type_terminates() {
        let mut table = DataConTable::new();
        let rec = with_fields(1, "Rec", 1, 1, "Rec");
        table.insert(rec.clone());
        table.set_field_types(rec.id, vec!["Maybe Rec".to_string()]);

        let doc = type_document(&table, "Rec");
        assert_eq!(doc, "data Rec = Rec (Maybe Rec)");
    }

    /// A two-type mutual cycle (A references B, B references A back) must
    /// also terminate, with each type rendered exactly once.
    #[test]
    fn mutual_cycle_terminates_with_each_type_rendered_once() {
        let mut table = DataConTable::new();
        let a = with_fields(1, "A", 1, 1, "A");
        table.insert(a.clone());
        table.set_field_types(a.id, vec!["B".to_string()]);
        let b = with_fields(2, "B", 1, 1, "B");
        table.insert(b.clone());
        table.set_field_types(b.id, vec!["A".to_string()]);

        let doc = type_document(&table, "A");
        assert_eq!(doc, "data A = A B\ndata B = B A");
    }

    // ---- degrade paths: missing types, arity mismatch, unknown type ----

    #[test]
    fn degrades_to_bare_name_when_field_types_missing() {
        let mut table = DataConTable::new();
        // rep_arity 2 but no field types ever set — extract omitted them (or
        // an older wire payload).
        table.insert(with_fields(1, "Pair", 1, 2, "Pair"));
        assert_eq!(type_synopsis(&table, "Pair"), "Pair");
        assert_eq!(type_document(&table, "Pair"), "Pair");
    }

    #[test]
    fn degrades_to_bare_name_on_field_type_arity_mismatch() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Pair", 1, 2, "Pair");
        table.insert(dc.clone());
        // Only one type for two fields.
        table.set_field_types(dc.id, vec!["Int".to_string()]);
        assert_eq!(type_synopsis(&table, "Pair"), "Pair");
    }

    #[test]
    fn degrades_to_bare_name_on_label_arity_mismatch() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Contribution", 1, 3, "Contribution");
        table.insert(dc.clone());
        table.set_field_labels(dc.id, vec!["addedIdeas".to_string()]); // only 1 of 3
        table.set_field_types(
            dc.id,
            vec!["[Text]".to_string(), "Text".to_string(), "Bool".to_string()],
        );
        assert_eq!(type_synopsis(&table, "Contribution"), "Contribution");
    }

    /// Mutation-close: one bad constructor in a multi-constructor type
    /// degrades the WHOLE type, not just that constructor.
    #[test]
    fn one_bad_constructor_degrades_the_whole_type() {
        let mut table = DataConTable::new();
        let left = with_fields(1, "Left", 1, 1, "Either");
        table.insert(left.clone());
        table.set_field_types(left.id, vec!["Text".to_string()]);
        // Right has fields but no field types at all.
        table.insert(with_fields(2, "Right", 2, 1, "Either"));
        assert_eq!(type_synopsis(&table, "Either"), "Either");
    }

    #[test]
    fn degrades_to_bare_name_for_unknown_type() {
        let table = DataConTable::new();
        assert_eq!(type_synopsis(&table, "NoSuchType"), "NoSuchType");
        assert_eq!(type_document(&table, "NoSuchType"), "NoSuchType");
    }

    /// A transitively-referenced type that itself degrades is simply not
    /// expanded into (no line, no further scan) — it does not abort the
    /// whole document, since the ROOT still rendered cleanly.
    #[test]
    fn transitive_degrade_is_skipped_not_fatal_to_the_document() {
        let mut table = DataConTable::new();
        let outer = with_fields(1, "Outer", 1, 1, "Outer");
        table.insert(outer.clone());
        table.set_field_labels(outer.id, vec!["broken".to_string()]);
        table.set_field_types(outer.id, vec!["Broken".to_string()]);
        // "Broken" has a constructor with fields but no field types set.
        table.insert(with_fields(2, "Broken", 1, 1, "Broken"));

        let doc = type_document(&table, "Outer");
        assert_eq!(doc, "data Outer = Outer { broken :: Broken }");
    }

    /// Derived from the table, not hardcoded: changing a record's field
    /// names or types changes the rendered document.
    #[test]
    fn document_follows_table_field_rename_and_retype() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Person", 1, 2, "Person");
        table.insert(dc.clone());
        table.set_field_labels(dc.id, vec!["name".to_string(), "age".to_string()]);
        table.set_field_types(dc.id, vec!["Text".to_string(), "Int".to_string()]);
        assert_eq!(
            type_synopsis(&table, "Person"),
            "Person { name :: Text, age :: Int }"
        );

        let mut table2 = DataConTable::new();
        let dc2 = with_fields(1, "Person", 1, 2, "Person");
        table2.insert(dc2.clone());
        table2.set_field_labels(dc2.id, vec!["fullName".to_string(), "yearsOld".to_string()]);
        table2.set_field_types(dc2.id, vec!["Text".to_string(), "Double".to_string()]);
        assert_eq!(
            type_synopsis(&table2, "Person"),
            "Person { fullName :: Text, yearsOld :: Double }"
        );
    }
}
