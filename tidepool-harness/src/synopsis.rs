//! A names-only type synopsis derived from a compiled artifact's
//! [`DataConTable`] — no Haskell Generic machinery, no fluency tax.
//!
//! The table captures field LABELS but not field TYPES, so [`type_synopsis`]
//! is honestly shallow: it can say what a type's constructors and selectors
//! are CALLED, never what shape a field itself has. That is enough for a
//! hole-card synopsis line ([`crate::engine::hole_card`]/
//! [`crate::engine::answerer_hole_card`]) and nothing more — a mechanical
//! FORM derived from this same table would have to render every field as a
//! blind text box, which is why no such form exists here; `FormShape`
//! (`Tidepool.Form.GForm`, `selfharness::operator::FormShape`) is the one
//! operator-presentation algebra, derived from a type's own `Generic`
//! metadata instead.

use tidepool_repr::DataConTable;

/// Render a names-only synopsis of `ty`'s shape from `table`:
///
/// - all of `ty`'s constructors have zero fields (a nullary sum) → the
///   constructor names, in `dataConTag` order — `"Advance | Hold | Abort"`.
/// - exactly one constructor, and it carries field labels (a record) → the
///   selector names — `"Contribution { addedIdeas, draftDelta, advance }"`.
/// - anything else (a multi-constructor type with fields, a single
///   positional constructor, or a type name `table` has no constructors
///   for) → the bare type name alone, `ty`. Never a partial/invented shape —
///   a hole card showing `Ty { a, b, ? }` would claim knowledge the table
///   doesn't have.
#[must_use]
pub fn type_synopsis(table: &DataConTable, ty: &str) -> String {
    let ids = table.constructors_of_type(ty);
    if !ids.is_empty() {
        let all_nullary = ids
            .iter()
            .all(|&id| table.get(id).is_some_and(|dc| dc.rep_arity == 0));
        if all_nullary {
            let names: Vec<&str> = ids.iter().filter_map(|&id| table.name_of(id)).collect();
            if names.len() == ids.len() {
                return names.join(" | ");
            }
        } else if let [id] = ids[..] {
            if let Some(labels) = table.field_labels_of(id) {
                if !labels.is_empty() {
                    return format!("{ty} {{ {} }}", labels.join(", "));
                }
            }
        }
    }
    ty.to_string()
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

    #[test]
    fn nullary_sum_synopsis_in_declaration_order() {
        let mut table = DataConTable::new();
        table.insert(nullary(3, "Abort", 3, "Verdict"));
        table.insert(nullary(1, "Advance", 1, "Verdict"));
        table.insert(nullary(2, "Hold", 2, "Verdict"));
        assert_eq!(type_synopsis(&table, "Verdict"), "Advance | Hold | Abort");
    }

    #[test]
    fn record_synopsis_lists_selector_names_in_field_order() {
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
        assert_eq!(
            type_synopsis(&table, "Contribution"),
            "Contribution { addedIdeas, draftDelta, advance }"
        );
    }

    /// Mutation-close the degrade path: each unsupported shape renders the
    /// BARE TYPE NAME and nothing else — never a partial/invented shape.
    #[test]
    fn unsupported_shapes_render_bare_type_name_only() {
        // Multi-constructor with fields — labels set on the FIRST
        // (lowest-tag) constructor specifically, so a synopsis that (wrongly)
        // special-cased "the first constructor with labels" instead of "the
        // ONLY constructor" would render a partial shape here.
        let mut either = DataConTable::new();
        let left = with_fields(1, "Left", 1, 1, "Either");
        either.insert(left.clone());
        either.set_field_labels(left.id, vec!["error".to_string()]);
        either.insert(with_fields(2, "Right", 2, 1, "Either"));
        assert_eq!(type_synopsis(&either, "Either"), "Either");

        // Single positional constructor (no captured field labels).
        let mut pair = DataConTable::new();
        pair.insert(with_fields(1, "Pair", 1, 2, "Pair"));
        assert_eq!(type_synopsis(&pair, "Pair"), "Pair");

        // Unknown type name.
        let empty = DataConTable::new();
        assert_eq!(type_synopsis(&empty, "NoSuchType"), "NoSuchType");
    }

    /// Derived from the table, not hardcoded: changing a record's field
    /// names changes the synopsis.
    #[test]
    fn record_synopsis_follows_table_field_name_changes() {
        let mut table = DataConTable::new();
        let dc = with_fields(1, "Person", 1, 2, "Person");
        table.insert(dc.clone());
        table.set_field_labels(dc.id, vec!["name".to_string(), "age".to_string()]);
        assert_eq!(type_synopsis(&table, "Person"), "Person { name, age }");

        let mut table2 = DataConTable::new();
        let dc2 = with_fields(1, "Person", 1, 2, "Person");
        table2.insert(dc2.clone());
        table2.set_field_labels(dc2.id, vec!["fullName".to_string(), "yearsOld".to_string()]);
        assert_eq!(
            type_synopsis(&table2, "Person"),
            "Person { fullName, yearsOld }"
        );
    }
}
