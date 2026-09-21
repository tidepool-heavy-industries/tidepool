//! Materialized values that may cross the Rust/Haskell bridge.
//!
//! Executable closures and thunks never cross this boundary. They remain owned
//! by a native machine through `ValueHandle`; this type contains only data.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use tidepool_repr::{DataConId, Literal};

pub type SharedByteArray = Arc<Mutex<Vec<u8>>>;

pub enum HaskellValue {
    Lit(Literal),
    Con(DataConId, Vec<HaskellValue>),
    ByteArray(SharedByteArray),
}

impl Clone for HaskellValue {
    fn clone(&self) -> Self {
        match self {
            Self::Lit(literal) => Self::Lit(literal.clone()),
            Self::ByteArray(bytes) => Self::ByteArray(Arc::clone(bytes)),
            Self::Con(id, fields) if fields.is_empty() => Self::Con(*id, Vec::new()),
            Self::Con(_, _) => clone_tree(self),
        }
    }
}

fn clone_tree(root: &HaskellValue) -> HaskellValue {
    recursion::expand_and_collapse::<HaskellValueFrame<'_, recursion::PartiallyApplied>, _, _>(
        root,
        HaskellValue::as_frame,
        |frame| match frame {
            HaskellValueFrame::Leaf(HaskellValue::Lit(literal)) => HaskellValue::Lit(literal.clone()),
            HaskellValueFrame::Leaf(HaskellValue::ByteArray(bytes)) => HaskellValue::ByteArray(Arc::clone(bytes)),
            HaskellValueFrame::Leaf(HaskellValue::Con(..)) => unreachable!("constructors have child frames"),
            HaskellValueFrame::Con(id, fields) => HaskellValue::Con(id, fields),
        },
    )
}

pub enum HaskellValueFrame<'a, X> {
    Leaf(&'a HaskellValue),
    Con(DataConId, Vec<X>),
}

impl<'a> recursion::MappableFrame for HaskellValueFrame<'a, recursion::PartiallyApplied> {
    type Frame<X> = HaskellValueFrame<'a, X>;

    fn map_frame<A, B>(input: Self::Frame<A>, f: impl FnMut(A) -> B) -> Self::Frame<B> {
        match input {
            HaskellValueFrame::Leaf(value) => HaskellValueFrame::Leaf(value),
            HaskellValueFrame::Con(id, fields) => HaskellValueFrame::Con(id, fields.into_iter().map(f).collect()),
        }
    }
}

impl HaskellValue {
    pub fn as_frame(&self) -> HaskellValueFrame<'_, &HaskellValue> {
        match self {
            Self::Con(id, fields) => HaskellValueFrame::Con(*id, fields.iter().collect()),
            Self::Lit(_) | Self::ByteArray(_) => HaskellValueFrame::Leaf(self),
        }
    }

    pub fn node_count(&self) -> usize {
        recursion::expand_and_collapse::<HaskellValueFrame<'_, recursion::PartiallyApplied>, _, _>(
            self,
            HaskellValue::as_frame,
            |frame| match frame {
                HaskellValueFrame::Leaf(_) => 1,
                HaskellValueFrame::Con(_, fields) => 1 + fields.into_iter().sum::<usize>(),
            },
        )
    }
}

impl std::fmt::Display for HaskellValue {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        format_tree(self, output, FormatMode::Display)
    }
}

impl std::fmt::Debug for HaskellValue {
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
    HaskellValue(&'a HaskellValue),
    Text(&'static str),
}

fn format_tree(
    root: &HaskellValue,
    output: &mut std::fmt::Formatter<'_>,
    mode: FormatMode,
) -> std::fmt::Result {
    let mut work = vec![FormatAction::HaskellValue(root)];
    while let Some(action) = work.pop() {
        match action {
            FormatAction::Text(text) => std::fmt::Write::write_str(output, text)?,
            FormatAction::HaskellValue(HaskellValue::Lit(literal)) if matches!(mode, FormatMode::Debug) => {
                write!(output, "Lit({literal:?})")?;
            }
            FormatAction::HaskellValue(HaskellValue::Lit(literal)) => match literal {
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
            FormatAction::HaskellValue(HaskellValue::Con(id, fields)) => {
                match mode {
                    FormatMode::Display => write!(output, "<Con#{}>", id.0)?,
                    FormatMode::Debug => {
                        write!(output, "Con({id:?}, [")?;
                        work.push(FormatAction::Text("])"));
                    }
                }
                for (index, field) in fields.iter().enumerate().rev() {
                    work.push(FormatAction::HaskellValue(field));
                    match mode {
                        FormatMode::Display => work.push(FormatAction::Text(" ")),
                        FormatMode::Debug if index > 0 => work.push(FormatAction::Text(", ")),
                        FormatMode::Debug => {}
                    }
                }
            }
            FormatAction::HaskellValue(HaskellValue::ByteArray(bytes)) if matches!(mode, FormatMode::Debug) => {
                write!(output, "ByteArray({bytes:?})")?;
            }
            FormatAction::HaskellValue(HaskellValue::ByteArray(bytes)) => match bytes.lock() {
                Ok(bytes) => write!(output, "<ByteArray# len={}>", bytes.len()),
                Err(_) => write!(output, "<ByteArray# poisoned>"),
            }?,
        }
    }
    Ok(())
}

pub fn render_capped(value: &HaskellValue, max_depth: usize) -> String {
    let mut rendered = String::new();
    let mut work = vec![(value, 0usize)];
    while let Some((value, depth)) = work.pop() {
        if depth >= max_depth {
            rendered.push('…');
            continue;
        }
        match value {
            HaskellValue::Con(id, fields) => {
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

thread_local! { static DROP_QUEUE: RefCell<Option<Vec<HaskellValue>>> = const { RefCell::new(None) }; }

impl Drop for HaskellValue {
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
            struct Reset<'a>(&'a RefCell<Option<Vec<HaskellValue>>>);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    // Release the borrow before any queued values are dropped:
                    // their Drop implementation re-enters this same queue.
                    let remaining = self.0.borrow_mut().take();
                    drop(remaining);
                }
            }
            let _reset = Reset(slot);
            loop {
                // A while-let scrutinee keeps this RefMut alive through the
                // body, where dropping `value` must borrow the queue again.
                let next = { slot.borrow_mut().as_mut().and_then(Vec::pop) };
                let Some(value) = next else { break };
                drop(value);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    fn deep_value(depth: usize) -> HaskellValue {
        let mut value = HaskellValue::Lit(Literal::LitInt(0));
        for _ in 0..depth {
            value = HaskellValue::Con(DataConId(7), vec![value]);
        }
        value
    }

    #[test]
    fn shallow_format_shape_is_preserved() {
        let value = HaskellValue::Con(
            DataConId(7),
            vec![
                HaskellValue::Lit(Literal::LitInt(1)),
                HaskellValue::Lit(Literal::LitInt(2)),
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
