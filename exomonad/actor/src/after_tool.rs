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
//! Only a *failure* warns. That distinction is the whole of the difference
//! between [`Annotation::Abstained`] and [`Disposition::Failed`].

use std::collections::HashMap;
use std::time::Duration;

/// The entry index the retained dispatcher serves the after-tool slot at.
/// `Tidepool.Agent.Contract.afterToolEntry` is the other half of this pair;
/// ordinary tool calls are entry zero.
pub(crate) const AFTER_TOOL_ENTRY: i64 = 1;

/// The slot name `installSpec` publishes when the spec fills the field.
pub(crate) const AFTER_TOOL_SLOT: &str = "afterTool";

/// How long a result waits for its slot. The point of waiting at all is that a
/// result delivered early, with its evidence following, could send the model
/// investigating or acting just before the thing it needed appears.
pub(crate) const AFTER_TOOL_WAIT: Duration = Duration::from_secs(300);

/// The one thing that shortens the wait, and the only reason it exists: the
/// timeout path has to be exercisable without a five-minute test. Nothing in a
/// run sets it.
pub const AFTER_TOOL_WAIT_ENV: &str = "EXOMONAD_AFTER_TOOL_WAIT_MS";

/// How long this result waits.
pub(crate) fn wait() -> Duration {
    std::env::var(AFTER_TOOL_WAIT_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .map_or(AFTER_TOOL_WAIT, Duration::from_millis)
}

/// The wait as the failure line names it.
pub(crate) fn describe_wait(wait: Duration) -> String {
    if wait.as_secs() > 0 {
        format!("{}s", wait.as_secs())
    } else {
        format!("{}ms", wait.as_millis())
    }
}

/// Past this, elapsed time is reported through the workbench posture — an
/// observation channel, which never wakes the model and causes no inference.
pub(crate) const AFTER_TOOL_PROGRESS: Duration = Duration::from_secs(30);

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
                .unwrap_or_default()
                .to_owned()
        };
        match value.get("kind").and_then(serde_json::Value::as_str) {
            Some("none") => Ok(Self::Nothing),
            Some("abstained") => Ok(Self::Abstained(field("reason"))),
            Some("annotated") => Ok(Self::Annotated(field("text"))),
            Some("pruned") => Ok(Self::Pruned {
                text: field("text"),
                handle: field("handle"),
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
    TimedOut(Duration),
}

impl Disposition {
    fn describe(&self) -> String {
        match self {
            Self::Silent => "silent".to_owned(),
            Self::Abstained(reason) => format!("abstained: {reason}"),
            Self::Annotated => "annotated".to_owned(),
            Self::Pruned(handle) => format!("pruned, whole result bound as {handle}"),
            Self::Failed(reason) => format!("failed: {reason}"),
            Self::TimedOut(wait) => format!("timed out after {}", describe_wait(*wait)),
        }
    }
}

/// One invocation, kept where a model can ask for it rather than repeated into
/// every result. This is where an abstention's reason lives, and where the
/// reference a failure line carries points.
#[derive(Debug, Clone)]
pub(crate) struct Invocation {
    pub(crate) ordinal: u64,
    pub(crate) tool: String,
    pub(crate) elapsed: Duration,
    pub(crate) provenance: String,
    pub(crate) disposition: Disposition,
}

/// Every after-tool invocation this actor has made, and what it takes to keep
/// one repeated failure from filling the conversation with copies of itself.
#[derive(Debug, Default)]
pub(crate) struct AfterToolLog {
    invocations: Vec<Invocation>,
    next: u64,
    /// Failure reason to the invocation that first reported it, and how many
    /// times it has been seen.
    seen: HashMap<String, (u64, u64)>,
}

/// Whether a failure is the first of its kind, and therefore whether the
/// diagnostic itself is worth the model's context a second time.
pub(crate) enum FailureNotice {
    First { ordinal: u64, reason: String },
    Again { ordinal: u64, count: u64 },
}

impl AfterToolLog {
    /// Claim the ordinal this invocation will be referred to by. Taken before
    /// the slot runs, so a timeout has a reference too.
    pub(crate) fn begin(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    pub(crate) fn record(&mut self, invocation: Invocation) {
        self.invocations.push(invocation);
        let overflow = self.invocations.len().saturating_sub(AFTER_TOOL_LOG);
        self.invocations.drain(..overflow);
    }

    /// The first sighting of a diagnostic earns the diagnostic. Every later
    /// sighting earns a reference to the first and nothing more.
    pub(crate) fn notice(&mut self, ordinal: u64, reason: &str) -> FailureNotice {
        match self.seen.get_mut(reason) {
            Some((first, count)) => {
                *count += 1;
                FailureNotice::Again {
                    ordinal: *first,
                    count: *count,
                }
            }
            None => {
                self.seen.insert(reason.to_owned(), (ordinal, 1));
                FailureNotice::First {
                    ordinal,
                    reason: reason.to_owned(),
                }
            }
        }
    }

    /// A rebuilt slot starts with a clean record of what it has been told.
    /// A failure that comes back after a repair is news, and the invocation
    /// its reference would name may no longer be among the rows kept.
    pub(crate) fn forget_failures(&mut self) {
        self.seen.clear();
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

/// One line, bounded. A diagnostic that could change how a result is read
/// earns a warning; it does not earn the model's whole context.
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

/// The one compact line a failure is allowed, and the reference that leads to
/// the rest of it. The original result is above it, unchanged.
pub(crate) fn failed(output: &str, notice: &FailureNotice) -> String {
    match notice {
        FailureNotice::First { ordinal, reason } => format!(
            "{output}\n\n[after-tool] This result is unannotated: the slot did not answer ({reason}). Reference after-tool#{ordinal}; `status` view=detailed has the rest."
        ),
        FailureNotice::Again { ordinal, count } => format!(
            "{output}\n\n[after-tool] Unannotated again, same failure as after-tool#{ordinal} ({count} times)."
        ),
    }
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

    /// A repeated failure earns a reference, not a second copy of the
    /// diagnostic. The first one earns the diagnostic.
    #[test]
    fn one_diagnostic_is_reported_once_however_often_the_slot_fails() {
        let mut log = AfterToolLog::default();
        let first = log.begin();
        let notice = log.notice(first, "divide by zero");
        let once = failed("result", &notice);
        assert!(once.contains("divide by zero"), "{once}");
        assert!(once.contains("after-tool#1"), "{once}");

        let second = log.begin();
        let notice = log.notice(second, "divide by zero");
        let twice = failed("result", &notice);
        assert!(!twice.contains("divide by zero"), "{twice}");
        assert!(twice.contains("after-tool#1"), "{twice}");
        assert!(twice.starts_with("result"), "{twice}");

        // A different diagnostic is a different thing to say.
        let third = log.begin();
        let notice = log.notice(third, "no such binding");
        let other = failed("result", &notice);
        assert!(other.contains("no such binding"), "{other}");
        assert!(other.contains("after-tool#3"), "{other}");
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
}
