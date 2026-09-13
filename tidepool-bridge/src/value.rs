//! Materialized values that may cross the Rust/Haskell bridge.
//!
//! Executable closures and thunks never cross this boundary. They remain owned
//! by a native machine through `ValueHandle`; this type contains only data.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use tidepool_repr::{DataConId, Literal};

pub type SharedByteArray = Arc<Mutex<Vec<u8>>>;

#[derive(Debug)]
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
            Self::Con(_, fields) if fields.is_empty() => match self {
                Self::Con(id, _) => Self::Con(*id, Vec::new()),
                _ => unreachable!(),
            },
            Self::Con(_, _) => clone_tree(self),
        }
    }
}

fn clone_tree(root: &Value) -> Value {
    enum Work<'a> {
        Visit(&'a Value),
        Build(DataConId, usize),
    }
    let mut work = vec![Work::Visit(root)];
    let mut values = Vec::new();
    while let Some(item) = work.pop() {
        match item {
            Work::Visit(Value::Lit(literal)) => values.push(Value::Lit(literal.clone())),
            Work::Visit(Value::ByteArray(bytes)) => {
                values.push(Value::ByteArray(Arc::clone(bytes)))
            }
            Work::Visit(Value::Con(id, fields)) => {
                work.push(Work::Build(*id, fields.len()));
                work.extend(fields.iter().rev().map(Work::Visit));
            }
            Work::Build(id, count) => {
                let split = values.len() - count;
                let fields = values.split_off(split);
                values.push(Value::Con(id, fields));
            }
        }
    }
    match values.pop() {
        Some(value) => value,
        None => unreachable!("a cloned value is present"),
    }
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
        let mut count = 0;
        let mut values = vec![self];
        while let Some(value) = values.pop() {
            count += 1;
            if let Self::Con(_, fields) = value {
                values.extend(fields);
            }
        }
        count
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lit(literal) => match literal {
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
            },
            Self::Con(id, fields) => {
                write!(output, "<Con#{}>", id.0)?;
                for field in fields {
                    write!(output, " {field}")?;
                }
                Ok(())
            }
            Self::ByteArray(bytes) => match bytes.lock() {
                Ok(bytes) => write!(output, "<ByteArray# len={}>", bytes.len()),
                Err(_) => write!(output, "<ByteArray# poisoned>"),
            },
        }
    }
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
