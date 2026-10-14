//! What the after-tool slot is allowed to do to a result, and how that reaches
//! the model.
//!
//! The slot annotates or prunes; it never rewrites. An annotation is attached
//! beside the tool's own output and attributed as derived, so an observation is
//! never read as the tool's own judgement. A pruned view states that it is a
//! selection and names the binding the whole result stays addressable under.
//! An abstention is silent: the original result is delivered untouched and the
//! reason is recorded here, in the actor's own inspectable log, because the
//! model never asked for a judgement on that result.
//!
//! A failed hook leaves presentation unchanged and records its diagnostic.
//! Tool success and presentation success are separate outcomes.

use std::time::Duration;

/// The entry index the retained dispatcher serves the after-tool slot at.
/// `Tidepool.Agent.Contract.afterToolEntry` is the other half of this pair;
/// ordinary tool calls are entry zero.
pub(crate) const AFTER_TOOL_ENTRY: i64 = 1;

/// The slot name `installSpec` publishes when the spec fills the field.
pub(crate) const AFTER_TOOL_SLOT: &str = "afterTool";

/// How many invocations the actor keeps for `status`.
const AFTER_TOOL_LOG: usize = 16;

/// What a slot said about a result it was shown, as
/// `Tidepool.Agent.Contract.annotationToJson` renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Annotation {
    /// Nothing worth adding, and nothing worth recording either.
    Nothing,
    /// A deliberate non-decision, with its reason. Silent to the model.
    Abstained(String),
    /// Derived context, attached beside the tool's own output.
    Annotated(String),
    /// A selection of the result, and the handle the whole of it stays
    /// addressable under.
    Pruned { text: String, handle: String },
}

impl Annotation {
    /// Read one annotation out of the single external encoding the slot
    /// produces. Anything else is a failure of the slot, not an abstention:
    /// the runtime cannot tell silence from a broken contract.
    pub(crate) fn decode(rendered: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(rendered.trim())
            .map_err(|error| format!("slot answer is not an annotation: {error}"))?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("slot annotation requires a string field {name:?}"))
        };
        match value.get("kind").and_then(serde_json::Value::as_str) {
            Some("none") => Ok(Self::Nothing),
            Some("abstained") => Ok(Self::Abstained(field("reason")?)),
            Some("annotated") => Ok(Self::Annotated(field("text")?)),
            Some("pruned") => Ok(Self::Pruned {
                text: field("text")?,
                handle: field("handle")?,
            }),
            other => Err(format!(
                "slot answered an unknown annotation kind {:?}",
                other.unwrap_or("(absent)")
            )),
        }
    }
}

/// What became of one invocation, as the actor records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Disposition {
    Silent,
    Abstained(String),
    Annotated,
    Pruned(String),
    Failed(String),
}

impl Disposition {
    fn describe(&self) -> String {
        match self {
            Self::Silent => "silent".to_owned(),
            Self::Abstained(reason) => format!("abstained: {reason}"),
            Self::Annotated => "annotated".to_owned(),
            Self::Pruned(handle) => format!("pruned, whole result bound as {handle}"),
            Self::Failed(reason) => format!("failed: {reason}"),
        }
    }
}

/// One invocation, retained for explicit inspection rather than repeated into
/// every result. Abstention and failure diagnostics stay here.
#[derive(Debug, Clone)]
pub(crate) struct Invocation {
    pub(crate) ordinal: u64,
    pub(crate) tool: String,
    pub(crate) elapsed: Duration,
    pub(crate) provenance: String,
    pub(crate) disposition: Disposition,
}

/// Bounded invocation history, independent of model-facing presentation.
#[derive(Debug, Default)]
pub(crate) struct AfterToolLog {
    invocations: Vec<Invocation>,
    next: u64,
}

impl AfterToolLog {
    /// Claim an ordinal before invoking the retained hook.
    pub(crate) fn begin(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    pub(crate) fn record(&mut self, invocation: Invocation) {
        self.invocations.push(invocation);
        let overflow = self.invocations.len().saturating_sub(AFTER_TOOL_LOG);
        self.invocations.drain(..overflow);
    }

    /// The status view's rows, newest last. Empty when no slot has ever run,
    /// which is the ordinary case.
    pub(crate) fn rows(&self) -> Vec<String> {
        self.invocations
            .iter()
            .map(|invocation| {
                format!(
                    "after-tool#{} {} {}ms {} [{}]",
                    invocation.ordinal,
                    invocation.tool,
                    invocation.elapsed.as_millis(),
                    invocation.disposition.describe(),
                    invocation.provenance,
                )
            })
            .collect()
    }
}

/// Bound one line of diagnostic text for the invocation log.
pub(crate) fn compact_reason(reason: &str) -> String {
    let line = reason
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(no detail)");
    match line.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// Derived context, attached beside the tool's own output and said to be
/// derived. Never mixed into the output itself: an observation the slot made
/// is not something the tool answered.
pub(crate) fn annotated(output: &str, text: &str, revision: &str) -> String {
    format!(
        "{output}\n\n[after-tool] Derived context, not part of the tool's output (spec revision {revision}):\n{text}"
    )
}

/// A selection, which says so, and names the binding the whole result is still
/// addressable under in a later Haskell cell.
pub(crate) fn pruned(text: &str, handle: &str, revision: &str) -> String {
    format!(
        "[after-tool] A selection of this result, not the whole of it (spec revision {revision}). The complete result stays addressable as `{handle} :: Text` in a Haskell cell.\n{text}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four shapes `annotationToJson` produces, and nothing else. A slot
    /// that answers something other than an annotation has failed, which is a
    /// different thing from having abstained.
    #[test]
    fn an_annotation_is_read_from_the_one_encoding_the_slot_produces() {
        assert_eq!(
            Annotation::decode(r#"{"kind":"none"}"#).unwrap(),
            Annotation::Nothing
        );
        assert_eq!(
            Annotation::decode(r#"{"kind":"abstained","reason":"nothing to prune"}"#).unwrap(),
            Annotation::Abstained("nothing to prune".into())
        );
        assert_eq!(
            Annotation::decode(r#"{"kind":"annotated","text":"three callers"}"#).unwrap(),
            Annotation::Annotated("three callers".into())
        );
        assert_eq!(
            Annotation::decode(r#"{"kind":"pruned","text":"two lines","handle":"toolResult1"}"#)
                .unwrap(),
            Annotation::Pruned {
                text: "two lines".into(),
                handle: "toolResult1".into()
            }
        );
        assert!(Annotation::decode("not json at all").is_err());
        assert!(Annotation::decode(r#"{"kind":"rewritten","text":"x"}"#).is_err());
    }

    /// An abstention's reason reaches the log and nothing else; only the log
    /// grows, and it grows bounded.
    #[test]
    fn the_log_keeps_the_most_recent_invocations_and_no_more() {
        let mut log = AfterToolLog::default();
        for _ in 0..AFTER_TOOL_LOG + 4 {
            let ordinal = log.begin();
            log.record(Invocation {
                ordinal,
                tool: "probe".into(),
                elapsed: Duration::from_millis(1),
                provenance: "install=1".into(),
                disposition: Disposition::Abstained("unprunable".into()),
            });
        }
        let rows = log.rows();
        assert_eq!(rows.len(), AFTER_TOOL_LOG);
        assert!(rows[0].contains("after-tool#5"), "{rows:?}");
        assert!(rows.last().unwrap().contains("abstained: unprunable"));
    }

    #[test]
    fn malformed_annotation_fields_are_refused() {
        for answer in [
            r#"{"kind":"abstained"}"#,
            r#"{"kind":"abstained","reason":null}"#,
            r#"{"kind":"annotated","text":4}"#,
            r#"{"kind":"pruned","text":"selection"}"#,
            r#"{"kind":"pruned","text":[],"handle":"toolResult1"}"#,
            r#"{"kind":"pruned","text":"selection","handle":false}"#,
        ] {
            assert!(Annotation::decode(answer).is_err(), "{answer}");
        }
        assert_eq!(
            Annotation::decode(r#"{"kind":"annotated","text":""}"#).unwrap(),
            Annotation::Annotated(String::new())
        );
    }
}
