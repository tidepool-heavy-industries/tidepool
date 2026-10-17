//! Per-line projection. Unknown payloads are consumed by Serde without a Value tree.
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::borrow::Cow;
use std::fmt;
use std::ops::Index;

#[derive(Debug, Default)]
pub(super) enum Atom<'a> {
    #[default]
    Missing,
    Text(Cow<'a, str>),
    Number(serde_json::Number),
    Bool(bool),
}
impl Atom<'_> {
    pub(super) fn as_str(&self) -> Option<&str> {
        match self { Self::Text(value) => Some(value), _ => None }
    }
    pub(super) fn as_u64(&self) -> Option<u64> {
        match self { Self::Number(value) => value.as_u64(), _ => None }
    }
    pub(super) fn as_bool(&self) -> Option<bool> {
        match self { Self::Bool(value) => Some(*value), _ => None }
    }
}
impl PartialEq<&str> for Atom<'_> {
    fn eq(&self, other: &&str) -> bool { self.as_str() == Some(*other) }
}

// The extra character distinguishes a maximum-length variant from an overlong
// one. Truncating at64 would turn an invalid65-character variant into a valid one.
const REJECTED_OUTPUT_CHARS: usize = "reply rejected: ".len() + 65;
fn prefix(text: &str, limit: Option<usize>) -> &str {
    match limit.and_then(|limit| text.char_indices().nth(limit)) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}
struct AtomSeed(Option<usize>);
impl<'de> DeserializeSeed<'de> for AtomSeed {
    type Value = Atom<'de>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for AtomSeed {
    type Value = Atom<'de>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a trace metadata scalar")
    }
    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(Atom::Text(Cow::Borrowed(prefix(value, self.0))))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Atom::Text(Cow::Owned(prefix(value, self.0).to_owned())))
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> { Ok(Atom::Bool(value)) }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> { Ok(Atom::Number(value.into())) }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> { Ok(Atom::Number(value.into())) }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(serde_json::Number::from_f64(value).map_or(Atom::Missing, Atom::Number))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> { Ok(Atom::Missing) }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> { Ok(Atom::Missing) }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Atom::Missing)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Atom::Missing)
    }
}

// These are the complete fields consumed by TraceEvents, summaries and timeline.
// Keeping a fixed roster prevents unrelated values from entering the projection.
const EVENT_FIELDS: [&str; 37] = [
    "actor", "call_id", "checkout_hold_ms", "checkout_wait_ms", "compile_count", "compile_ms",
    "detail", "disposition", "effect", "elapsed_ms", "exec_ms", "execution", "failure_class",
    "from_slot", "incarnation", "index", "input_unit_index", "jev_count", "jev_ms", "message",
    "outcome", "output", "phase", "preparation_wait_count", "preparation_wait_ms", "reason",
    "refusal_kind", "rejection", "status", "target", "thread_id", "time.busy", "time.idle",
    "to", "to_request", "tool", "total_ms",
];
const SPAN_FIELDS: [&str; 6] = ["actor", "call_id", "compile_request", "execution", "name", "tool"];

#[derive(Debug)]
pub(super) struct Object<'a, const N: usize> {
    names: &'static [&'static str; N],
    values: [Atom<'a>; N],
}
impl<'a, const N: usize> Index<&str> for Object<'a, N> {
    type Output = Atom<'a>;
    fn index(&self, name: &str) -> &Self::Output {
        self.names.iter().position(|candidate| *candidate == name)
            .map_or(&Atom::Missing, |index| &self.values[index])
    }
}
impl<const N: usize> Object<'_, N> {
    fn empty(names: &'static [&'static str; N]) -> Self {
        Self { names, values: std::array::from_fn(|_| Atom::Missing) }
    }
}
pub(super) type Fields<'a> = Object<'a, 37>;
pub(super) type Span<'a> = Object<'a, 6>;

struct ObjectSeed<const N: usize>(&'static [&'static str; N]);
impl<'de, const N: usize> DeserializeSeed<'de> for ObjectSeed<N> {
    type Value = Object<'de, N>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de, const N: usize> Visitor<'de> for ObjectSeed<N> {
    type Value = Object<'de, N>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("trace metadata fields")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut result = Object::empty(self.0);
        while let Some(key) = map.next_key_seed(AtomSeed(None))? {
            if let Some(index) = key.as_str().and_then(|key| self.0.iter().position(|name| *name == key)) {
                let limit = match self.0[index] {
                    "output" => Some(REJECTED_OUTPUT_CHARS),
                    "reason" | "detail" => Some(super::trace::TEXT_PREFIX_CHARS),
                    _ => None,
                };
                // Assignment also clears an earlier duplicate on null/wrong type.
                result.values[index] = map.next_value_seed(AtomSeed(limit))?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(result)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> { Ok(Object::empty(self.0)) }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Object::empty(self.0))
    }
}

#[derive(Debug)]
pub(super) struct Record<'a> {
    pub timestamp: Atom<'a>,
    pub target: Atom<'a>,
    pub fields: Fields<'a>,
    pub span: Span<'a>,
    pub spans: Option<Vec<Span<'a>>>,
}
impl Default for Record<'_> {
    fn default() -> Self {
        Self { timestamp: Atom::Missing, target: Atom::Missing,
            fields: Object::empty(&EVENT_FIELDS), span: Object::empty(&SPAN_FIELDS), spans: None }
    }
}
struct RecordVisitor;
impl<'de> Deserialize<'de> for Record<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RecordVisitor)
    }
}
impl<'de> Visitor<'de> for RecordVisitor {
    type Value = Record<'de>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a host trace record")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut record = Record::default();
        while let Some(key) = map.next_key_seed(AtomSeed(None))? {
            match key.as_str() {
                Some("timestamp") => record.timestamp = map.next_value_seed(AtomSeed(None))?,
                Some("target") => record.target = map.next_value_seed(AtomSeed(None))?,
                Some("fields") => record.fields = map.next_value_seed(ObjectSeed(&EVENT_FIELDS))?,
                Some("span") => record.span = map.next_value_seed(ObjectSeed(&SPAN_FIELDS))?,
                Some("spans") => record.spans = map.next_value_seed(SpansSeed)?,
                _ => { map.next_value::<IgnoredAny>()?; }
            }
        }
        Ok(record)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> { Ok(Record::default()) }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Record::default())
    }
}
struct SpansSeed;
impl<'de> DeserializeSeed<'de> for SpansSeed {
    type Value = Option<Vec<Span<'de>>>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for SpansSeed {
    type Value = Option<Vec<Span<'de>>>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result { formatter.write_str("trace spans") }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut spans = Vec::new();
        while let Some(span) = sequence.next_element_seed(ObjectSeed(&SPAN_FIELDS))? { spans.push(span); }
        Ok(Some(spans))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> { Ok(None) }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> { self.visit_unit() }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}
