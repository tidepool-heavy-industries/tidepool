//! Local-file persistence for the self-iterating harness: no DB, plain
//! files under `<cache_dir>/selfharness/`.
//!
//! - **[`Checkpoint`]**: the one durable record a restart reads. It carries
//!   a completed cycle's `State` json, the compaction summary in force at
//!   that same cycle, a monotonic `generation`, and a fingerprint of the
//!   harness source that produced it, all written together so a restart can
//!   never pair a state from one cycle with a summary from another.
//!   [`save_checkpoint`]/[`load_checkpoint`] round-trip it through a file,
//!   written atomically (a sibling `.tmp` file, then renamed over the
//!   target) so a kill mid-write never leaves a torn file for
//!   [`load_checkpoint`] to observe.
//! - **transcript → jsonl**: [`JsonlObserver`] is an
//!   [`crate::selfharness::observer::Observer`] impl that appends every
//!   driver [`Event`](crate::selfharness::observer::Event) as one jsonl
//!   line — reuses the existing pluggable observer seam rather than adding
//!   a second logging path into `driver.rs`.
//!
//! Harness-module reload on restart needs no code here:
//! [`crate::selfharness::harness_source::load_harness_source`] always
//! resolves from the on-disk path, and
//! [`crate::selfharness::driver::SelfHarnessDriver::bootstrap`] compiles
//! from that source fresh for a new process — a restarted process
//! constructing a new driver and calling `load_harness_source` again
//! already picks up an edited harness file; only the checkpoint needs an
//! explicit save/restore path.

use std::io::Write;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use super::observer::{Event, Observer};

/// A checkpoint's monotonic generation. `0` never appears on disk — the first
/// commit is generation `1` — so this wraps [`NonZeroU64`] rather than a plain
/// `u64`: a `0` in a checkpoint file is a typed [`PersistenceError::Json`] at
/// [`load_checkpoint`] (serde's own `NonZeroU64` support rejects it), never a
/// silently-accepted invariant breach. `#[serde(transparent)]` keeps the wire
/// byte for byte identical to the plain `u64` this replaces.
///
/// Distinct from [`LoopIteration`] on purpose: the two used to be
/// interchangeable public `u64` fields on [`Checkpoint`], which meant nothing
/// stopped a restore/commit edit from swapping them (both compile, both
/// serialize, and the wrong one is only wrong at replay time).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CheckpointGeneration(NonZeroU64);

impl CheckpointGeneration {
    /// The generation of the very first checkpoint a fresh table ever commits.
    pub const FIRST: CheckpointGeneration = CheckpointGeneration(NonZeroU64::MIN);

    pub fn get(self) -> u64 {
        self.0.get()
    }

    /// The checked successor. Private: the only place a generation ever
    /// advances is [`Checkpoint::committed`], so there is exactly one call
    /// site that can get this wrong.
    fn next(self) -> Self {
        #[allow(
            clippy::expect_used,
            reason = "CheckpointGeneration::next is the only place a generation ever advances"
        )]
        CheckpointGeneration(self.0.checked_add(1).expect(
            "checkpoint generation overflowed u64 — this would take billions of committed cycles",
        ))
    }
}

/// The loop-iteration count carried in the checkpoint envelope — a runtime
/// fact, never part of the authored `State`
/// (`plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
/// context is the runtime's job"). `#[serde(transparent)]` keeps the wire byte
/// for byte identical to the plain `u64` this replaces. See
/// [`CheckpointGeneration`]'s docs for why this is a distinct type rather than
/// a second `u64` field with a different persistence law.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoopIteration(u64);

impl LoopIteration {
    pub fn new(n: u64) -> Self {
        LoopIteration(n)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Set to `"1"` to forcibly claim a run whose lease still names a LIVE prior
/// process — see [`PersistenceError::LiveLeaseHeld`] and
/// `crate::selfharness::resume::acquire_lease`'s doc. Absent (the default),
/// a live-owned lease is a hard refusal, never a silent join: two processes
/// committing the same effects twice — writes, commits, model calls — is
/// exactly the hazard PRD 20's segments-not-single-file design leaves open,
/// since segment safety only protects the JOURNAL bytes, never anything an
/// answerer turn already did in the outside world. Read directly by
/// `acquire_lease`, and named here (beside the error it gates) rather than
/// in `resume.rs`, so the constant and the message that tells an operator to
/// set it can never drift apart.
pub const LEASE_TAKEOVER_ENV_VAR: &str = "TIDEPOOL_SELFHARNESS_TAKEOVER";

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("selfharness persistence io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("selfharness persistence: JSON at {path} is malformed: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error(
        "selfharness lease: run {run_id:?} is already held by live process pid {pid} — \
         refusing to join it (joining would double-run every effect that process already \
         committed — writes, commits, model calls). If pid {pid} is genuinely gone (a stale \
         record after a reboot, or a pid a killed process's slot was reused by something \
         unrelated), set TIDEPOOL_SELFHARNESS_TAKEOVER=1 and restart to forcibly take over the \
         run; otherwise stop that process first."
    )]
    LiveLeaseHeld { run_id: String, pid: u32 },
}

/// The one durable record a restart reads: a completed cycle's `State`,
/// the compaction summary in force at that same cycle, a monotonic
/// generation counter, a fingerprint of the harness source that produced
/// it, and the loop-iteration count. Written as a whole at one commit
/// boundary — never assembled from two separately-timed writes — so a state
/// and a summary read back together are always from the same generation.
///
/// `generation` and `iteration` are private, readable only through
/// [`Self::generation`]/[`Self::iteration`]: the two used to be plain,
/// interchangeable `u64` fields that a construction site could swap (both
/// compile, both serialize, and only replay ever notices). The only normal
/// write path is [`Self::committed`], which DERIVES the generation from the
/// previous one rather than accepting it as an argument — so it can be
/// skipped, repeated, or confused with `iteration` by nothing this crate
/// writes. `state`/`compaction`/`harness_source` stay plain public fields:
/// nothing about them is interchangeable with a counter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Monotonic, incremented by one per committed cycle. `0` never appears
    /// on disk — the first commit is generation `1`.
    generation: CheckpointGeneration,
    /// The loop-boundary `State` json this generation's cycle produced
    /// ([`crate::selfharness::state_cross::state_out`]).
    pub state: Json,
    /// The compaction summary in force when this generation committed —
    /// `None` if no compaction has fired yet at any point up to and
    /// including this cycle.
    pub compaction: Option<String>,
    /// [`crate::selfharness::harness_source::HarnessSource::fingerprint`] of
    /// the source that produced this generation.
    pub harness_source: String,
    /// The number of loop cycles completed as of this generation — a
    /// runtime fact, not part of the authored `State` (see
    /// `plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
    /// context is the runtime's job"). `0` before any cycle has completed;
    /// incremented by one per completed cycle, alongside `generation`.
    iteration: LoopIteration,
    /// Whether the driver was PARKED on the between-turns operator gate
    /// (`SelfHarnessDriver::between_loops_gate`, `gate.await_continue()`) the
    /// last time this generation's checkpoint was written to disk. The gate
    /// park itself is a live continuation only — it holds no state a
    /// checkpoint otherwise captures — so without this flag a kill while
    /// parked and a kill mid-cycle look identical on restore, and the restart
    /// silently runs the next turn instead of re-presenting the "start the
    /// next turn?" gate to the operator. Set (and the checkpoint re-saved, at
    /// the SAME generation — see [`Self::with_awaiting_continue`]) right
    /// before the driver blocks on `await_continue`; cleared the moment a
    /// continue signal actually arrives, before any further turn work. A
    /// checkpoint committed for a genuinely completed cycle
    /// ([`Self::committed`]) is never parked, so it is always `false` there.
    /// `#[serde(default)]` plus `skip_serializing_if`: an old checkpoint file
    /// with no such key deserializes as `false` (today's behavior, byte for
    /// byte), and a checkpoint that was never parked serializes with no new
    /// key at all — the precedent is `Usage.cached_input_tokens`/
    /// `TurnDelta.reasoning`'s additive widenings.
    #[serde(default, skip_serializing_if = "is_false")]
    awaiting_continue: bool,
    /// The operator's between-loops message
    /// ([`crate::selfharness::operator::ContinueSignal::ContinueWithInput`]),
    /// captured in the SAME atomic write that clears `awaiting_continue`
    /// (`SelfHarnessDriver::between_loops_gate`) so a kill anywhere after
    /// that write cannot separate "gate cleared" from "operator text
    /// captured" — restore rehydrates it, and the end-of-cycle
    /// [`Self::committed`] clears it again once it has been consumed into a
    /// window's framing. `#[serde(default, skip_serializing_if)]`: the same
    /// additive-widening precedent as `awaiting_continue` — an old
    /// checkpoint with no such key deserializes as `None`, and a checkpoint
    /// with nothing pending serializes with no new key at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_operator_input: Option<String>,
    /// The highest [`crate::selfharness::observer::AskId`] minted as of this
    /// checkpoint — seeds [`SelfHarnessDriver`](crate::selfharness::driver::SelfHarnessDriver)'s
    /// ask-id counter on restore so a restarted process cannot re-mint an id
    /// already used earlier in the SAME `transcript.jsonl` (which spans
    /// restarts). `0` before any `askUser`/`note` form has ever been
    /// presented. `#[serde(default, skip_serializing_if)]`: same additive
    /// precedent as `awaiting_continue`/`pending_operator_input` — an old
    /// checkpoint with no such key deserializes as `0`, and a checkpoint
    /// that never minted an id serializes with no new key at all.
    #[serde(default, skip_serializing_if = "is_zero")]
    ask_id_high_water: u64,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Checkpoint {
    pub fn generation(&self) -> CheckpointGeneration {
        self.generation
    }

    pub fn iteration(&self) -> LoopIteration {
        self.iteration
    }

    pub fn awaiting_continue(&self) -> bool {
        self.awaiting_continue
    }

    pub fn pending_operator_input(&self) -> Option<&str> {
        self.pending_operator_input.as_deref()
    }

    pub fn ask_id_high_water(&self) -> u64 {
        self.ask_id_high_water
    }

    /// Commit the generation AFTER `previous` (or [`CheckpointGeneration::FIRST`]
    /// when there is none yet — the very first checkpoint) — the only normal
    /// write path. A caller never supplies a generation directly, which is
    /// what makes "advances exactly once, from what came before" true by
    /// construction rather than by convention at each call site. A newly
    /// committed cycle is never parked — `awaiting_continue` is always
    /// `false` here; see [`Self::with_awaiting_continue`] for the gate-park
    /// marker write, which does NOT go through this constructor (it must
    /// NOT advance the generation).
    pub fn committed(
        previous: Option<CheckpointGeneration>,
        state: Json,
        compaction: Option<String>,
        harness_source: String,
        iteration: LoopIteration,
    ) -> Self {
        Checkpoint {
            generation: previous.map_or(CheckpointGeneration::FIRST, CheckpointGeneration::next),
            state,
            compaction,
            harness_source,
            iteration,
            awaiting_continue: false,
            pending_operator_input: None,
            ask_id_high_water: 0,
        }
    }

    /// A copy of `self` with `awaiting_continue` set, at the SAME generation
    /// — this is not a newly completed cycle, only a record of whether the
    /// driver is currently parked on the between-turns gate, so it must NOT
    /// advance the generation the way [`Self::committed`] does. The only
    /// legitimate way to change this flag: there is no public constructor
    /// that accepts an arbitrary generation directly.
    pub fn with_awaiting_continue(&self, awaiting_continue: bool) -> Self {
        Checkpoint {
            awaiting_continue,
            ..self.clone()
        }
    }

    /// A copy of `self` with `pending_operator_input` set, at the SAME
    /// generation — chained onto [`Self::with_awaiting_continue`] by
    /// `SelfHarnessDriver::between_loops_gate` so the gate-clear write and
    /// the operator's captured text land in the ONE `save_checkpoint` call
    /// that write performs, never two separately-timed writes.
    pub fn with_pending_operator_input(&self, pending_operator_input: Option<String>) -> Self {
        Checkpoint {
            pending_operator_input,
            ..self.clone()
        }
    }

    /// A copy of `self` with `ask_id_high_water` set, at the SAME generation
    /// — used both by [`Self::committed`]'s caller (after minting the
    /// checkpoint for a completed cycle) and by the gate-clear write, so the
    /// persisted high-water mark tracks whatever the driver's own counter
    /// last reached at either checkpoint-write site.
    pub fn with_ask_id_high_water(&self, ask_id_high_water: u64) -> Self {
        Checkpoint {
            ask_id_high_water,
            ..self.clone()
        }
    }
}

/// Default checkpoint path: `<cache_dir>/selfharness/checkpoint.json`. A
/// test (or the binary's caller) can point
/// [`crate::selfharness::driver::SelfHarnessDriver::set_checkpoint_path`]
/// elsewhere instead — this is only the production default.
pub fn default_checkpoint_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("checkpoint.json")
}

/// Default transcript jsonl path: `<cache_dir>/selfharness/transcript.jsonl`.
pub fn default_transcript_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("transcript.jsonl")
}

/// Default DURABLE per-node event-log path:
/// `<cache_dir>/selfharness/log.jsonl`. This is the [`crate::log`] append-only
/// jsonl the answerer [`Harness`](crate::harness::Harness)'s [`LogWriter`](crate::log::LogWriter)
/// writes — `Event::TurnStart { source, .. }` (the executed Haskell of every
/// answerer turn) and `Event::Effect { req, resp, .. }` (drained per turn by
/// `Harness::flush_effects`) for the self-iterating answerer nodes — as opposed
/// to [`default_transcript_path`], which is the loop-level driver-[`Event`]
/// stream. Sits alongside `checkpoint.json`/`transcript.jsonl` under the same
/// dir.
///
/// The production binary (`tidepool-web/src/bin/tidepool-selfharness.rs`)
/// does NOT write to this exact path: `LogWriter` refuses to overwrite an
/// existing run's log, so each boot mints its own `log-<epoch>.jsonl`
/// sibling in this function's PARENT directory (only the directory comes
/// from here) — tail the newest `log-*.jsonl`, not a fixed `log.jsonl`. This
/// function still IS the fixed path a direct test caller uses when it wants
/// one stable, reused filename across a process's whole run.
pub fn default_log_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("log.jsonl")
}

/// Restore the persisted [`Checkpoint`] from `path`, if one exists there
/// yet — `Ok(None)` (NOT an error) when the file is simply absent, which is
/// the expected case for the very first run. A file that exists but fails to
/// parse is a typed [`PersistenceError`], never a silent reset to `None`.
pub fn load_checkpoint(path: &Path) -> Result<Option<Checkpoint>, PersistenceError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(PersistenceError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let checkpoint = serde_json::from_slice(&bytes).map_err(|source| PersistenceError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(checkpoint))
}

/// Persist `checkpoint` to `path`, creating the containing directory if
/// needed. Written via the shared durable atomic-write helper — a
/// uniquely-named temp sibling, fsynced, then renamed over `path` — so
/// [`load_checkpoint`] never observes a partially-written file even if the
/// process is killed mid-write, and two callers racing on the same path
/// never share (and so can never collide on) a tmp name.
pub fn save_checkpoint(path: &Path, checkpoint: &Checkpoint) -> Result<(), PersistenceError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let bytes = serde_json::to_vec_pretty(checkpoint).map_err(|source| PersistenceError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    tidepool_atomic_write::write_durable(path, &bytes).map_err(|e| PersistenceError::Io {
        path: e.path,
        source: e.source,
    })
}

/// A transcript [`Observer`]: appends every driver [`Event`] to a jsonl
/// file, one line per event, opened in append mode so a restarted process
/// resumes the same file rather than truncating prior history. Reuses the
/// existing observer seam ([`crate::selfharness::driver::SelfHarnessDriver`]
/// emits to whatever [`Observer`] it was constructed with) rather than
/// adding a second event-emission path.
pub struct JsonlObserver {
    file: Mutex<std::fs::File>,
    path: PathBuf,
}

impl JsonlObserver {
    /// Open (creating if absent, appending if present) the jsonl transcript
    /// at `path`, creating its containing directory if needed.
    pub fn create(path: &Path) -> Result<Self, PersistenceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| PersistenceError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_path_buf(),
        })
    }
}

impl Observer for JsonlObserver {
    fn on_event(&self, event: &Event) {
        let line = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[selfharness] transcript: failed to serialize event: {e}");
                return;
            }
        };
        let mut file = self.file.lock();
        if let Err(e) = writeln!(file, "{line}") {
            eprintln!(
                "[selfharness] transcript: write to {} failed: {e}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The remedy [`PersistenceError::LiveLeaseHeld`]'s message tells an
    /// operator to set names the SAME constant `acquire_lease` actually
    /// reads — a hand-typed env var name in the message that drifted from
    /// the real one would be a silent operator-facing footgun.
    #[test]
    fn live_lease_held_message_names_the_real_takeover_env_var() {
        let err = PersistenceError::LiveLeaseHeld {
            run_id: "run-x".to_string(),
            pid: 4242,
        };
        let msg = err.to_string();
        assert!(msg.contains(LEASE_TAKEOVER_ENV_VAR), "{msg}");
        assert!(msg.contains("4242"), "{msg}");
        assert!(msg.contains("run-x"), "{msg}");
    }

    fn checkpoint(generation: u64) -> Checkpoint {
        Checkpoint {
            generation: CheckpointGeneration(
                NonZeroU64::new(generation).expect("nonzero in tests"),
            ),
            state: serde_json::json!({"mode": "Deciding"}),
            compaction: Some("a summary".to_string()),
            harness_source: "fingerprint-abc".to_string(),
            iteration: LoopIteration(generation),
            awaiting_continue: false,
            pending_operator_input: None,
            ask_id_high_water: 0,
        }
    }

    /// The wire-bytes golden item 3 promises: `CheckpointGeneration`/
    /// `LoopIteration` must serialize EXACTLY as the plain `u64` fields they
    /// replaced. Pinned literally — this is what
    /// `Checkpoint { generation: u64, ..., iteration: u64 }` produced before
    /// either newtype existed; if `#[serde(transparent)]` ever stops being
    /// transparent, this is the test that notices.
    #[test]
    fn wire_bytes_are_unchanged_by_the_typed_generation_and_iteration() {
        let cp = checkpoint(3);
        let json = serde_json::to_string(&cp).expect("serialize");
        assert_eq!(
            json,
            r#"{"generation":3,"state":{"mode":"Deciding"},"compaction":"a summary","harness_source":"fingerprint-abc","iteration":3}"#,
            "Checkpoint's wire shape drifted from the plain-u64 version — field \
             names, field order, or the numeric encoding of generation/iteration \
             changed"
        );
    }

    /// Deserializing a `0` generation must be a typed [`PersistenceError::Json`],
    /// never a silent invariant breach — `0` cannot even construct a
    /// [`CheckpointGeneration`] (it wraps `NonZeroU64`), so serde's own
    /// `NonZeroU64` support rejects it before this crate ever sees the value.
    #[test]
    fn a_zero_generation_on_disk_is_a_typed_deserialization_error() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"generation":0,"state":{"mode":"Deciding"},"compaction":null,"harness_source":"fp","iteration":0}"#,
        )
        .expect("write a checkpoint with generation 0");
        let err = load_checkpoint(&path)
            .expect_err("generation 0 must not deserialize into a CheckpointGeneration");
        assert!(matches!(err, PersistenceError::Json { .. }));
    }

    /// The backward-compat contract item 2 (b) requires: a checkpoint written
    /// before `awaiting_continue` existed — no such key in the JSON at all —
    /// must restore exactly as if the flag were `false`, never a
    /// deserialization error.
    #[test]
    fn a_checkpoint_with_no_awaiting_continue_key_deserializes_as_not_parked() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"generation":1,"state":{"mode":"Deciding"},"compaction":null,"harness_source":"fp","iteration":1}"#,
        )
        .expect("write a pre-marker-field checkpoint");
        let loaded = load_checkpoint(&path)
            .expect("a pre-existing checkpoint shape must still deserialize")
            .expect("some checkpoint");
        assert!(
            !loaded.awaiting_continue(),
            "an absent key must default to not-parked, never an error or a stray true"
        );
    }

    /// `with_awaiting_continue` flips ONLY the marker: same generation, same
    /// state/compaction/harness_source/iteration — this is the operation
    /// `between_loops_gate` uses to record/clear the gate park without
    /// treating it as a newly completed cycle (unlike [`Checkpoint::committed`],
    /// which always advances the generation).
    #[test]
    fn with_awaiting_continue_flips_the_flag_and_nothing_else() {
        let cp = checkpoint(3);
        assert!(!cp.awaiting_continue());

        let parked = cp.with_awaiting_continue(true);
        assert!(parked.awaiting_continue());
        assert_eq!(parked.generation(), cp.generation());
        assert_eq!(parked.state, cp.state);
        assert_eq!(parked.iteration(), cp.iteration());

        let cleared = parked.with_awaiting_continue(false);
        assert_eq!(
            cleared, cp,
            "clearing the marker restores byte-for-byte equality"
        );
    }

    /// `skip_serializing_if` keeps an unparked checkpoint's wire bytes
    /// unchanged (the pinned string in
    /// [`wire_bytes_are_unchanged_by_the_typed_generation_and_iteration`]
    /// still holds), while a parked one carries the key explicitly — so a
    /// reader can tell "never recorded" apart from "explicitly not parked"
    /// is not a distinction this wire makes, by design (both omit the key).
    #[test]
    fn awaiting_continue_key_is_only_present_on_the_wire_when_true() {
        let cp = checkpoint(1);
        assert!(!serde_json::to_string(&cp)
            .expect("serialize")
            .contains("awaiting_continue"));

        let parked = cp.with_awaiting_continue(true);
        let wire = serde_json::to_string(&parked).expect("serialize");
        assert!(wire.contains("\"awaiting_continue\":true"));

        let round_tripped: Checkpoint = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(round_tripped, parked);
    }

    /// Backward compat for BOTH new fields (item 1's compatibility pin,
    /// mirroring [`a_checkpoint_with_no_awaiting_continue_key_deserializes_as_not_parked`]):
    /// a checkpoint written before `pending_operator_input`/`ask_id_high_water`
    /// existed — neither key in the JSON at all — must restore as `None`/`0`,
    /// never a deserialization error.
    #[test]
    fn a_checkpoint_with_no_new_gate_fields_deserializes_with_their_defaults() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"generation":1,"state":{"mode":"Deciding"},"compaction":null,"harness_source":"fp","iteration":1}"#,
        )
        .expect("write a pre-widening checkpoint");
        let loaded = load_checkpoint(&path)
            .expect("a pre-existing checkpoint shape must still deserialize")
            .expect("some checkpoint");
        assert_eq!(
            loaded.pending_operator_input(),
            None,
            "an absent key must default to no pending operator input"
        );
        assert_eq!(
            loaded.ask_id_high_water(),
            0,
            "an absent key must default to a zero high-water mark"
        );
    }

    /// Forward compat: the wire-bytes golden item 3 promise extends to the two
    /// new fields — a checkpoint carrying neither (the overwhelming common
    /// case, same as an unparked `awaiting_continue`) must still produce the
    /// EXACT pinned string from
    /// [`wire_bytes_are_unchanged_by_the_typed_generation_and_iteration`], so
    /// an OLD binary reading a checkpoint a NEW binary wrote (before either
    /// field was ever populated) sees byte-identical JSON.
    #[test]
    fn checkpoint_with_neither_new_field_populated_matches_the_pinned_wire_bytes() {
        let cp = checkpoint(3);
        let json = serde_json::to_string(&cp).expect("serialize");
        assert_eq!(
            json,
            r#"{"generation":3,"state":{"mode":"Deciding"},"compaction":"a summary","harness_source":"fingerprint-abc","iteration":3}"#,
        );
    }

    /// Both new fields round-trip when populated, and stay byte-for-byte
    /// independent of `awaiting_continue`/each other — the same additive
    /// widening shape as [`awaiting_continue_key_is_only_present_on_the_wire_when_true`].
    #[test]
    fn pending_operator_input_and_ask_id_high_water_round_trip_when_populated() {
        let cp = checkpoint(1)
            .with_pending_operator_input(Some("steer left".to_string()))
            .with_ask_id_high_water(7);
        let wire = serde_json::to_string(&cp).expect("serialize");
        assert!(wire.contains(r#""pending_operator_input":"steer left""#));
        assert!(wire.contains(r#""ask_id_high_water":7"#));

        let round_tripped: Checkpoint = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(round_tripped, cp);
        assert_eq!(round_tripped.pending_operator_input(), Some("steer left"));
        assert_eq!(round_tripped.ask_id_high_water(), 7);
    }

    /// [`Checkpoint::committed`] always clears BOTH new fields, exactly as it
    /// always clears `awaiting_continue` — a newly completed cycle has no
    /// live gate-park state, whatever the previous checkpoint carried.
    #[test]
    fn committed_clears_pending_operator_input_and_does_not_inherit_ask_id_high_water() {
        let parked = checkpoint(1)
            .with_pending_operator_input(Some("leftover".to_string()))
            .with_ask_id_high_water(9);
        let committed = Checkpoint::committed(
            Some(parked.generation()),
            serde_json::json!({"mode": "Deciding"}),
            None,
            "fingerprint-abc".to_string(),
            LoopIteration(2),
        );
        assert_eq!(committed.pending_operator_input(), None);
        assert_eq!(committed.ask_id_high_water(), 0);
    }

    #[test]
    fn load_checkpoint_missing_file_is_none_not_error() {
        let dir = tempfile_dir();
        let path = dir.join("nope").join("checkpoint.json");
        assert_eq!(
            load_checkpoint(&path).expect("missing file is Ok(None)"),
            None
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile_dir();
        let path = dir.join("nested").join("checkpoint.json");
        let cp = checkpoint(3);
        save_checkpoint(&path, &cp).expect("save_checkpoint");
        let loaded = load_checkpoint(&path)
            .expect("load_checkpoint")
            .expect("some checkpoint");
        assert_eq!(loaded, cp);
    }

    /// Pure-Rust smoke proof of the restart-continuity requirement
    /// (`plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
    /// context is the runtime's job": the driver "MUST persist the
    /// iteration count in the checkpoint ENVELOPE so restart behavior stays
    /// continuous"), independent of the JIT/GHC-extract-backed acceptance
    /// path ([`crate::selfharness::driver`]'s `SelfHarnessDriver` needs a
    /// compiled harness to run a cycle at all, so this exercises the
    /// envelope logic — [`save_checkpoint`]/[`load_checkpoint`] plus the
    /// `Checkpoint.iteration` field — directly).
    #[test]
    fn iteration_round_trips_through_save_and_load() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");

        // "Cycle 1" commits generation 1 with iteration advanced to 1.
        let cp1 = checkpoint(1);
        assert_eq!(cp1.iteration().get(), 1);
        save_checkpoint(&path, &cp1).expect("save cycle 1");

        // THE point: a bare reload — no cycle run in between — must yield the
        // PERSISTED iteration, not 0. This covers the ENVELOPE half of restart
        // continuity: that `iteration` survives the write/read round trip at
        // all. That the driver then RESUMES from it is a separate claim this
        // test does not cover.
        let restored = load_checkpoint(&path)
            .expect("load after cycle 1")
            .expect("cycle 1's checkpoint is on disk");
        assert_eq!(
            restored.iteration().get(),
            1,
            "a restore with no cycle run must resume at the persisted iteration, not reset to 0"
        );

        // "Cycle 2" commits generation 2, continuing the iteration from what
        // was just restored (mirrors `run_one_cycle`'s `self.iteration += 1`
        // after a successful loop, then `commit_checkpoint` persisting it).
        let cp2 = Checkpoint {
            generation: CheckpointGeneration(NonZeroU64::new(2).expect("nonzero in tests")),
            iteration: LoopIteration(restored.iteration().get() + 1),
            ..checkpoint(2)
        };
        save_checkpoint(&path, &cp2).expect("save cycle 2");
        let restored2 = load_checkpoint(&path)
            .expect("load after cycle 2")
            .expect("cycle 2's checkpoint is on disk");
        assert_eq!(
            restored2.iteration().get(),
            2,
            "iteration must continue from the restored value across a second commit, not reset"
        );
    }

    #[test]
    fn save_checkpoint_leaves_no_tmp_file_behind() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        save_checkpoint(&path, &checkpoint(1)).expect("save_checkpoint");
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        assert!(!tmp.exists(), "temp file should be renamed away");
        assert!(path.exists());
    }

    #[test]
    fn truncated_checkpoint_file_is_a_typed_error() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        save_checkpoint(&path, &checkpoint(1)).expect("save_checkpoint");
        let mut bytes = std::fs::read(&path).expect("read back");
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&path, &bytes).expect("write truncated bytes");

        let err = load_checkpoint(&path).expect_err("truncated json must not silently reset");
        assert!(matches!(err, PersistenceError::Json { .. }));
    }

    #[test]
    fn jsonl_observer_appends_one_line_per_event() {
        let dir = tempfile_dir();
        let path = dir.join("transcript.jsonl");
        let observer = JsonlObserver::create(&path).expect("create");
        observer.on_event(&Event::LoopBoundary);
        observer.on_event(&Event::CompactionTrigger {
            node: crate::tree::NodeId(7),
            summary: "distilled work summary".to_string(),
            pre_input_tokens: 900,
            post_input_tokens: 120,
        });
        drop(observer);

        let contents = std::fs::read_to_string(&path).expect("read transcript");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("loop_boundary"));
        assert!(lines[1].contains("compaction_trigger"));
        assert!(lines[1].contains("distilled work summary"));
        assert!(lines[1].contains("900"));
    }

    #[test]
    fn jsonl_observer_reopens_in_append_mode_across_restarts() {
        let dir = tempfile_dir();
        let path = dir.join("transcript.jsonl");
        {
            let observer = JsonlObserver::create(&path).expect("create");
            observer.on_event(&Event::LoopBoundary);
        }
        {
            // Simulates a restart: a fresh JsonlObserver over the same path
            // must not truncate the prior line.
            let observer = JsonlObserver::create(&path).expect("re-create");
            observer.on_event(&Event::CompactionTrigger {
                node: crate::tree::NodeId(1),
                summary: "s".to_string(),
                pre_input_tokens: 1,
                post_input_tokens: 1,
            });
        }
        let contents = std::fs::read_to_string(&path).expect("read transcript");
        assert_eq!(contents.lines().count(), 2);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "selfharness-persistence-test-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    static NEXT_TEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
}
