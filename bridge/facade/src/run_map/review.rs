//! Review sections: the actor tree, inbox deliveries, notifications, slowest
//! calls, rejections, nudges and hosted-call cancellations.
//!
//! Each section is derived from recorded artifacts only and degrades to
//! `unavailable` with its reason when a source is missing. Deliveries are the
//! current durable state; every other event list honours the time window.
use super::trace::{CallTiming, Cancellation, TraceEvents};
use super::{ActorNode, Evidence, TimeWindow};
use exomonad_actor::ActorRecoveryJournal;
use exomonad_node::DeliveryPhase;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A front delivery held in an unconfirmed phase this long fences its inbox.
pub const FENCE_AFTER_MS: u64 = 60_000;

/// Inputs a review reads beyond the run directory.
#[derive(Debug, Clone)]
pub struct Observation {
    /// UTC Unix milliseconds that ages are measured against.
    pub now_unix_ms: u64,
    /// The Codex home whose `queue_1.sqlite` holds host-input rows; `None`
    /// leaves every host-input column unavailable.
    pub codex_home: Option<PathBuf>,
    pub slowest_calls: usize,
}

impl Observation {
    pub fn at(now_unix_ms: u64) -> Self {
        Self {
            now_unix_ms,
            codex_home: None,
            slowest_calls: 15,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum Section<T> {
    Available { rows: T },
    Unavailable { reason: String },
}

#[derive(Debug, Serialize)]
pub struct Review {
    pub now_unix_ms: u64,
    pub tree: Section<Vec<TreeNode>>,
    pub deliveries: Section<Deliveries>,
    pub notifications: Section<Vec<Notification>>,
    pub slowest_calls: Section<SlowCalls>,
    pub rejections: Section<Vec<RepeatGroup>>,
    pub nudges: Section<Vec<RepeatGroup>>,
    pub cancellations: Section<Vec<CancellationRow>>,
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub actor: String,
    pub depth: usize,
    pub parent: Option<String>,
    pub label: String,
    pub role: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Whether the actor has its own provider session; inline forks do not.
    pub own_session: bool,
    pub thread: Option<String>,
    pub started_at_unix_ms: Option<u64>,
    pub launched_at_unix_ms: Option<u64>,
    pub first_tool_call_at_unix_ms: Option<u64>,
    pub first_reply_at_unix_ms: Option<u64>,
    pub last_reply_at_unix_ms: Option<u64>,
    pub standing: Option<String>,
    pub standing_request: Option<u64>,
    pub terminal: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Deliveries {
    pub host_input_db: Section<String>,
    pub actors: Vec<ActorDeliveries>,
}

#[derive(Debug, Serialize)]
pub struct ActorDeliveries {
    pub actor: String,
    pub label: Option<String>,
    pub cursor: Option<u64>,
    pub pending: usize,
    /// Every retained phase has held at least this long: no inbox file has
    /// been written since.
    pub phase_age_at_least_ms: Option<u64>,
    pub fenced: Option<Fence>,
    pub rows: Vec<DeliveryRow>,
    pub unavailable: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Fence {
    pub sequence: u64,
    pub durable_phase: Option<DeliveryPhase>,
    pub age_at_least_ms: u64,
    /// The fence began no later than this instant.
    pub since_at_most_unix_ms: u64,
    pub rows_behind: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeliveryRow {
    pub sequence: u64,
    pub provenance: Provenance,
    pub phase: MessagePhase,
    /// The raw `DurableInbox` phase; `None` behind the cursor without a retained receipt.
    pub durable_phase: Option<DeliveryPhase>,
    pub host_input: HostInput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    Notification {
        sender: String,
    },
    RequestUpdate {
        owner: String,
        request: u64,
        update: u64,
    },
    Other,
}

/// The message phase a model sees, mapped from durable inbox evidence.
/// `incorporated` needs evidence this reader does not consume and is never
/// produced here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    /// Durably accepted into the recipient's inbox, not yet sent.
    Receipt,
    /// Sent or possibly sent to the provider, without presentation evidence.
    Queued,
    Presented,
    /// Behind the inbox cursor.
    Acknowledged,
    /// The front row, held unconfirmed past the fence interval.
    Fenced,
    Withdrawn,
    Rejected,
}

impl MessagePhase {
    fn of(durable: Option<DeliveryPhase>) -> Self {
        match durable {
            Some(DeliveryPhase::Accepted) => Self::Receipt,
            Some(
                DeliveryPhase::InFlight | DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed,
            ) => Self::Queued,
            Some(DeliveryPhase::Presented) => Self::Presented,
            Some(DeliveryPhase::Withdrawn) => Self::Withdrawn,
            Some(DeliveryPhase::Rejected) => Self::Rejected,
            Some(DeliveryPhase::Compacted) | None => Self::Acknowledged,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Receipt => "receipt",
            Self::Queued => "queued",
            Self::Presented => "presented",
            Self::Acknowledged => "acknowledged",
            Self::Fenced => "fenced",
            Self::Withdrawn => "withdrawn",
            Self::Rejected => "rejected",
        }
    }
}

fn durable_label(phase: Option<DeliveryPhase>) -> &'static str {
    match phase {
        Some(DeliveryPhase::Accepted) => "accepted",
        Some(DeliveryPhase::InFlight) => "in_flight",
        Some(DeliveryPhase::Submitted) => "submitted",
        Some(DeliveryPhase::Presented) => "presented",
        Some(DeliveryPhase::Withdrawn) => "withdrawn",
        Some(DeliveryPhase::Rejected) => "rejected",
        Some(DeliveryPhase::Unconfirmed) => "unconfirmed",
        Some(DeliveryPhase::Compacted) => "compacted",
        None => "none",
    }
}

/// Sent or possibly sent, with no presentation evidence yet.
fn awaits_confirmation(phase: Option<DeliveryPhase>) -> bool {
    matches!(
        phase,
        Some(DeliveryPhase::InFlight | DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed)
    )
}

/// The Codex host-input row for the same producer and sequence.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostInput {
    Row {
        state: String,
        updated_at_unix_ms: i64,
    },
    NoRow,
    Unavailable,
}

impl HostInput {
    fn label(&self) -> String {
        match self {
            Self::Row { state, .. } => state.clone(),
            Self::NoRow => "no row".into(),
            Self::Unavailable => "unavailable".into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Notification {
    pub at_unix_ms: u64,
    pub sender: String,
    pub target: String,
    pub from_slot: bool,
    pub text_prefix: String,
    pub receipt: Receipt,
}

/// Receipt evidence from the target's inbox, never from transcripts.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Receipt {
    Row {
        sequence: u64,
        phase: MessagePhase,
        durable_phase: Option<DeliveryPhase>,
        presented: bool,
        host_input: HostInput,
    },
    /// The target's inbox holds no matching row.
    NoRow,
    /// The target has no inbox directory in this run.
    NoInbox,
}

#[derive(Debug, Serialize)]
pub struct SlowCalls {
    pub slowest: Vec<CallTiming>,
    pub per_tool: Vec<ToolPercentiles>,
    pub checkout_wait_share_percent: f64,
}

#[derive(Debug, Serialize)]
pub struct ToolPercentiles {
    pub tool: String,
    pub count: usize,
    pub total_ms: Percentiles,
    pub checkout_wait_ms: Percentiles,
}

#[derive(Debug, Serialize)]
pub struct Percentiles {
    pub p50: u64,
    pub p90: u64,
    pub max: u64,
}

impl Percentiles {
    fn of(mut values: Vec<u64>) -> Self {
        values.sort_unstable();
        let rank = |p: f64| {
            values
                .get(((values.len() as f64 * p) as usize).min(values.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        Self {
            p50: rank(0.5),
            p90: rank(0.9),
            max: values.last().copied().unwrap_or(0),
        }
    }
}

/// Repeated occurrences of one (actor, kind, reason, detail).
#[derive(Debug, Serialize)]
pub struct RepeatGroup {
    pub actor: String,
    pub kind: String,
    pub reason: String,
    pub detail: Option<String>,
    pub count: u64,
    pub first_at_unix_ms: u64,
    pub last_at_unix_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct CancellationRow {
    pub actor: Option<String>,
    #[serde(flatten)]
    pub cancellation: Cancellation,
}

type GroupKey = (String, String, String, Option<String>);

fn group(groups: &mut BTreeMap<GroupKey, (u64, u64, u64)>, key: GroupKey, at: u64) {
    let entry = groups.entry(key).or_insert((0, at, at));
    entry.0 += 1;
    entry.1 = entry.1.min(at);
    entry.2 = entry.2.max(at);
}

fn groups_into_rows(groups: BTreeMap<GroupKey, (u64, u64, u64)>) -> Vec<RepeatGroup> {
    let mut rows: Vec<_> = groups
        .into_iter()
        .map(
            |((actor, kind, reason, detail), (count, first, last))| RepeatGroup {
                actor,
                kind,
                reason,
                detail,
                count,
                first_at_unix_ms: first,
                last_at_unix_ms: last,
            },
        )
        .collect();
    rows.sort_by_key(|row| (row.first_at_unix_ms, row.actor.clone()));
    rows
}

impl Review {
    pub(super) fn placeholder() -> Self {
        fn pending<T>() -> Section<T> {
            Section::Unavailable {
                reason: "not yet read".into(),
            }
        }
        Self {
            now_unix_ms: 0,
            tree: pending(),
            deliveries: pending(),
            notifications: pending(),
            slowest_calls: pending(),
            rejections: pending(),
            nudges: pending(),
            cancellations: pending(),
        }
    }
}

pub(super) fn build(
    run: &Path,
    actors: &[ActorNode],
    trace: Option<&TraceEvents>,
    trace_unavailable: &str,
    window: TimeWindow,
    observation: &Observation,
) -> Review {
    let now = observation.now_unix_ms;
    let queue = read_host_inputs(run, actors, observation.codex_home.as_deref());
    let tree = tree(run, trace);
    let labels: BTreeMap<&str, &str> = match &tree {
        Section::Available { rows } => rows
            .iter()
            .map(|node| (node.actor.as_str(), node.label.as_str()))
            .collect(),
        Section::Unavailable { .. } => BTreeMap::new(),
    };
    // Per inbox: the delivery projection and, index-aligned with its rows,
    // each row's notification text prefix.
    let inboxes: BTreeMap<String, (ActorDeliveries, Vec<Option<String>>)> = actors
        .iter()
        .map(|node| {
            let state = actor_state(node, &queue, now);
            let mut deliveries = state.deliveries();
            deliveries.label = labels
                .get(state.actor.as_str())
                .map(|label| (*label).to_owned());
            let texts = state.rows.into_iter().map(|(_, text)| text).collect();
            (state.actor, (deliveries, texts))
        })
        .collect();
    let threads: BTreeMap<&str, String> = actors
        .iter()
        .filter_map(|node| match &node.provider_thread {
            Evidence::Observed { value, .. } => Some((
                value.as_str(),
                format!("{}@{}", node.actor, node.incarnation),
            )),
            _ => None,
        })
        .collect();
    Review {
        now_unix_ms: now,
        notifications: traced(trace, trace_unavailable, |events| {
            notifications(events, &inboxes, window)
        }),
        tree,
        deliveries: Section::Available {
            rows: Deliveries {
                host_input_db: match &queue {
                    Ok((path, _)) => Section::Available { rows: path.clone() },
                    Err(reason) => Section::Unavailable {
                        reason: reason.clone(),
                    },
                },
                actors: inboxes
                    .into_values()
                    .map(|(deliveries, _)| deliveries)
                    .collect(),
            },
        },
        slowest_calls: traced(trace, trace_unavailable, |events| {
            slow_calls(events, window, observation.slowest_calls)
        }),
        rejections: traced(trace, trace_unavailable, |events| {
            rejections(events, window)
        }),
        nudges: traced(trace, trace_unavailable, |events| nudges(events, window)),
        cancellations: traced(trace, trace_unavailable, |events| {
            events
                .cancellations
                .iter()
                .filter(|row| window.contains(row.at_unix_ms))
                .map(|row| CancellationRow {
                    actor: threads.get(row.thread_id.as_str()).cloned(),
                    cancellation: row.clone(),
                })
                .collect()
        }),
    }
}

fn traced<T>(
    trace: Option<&TraceEvents>,
    unavailable: &str,
    read: impl FnOnce(&TraceEvents) -> T,
) -> Section<T> {
    match trace {
        Some(events) => Section::Available { rows: read(events) },
        None => Section::Unavailable {
            reason: unavailable.to_owned(),
        },
    }
}

fn tree(run: &Path, trace: Option<&TraceEvents>) -> Section<Vec<TreeNode>> {
    let path = run.join("actor-lifecycle.v2.jsonl");
    if !path.is_file() {
        return Section::Unavailable {
            reason: format!("{} absent", path.display()),
        };
    }
    let records = match ActorRecoveryJournal::read_observed(&path) {
        Ok(records) => records,
        Err(error) => {
            return Section::Unavailable {
                reason: format!("{}: {error}", path.display()),
            }
        }
    };
    let key = |actor: exomonad_actor::ActorRef| actor.to_string();
    let first =
        |at: Option<u64>, candidate: u64| Some(at.map_or(candidate, |at| at.min(candidate)));
    let mut nodes: BTreeMap<String, TreeNode> = BTreeMap::new();
    let mut children: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
    for record in &records {
        let admission = &record.admission;
        let actor = key(admission.actor);
        let parent = admission.supervisor_parent.map(key);
        children
            .entry(parent.clone())
            .or_default()
            .push(actor.clone());
        nodes.insert(
            actor.clone(),
            TreeNode {
                actor,
                depth: 0,
                parent,
                label: admission.label.clone(),
                role: admission.role.clone(),
                model: admission.model.clone(),
                effort: admission.effort.clone(),
                own_session: record.application.is_some(),
                thread: record
                    .application
                    .as_ref()
                    .and_then(|application| application.conversation.clone()),
                started_at_unix_ms: None,
                launched_at_unix_ms: None,
                first_tool_call_at_unix_ms: None,
                first_reply_at_unix_ms: None,
                last_reply_at_unix_ms: None,
                standing: None,
                standing_request: None,
                terminal: record
                    .terminal
                    .as_ref()
                    .map(|terminal| format!("{:?}", terminal.kind)),
            },
        );
    }
    if let Some(events) = trace {
        for transition in &events.standing {
            if let Some(node) = nodes.get_mut(&transition.actor) {
                node.started_at_unix_ms = first(node.started_at_unix_ms, transition.at);
                node.standing = Some(transition.to.clone());
                node.standing_request = transition.to_request;
            }
        }
        for (at, actor) in &events.launched {
            if let Some(node) = nodes.get_mut(actor) {
                node.launched_at_unix_ms = first(node.launched_at_unix_ms, *at);
            }
        }
        for (actor, at) in &events.first_dispatch {
            if let Some(node) = nodes.get_mut(actor) {
                node.first_tool_call_at_unix_ms = Some(*at);
            }
        }
        for effect in &events.effects {
            let Some(node) = effect.actor.as_ref().and_then(|actor| nodes.get_mut(actor)) else {
                continue;
            };
            if effect.effect == "reply" && effect.disposition == "Committed" {
                node.first_reply_at_unix_ms = first(node.first_reply_at_unix_ms, effect.at);
                node.last_reply_at_unix_ms = Some(
                    node.last_reply_at_unix_ms
                        .map_or(effect.at, |at| at.max(effect.at)),
                );
            }
        }
    }
    // Depth-first from the roots; an actor whose parent is unrecorded is a root.
    let mut ordered = Vec::with_capacity(nodes.len());
    let mut stack: Vec<(String, usize)> = children
        .iter()
        .filter(|(parent, _)| {
            parent
                .as_ref()
                .is_none_or(|parent| !nodes.contains_key(parent))
        })
        .flat_map(|(_, actors)| actors.iter().rev().map(|actor| (actor.clone(), 0)))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some((actor, depth)) = stack.pop() {
        if !seen.insert(actor.clone()) {
            continue;
        }
        if let Some(kids) = children.get(&Some(actor.clone())) {
            stack.extend(kids.iter().rev().map(|kid| (kid.clone(), depth + 1)));
        }
        if let Some(mut node) = nodes.remove(&actor) {
            node.depth = depth;
            ordered.push(node);
        }
    }
    ordered.extend(nodes.into_values());
    Section::Available { rows: ordered }
}

type HostInputs = BTreeMap<String, BTreeMap<u64, HostInput>>;

struct ActorState {
    actor: String,
    now: u64,
    cursor: Option<u64>,
    pending: usize,
    phase_age_at_least_ms: Option<u64>,
    rows: Vec<(DeliveryRow, Option<String>)>,
    unavailable: Option<String>,
}

impl ActorState {
    fn deliveries(&self) -> ActorDeliveries {
        let fenced = self.cursor.and_then(|cursor| {
            let front = self
                .rows
                .iter()
                .map(|(row, _)| row)
                .find(|row| row.sequence > cursor)?;
            let age = self.phase_age_at_least_ms?;
            (awaits_confirmation(front.durable_phase) && age >= FENCE_AFTER_MS).then(|| Fence {
                sequence: front.sequence,
                durable_phase: front.durable_phase,
                age_at_least_ms: age,
                since_at_most_unix_ms: self.now.saturating_sub(age),
                rows_behind: self
                    .pending
                    .saturating_sub(1 + self.untracked_before(cursor, front.sequence)),
            })
        });
        let rows = self
            .rows
            .iter()
            .map(|(row, _)| {
                let mut row = row.clone();
                if fenced
                    .as_ref()
                    .is_some_and(|fence| fence.sequence == row.sequence)
                {
                    row.phase = MessagePhase::Fenced;
                }
                row
            })
            .collect();
        ActorDeliveries {
            actor: self.actor.clone(),
            label: None,
            cursor: self.cursor,
            pending: self.pending,
            phase_age_at_least_ms: self.phase_age_at_least_ms,
            fenced,
            rows,
            unavailable: self.unavailable.clone(),
        }
    }

    /// Pending rows ahead of `front` that carry no receipt context.
    fn untracked_before(&self, cursor: u64, front: u64) -> usize {
        let tracked = self
            .rows
            .iter()
            .filter(|(row, _)| row.sequence > cursor && row.sequence < front)
            .count();
        (front - cursor - 1) as usize - tracked
    }
}

fn modified_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(elapsed.as_millis()).ok()
}

fn actor_state(
    node: &ActorNode,
    queue: &Result<(String, HostInputs), String>,
    now: u64,
) -> ActorState {
    let actor = format!("{}@{}", node.actor, node.incarnation);
    let mut state = ActorState {
        actor: actor.clone(),
        now,
        cursor: None,
        pending: 0,
        phase_age_at_least_ms: None,
        rows: Vec::new(),
        unavailable: None,
    };
    let cursor_path = node.directory.join("inbox.cursor");
    let rows_path = node.directory.join("inbox.jsonl");
    // Missing checkpoint reads as sequence 0 with no receipts: the inbox
    // writes its checkpoint on first acknowledgement.
    let (cursor, receipts) = match exomonad_node::read_checkpoint::<Value>(&cursor_path) {
        Ok(checkpoint) => (checkpoint.sequence, checkpoint.receipts),
        Err(error) => {
            state.unavailable = Some(format!("{}: {error}", cursor_path.display()));
            return state;
        }
    };
    state.cursor = Some(cursor);
    state.pending = node.rows.iter().filter(|row| row.sequence > cursor).count();
    state.phase_age_at_least_ms = [modified_ms(&cursor_path), modified_ms(&rows_path)]
        .into_iter()
        .flatten()
        .max()
        .map(|written| now.saturating_sub(written));
    let host_inputs = match queue {
        Ok((_, rows)) => Some(rows.get(&actor)),
        Err(_) => None,
    };
    for row in &node.rows {
        let Some(context) = &row.context else {
            continue;
        };
        let durable_phase = match receipts.get(&row.sequence) {
            Some(evidence) => Some(evidence.phase),
            None if row.sequence > cursor => Some(DeliveryPhase::Accepted),
            None => None,
        };
        let text = |name: &str| {
            serde_json::from_value::<exomonad_actor::ActorRef>(context[name].clone())
                .ok()
                .map(|actor| actor.to_string())
        };
        let provenance = if let Some(sender) = text("sender") {
            Provenance::Notification { sender }
        } else if let (Some(owner), Some(request), Some(update)) = (
            text("owner"),
            context["request"].as_u64(),
            context["update"].as_u64(),
        ) {
            Provenance::RequestUpdate {
                owner,
                request,
                update,
            }
        } else {
            Provenance::Other
        };
        let host_input = match host_inputs {
            None => HostInput::Unavailable,
            Some(rows) => rows
                .and_then(|rows| rows.get(&row.sequence))
                .cloned()
                .unwrap_or(HostInput::NoRow),
        };
        state.rows.push((
            DeliveryRow {
                sequence: row.sequence,
                provenance,
                phase: MessagePhase::of(durable_phase),
                durable_phase,
                host_input,
            },
            row.text_prefix.clone(),
        ));
    }
    state
}

/// Codex host-input rows for this run's actors, read through a read-only
/// connection. Producers are matched by their run directory name and actor
/// identity: `<run root>\0<inbox key>\0<actor id>\0<incarnation>`, as issued by
/// the actor host.
fn read_host_inputs(
    run: &Path,
    actors: &[ActorNode],
    codex_home: Option<&Path>,
) -> Result<(String, HostInputs), String> {
    let home = codex_home.ok_or("no Codex home given")?;
    let path = home.join("queue_1.sqlite");
    if !path.is_file() {
        return Err(format!("{} absent", path.display()));
    }
    let connection = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(500))
        .map_err(|error| error.to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT producer_id, sequence, state, updated_at_ms \
             FROM host_input_operations WHERE thread_id = ?1",
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let run_name = run.file_name();
    let mut result = HostInputs::new();
    for node in actors {
        let Evidence::Observed { value: thread, .. } = &node.provider_thread else {
            continue;
        };
        let actor = format!("{}@{}", node.actor, node.incarnation);
        let rows = statement
            .query_map([thread], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|error| format!("{}: {error}", path.display()))?;
        // Mirrors `notification_inbox_key` at the actor's launch site
        // (`actor_host::launch_interactive_application`); the producer id
        // itself is built through the shared `input_producer_id`, never
        // re-derived from its wire format here.
        let inbox_key = format!(
            "{}:{}:{}",
            run_name
                .map(|name| name.to_string_lossy())
                .unwrap_or_default(),
            node.actor,
            node.incarnation
        );
        let actor_ref = exomonad_actor::ActorRef {
            id: exomonad_actor::ActorId(node.actor),
            incarnation: exomonad_actor::Incarnation(node.incarnation),
        };
        let expected = crate::actor_host::input_producer_id(run, actor_ref, &inbox_key)
            .map_err(|error| error.to_string())?;
        let entry = result.entry(actor).or_default();
        for row in rows {
            let (producer, sequence, state, updated) =
                row.map_err(|error| format!("{}: {error}", path.display()))?;
            if producer != expected.as_str() {
                continue;
            }
            if let Ok(sequence) = u64::try_from(sequence) {
                entry.insert(
                    sequence,
                    HostInput::Row {
                        state,
                        updated_at_unix_ms: updated,
                    },
                );
            }
        }
    }
    Ok((path.display().to_string(), result))
}

fn notifications(
    events: &TraceEvents,
    inboxes: &BTreeMap<String, (ActorDeliveries, Vec<Option<String>>)>,
    window: TimeWindow,
) -> Vec<Notification> {
    let mut matched: BTreeSet<(String, u64)> = BTreeSet::new();
    events
        .notifications
        .iter()
        .filter(|sent| window.contains(sent.at))
        .map(|sent| {
            let receipt = match inboxes.get(&sent.target) {
                None => Receipt::NoInbox,
                Some((deliveries, texts)) => deliveries
                    .rows
                    .iter()
                    .zip(texts)
                    .find(|(row, text)| {
                        matches!(&row.provenance, Provenance::Notification { sender } if *sender == sent.sender)
                            && text.as_deref() == Some(sent.text_prefix.as_str())
                            && !matched.contains(&(sent.target.clone(), row.sequence))
                    })
                    .map_or(Receipt::NoRow, |(row, _)| {
                        matched.insert((sent.target.clone(), row.sequence));
                        Receipt::Row {
                            sequence: row.sequence,
                            phase: row.phase,
                            durable_phase: row.durable_phase,
                            presented: row.durable_phase == Some(DeliveryPhase::Presented),
                            host_input: row.host_input.clone(),
                        }
                    }),
            };
            Notification {
                at_unix_ms: sent.at,
                sender: sent.sender.clone(),
                target: sent.target.clone(),
                from_slot: sent.from_slot,
                text_prefix: sent.text_prefix.clone(),
                receipt,
            }
        })
        .collect()
}

fn slow_calls(events: &TraceEvents, window: TimeWindow, top: usize) -> SlowCalls {
    let calls: Vec<&CallTiming> = events
        .calls
        .iter()
        .filter(|call| window.contains(call.at_unix_ms))
        .collect();
    let mut slowest: Vec<CallTiming> = calls.iter().map(|call| (*call).clone()).collect();
    slowest.sort_by(|a, b| b.total_ms.cmp(&a.total_ms));
    slowest.truncate(top);
    let tools: BTreeSet<&str> = calls.iter().map(|call| call.tool.as_str()).collect();
    let mut per_tool: Vec<ToolPercentiles> = tools
        .into_iter()
        .map(Some)
        .chain([None])
        .map(|tool| {
            let selected: Vec<&&CallTiming> = calls
                .iter()
                .filter(|call| tool.is_none_or(|tool| call.tool == tool))
                .collect();
            ToolPercentiles {
                tool: tool.unwrap_or("ALL").to_owned(),
                count: selected.len(),
                total_ms: Percentiles::of(selected.iter().map(|call| call.total_ms).collect()),
                checkout_wait_ms: Percentiles::of(
                    selected.iter().map(|call| call.checkout_wait_ms).collect(),
                ),
            }
        })
        .collect();
    if calls.is_empty() {
        per_tool.clear();
    }
    let total: u64 = calls.iter().map(|call| call.total_ms).sum();
    let wait: u64 = calls.iter().map(|call| call.checkout_wait_ms).sum();
    SlowCalls {
        slowest,
        per_tool,
        checkout_wait_share_percent: if total == 0 {
            0.0
        } else {
            (wait as f64 * 1000.0 / total as f64).round() / 10.0
        },
    }
}

fn rejections(events: &TraceEvents, window: TimeWindow) -> Vec<RepeatGroup> {
    // A typed "reply rejected" line shares the rejected reply's cell span.
    let mut typed: BTreeMap<(Option<&str>, Option<&str>), Vec<&str>> = BTreeMap::new();
    for rejection in &events.reply_rejections {
        typed
            .entry((rejection.actor.as_deref(), rejection.execution.as_deref()))
            .or_default()
            .push(&rejection.rejection);
    }
    let mut groups = BTreeMap::new();
    for effect in events
        .effects
        .iter()
        .filter(|effect| effect.disposition == "Rejected")
    {
        let key = (effect.actor.as_deref(), effect.execution.as_deref());
        let reason = typed
            .get_mut(&key)
            .and_then(|reasons| (!reasons.is_empty()).then(|| reasons.remove(0).to_owned()))
            .or_else(|| {
                let execution = effect.execution.clone()?;
                events
                    .rejected_units
                    .get(&(execution, effect.input_unit_index?))
                    .cloned()
                    .flatten()
            })
            .unwrap_or_else(|| "unrecorded".into());
        if !window.contains(effect.at) {
            continue;
        }
        group(
            &mut groups,
            (
                effect.actor.clone().unwrap_or_else(|| "unknown".into()),
                effect.effect.clone(),
                reason,
                None,
            ),
            effect.at,
        );
    }
    groups_into_rows(groups)
}

fn nudges(events: &TraceEvents, window: TimeWindow) -> Vec<RepeatGroup> {
    let mut groups = BTreeMap::new();
    for invocation in events.slot_invocations.iter().filter(|invocation| {
        invocation.disposition != "Abstained" && window.contains(invocation.at)
    }) {
        group(
            &mut groups,
            (
                invocation.actor.clone(),
                "after_tool".into(),
                invocation.disposition.clone(),
                Some(invocation.detail_prefix.clone()),
            ),
            invocation.at,
        );
    }
    for sent in events
        .notifications
        .iter()
        .filter(|sent| sent.from_slot && window.contains(sent.at))
    {
        group(
            &mut groups,
            (
                sent.sender.clone(),
                "slot_notification".into(),
                format!("to {}", sent.target),
                Some(sent.text_prefix.clone()),
            ),
            sent.at,
        );
    }
    groups_into_rows(groups)
}

/// `HH:MM:SS` UTC for a Unix-millisecond timestamp.
fn clock(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1000) % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn clock_or_dash(unix_ms: Option<u64>) -> String {
    unix_ms.map_or_else(|| "-".into(), clock)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn render<T>(
    output: &mut String,
    title: &str,
    section: &Section<T>,
    body: impl FnOnce(&mut String, &T),
) {
    match section {
        Section::Available { rows } => {
            output.push_str(&format!("\n{title}"));
            body(output, rows);
        }
        Section::Unavailable { reason } => {
            output.push_str(&format!("\n{title} unavailable: {reason}"));
        }
    }
}

fn render_groups(output: &mut String, rows: &Vec<RepeatGroup>) {
    if rows.is_empty() {
        output.push_str("\n  none");
    }
    for row in rows {
        output.push_str(&format!(
            "\n  {} {} {} x{} {}..{}",
            row.actor,
            row.kind,
            row.reason,
            row.count,
            clock(row.first_at_unix_ms),
            clock(row.last_at_unix_ms)
        ));
        if let Some(detail) = &row.detail {
            output.push_str(&format!(" \"{}\"", one_line(detail)));
        }
    }
}

impl Review {
    /// Compact, line-oriented rendering with stable column order.
    pub fn concise(&self) -> String {
        let mut output = String::new();
        render(
            &mut output,
            "tree (actor label role model/effort session | started launched first_call first_reply last_reply | standing terminal):",
            &self.tree,
            |output, rows| {
                for node in rows {
                    output.push_str(&format!(
                        "\n  {}{} {} {} {}/{} {} | {} {} {} {} {} | {}{} {}",
                        "  ".repeat(node.depth),
                        node.actor,
                        node.label,
                        node.role,
                        node.model.as_deref().unwrap_or("-"),
                        node.effort.as_deref().unwrap_or("-"),
                        if node.own_session { "session" } else { "inline" },
                        clock_or_dash(node.started_at_unix_ms),
                        clock_or_dash(node.launched_at_unix_ms),
                        clock_or_dash(node.first_tool_call_at_unix_ms),
                        clock_or_dash(node.first_reply_at_unix_ms),
                        clock_or_dash(node.last_reply_at_unix_ms),
                        node.standing.as_deref().unwrap_or("-"),
                        node.standing_request
                            .map_or_else(String::new, |request| format!(" r{request}")),
                        node.terminal.as_deref().unwrap_or("-"),
                    ));
                }
            },
        );
        render(
            &mut output,
            "deliveries (current durable state, window not applied; label actor cursor pending inbox last_message):",
            &self.deliveries,
            |output, deliveries| {
                if let Section::Unavailable { reason } = &deliveries.host_input_db {
                    output.push_str(&format!("\n  host_input unavailable: {reason}"));
                }
                for actor in &deliveries.actors {
                    let name = actor.label.as_deref().unwrap_or("-");
                    if let Some(reason) = &actor.unavailable {
                        output.push_str(&format!(
                            "\n  {name} {} inbox=unavailable({reason})",
                            actor.actor
                        ));
                        continue;
                    }
                    let inbox = match &actor.fenced {
                        Some(fence) => {
                            let front = actor.rows.iter().find(|row| row.sequence == fence.sequence);
                            format!(
                                "fenced(ref{} {}, {} behind, host_input={}, since<={})",
                                fence.sequence,
                                durable_label(fence.durable_phase),
                                fence.rows_behind,
                                front.map_or_else(|| "-".into(), |row| row.host_input.label()),
                                clock(fence.since_at_most_unix_ms)
                            )
                        }
                        None if actor.pending == 0 => "clear".into(),
                        None => format!("pending({})", actor.pending),
                    };
                    let last = actor.rows.last().map_or_else(
                        || "none".into(),
                        |row| {
                            format!(
                                "ref{} {}/{}",
                                row.sequence,
                                row.phase.label(),
                                if row.durable_phase == Some(DeliveryPhase::Presented)
                                    || row.phase == MessagePhase::Acknowledged
                                {
                                    "presented"
                                } else {
                                    "not-presented"
                                }
                            )
                        },
                    );
                    output.push_str(&format!(
                        "\n  {name} {} cursor={} pending={} inbox={inbox} last_message={last}",
                        actor.actor,
                        actor.cursor.unwrap_or(0),
                        actor.pending,
                    ));
                    let cursor = actor.cursor.unwrap_or(0);
                    for row in actor.rows.iter().filter(|row| row.sequence > cursor) {
                        let provenance = match &row.provenance {
                            Provenance::Notification { sender } => {
                                format!("notification from {sender}")
                            }
                            Provenance::RequestUpdate {
                                owner,
                                request,
                                update,
                            } => format!("request_update r{request}u{update} from {owner}"),
                            Provenance::Other => "other".into(),
                        };
                        output.push_str(&format!(
                            "\n    ref{} {provenance} {} ({}) host_input={}",
                            row.sequence,
                            row.phase.label(),
                            durable_label(row.durable_phase),
                            row.host_input.label()
                        ));
                    }
                }
            },
        );
        render(
            &mut output,
            "notifications (time sender->target receipt \"text\"):",
            &self.notifications,
            |output, rows| {
                if rows.is_empty() {
                    output.push_str("\n  none");
                }
                for row in rows {
                    let receipt = match &row.receipt {
                        Receipt::Row {
                            sequence,
                            presented: true,
                            ..
                        } => format!("presented at ref{sequence}"),
                        Receipt::Row {
                            sequence,
                            phase,
                            durable_phase,
                            host_input,
                            ..
                        } => format!(
                            "not-presented ref{sequence} {} ({}) host_input={}",
                            phase.label(),
                            durable_label(*durable_phase),
                            host_input.label()
                        ),
                        Receipt::NoRow => "not presented: no inbox row".into(),
                        Receipt::NoInbox => "not presented: target has no inbox".into(),
                    };
                    output.push_str(&format!(
                        "\n  {} {}->{}{} {receipt} \"{}\"",
                        clock(row.at_unix_ms),
                        row.sender,
                        row.target,
                        if row.from_slot { " (slot)" } else { "" },
                        one_line(&row.text_prefix)
                    ));
                }
            },
        );
        render(
            &mut output,
            "slowest calls (time actor tool total wait hold compile(n) jev(n) exec outcome):",
            &self.slowest_calls,
            |output, calls| {
                for call in &calls.slowest {
                    output.push_str(&format!(
                        "\n  {} {} {} total={} wait={} hold={} compile={}({}) jev={}({}) exec={} {}",
                        clock(call.at_unix_ms),
                        call.actor,
                        call.tool,
                        call.total_ms,
                        call.checkout_wait_ms,
                        call.checkout_hold_ms,
                        call.compile_ms,
                        call.compile_count,
                        call.jev_ms,
                        call.jev_count,
                        call.exec_ms,
                        call.outcome
                    ));
                }
                output
                    .push_str("\n  per tool (n total p50/p90/max | checkout_wait p50/p90/max ms):");
                for tool in &calls.per_tool {
                    output.push_str(&format!(
                        "\n    {} n={} {}/{}/{} | {}/{}/{}",
                        tool.tool,
                        tool.count,
                        tool.total_ms.p50,
                        tool.total_ms.p90,
                        tool.total_ms.max,
                        tool.checkout_wait_ms.p50,
                        tool.checkout_wait_ms.p90,
                        tool.checkout_wait_ms.max
                    ));
                }
                output.push_str(&format!(
                    "\n    checkout_wait share of call time: {}%",
                    calls.checkout_wait_share_percent
                ));
            },
        );
        render(
            &mut output,
            "rejections (actor effect reason xcount first..last):",
            &self.rejections,
            render_groups,
        );
        render(
            &mut output,
            "nudges (actor kind reason xcount first..last \"detail\"):",
            &self.nudges,
            render_groups,
        );
        render(
            &mut output,
            "cancellations (time actor call outcome execution):",
            &self.cancellations,
            |output, rows| {
                if rows.is_empty() {
                    output.push_str("\n  none");
                }
                for row in rows {
                    output.push_str(&format!(
                        "\n  {} {} {} {} {}",
                        clock(row.cancellation.at_unix_ms),
                        row.actor.as_deref().unwrap_or(&row.cancellation.thread_id),
                        row.cancellation.call_id,
                        row.cancellation.outcome,
                        row.cancellation.execution.as_deref().unwrap_or("-")
                    ));
                }
            },
        );
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_map::{read_observed_run, Limits};
    use serde_json::json;
    use std::fs;

    const TRACE: &str = include_str!("fixtures/review-trace.jsonl");

    fn lines(rows: &[Value]) -> String {
        rows.iter().map(|row| format!("{row}\n")).collect()
    }

    /// A run with a root, a lead with its own session and a fenced inbox, and
    /// an inline research fork. Returns (run directory, Codex home).
    fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
        let run = dir.join("run-7");
        let workspace = dir.join("workspace");
        let codex = dir.join("codex");
        fs::create_dir_all(run.join("2-1")).unwrap();
        fs::create_dir_all(workspace.join(".exomonad/logs")).unwrap();
        fs::create_dir_all(&codex).unwrap();
        fs::write(
            run.join("status.json"),
            json!({
                "version":4,"run_id":"run-7","workspace":workspace,"session":"test",
                "agent":{"model":"test","effort":"low"},
                "phase":{"state":"ready","root_actor":{"id":1,"incarnation":1},"root_thread":"thread-root"}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(workspace.join(".exomonad/logs/run-7.jsonl"), TRACE).unwrap();
        let admission = |id: u64, parent: Option<u64>, label: &str, role: &str| {
            json!({"version":2,"event":"admitted","admission":{
                "actor":{"id":id,"incarnation":1},"label":label,
                "creator":parent.map(|id| json!({"id":id,"incarnation":1})),
                "supervisor_parent":parent.map(|id| json!({"id":id,"incarnation":1})),
                "context_parent":null,"actor_path":null,"role":role,
                "model":"executor","effort":"medium","instructions":null,
                "launch_worktrees":[],"source_layer":[]}})
        };
        let bind = |id: u64, thread: &str| {
            [
                json!({"version":2,"event":"application_prepared","actor":{"id":id,"incarnation":1},
                    "binding_path":format!("/run/{id}-1/binding.json")}),
                json!({"version":2,"event":"application_bound","actor":{"id":id,"incarnation":1},
                    "conversation":thread}),
            ]
        };
        let mut journal = vec![
            json!({"version":2,"event":"created"}),
            admission(1, None, "root", "root"),
            admission(2, Some(1), "lead", "coding"),
            admission(3, Some(1), "work", "research"),
        ];
        journal.extend(bind(1, "thread-root"));
        journal.extend(bind(2, "thread-lead"));
        for (index, row) in journal.iter_mut().enumerate() {
            row["sequence"] = json!(index + 1);
        }
        fs::write(run.join("actor-lifecycle.v2.jsonl"), lines(&journal)).unwrap();
        fs::write(
            run.join("2-1/binding.json"),
            json!({"version":5,"thread":"thread-lead"}).to_string(),
        )
        .unwrap();
        let from_root =
            json!({"sender":{"id":1,"incarnation":1},"target":{"id":2,"incarnation":1}});
        fs::write(
            run.join("2-1/inbox.jsonl"),
            lines(&[
                json!({"sequence":1,"payload":"Earlier note.","receipt_context":from_root}),
                json!({"sequence":2,"payload":"Plan accepted; no planner hold.","receipt_context":from_root}),
                json!({"sequence":3,"payload":{"type":"childExited"}}),
                json!({"sequence":4,"payload":"Later note.","receipt_context":from_root}),
            ]),
        )
        .unwrap();
        fs::write(
            run.join("2-1/inbox.cursor"),
            json!({"version":2,"checkpoint":{"sequence":1,"watermarks":{},"receipts":{
                "1":{"context":from_root,"phase":"presented"},
                "2":{"context":from_root,"phase":"unconfirmed"}}}})
            .to_string(),
        )
        .unwrap();
        let db = rusqlite::Connection::open(codex.join("queue_1.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE host_input_operations (thread_id TEXT NOT NULL, producer_id TEXT NOT NULL, \
             sequence INTEGER NOT NULL, state TEXT NOT NULL, updated_at_ms INTEGER NOT NULL);",
        )
        .unwrap();
        for (producer, sequence) in [
            (
                [run.display().to_string().as_str(), "run-7:2:1", "2", "1"].join("\0"),
                1,
            ),
            // Another run's producer on the same thread is not this inbox's row.
            (["/elsewhere/run-6", "run-6:2:1", "2", "1"].join("\0"), 2),
        ] {
            db.execute(
                "INSERT INTO host_input_operations VALUES ('thread-lead', ?1, ?2, 'presented', 5)",
                rusqlite::params![producer, sequence],
            )
            .unwrap();
        }
        (run, codex)
    }

    fn observe(run: &Path, codex: Option<PathBuf>, after_write_ms: u64) -> crate::run_map::RunMap {
        let written = modified_ms(&run.join("2-1/inbox.cursor")).unwrap();
        read_observed_run(
            run,
            Limits::default(),
            TimeWindow::default(),
            &Observation {
                now_unix_ms: written + after_write_ms,
                codex_home: codex,
                slowest_calls: 1,
            },
        )
        .unwrap()
    }

    fn rows<T>(section: &Section<T>) -> &T {
        match section {
            Section::Available { rows } => rows,
            Section::Unavailable { reason } => panic!("section unavailable: {reason}"),
        }
    }

    #[test]
    fn review_fences_front_row_and_joins_every_source() {
        let dir = tempfile::tempdir().unwrap();
        let (run, codex) = fixture(dir.path());
        let report = observe(&run, Some(codex), FENCE_AFTER_MS + 5_000);
        let review = &report.review;

        let tree = rows(&review.tree);
        let order: Vec<_> = tree
            .iter()
            .map(|node| (node.actor.as_str(), node.depth))
            .collect();
        assert_eq!(order, [("1@1", 0), ("2@1", 1), ("3@1", 1)]);
        assert!(tree[1].own_session && !tree[2].own_session);
        assert_eq!(tree[1].standing.as_deref(), Some("interactive"));
        assert_eq!(tree[1].standing_request, Some(1));
        assert!(tree[1].first_reply_at_unix_ms.is_some());
        assert!(tree[0].first_tool_call_at_unix_ms.is_some());

        let lead = &rows(&review.deliveries).actors[0];
        let fence = lead.fenced.as_ref().expect("front row fenced");
        assert_eq!((fence.sequence, fence.rows_behind), (2, 2));
        assert!(fence.age_at_least_ms >= FENCE_AFTER_MS);
        let phases: Vec<_> = lead
            .rows
            .iter()
            .map(|row| (row.sequence, row.phase))
            .collect();
        assert_eq!(
            phases,
            [
                (1, MessagePhase::Presented),
                (2, MessagePhase::Fenced),
                (4, MessagePhase::Receipt)
            ]
        );
        assert!(matches!(lead.rows[0].host_input, HostInput::Row { .. }));
        assert!(matches!(lead.rows[1].host_input, HostInput::NoRow));

        let notifications = rows(&review.notifications);
        assert_eq!(notifications.len(), 3);
        assert!(matches!(
            notifications[1].receipt,
            Receipt::Row {
                sequence: 2,
                phase: MessagePhase::Fenced,
                presented: false,
                ..
            }
        ));
        assert!(matches!(notifications[2].receipt, Receipt::NoInbox));
        // The root has no inbox directory in this fixture.
        assert!(matches!(notifications[0].receipt, Receipt::NoInbox));

        let calls = rows(&review.slowest_calls);
        assert_eq!(calls.slowest.len(), 1);
        assert_eq!(calls.slowest[0].total_ms, 38_986);
        let all = calls
            .per_tool
            .iter()
            .find(|tool| tool.tool == "ALL")
            .unwrap();
        assert_eq!(
            (all.count, all.total_ms.max, all.checkout_wait_ms.max),
            (2, 38_986, 94)
        );

        let rejections = rows(&review.rejections);
        assert_eq!(rejections.len(), 1);
        assert_eq!(
            (rejections[0].reason.as_str(), rejections[0].count),
            ("UpdatePending", 2)
        );
        let nudges = rows(&review.nudges);
        assert_eq!(nudges.len(), 1);
        assert_eq!(
            (nudges[0].reason.as_str(), nudges[0].count),
            ("Annotated", 2)
        );
        let cancellations = rows(&review.cancellations);
        assert_eq!(cancellations[0].actor.as_deref(), Some("2@1"));

        let text = report.concise();
        assert!(
            text.contains("lead 2@1 cursor=1 pending=3 inbox=fenced(ref2 unconfirmed, 2 behind, host_input=no row"),
            "{text}"
        );
        assert!(
            text.contains("last_message=ref4 receipt/not-presented"),
            "{text}"
        );
        assert!(!report
            .diagnostics
            .iter()
            .any(|line| line.contains("missing event type")));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["review"]["deliveries"]["rows"]["actors"][0]["rows"][1]["durable_phase"],
            "unconfirmed"
        );
    }

    #[test]
    fn review_degrades_per_section_when_sources_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (run, _) = fixture(dir.path());
        // Too recent to fence, and no Codex home: host input is unavailable.
        let report = observe(&run, None, 1_000);
        let deliveries = rows(&report.review.deliveries);
        assert!(deliveries.actors[0].fenced.is_none());
        assert!(matches!(
            deliveries.host_input_db,
            Section::Unavailable { .. }
        ));
        assert!(matches!(
            deliveries.actors[0].rows[0].host_input,
            HostInput::Unavailable
        ));

        fs::remove_file(dir.path().join("workspace/.exomonad/logs/run-7.jsonl")).unwrap();
        fs::remove_file(run.join("actor-lifecycle.v2.jsonl")).unwrap();
        let report = observe(&run, None, FENCE_AFTER_MS);
        let review = &report.review;
        assert!(matches!(review.tree, Section::Unavailable { .. }));
        assert!(matches!(review.notifications, Section::Unavailable { .. }));
        assert!(matches!(review.slowest_calls, Section::Unavailable { .. }));
        assert!(rows(&review.deliveries).actors[0].fenced.is_some());
        let text = report.concise();
        assert!(
            text.contains("notifications (time sender->target receipt \"text\"): unavailable: "),
            "{text}"
        );
    }

    #[test]
    fn duration_parser_accepts_units_and_rejects_bare_numbers() {
        use crate::run_map::parse_duration_ms;
        assert_eq!(parse_duration_ms("15m"), Ok(900_000));
        assert_eq!(parse_duration_ms("2h"), Ok(7_200_000));
        assert_eq!(parse_duration_ms("90s"), Ok(90_000));
        assert!(parse_duration_ms("15").is_err());
        assert!(parse_duration_ms("m").is_err());
        assert!(parse_duration_ms("5y").is_err());
    }
}
