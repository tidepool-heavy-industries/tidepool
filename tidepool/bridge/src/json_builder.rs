//! Streaming construction for compiler-authenticated JSON layouts.

use crate::{BridgeError, HaskellVisitor};
use tidepool_repr::execution_schema::{JsonLayout, RuntimeRep};
use tidepool_repr::{DataConId, Literal};

fn visit_text(
    text: &str,
    layout: &JsonLayout<DataConId>,
    visitor: &mut dyn HaskellVisitor,
) -> Result<(), BridgeError> {
    visitor.begin_constructor(layout.text, 3)?;
    visitor.byte_array(text.as_bytes().to_vec())?;
    visitor.literal(Literal::LitInt(0))?;
    visitor.literal(Literal::LitInt(text.len() as i64))?;
    visitor.end_constructor()
}

fn visit_map(
    entries: &[(&String, &serde_json::Value)],
    layout: &JsonLayout<DataConId>,
    visitor: &mut dyn HaskellVisitor,
) -> Result<(), BridgeError> {
    if entries.is_empty() {
        visitor.begin_constructor(layout.map_tip, 0)?;
        return visitor.end_constructor();
    }
    let mid = entries.len() / 2;
    let (key, value) = entries[mid];
    visitor.begin_constructor(layout.map_bin, 5)?;
    if matches!(visitor.expected_field_rep(), Some(RuntimeRep::Int(_))) {
        visitor.literal(Literal::LitInt(entries.len() as i64))?;
    } else {
        visitor.begin_constructor(layout.int, 1)?;
        visitor.literal(Literal::LitInt(entries.len() as i64))?;
        visitor.end_constructor()?;
    }
    visit_text(key, layout, visitor)?;
    visit_json(value, layout, visitor)?;
    visit_map(&entries[..mid], layout, visitor)?;
    visit_map(&entries[mid + 1..], layout, visitor)?;
    visitor.end_constructor()
}

/// Stream a parsed JSON value through the layout GHC authenticated for the
/// owning prepared program. No constructor names or table search participate
/// in this conversion.
pub fn visit_json(
    value: &serde_json::Value,
    layout: &JsonLayout<DataConId>,
    visitor: &mut dyn HaskellVisitor,
) -> Result<(), BridgeError> {
    match value {
        serde_json::Value::Null => {
            visitor.begin_constructor(layout.null, 0)?;
            visitor.end_constructor()
        }
        serde_json::Value::Bool(value) => {
            visitor.begin_constructor(layout.bool_, 1)?;
            visitor.begin_constructor(if *value { layout.true_ } else { layout.false_ }, 0)?;
            visitor.end_constructor()?;
            visitor.end_constructor()
        }
        serde_json::Value::Number(number) => {
            let (coefficient, exponent) =
                crate::decimal::Decimal::parse_token(number.as_str())?.into_parts();
            visitor.begin_constructor(layout.number, 1)?;
            visitor.begin_constructor(layout.scientific, 2)?;
            crate::shapes::visit_integer_from_decimal(
                &coefficient,
                layout.integer_small,
                layout.integer_positive,
                layout.integer_negative,
                visitor,
            )?;
            visitor.literal(Literal::LitInt(exponent))?;
            visitor.end_constructor()?;
            visitor.end_constructor()
        }
        serde_json::Value::String(text) => {
            visitor.begin_constructor(layout.string, 1)?;
            visit_text(text, layout, visitor)?;
            visitor.end_constructor()
        }
        serde_json::Value::Array(items) => {
            visitor.begin_constructor(layout.array, 1)?;
            for item in items {
                visitor.begin_constructor(layout.cons, 2)?;
                visit_json(item, layout, visitor)?;
            }
            visitor.begin_constructor(layout.nil, 0)?;
            visitor.end_constructor()?;
            for _ in items {
                visitor.end_constructor()?;
            }
            visitor.end_constructor()
        }
        serde_json::Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            visitor.begin_constructor(layout.object, 1)?;
            visit_map(&entries, layout, visitor)?;
            visitor.end_constructor()
        }
    }
}

/// Count the bridged shape without building it or copying string/limb payloads.
pub fn bridged_node_count(
    value: &serde_json::Value,
) -> Result<usize, crate::decimal::DecimalError> {
    let mut pending = vec![value];
    let mut count = 0usize;
    while let Some(value) = pending.pop() {
        let local = match value {
            serde_json::Value::Null => 1,
            serde_json::Value::Bool(_) => 2,
            serde_json::Value::String(_) => 5,
            serde_json::Value::Number(number) => {
                crate::decimal::Decimal::parse_token(number.as_str())?;
                5
            }
            serde_json::Value::Array(items) => {
                pending.extend(items);
                items.len().saturating_add(2)
            }
            serde_json::Value::Object(entries) => {
                pending.extend(entries.values());
                entries.len().saturating_mul(8).saturating_add(2)
            }
        };
        count = count.saturating_add(local);
    }
    Ok(count)
}
