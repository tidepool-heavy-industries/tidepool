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

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tidepool_repr::jsonl::{self, SyncPolicy};
use tidepool_repr::version_ladder::{self, LadderError, Migration, MigrationError};

use super::observer::{Event, Observer};

/// [`Checkpoint`]'s own envelope version — kind 1 of
/// `plans/persistence-versioning-design.md`'s six persistence-versioning
/// kinds. Governs every top-level `Checkpoint` field EXCEPT `state`, which
/// carries its own independent counter (see [`STATE_CURRENT`]) since it is
/// opaque, harness-authored JSON this crate never interprets.
pub const ENVELOPE_CURRENT: u32 = 1;
/// The oldest envelope version this build still loads. `0` — a checkpoint
/// written before this scheme existed, carrying no `"version"` key at all —
/// stays accepted so an existing operator's `checkpoint.json` keeps
/// loading; see `persistence-versioning-design.md` §6 for when this is ever
/// raised.
pub const ENVELOPE_FLOOR: u32 = 0;

fn envelope_v0_to_v1(v: Json) -> Result<Json, MigrationError> {
    // Purely additive: `0` and `1` are the same envelope shape, this step
    // only makes the version explicit (the unstamped-file convention —
    // see `tidepool_repr::version_ladder`'s module doc).
    Ok(version_ladder::set_version(v, 1))
}

/// Indexed from [`ENVELOPE_FLOOR`].
const ENVELOPE_MIGRATIONS: &[Migration] = &[envelope_v0_to_v1];

/// The `state` blob's own version — kind 2 ("per-harness `State` blob") of
/// the six persistence-versioning kinds, per Open Question 1's answer
/// (Rust-side JSON surgery, kept deliberately minimal per the operator's
/// caveat that `State`'s own longevity is uncertain): ONE flat ladder
/// shared across every harness, not a per-harness-fingerprint registry.
/// Lives as a SIBLING envelope field (`state_version`, never a key inside
/// `state` itself) — `state` stays exactly what a harness's own Haskell
/// `State` type produced, with no reserved key this crate imposes on every
/// harness author.
pub const STATE_CURRENT: u32 = 1;
/// See [`ENVELOPE_FLOOR`]'s doc — same unstamped-file convention, scoped to
/// `state` instead of the envelope.
pub const STATE_FLOOR: u32 = 0;

fn state_v0_to_v1(v: Json) -> Result<Json, MigrationError> {
    // No real migration exists yet (see this module's doc and
    // `persistence-versioning-design.md` §5's rollout order: the stamp
    // lands with the ladder wired but empty). `state_version` — not a key
    // inside `state` — is what records that this blob has passed through
    // the (identity) `0 -> 1` step; see `load_checkpoint`.
    Ok(v)
}

/// Indexed from [`STATE_FLOOR`].
const STATE_MIGRATIONS: &[Migration] = &[state_v0_to_v1];

fn ladder_err_to_persistence_err(
    e: LadderError,
    path: &Path,
    of: &'static str,
) -> PersistenceError {
    match e {
        LadderError::BelowFloor { found, floor } => PersistenceError::BelowFloor {
            path: path.to_path_buf(),
            of,
            found,
            floor,
        },
        LadderError::UnsupportedVersion { found, current } => PersistenceError::FutureVersion {
            path: path.to_path_buf(),
            of,
            found,
            current,
        },
        LadderError::Migration { from, source } => PersistenceError::Migration {
            path: path.to_path_buf(),
            of,
            from,
            detail: source.0,
        },
    }
}

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

    /// `of`'s version is below the floor this build still carries a
    /// migration path from — never a silent reset. `of` names which of
    /// [`Checkpoint`]'s two independent counters (`"envelope"` or
    /// `"state"`, see [`ENVELOPE_CURRENT`]/[`STATE_CURRENT`]) rejected the
    /// read. Mirrors `PersistenceError::LiveLeaseHeld`'s style: a full
    /// paragraph naming the exact path and the two real remedies, not a
    /// terse code (`persistence-versioning-design.md` §6).
    #[error(
        "checkpoint {path:?} {of} version {found} is below the floor this build still supports \
         ({floor}) — archive or delete the checkpoint and start a fresh run, or restore it with \
         an older tidepool build that still supports {of} version {found}."
    )]
    BelowFloor {
        path: PathBuf,
        of: &'static str,
        found: u32,
        floor: u32,
    },

    /// `of`'s version is newer than this build knows how to read.
    #[error(
        "checkpoint {path:?} {of} version {found} is newer than this build supports (current \
         {current}) — rebuild against a newer tidepool, or archive/delete the checkpoint and \
         start a fresh run."
    )]
    FutureVersion {
        path: PathBuf,
        of: &'static str,
        found: u32,
        current: u32,
    },

    /// A migration step itself failed.
    #[error("checkpoint {path:?} {of} migration from version {from} failed: {detail}")]
    Migration {
        path: PathBuf,
        of: &'static str,
        from: u32,
        detail: String,
    },
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
    /// The envelope's own version — see [`ENVELOPE_CURRENT`]. `#[serde(default)]`:
    /// an unstamped checkpoint (written before this scheme existed) has no
    /// such key at all and decodes as `0`, the unstamped-file convention
    /// [`tidepool_repr::version_ladder`] documents. [`load_checkpoint`]
    /// always migrates to [`ENVELOPE_CURRENT`] before decoding into this
    /// struct, so in practice this field is always [`ENVELOPE_CURRENT`] on
    /// anything that made it this far — it is not itself the gate.
    #[serde(default)]
    pub version: u32,
    /// Monotonic, incremented by one per committed cycle. `0` never appears
    /// on disk — the first commit is generation `1`.
    generation: CheckpointGeneration,
    /// The loop-boundary `State` json this generation's cycle produced
    /// ([`crate::selfharness::state_cross::state_out`]).
    pub state: Json,
    /// `state`'s own independent version — see [`STATE_CURRENT`]'s doc for
    /// why this is a sibling field rather than a key inside `state` itself.
    /// Same `#[serde(default)]`/unstamped-file convention as `version`.
    #[serde(default)]
    pub state_version: u32,
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
    /// The operator's between-loops message, threaded into the next
    /// cognition window's framing
    /// (`SelfHarnessDriver::between_loops_gate`/`render_framing_with`).
    /// DESERIALIZE-ONLY legacy compatibility: no production code writes this
    /// field anymore (the between-turns gate writes no checkpoint of its
    /// own, and [`Self::committed`] always clears it to `None`), but an OLD
    /// checkpoint written while a bespoke gate-park marker still existed
    /// (carrying this key non-`None`) must still decode — restore still
    /// imports a populated value from such a checkpoint into the next
    /// framing (`driver.rs`'s restore path). No builder constructs a
    /// non-`None` value anymore; the only way this field is ever `Some` is
    /// decoding one from disk. The precedent for the additive
    /// `#[serde(default, skip_serializing_if)]` shape is
    /// `Usage.cached_input_tokens`/`TurnDelta.reasoning`'s widenings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_operator_input: Option<String>,
    /// The highest [`crate::selfharness::observer::AskId`] minted as of this
    /// checkpoint — seeds [`SelfHarnessDriver`](crate::selfharness::driver::SelfHarnessDriver)'s
    /// ask-id counter on restore so a restarted process cannot re-mint an id
    /// already used earlier in the SAME `transcript.jsonl` (which spans
    /// restarts). `0` before any `askUser`/`note` form has ever been
    /// presented. `#[serde(default, skip_serializing_if)]`: same additive
    /// precedent as `pending_operator_input` — an old checkpoint with no such
    /// key deserializes as `0`, and a checkpoint that never minted an id
    /// serializes with no new key at all.
    #[serde(default, skip_serializing_if = "is_zero")]
    ask_id_high_water: u64,
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
    /// construction rather than by convention at each call site.
    pub fn committed(
        previous: Option<CheckpointGeneration>,
        state: Json,
        compaction: Option<String>,
        harness_source: String,
        iteration: LoopIteration,
    ) -> Self {
        Checkpoint {
            version: ENVELOPE_CURRENT,
            generation: previous.map_or(CheckpointGeneration::FIRST, CheckpointGeneration::next),
            state,
            state_version: STATE_CURRENT,
            compaction,
            harness_source,
            iteration,
            pending_operator_input: None,
            ask_id_high_water: 0,
        }
    }

    /// A copy of `self` with `ask_id_high_water` set, at the SAME generation
    /// — used by [`Self::committed`]'s caller (after minting the checkpoint
    /// for a completed cycle) so the persisted high-water mark tracks
    /// whatever the driver's own counter last reached.
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

/// Default live-dial settings path: `<cache_dir>/selfharness/settings.json`
/// — the operator's durable model/reasoning-effort dial choice
/// (`crate::provider::settings::SharedModelSettings`), sitting beside
/// `checkpoint.json`. A durable dial choice OUTRANKS a stale env/clap
/// default on restart: env/clap only seed the settings this file holds when
/// it doesn't exist yet — see [`crate::provider::settings::SharedModelSettings::load_or`].
pub fn default_settings_path() -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("selfharness")
        .join("settings.json")
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
/// The production binary (`tidepool/src/bin/tidepool-selfharness.rs`)
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
    let mut value: Json =
        serde_json::from_slice(&bytes).map_err(|source| PersistenceError::Json {
            path: path.to_path_buf(),
            source,
        })?;

    let envelope_found = version_ladder::found_version(&value);
    value = version_ladder::migrate_to_current(
        value,
        envelope_found,
        ENVELOPE_FLOOR,
        ENVELOPE_CURRENT,
        ENVELOPE_MIGRATIONS,
    )
    .map_err(|e| ladder_err_to_persistence_err(e, path, "envelope"))?;

    // `state`'s own version is a SIBLING envelope field (`state_version`),
    // never a key inside `state` itself — see `STATE_CURRENT`'s doc.
    let state_found = value
        .get("state_version")
        .and_then(Json::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let state_value = value.get("state").cloned().unwrap_or(Json::Null);
    let migrated_state = version_ladder::migrate_to_current(
        state_value,
        state_found,
        STATE_FLOOR,
        STATE_CURRENT,
        STATE_MIGRATIONS,
    )
    .map_err(|e| ladder_err_to_persistence_err(e, path, "state"))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("state".to_string(), migrated_state);
        obj.insert("state_version".to_string(), Json::from(STATE_CURRENT));
    }

    let checkpoint = serde_json::from_value(value).map_err(|source| PersistenceError::Json {
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

/// This transcript's version — kind 6 of
/// `plans/persistence-versioning-design.md`'s six persistence-versioning
/// kinds, the lowest-risk of the four JSONL consumers (§2): the stream is
/// explicitly best-effort ([`SyncPolicy::None`]) and, as of this writing,
/// has no first-party reader anywhere in this codebase to break — nothing
/// GATES on this version yet, only [`JsonlObserver::create`] stamps it.
/// [`read_transcript_header`] exists to give the old-corpus replay test
/// (`persistence-versioning-design.md` §4) something real to read, ahead of
/// whatever the first production reader turns out to need.
pub const TRANSCRIPT_CURRENT: u32 = 1;

impl JsonlObserver {
    /// Open (creating if absent, appending if present) the jsonl transcript
    /// at `path`, creating its containing directory if needed. A FRESH file
    /// (the common case: one per selfharness run) gets a version-stamped
    /// header as its first line; a reopened, already-populated file (a
    /// restart resuming the SAME transcript, per this type's own doc) is
    /// left exactly as found — the header is written once, at file birth,
    /// the same discipline `LogWriter::create`/`EventJournal::open` use for
    /// their own header/first lines.
    pub fn create(path: &Path) -> Result<Self, PersistenceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let is_fresh = !path.exists();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| PersistenceError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if is_fresh {
            let header = serde_json::json!({"version": TRANSCRIPT_CURRENT}).to_string();
            jsonl::write_line(&mut file, &header, SyncPolicy::None).map_err(|source| {
                PersistenceError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        }
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_path_buf(),
        })
    }
}

/// Read a transcript's header line and return its stamped version — `0`
/// when the file is empty or its first line carries no `"version"` key at
/// all (an unstamped, pre-versioning transcript). Not consumed by any
/// production code path yet — see [`TRANSCRIPT_CURRENT`]'s doc — this
/// exists for the old-corpus replay test.
pub fn read_transcript_header(path: &Path) -> Result<u32, PersistenceError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(PersistenceError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let Some(first_line) = text.lines().next() else {
        return Ok(0);
    };
    let value: Json = match serde_json::from_str(first_line) {
        Ok(v) => v,
        Err(_) => return Ok(0),
    };
    Ok(version_ladder::found_version(&value))
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
        // SyncPolicy::None: this observer's writes are best-effort, never
        // load-bearing for correctness the way a journal's are — matches the
        // original bare `writeln!` (no fsync).
        if let Err(e) = jsonl::write_line(&mut file, &line, SyncPolicy::None) {
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
            version: ENVELOPE_CURRENT,
            generation: CheckpointGeneration(
                NonZeroU64::new(generation).expect("nonzero in tests"),
            ),
            state: serde_json::json!({"mode": "Deciding"}),
            state_version: STATE_CURRENT,
            compaction: Some("a summary".to_string()),
            harness_source: "fingerprint-abc".to_string(),
            iteration: LoopIteration(generation),
            pending_operator_input: None,
            ask_id_high_water: 0,
        }
    }

    /// The wire-bytes golden item 3 promises: `CheckpointGeneration`/
    /// `LoopIteration` must serialize EXACTLY as the plain `u64` fields they
    /// replaced. Pinned literally — this is what
    /// `Checkpoint { generation: u64, ..., iteration: u64 }` produced before
    /// either newtype existed; if `#[serde(transparent)]` ever stops being
    /// transparent, this is the test that notices. The `version`/
    /// `state_version` keys are the ONE deliberate, documented wire-shape
    /// change this pin now carries — persistence versioning (#21) replaces
    /// the prior social wire-freeze with a mechanism, so a NEWLY-WRITTEN
    /// checkpoint always stamps both; see the (unmodified) legacy-decode
    /// tests below for the complementary promise that an OLD, unstamped
    /// checkpoint still loads.
    #[test]
    fn wire_bytes_are_unchanged_by_the_typed_generation_and_iteration() {
        let cp = checkpoint(3);
        let json = serde_json::to_string(&cp).expect("serialize");
        assert_eq!(
            json,
            r#"{"version":1,"generation":3,"state":{"mode":"Deciding"},"state_version":1,"compaction":"a summary","harness_source":"fingerprint-abc","iteration":3}"#,
            "Checkpoint's wire shape drifted — field names, field order, or the \
             numeric encoding of generation/iteration/version/state_version changed"
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

    /// The unified between-turns gate deleted the bespoke `awaiting_continue`
    /// marker field entirely (the restart rule simplified to "any boot that
    /// restores a checkpoint re-presents the between-turns ask" — no marker
    /// needed to distinguish a mid-turn crash from a parked-on-the-gate one).
    /// A checkpoint written by an OLD binary that still had the field — key
    /// present on the wire — must still decode: an unrecognized key is
    /// silently ignored by `serde_json`'s default (non-`deny_unknown_fields`)
    /// behavior, never a deserialization error.
    #[test]
    fn a_checkpoint_carrying_a_stale_awaiting_continue_key_still_decodes() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"generation":1,"state":{"mode":"Deciding"},"compaction":null,"harness_source":"fp","iteration":1,"awaiting_continue":true}"#,
        )
        .expect("write a checkpoint carrying the retired marker key");
        let loaded = load_checkpoint(&path)
            .expect("a checkpoint carrying an unrecognized extra key must still deserialize")
            .expect("some checkpoint");
        assert_eq!(loaded.generation().get(), 1);
        assert_eq!(loaded.iteration().get(), 1);
    }

    /// Backward compat for the still-live new fields (item 1's compatibility
    /// pin): a checkpoint written before `pending_operator_input`/
    /// `ask_id_high_water`
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
    /// case) must still produce the EXACT pinned string from
    /// [`wire_bytes_are_unchanged_by_the_typed_generation_and_iteration`], so
    /// an OLD binary reading a checkpoint a NEW binary wrote (before either
    /// field was ever populated) sees byte-identical JSON.
    #[test]
    fn checkpoint_with_neither_new_field_populated_matches_the_pinned_wire_bytes() {
        let cp = checkpoint(3);
        let json = serde_json::to_string(&cp).expect("serialize");
        assert_eq!(
            json,
            r#"{"version":1,"generation":3,"state":{"mode":"Deciding"},"state_version":1,"compaction":"a summary","harness_source":"fingerprint-abc","iteration":3}"#,
        );
    }

    /// `ask_id_high_water` round-trips when populated — the live-writer
    /// field `with_ask_id_high_water` still constructs (unlike
    /// `pending_operator_input`, whose only constructor is decoding an old
    /// checkpoint off disk — see the two tests below).
    #[test]
    fn ask_id_high_water_round_trips_when_populated() {
        let cp = checkpoint(1).with_ask_id_high_water(7);
        let wire = serde_json::to_string(&cp).expect("serialize");
        assert!(wire.contains(r#""ask_id_high_water":7"#));

        let round_tripped: Checkpoint = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(round_tripped, cp);
        assert_eq!(round_tripped.ask_id_high_water(), 7);
    }

    /// `pending_operator_input` is DESERIALIZE-ONLY: production stopped
    /// writing it once the between-turns gate was unified (see the field's
    /// own doc), but an OLD checkpoint that still carries the key — the only
    /// way this field is ever non-`None` now — must keep decoding it.
    #[test]
    fn a_checkpoint_carrying_a_populated_legacy_pending_operator_input_key_still_decodes() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"generation":1,"state":{"mode":"Deciding"},"compaction":null,"harness_source":"fp","iteration":1,"pending_operator_input":"steer left"}"#,
        )
        .expect("write a checkpoint carrying a populated legacy field");
        let loaded = load_checkpoint(&path)
            .expect("a checkpoint carrying the legacy key must still deserialize")
            .expect("some checkpoint");
        assert_eq!(loaded.pending_operator_input(), Some("steer left"));
    }

    /// [`Checkpoint::committed`] always clears `pending_operator_input` to
    /// `None` and starts `ask_id_high_water` at `0` — a newly completed
    /// cycle has no live gate-park state, whatever the previous checkpoint
    /// carried (including one restored from an old binary's still-set
    /// legacy key — constructed here via struct-update syntax, since there
    /// is no live builder for it anymore).
    #[test]
    fn committed_clears_pending_operator_input_and_does_not_inherit_ask_id_high_water() {
        let parked = Checkpoint {
            pending_operator_input: Some("leftover".to_string()),
            ..checkpoint(1).with_ask_id_high_water(9)
        };
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

    /// A future envelope version is a loud, typed refusal — never a silent
    /// best-effort read. Mirrors `persistence-versioning-design.md` §6.
    #[test]
    fn future_envelope_version_is_a_typed_rejection() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"version":9999,"generation":1,"state":{"mode":"Deciding"},"state_version":1,"compaction":null,"harness_source":"fp","iteration":1}"#,
        )
        .expect("write a future-version checkpoint");
        let err = load_checkpoint(&path).expect_err("a future envelope version must be refused");
        assert!(
            matches!(
                err,
                PersistenceError::FutureVersion {
                    of: "envelope",
                    found: 9999,
                    ..
                }
            ),
            "expected FutureVersion, got {err:?}"
        );
    }

    /// Same refusal, scoped to the `state` blob's own independent counter.
    #[test]
    fn future_state_version_is_a_typed_rejection() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"version":1,"generation":1,"state":{"mode":"Deciding"},"state_version":9999,"compaction":null,"harness_source":"fp","iteration":1}"#,
        )
        .expect("write a future-state-version checkpoint");
        let err = load_checkpoint(&path).expect_err("a future state version must be refused");
        assert!(
            matches!(
                err,
                PersistenceError::FutureVersion {
                    of: "state",
                    found: 9999,
                    ..
                }
            ),
            "expected FutureVersion, got {err:?}"
        );
    }

    /// An unstamped, pre-versioning checkpoint (no `"version"`/`"state_version"`
    /// key at all — exactly what every one of the LEGACY-decode tests above
    /// write) must load as version `0` and migrate through the identity
    /// `0 -> 1` step on BOTH counters. Complements those tests by asserting
    /// the post-load version fields explicitly, not just that decode
    /// succeeded.
    #[test]
    fn unstamped_legacy_checkpoint_migrates_to_current_on_both_counters() {
        let dir = tempfile_dir();
        let path = dir.join("checkpoint.json");
        std::fs::write(
            &path,
            r#"{"generation":1,"state":{"mode":"Deciding"},"compaction":null,"harness_source":"fp","iteration":1}"#,
        )
        .expect("write an unstamped pre-versioning checkpoint");
        let loaded = load_checkpoint(&path)
            .expect("an unstamped checkpoint must still load")
            .expect("some checkpoint");
        assert_eq!(loaded.version, ENVELOPE_CURRENT);
        assert_eq!(loaded.state_version, STATE_CURRENT);
        assert_eq!(loaded.state, serde_json::json!({"mode": "Deciding"}));
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
        // was just restored (mirrors `run_one_loop_iteration`'s `self.iteration += 1`
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
        // Line 0 is the version-stamped header a FRESH file gets; the two
        // events follow it.
        assert_eq!(lines.len(), 3);
        assert_eq!(
            read_transcript_header(&path).expect("read header"),
            TRANSCRIPT_CURRENT
        );
        assert!(lines[1].contains("loop_boundary"));
        assert!(lines[2].contains("compaction_trigger"));
        assert!(lines[2].contains("distilled work summary"));
        assert!(lines[2].contains("900"));
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
            // must not truncate the prior line, and — since the file
            // already exists — must NOT write a second header line.
            let observer = JsonlObserver::create(&path).expect("re-create");
            observer.on_event(&Event::CompactionTrigger {
                node: crate::tree::NodeId(1),
                summary: "s".to_string(),
                pre_input_tokens: 1,
                post_input_tokens: 1,
            });
        }
        let contents = std::fs::read_to_string(&path).expect("read transcript");
        // header + 2 events; exactly one header, even across the reopen.
        assert_eq!(contents.lines().count(), 3);
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
