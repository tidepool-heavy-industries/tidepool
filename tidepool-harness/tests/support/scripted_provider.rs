//! A shared STRUCTURAL scripted [`ModelProvider`] — one `(path, phase)`-keyed
//! provider, home for the pattern `companion_recursive_slice.rs` proved out
//! (commit b135d174, "re-key scripted provider on (path, phase) structure,
//! prune text pins") and that `delegate_positive_path.rs`/
//! `delegate_merge_fold.rs` had each independently re-derived by hand as
//! needle-SET matching against the same harness's rendered prose — three
//! copies of one mechanism, each free to drift from the others. This is the
//! one copy.
//!
//! # Why `(path, phase)`, never an arbitrary prompt substring
//!
//! `ModelProvider::complete` sees only the assembled [`TurnRequest`] — no
//! `NodeId`/`SiteId`/hole-kind rides the wire (see `tidepool-harness/CLAUDE.md`
//! and `tidepool_harness::provider::TurnRequest`'s own fields: `messages` and
//! `max_tokens`, nothing else). Adding one would be a production-code change
//! to make tests easier, which is out of scope here. What the self-iterating
//! driver DOES render, in every cognition window's own prompt, is ONE
//! canonical, stable header line: `NODE <path> — DISCOVER (…)` / `NODE <path>
//! — FOLD (…)` (`Harness.hs`'s own voice, structurally analogous to the
//! `bulkLayerWindow` label riding the wire for the driver's OWN routing).
//! [`parse_window`] is the ONE place any test parses it — a single stable
//! parse point, never a scattered needle-substring probe against arbitrary
//! prose — so a prompt-wording change elsewhere (framing, hole-card boilerplate,
//! effect descriptions) cannot desync a scripted scenario from the window it
//! means to answer. This is "structural" in the sense that matters here: the
//! key is a harness-rendered IDENTITY marker in a fixed format, not
//! model-facing prose content a wording edit is free to reword.
//!
//! `ReplayProvider` cannot serve this shape: its queue is strictly FIFO, and
//! the order in which sibling windows (a fanout/branch-fanout's concurrent
//! children) reach the provider is itself sometimes the thing under test.
//!
//! # Using this from a new suite
//!
//! ```ignore
//! let provider = KeyedProvider::new(vec![
//!     script(PathKey::Exact("root"), Phase::Discover, some_reply()),
//!     script(PathKey::Prefix("root/2-"), Phase::Discover, another_reply()),
//!     // catch-all fold entry, matched LAST (first-match-wins order):
//!     script(PathKey::Prefix(""), Phase::Fold, fold_reply()),
//! ]);
//! ```
//!
//! Entries are matched IN DECLARATION ORDER, first match wins — put more
//! specific keys before a catch-all `Prefix("")`. A request whose header
//! matches no entry is a loud [`ProviderError`], never a default reply.

#![allow(dead_code)]

use parking_lot::Mutex;

use tidepool_harness::provider::{
    ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage,
};

/// Which cognition-window pass a prompt belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Discover,
    Fold,
}

/// Which structural key one [`Script`] answers.
///
/// `Exact` is for a branch whose title the SUITE authored (so its rendered
/// slug is known ahead of time). `Prefix` is for a branch whose title is
/// MODEL-produced (a hostile title, an operator's `Add` verdict) or whose
/// exact slug the suite does not want to hardcode — only the POSITION segment
/// (`"root/2-"`) is knowable in advance, and the harness is left to own the
/// slug.
#[derive(Debug, Clone, Copy)]
pub enum PathKey {
    Exact(&'static str),
    Prefix(&'static str),
}

impl PathKey {
    pub fn matches(&self, path: &str) -> bool {
        match self {
            PathKey::Exact(p) => path == *p,
            PathKey::Prefix(p) => path.starts_with(p),
        }
    }
}

/// One scripted window, keyed on `(path, phase)` — never on prompt text.
///
/// `replies` is consumed front-to-back across successive matches of this SAME
/// key, and the last entry sticks for every match past the end of the list —
/// a single-reply `Script` repeats its one reply for every round of a window
/// that never finalizes; a multi-entry list serves a genuinely different
/// reply per call (a multi-round retry's second, corrected attempt) without
/// matching a marker string inside a later prompt to tell the calls apart.
pub struct Script {
    pub path: PathKey,
    pub phase: Phase,
    pub replies: Vec<String>,
}

pub fn script(path: PathKey, phase: Phase, reply: String) -> Script {
    Script {
        path,
        phase,
        replies: vec![reply],
    }
}

/// As [`script`], for a window that needs more than one scripted reply across
/// successive rounds (see [`Script::replies`]'s doc).
pub fn script_rounds(path: PathKey, phase: Phase, replies: Vec<String>) -> Script {
    Script {
        path,
        phase,
        replies,
    }
}

/// `NODE <path> — DISCOVER (…` / `NODE <path> — FOLD (…` → `(path, phase)`.
/// The ONE place this module parses a prompt, so the coupling to
/// `Harness.hs`'s prompt headers is a single line rather than scattered.
///
/// Locates the header rather than anchoring at the start of `prompt`: a
/// driver-recorded hole prompt (`DriverEvent::RunLLMTurnHole`) starts with it
/// verbatim, but the FULL message a provider request embeds it in carries the
/// hole card's own lead-in first ("The loop needs a typed answer of type
/// `T`.\n\n") — every caller shares this one function rather than re-deriving
/// the search.
pub fn parse_window(prompt: &str) -> Option<(String, Phase)> {
    let start = prompt.find("NODE ")?;
    let rest = &prompt[start + "NODE ".len()..];
    let (path, tail) = rest.split_once(" — ")?;
    let phase = if tail.starts_with("DISCOVER") {
        Phase::Discover
    } else if tail.starts_with("FOLD") {
        Phase::Fold
    } else {
        return None;
    };
    Some((path.to_string(), phase))
}

/// The message a request is MATCHED against: the last one carrying a window
/// prompt header, not simply the last message.
///
/// Two things make "the last message" wrong here, and both are real: a
/// branch child's request opens with its PARENT's frozen transcript (which
/// contains the parent's own prompt header), so matching must look at the
/// LAST such header, not the first; and a window that burns its round budget
/// is re-prompted with the driver's round-cap ultimatum, which carries no
/// header at all — a starved window would otherwise stop matching its own
/// scripted entry halfway through being starved.
pub fn window_message(req: &TurnRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.content.contains(" — DISCOVER") || m.content.contains(" — FOLD"))
        .or_else(|| req.messages.last())
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// A [`ModelProvider`] that answers each cognition window by its STRUCTURAL
/// `(path, phase)` key (see this module's doc), never by matching an
/// arbitrary substring anywhere in the request, and never by call ORDER.
///
/// Entries are matched IN ORDER, first match wins, so a catch-all
/// ([`PathKey::Prefix`] of `""`) can sit last. A request nothing matches is a
/// loud [`ProviderError`], never a default reply: "a window that must not
/// run, ran" has to fail the run rather than be quietly served.
pub struct KeyedProvider {
    scripted: Vec<Script>,
    /// Per-entry index into `scripted[i].replies`, advanced on every match.
    cursors: Mutex<Vec<usize>>,
    /// The prompt of every window the provider was asked to answer, in
    /// order.
    seen: Mutex<Vec<String>>,
}

impl KeyedProvider {
    pub fn new(scripted: Vec<Script>) -> Self {
        let cursors = Mutex::new(vec![0; scripted.len()]);
        KeyedProvider {
            scripted,
            cursors,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Every window prompt this provider was asked to answer, in order.
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().clone()
    }
}

impl ModelProvider for KeyedProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let last = window_message(&req);
        self.seen.lock().push(last.clone());

        let (path, phase) = parse_window(&last).ok_or_else(|| {
            ProviderError::Api(format!(
                "KeyedProvider: request carries no \"NODE <path> — DISCOVER/FOLD\" \
                 header to key a reply on:\n{last}"
            ))
        })?;

        let idx = self
            .scripted
            .iter()
            .position(|s| s.phase == phase && s.path.matches(&path))
            .ok_or_else(|| {
                ProviderError::Api(format!(
                    "KeyedProvider: no scripted reply for path {path:?} phase {phase:?}"
                ))
            })?;

        let reply = {
            let mut cursors = self.cursors.lock();
            let replies = &self.scripted[idx].replies;
            let cursor = cursors[idx].min(replies.len() - 1);
            cursors[idx] += 1;
            replies[cursor].clone()
        };

        Ok(TurnResponse {
            text: reply,
            usage: Usage {
                input_tokens: 50,
                output_tokens: 10,
                cached_input_tokens: None,
                cache_write_tokens: None,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}
