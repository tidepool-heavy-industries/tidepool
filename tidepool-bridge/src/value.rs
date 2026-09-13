//! Materialized values that may cross the Rust/Haskell bridge.
//!
//! Executable closures and thunks never cross this boundary. They remain owned
//! by a native machine through `ValueHandle`; this type contains only data.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use tidepool_repr::{DataConId, Literal};

pub type SharedByteArray = Arc<Mutex<Vec<u8>>>;

pub enum Value {
    Lit(Literal),
    Con(DataConId, Vec<Value>),
    ByteArray(SharedByteArray),
}

impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Self::Lit(literal) => Self::Lit(literal.clone()),
            Self::ByteArray(bytes) => Self::ByteArray(Arc::clone(bytes)),
            Self::Con(id, fields) if fields.is_empty() => Self::Con(*id, Vec::new()),
            Self::Con(_, _) => clone_tree(self),
        }
    }
}

fn clone_tree(root: &Value) -> Value {
    recursion::expand_and_collapse::<ValueFrame<'_, recursion::PartiallyApplied>, _, _>(
        root,
        Value::as_frame,
        |frame| match frame {
            ValueFrame::Leaf(Value::Lit(literal)) => Value::Lit(literal.clone()),
            ValueFrame::Leaf(Value::ByteArray(bytes)) => Value::ByteArray(Arc::clone(bytes)),
            ValueFrame::Leaf(Value::Con(..)) => unreachable!("constructors have child frames"),
            ValueFrame::Con(id, fields) => Value::Con(id, fields),
        },
    )
}

pub enum ValueFrame<'a, X> {
    Leaf(&'a Value),
    Con(DataConId, Vec<X>),
}

impl<'a> recursion::MappableFrame for ValueFrame<'a, recursion::PartiallyApplied> {
    type Frame<X> = ValueFrame<'a, X>;

    fn map_frame<A, B>(input: Self::Frame<A>, f: impl FnMut(A) -> B) -> Self::Frame<B> {
        match input {
            ValueFrame::Leaf(value) => ValueFrame::Leaf(value),
            ValueFrame::Con(id, fields) => ValueFrame::Con(id, fields.into_iter().map(f).collect()),
        }
    }
}

impl Value {
    pub fn as_frame(&self) -> ValueFrame<'_, &Value> {
        match self {
            Self::Con(id, fields) => ValueFrame::Con(*id, fields.iter().collect()),
            Self::Lit(_) | Self::ByteArray(_) => ValueFrame::Leaf(self),
        }
    }

    pub fn node_count(&self) -> usize {
        recursion::expand_and_collapse::<ValueFrame<'_, recursion::PartiallyApplied>, _, _>(
            self,
            Value::as_frame,
            |frame| match frame {
                ValueFrame::Leaf(_) => 1,
                ValueFrame::Con(_, fields) => 1 + fields.into_iter().sum::<usize>(),
            },
        )
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        format_tree(self, output, FormatMode::Display)
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        format_tree(self, output, FormatMode::Debug)
    }
}

#[derive(Clone, Copy)]
enum FormatMode {
    Display,
    Debug,
}

enum FormatAction<'a> {
    Value(&'a Value),
    Text(&'static str),
}

fn format_tree(
    root: &Value,
    output: &mut std::fmt::Formatter<'_>,
    mode: FormatMode,
) -> std::fmt::Result {
    let mut work = vec![FormatAction::Value(root)];
    while let Some(action) = work.pop() {
        match action {
            FormatAction::Text(text) => std::fmt::Write::write_str(output, text)?,
            FormatAction::Value(Value::Lit(literal)) if matches!(mode, FormatMode::Debug) => {
                write!(output, "Lit({literal:?})")?;
            }
            FormatAction::Value(Value::Lit(literal)) => match literal {
                Literal::LitInt(n) => write!(output, "{n}"),
                Literal::LitWord(n) => write!(output, "{n}"),
                Literal::LitChar(c) => write!(output, "'{}'", c.escape_default()),
                Literal::LitString(bytes) => match std::str::from_utf8(bytes) {
                    Ok(text) => write!(output, "{text:?}"),
                    Err(_) => write!(output, "<bytes len={}>", bytes.len()),
                },
                Literal::LitByteArray(bytes) => write!(output, "<bytearray len={}>", bytes.len()),
                Literal::LitFloat(bits) => match u32::try_from(*bits) {
                    Ok(bits) => write!(output, "{}", f32::from_bits(bits)),
                    Err(_) => write!(output, "<invalid f32 bits=0x{bits:016x}>"),
                },
                Literal::LitDouble(bits) => write!(output, "{}", f64::from_bits(*bits)),
            }?,
            FormatAction::Value(Value::Con(id, fields)) => {
                match mode {
                    FormatMode::Display => write!(output, "<Con#{}>", id.0)?,
                    FormatMode::Debug => {
                        write!(output, "Con({id:?}, [")?;
                        work.push(FormatAction::Text("])"));
                    }
                }
                for (index, field) in fields.iter().enumerate().rev() {
                    work.push(FormatAction::Value(field));
                    match mode {
                        FormatMode::Display => work.push(FormatAction::Text(" ")),
                        FormatMode::Debug if index > 0 => work.push(FormatAction::Text(", ")),
                        FormatMode::Debug => {}
                    }
                }
            }
            FormatAction::Value(Value::ByteArray(bytes)) if matches!(mode, FormatMode::Debug) => {
                write!(output, "ByteArray({bytes:?})")?;
            }
            FormatAction::Value(Value::ByteArray(bytes)) => match bytes.lock() {
                Ok(bytes) => write!(output, "<ByteArray# len={}>", bytes.len()),
                Err(_) => write!(output, "<ByteArray# poisoned>"),
            }?,
        }
    }
    Ok(())
}

pub fn render_capped(value: &Value, max_depth: usize) -> String {
    let mut rendered = String::new();
    let mut work = vec![(value, 0usize)];
    while let Some((value, depth)) = work.pop() {
        if depth >= max_depth {
            rendered.push('…');
            continue;
        }
        match value {
            Value::Con(id, fields) => {
                rendered.push_str(&format!("<Con#{}>", id.0));
                for field in fields.iter().rev() {
                    rendered.push(' ');
                    work.push((field, depth + 1));
                }
            }
            other => rendered.push_str(&other.to_string()),
        }
    }
    if value.node_count() > max_depth {
        rendered.push_str(&format!(" …(elided; {} total nodes)", value.node_count()));
    }
    rendered
}

thread_local! { static DROP_QUEUE: RefCell<Option<Vec<Value>>> = const { RefCell::new(None) }; }

impl Drop for Value {
    fn drop(&mut self) {
        let Self::Con(_, fields) = self else { return };
        if fields.is_empty() {
            return;
        }
        DROP_QUEUE.with(|slot| {
            if let Some(queue) = slot.borrow_mut().as_mut() {
                queue.extend(std::mem::take(fields));
                return;
            }
            let queue = std::mem::take(fields);
            *slot.borrow_mut() = Some(queue);
            struct Reset<'a>(&'a RefCell<Option<Vec<Value>>>);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    *self.0.borrow_mut() = None;
                }
            }
            let _reset = Reset(slot);
            while let Some(value) = slot.borrow_mut().as_mut().and_then(Vec::pop) {
                drop(value);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    fn deep_value(depth: usize) -> Value {
        let mut value = Value::Lit(Literal::LitInt(0));
        for _ in 0..depth {
            value = Value::Con(DataConId(7), vec![value]);
        }
        value
    }

    #[test]
    fn shallow_format_shape_is_preserved() {
        let value = Value::Con(
            DataConId(7),
            vec![
                Value::Lit(Literal::LitInt(1)),
                Value::Lit(Literal::LitInt(2)),
            ],
        );
        assert_eq!(format!("{value}"), "<Con#7> 1 2");
        assert_eq!(
            format!("{value:?}"),
            "Con(DataConId(7), [Lit(LitInt(1)), Lit(LitInt(2))])"
        );
    }

    #[test]
    fn deep_clone_count_display_debug_and_drop_fit_small_stack() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                const DEPTH: usize = 30_000;
                let value = deep_value(DEPTH);
                assert_eq!(value.node_count(), DEPTH + 1);
                let cloned = value.clone();
                assert_eq!(cloned.node_count(), DEPTH + 1);

                let displayed = format!("{value}");
                assert_eq!(displayed.matches("<Con#7>").count(), DEPTH);
                assert!(displayed.ends_with(" 0"));

                let debugged = format!("{cloned:?}");
                assert_eq!(debugged.matches("Con(DataConId(7), [").count(), DEPTH);
                assert!(debugged.contains("Lit(LitInt(0))"));

                drop(cloned);
                drop(value);
            })
            .expect("spawn small-stack value test")
            .join()
            .expect("small-stack value test");
    }

    struct FailAfter {
        remaining: usize,
    }

    impl fmt::Write for FailAfter {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            if text.len() > self.remaining {
                return Err(fmt::Error);
            }
            self.remaining -= text.len();
            Ok(())
        }
    }

    #[test]
    fn early_format_failure_cleans_up_deep_value_on_small_stack() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let value = deep_value(30_000);
                let mut display_writer = FailAfter { remaining: 32 };
                assert!(fmt::write(&mut display_writer, format_args!("{value}")).is_err());
                let mut debug_writer = FailAfter { remaining: 32 };
                assert!(fmt::write(&mut debug_writer, format_args!("{value:?}")).is_err());
                drop(value);
            })
            .expect("spawn small-stack formatting test")
            .join()
            .expect("small-stack formatting test");
    }
}
