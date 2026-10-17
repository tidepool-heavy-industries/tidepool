//! Per-line telemetry projection, not whole-record Unicode/Value validation.
//! Unknown fields and wrong-shaped known values are unselected opaque payloads;
//! Serde skips their syntax without decoding their strings or materializing trees.
//! Selected scalar strings still undergo Serde's Unicode validation.
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
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
    pub(super) fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => value.as_u64(),
            _ => None,
        }
    }
    pub(super) fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}
impl PartialEq<&str> for Atom<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
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
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Atom::Bool(value))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Atom::Number(value.into()))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Atom::Number(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(serde_json::Number::from_f64(value).map_or(Atom::Missing, Atom::Number))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Atom::Missing)
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Atom::Missing)
    }
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
    "actor",
    "call_id",
    "checkout_hold_ms",
    "checkout_wait_ms",
    "compile_count",
    "compile_ms",
    "detail",
    "disposition",
    "effect",
    "elapsed_ms",
    "exec_ms",
    "execution",
    "failure_class",
    "from_slot",
    "incarnation",
    "index",
    "input_unit_index",
    "jev_count",
    "jev_ms",
    "message",
    "outcome",
    "output",
    "phase",
    "preparation_wait_count",
    "preparation_wait_ms",
    "reason",
    "refusal_kind",
    "rejection",
    "status",
    "target",
    "thread_id",
    "time.busy",
    "time.idle",
    "to",
    "to_request",
    "tool",
    "total_ms",
];
const SPAN_FIELDS: [&str; 6] = [
    "actor",
    "call_id",
    "compile_request",
    "execution",
    "name",
    "tool",
];

#[derive(Debug)]
pub(super) struct Object<'a, const N: usize> {
    names: &'static [&'static str; N],
    values: [Atom<'a>; N],
}
impl<'a, const N: usize> Index<&str> for Object<'a, N> {
    type Output = Atom<'a>;
    fn index(&self, name: &str) -> &Self::Output {
        let index = self
            .names
            .iter()
            .position(|candidate| *candidate == name)
            .expect("trace consumer field must be registered");
        &self.values[index]
    }
}
impl<const N: usize> Object<'_, N> {
    fn empty(names: &'static [&'static str; N]) -> Self {
        Self {
            names,
            values: std::array::from_fn(|_| Atom::Missing),
        }
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
            if let Some(index) = key
                .as_str()
                .and_then(|key| self.0.iter().position(|name| *name == key))
            {
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
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Object::empty(self.0))
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        self.visit_unit()
    }
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
        Self {
            timestamp: Atom::Missing,
            target: Atom::Missing,
            fields: Object::empty(&EVENT_FIELDS),
            span: Object::empty(&SPAN_FIELDS),
            spans: None,
        }
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
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(record)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Record::default())
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        self.visit_unit()
    }
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
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("trace spans")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut spans = Vec::new();
        while let Some(span) = sequence.next_element_seed(ObjectSeed(&SPAN_FIELDS))? {
            spans.push(span);
        }
        Ok(Some(spans))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        self.visit_unit()
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "trace consumer field must be registered")]
    fn consumer_cannot_request_an_unregistered_projection_field() {
        let record = Record::default();
        let _ = &record.fields["new_consumer_without_decoder_registration"];
    }

    #[test]
    fn selected_fields_borrow_and_last_duplicates_clear_previous_values() {
        let bytes = br#"{"timestamp":"2026-09-23T10:00:00Z","fields":{"actor":"old","actor":null,"total_ms":19,"total_ms":"23","detail":"\u03bb"},"span":{"execution":"old"},"span":false,"spans":[{"execution":"outer"},{"execution":17}]}"#;
        let record: Record<'_> = serde_json::from_slice(bytes).unwrap();
        assert!(matches!(record.timestamp, Atom::Text(Cow::Borrowed(_))));
        assert!(record.fields["actor"].as_str().is_none());
        assert_eq!(record.fields["total_ms"].as_str(), Some("23"));
        assert_eq!(record.fields["detail"].as_str(), Some("λ"));
        assert!(record.span["execution"].as_str().is_none());
        assert_eq!(
            record.spans.as_ref().unwrap()[0]["execution"].as_str(),
            Some("outer")
        );
        assert!(
            record.spans.as_ref().unwrap()[1]["execution"]
                .as_str()
                .is_none()
        );
        for bytes in [
            br#"{"fields":{"actor":"old"},"fields":null}"#.as_slice(),
            br#"{"spans":[{"actor":"old"}],"spans":{}}"#,
        ] {
            let record: Record<'_> = serde_json::from_slice(bytes).unwrap();
            assert!(record.fields["actor"].as_str().is_none());
            assert!(record.spans.is_none());
        }
    }

    #[test]
    fn opaque_subtrees_have_explicit_skipped_value_acceptance() {
        // The observation policy deliberately does not decode discarded Unicode
        // or apply Value's recursive-tree materialization limit to opaque data.
        let surrogate = br#"{"unknown":"\ud800","fields":{"status":"Committed"}}"#;
        assert!(serde_json::from_slice::<serde_json::Value>(surrogate).is_err());
        let record: Record<'_> = serde_json::from_slice(surrogate).unwrap();
        assert_eq!(record.fields["status"].as_str(), Some("Committed"));
        let opaque_utf8 = b"{\"unknown\":\"\xff\",\"fields\":{\"status\":\"Committed\"}}";
        assert!(serde_json::from_slice::<serde_json::Value>(opaque_utf8).is_err());
        assert!(serde_json::from_slice::<Record<'_>>(opaque_utf8).is_ok());
        assert!(
            serde_json::from_slice::<Record<'_>>(b"{\"fields\":{\"status\":\"\xff\"}}").is_err()
        );
        for bytes in [
            br#"{"fields":{"actor":["\ud800"],"status":"Committed"}}"#.as_slice(),
            br#"{"fields":[{"status":"\ud800"}]}"#,
            br#"{"span":[{"execution":"\ud800"}]}"#,
            br#"{"spans":{"execution":"\ud800"}}"#,
        ] {
            assert!(serde_json::from_slice::<serde_json::Value>(bytes).is_err());
            let record: Record<'_> = serde_json::from_slice(bytes).unwrap();
            assert!(record.fields["actor"].as_str().is_none());
            assert!(record.span["execution"].as_str().is_none());
            assert!(record.spans.is_none());
        }
        // Value's arbitrary_precision visitor treats this reserved object as a
        // Number. A typed telemetry scalar deliberately treats objects as absent.
        let reserved = br#"{"fields":{"total_ms":{"$serde_json::private::Number":"19"}}}"#;
        let old: serde_json::Value = serde_json::from_slice(reserved).unwrap();
        assert_eq!(old["fields"]["total_ms"].as_u64(), Some(19));
        let record: Record<'_> = serde_json::from_slice(reserved).unwrap();
        assert!(record.fields["total_ms"].as_u64().is_none());
        let deep = format!("{{\"unknown\":{}0{}}}", "[".repeat(200), "]".repeat(200));
        assert!(serde_json::from_str::<serde_json::Value>(&deep).is_err());
        assert!(serde_json::from_str::<Record<'_>>(&deep).is_ok());
        let number = format!(
            "{{\"unknown\":{},\"fields\":{{\"total_ms\":{}}}}}",
            "9".repeat(400),
            "9".repeat(400)
        );
        assert!(serde_json::from_str::<serde_json::Value>(&number).is_ok());
        assert!(
            serde_json::from_str::<Record<'_>>(&number).unwrap().fields["total_ms"]
                .as_u64()
                .is_none()
        );
        for malformed in [
            r#"{"unknown":01}"#,
            r#"{"unknown":1e+}"#,
            r#"{"unknown":"\q"}"#,
            r#"{"unknown":true} trailing"#,
            r#"{"fields":{"status":"\ud800"}}"#,
        ] {
            assert!(
                serde_json::from_str::<Record<'_>>(malformed).is_err(),
                "{malformed}"
            );
        }
    }

    #[test]
    fn selected_scalars_and_character_prefixes_preserve_consumed_semantics() {
        let reason = "🦀".repeat(81);
        let detail = "λ".repeat(81);
        let output = format!("reply rejected: {}(secret)", "A".repeat(65));
        let bytes = serde_json::to_vec(&serde_json::json!({"fields":{
            "reason":reason,"detail":detail,"output":output}}))
        .unwrap();
        let record: Record<'_> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record.fields["reason"].as_str().unwrap(), "🦀".repeat(80));
        assert_eq!(record.fields["detail"].as_str().unwrap(), "λ".repeat(80));
        assert_eq!(
            record.fields["output"].as_str().unwrap(),
            format!("reply rejected: {}", "A".repeat(65))
        );
        for value in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(u64::MAX),
            serde_json::json!(1.5),
            serde_json::json!("19"),
            serde_json::json!("\nλ"),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let fields = EVENT_FIELDS
                .iter()
                .map(|name| ((*name).to_owned(), value.clone()))
                .collect::<serde_json::Map<_, _>>();
            let bytes = serde_json::to_vec(&serde_json::json!({"fields":fields})).unwrap();
            let record: Record<'_> = serde_json::from_slice(&bytes).unwrap();
            for name in EVENT_FIELDS {
                assert_eq!(
                    record.fields[name].as_str(),
                    value.as_str(),
                    "{name}: {value}"
                );
                assert_eq!(
                    record.fields[name].as_u64(),
                    value.as_u64(),
                    "{name}: {value}"
                );
                assert_eq!(
                    record.fields[name].as_bool(),
                    value.as_bool(),
                    "{name}: {value}"
                );
            }
        }
    }

    // A narrow, disabled-by-default allocator window follows the existing
    // codegen cost-test pattern. It measures this thread's decoder allocations,
    // excluding input acquisition, producer serialization and other threads.
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    thread_local! { static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) }; }
    fn allocated(bytes: usize) {
        let _ = ALLOCATED.try_with(|counter| {
            if let Some(total) = counter.get() {
                counter.set(Some(total + bytes));
            }
        });
    }
    struct CountingAllocator;
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                allocated(layout.size());
            }
            pointer
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() {
                allocated(layout.size());
            }
            pointer
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let pointer = unsafe { System.realloc(pointer, layout, size) };
            if !pointer.is_null() {
                allocated(size);
            }
            pointer
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) };
        }
    }
    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;
    fn measured<T>(action: impl FnOnce() -> T) -> (T, usize) {
        struct Window;
        impl Drop for Window {
            fn drop(&mut self) {
                ALLOCATED.with(|counter| counter.set(None));
            }
        }
        ALLOCATED.with(|counter| {
            assert!(counter.get().is_none());
            counter.set(Some(0));
        });
        let window = Window;
        let value = action();
        let count = ALLOCATED.with(|counter| counter.replace(None).unwrap());
        drop(window);
        (value, count)
    }

    #[test]
    fn ignored_content_growth_does_not_allocate_a_payload_tree() {
        let small = crate::exomonad::run_map_trace_fixture("\n🦀\\").into_bytes();
        let large = crate::exomonad::run_map_trace_fixture(&"\n🦀\\".repeat(16_384)).into_bytes();
        fn project(bytes: &[u8]) -> Vec<Record<'_>> {
            bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<Record<'_>>(line).unwrap())
                .collect::<Vec<_>>()
        }
        let (small_projection, small_allocated) = measured(|| project(&small));
        let (large_projection, large_allocated) = measured(|| project(&large));
        for records in [&small_projection, &large_projection] {
            assert_eq!(
                records
                    .iter()
                    .filter(|row| row.fields["status"] == "Committed")
                    .count(),
                1
            );
        }
        assert_eq!(
            large_allocated, small_allocated,
            "only ignored source content grew"
        );
        // An actual old-reader comparison calibrates this test's sensitivity;
        // equal semantic metadata is insufficient to establish removed work.
        let (_, old_small) = measured(|| {
            serde_json::Deserializer::from_slice(&small)
                .into_iter::<serde_json::Value>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        });
        let (_, old_large) = measured(|| {
            serde_json::Deserializer::from_slice(&large)
                .into_iter::<serde_json::Value>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        });
        assert!(
            old_large > old_small + 64 * 1024,
            "full Value decoding must expose the removed allocation"
        );
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        let workspace = root.path().join("workspace");
        let trace = workspace.join(".exomonad/logs/projection.jsonl");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(trace.parent().unwrap()).unwrap();
        let status = crate::exomonad::run_map_status_fixture(&workspace);
        let status_bytes = serde_json::to_vec(&status).unwrap();
        assert_eq!(
            crate::exomonad::decode_run_status(&status_bytes).unwrap(),
            status,
        );
        std::fs::write(run.join("status.json"), status_bytes).unwrap();
        for bytes in [&small, &large] {
            std::fs::write(&trace, bytes).unwrap();
            let report = super::super::read_observed_run_filtered(
                &run,
                super::super::Limits::default(),
                super::super::TimeWindow::default(),
                &super::super::Observation::at(0),
                &super::super::TimelineFilter::default(),
            )
            .unwrap();
            assert_eq!(report.trace.input_units.get("Committed"), Some(&1));
            assert_eq!(
                report
                    .trace
                    .timeline
                    .iter()
                    .filter(|event| event.name == "input unit receipt")
                    .count(),
                1
            );
            assert!(
                !serde_json::to_string(&report).unwrap().contains("🦀"),
                "discarded source cannot enter reports"
            );
        }
        eprintln!(
            "run-map projection allocation bytes: small={small_allocated} large={large_allocated}; Value small={old_small} large={old_large}; input bytes={}/{} excluded",
            small.len(),
            large.len()
        );
    }
}
