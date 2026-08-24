# Persistence versioning design (#21)

Status: design only, no code. Written for operator review. Every claim below
cites the file/symbol it rests on; the two places this doc genuinely doesn't
know the answer are called out as open questions at the end, not decided
quietly.

## Problem statement

Today the harness persistence wire — the log `Event` enum
(`tidepool-harness/src/log/mod.rs:60-214`), `Checkpoint`
(`tidepool-harness/src/selfharness/persistence.rs:148-196`), and the three
sibling JSONL journals (worktree, handlers, selfharness transcript) — is
frozen by social discipline, not by a mechanism: wire bytes must not change
until this design lands (the log `Event` enum, `Checkpoint` struct, serde
tags, journal kinds). The concrete
cost of that freeze already shipped: commit `26080349`
("flatten(recursive-companion): lastRun :: Maybe RunSummary -> lastAnswer ::
Maybe Text") deleted a one-field wrapper type inside the recursive-companion
harness's own `State`, with "No migration/compat path — the operator has
scheduled a fresh-checkpoint window at the next redeploy" (commit message).
That rename could only ship because a checkpoint reset happened to already be
scheduled alongside it. The next such change won't have that luxury for free.

`tidepool-repr`'s CBOR wire format is the one place in the codebase that has
actually solved a version of this problem — Section 1 surveys it. Sections
2-6 ask, symbol by symbol, how much of that solution transfers to the harness
wire and where new machinery is genuinely needed.

---

## 1. Survey: how tidepool-repr's CBOR wire-format versioning works

Source: `tidepool-repr/src/serial/{mod,read,write}.rs`.

**The stamp.** An 8-byte header precedes every payload
(`tidepool-repr/src/serial/mod.rs:72-89`):

```rust
pub const HEADER_MAGIC: [u8; 4] = [0x54, 0x50, 0x4C, 0x52]; // "TPLR"
pub const VERSION_MAJOR: u16 = 3;
pub const VERSION_MINOR: u16 = 0;
pub const HEADER_LEN: usize = 8;
```

Written unconditionally by `write_header` (`serial/write.rs:9-14`) before the
ciborium-encoded body. Validated by `strip_header` on every read
(`serial/read.rs:9-25`):

```rust
fn strip_header(bytes: &[u8]) -> Result<&[u8], ReadError> {
    if bytes.len() < 4 || bytes[..4] != super::HEADER_MAGIC {
        return Err(ReadError::MissingHeader);
    }
    if bytes.len() < super::HEADER_LEN {
        return Err(ReadError::TruncatedHeader);
    }
    let major = u16::from_be_bytes([bytes[4], bytes[5]]);
    let minor = u16::from_be_bytes([bytes[6], bytes[7]]);
    if major != super::VERSION_MAJOR || minor > super::VERSION_MINOR {
        return Err(ReadError::UnsupportedVersion(major, minor));
    }
    Ok(&bytes[super::HEADER_LEN..])
}
```

Called by both `read_cbor` and `read_metadata` (`serial/read.rs:31-32,122`).

**The rejection is typed and loud, never silent.** `ReadError::MissingHeader`,
`TruncatedHeader`, and `UnsupportedVersion(u16, u16)`
(`serial/mod.rs:38-49`) are distinct variants with operator-legible messages
— `MissingHeader`'s text is explicit that this is a deliberate design choice:
"stale fixtures/caches must be regenerated, not tolerated."

**The compatibility rule.** Major mismatch, in *either* direction, is a hard
reject. Minor: strictly newer than the build supports is a hard reject;
minor *older than or equal to* the build's, at the *same* major, is accepted
— a forward-compatible read of an older-but-still-current-major payload.
This is the whole of the tolerance the format offers; there is no
multi-step "upgrade this payload in memory" ladder anywhere in this module.
Tolerance is achieved by the reader always decoding directly into the
CURRENT shape, with older-minor payloads simply omitting optional pieces the
reader already knows how to default. The worked example
(`tidepool-repr/CLAUDE.md`, "CBOR wire format" section) is the `2.1` minor
bump adding an optional `poisoned` warnings key: the reader accepts an
absent key as empty, the writer omits it when empty, and a `2.0` payload
still reads and re-encodes byte-identically. `ReadError::UnknownMetadataKey`'s
own doc comment (`serial/mod.rs:62-69`) spells out why this works: the
major/minor gate has already rejected anything with a newer minor before
this code ever runs, so an unrecognized key at this point can only mean a
foreign or corrupt payload, never a legitimate forward-compat addition still
in flight.

**The MAJOR-bump answer does not transfer.** A breaking shape change bumps
`VERSION_MAJOR`, and the remedy the crate actually uses is: bump the Rust
constant and the Haskell serializer
(`Tidepool.CborEncode.tplrHeader`'s byte literal — the two are hardcoded
independently with no shared formatter, so both must move in the same
commit) and **regenerate every committed fixture corpus**
(`haskell/regen-corpus.sh`), pinned byte-identical by
`tidepool-repr/tests/golden_wire_contract.rs`. This works because a `.cbor`
fixture is a **build artifact** — fully reproducible from the Haskell source
that produced it, so "destroy and regenerate" costs nothing but CI time.

A `Checkpoint` or a journal segment is not a build artifact. It is the
durable record of one real operator's run — irreplaceable, not
re-derivable from anything else on disk. "Regenerate the corpus" is exactly
the operation `26080349` had to fall back to (reset the checkpoint), and
it is precisely what this design exists to stop requiring. **What this
design borrows from repr's CBOR scheme is the stamp shape (magic +
version, or a plain monotonic integer) and the discipline (typed loud
rejection, additive-only tolerance is free, anything else needs a
deliberate bump) — not its major-bump remedy.** For anything past pure
addition, the harness wire needs machinery repr's CBOR format has never
had to build: composed migration functions that transform an old payload
into the current shape in place, covered in Section 3.

---

## 2. Stamp placement

The brief's working hypothesis was that versioning belongs at the shared
durable-JSONL primitive (`tidepool-repr::jsonl`) so all four consumers
inherit it for free. The code argues against that placement, and for a
different one.

### Why the version stamp cannot live inside `jsonl.rs` itself

`tidepool-repr/src/jsonl.rs`'s own module doc is explicit about its scope
(`jsonl.rs:8-14`): "What lives here is MECHANISM only — how a line is
durably appended and how a torn final line is read back — never a row
SCHEMA, an envelope (sequence numbers, cursors, segment ordinals), or a
durability POLICY." Its public surface confirms this by construction:

- `append_new_line`/`write_line` (`jsonl.rs:107-124`) take an opaque `&str`
  — the line's content is never inspected.
- `read_tail<T>(path, parse: impl Fn(&str) -> Result<T, String>, policy)`
  (`jsonl.rs:192-288`) is generic over `T` and delegates ALL structural
  interpretation to the caller's own `parse` closure. `jsonl.rs` never sees
  a field, a header, or a version number — only bytes and line boundaries.

Putting a version check inside `read_tail`/`append_new_line` would require
`jsonl.rs` to know what a "row" or a "header" means for some particular
consumer, which is exactly the schema/envelope boundary its own doc draws
and which the crate's four real consumers already respect by keeping their
own distinct row shapes (`jsonl.rs:16-31` enumerates all four by name).
Root CLAUDE.md's own rule cuts the other way here too: this is not "an API
one notch too narrow" that should be widened — `jsonl.rs` is scoped
correctly for what it does; a version stamp is a schema concern the module
deliberately pushes to each caller.

### Current state of the four consumers + `Checkpoint`, from the code

| Artifact | Header line? | Version field? | Uses `jsonl::read_tail`? |
|---|---|---|---|
| `tidepool-worktree::journal::EventJournal` (`tidepool-worktree/src/journal.rs:36-44`) | No | No | Yes — `TailPolicy::Repair` (`journal.rs:62-107`) |
| `tidepool-handlers::handlers::journal::{JournalHandler,load_journal}` (`tidepool-handlers/src/handlers/journal.rs:33-40`) | No | No (only a composed `seq = segment_ordinal<<32 \| local_seq`, `journal.rs:359-393` — provenance, not schema) | Yes — `TailPolicy::Observe` (`journal.rs:178-212`) |
| `tidepool-harness::log::{LogWriter,LogReader}` (`tidepool-harness/src/log/mod.rs:37-53`) | **Yes** — `LogHeader{prelude_hash,extract_fingerprint,harness_version}` as the raw first line | `harness_version` is set to `CARGO_PKG_VERSION` at the one production call site (`tidepool/src/bin/tidepool-selfharness.rs:193-199`) but **never validated on read** — no gate exists | **No** — `LogReader::open`/`EventIter` (`log/reader.rs:23-85`) hand-roll their own torn-tail-tolerant reader instead of calling `jsonl::read_tail`; only the writer uses `jsonl::write_line` (`log/writer.rs:8,50`) |
| `tidepool-harness::selfharness::persistence::JsonlObserver` (`persistence.rs:357-407`) | No | No | No reader exists for this file in this codebase at all (`SyncPolicy::None`, best-effort by design) |
| `tidepool-harness::selfharness::persistence::Checkpoint` (`persistence.rs:148-196`) | N/A — not JSONL at all, one `serde_json` object via `to_vec_pretty`/`from_slice` + `tidepool_atomic_write::write_durable` (`persistence.rs:334-349`) | No explicit version; `generation: CheckpointGeneration` is a monotonic run counter, not a schema version. Forward-compat is entirely per-field `#[serde(default, skip_serializing_if=...)]` (`pending_operator_input` at 183-184, `ask_id_high_water` at 194-195), pinned by tests at 509-531 and the wire-bytes golden at 449-460 | N/A |

Two more findings worth naming because they change the placement argument:

- **The harness log already has a header-shaped field that looks like a
  version and isn't enforced as one.** `harness_version` carries real build
  identity (`CARGO_PKG_VERSION`) but `LogReader::open` never compares it to
  anything — a header line that fails to deserialize is an ordinary
  `ReadError::Parse`, not a version-aware rejection. This is the nearest
  existing candidate to retrofit, not a green field.
- **`LogReader`/`EventIter` bypass the shared primitive on the read side**,
  duplicating `jsonl::read_tail`'s exact "a malformed row is forgivable only
  as the last line" logic by hand (`log/reader.rs:58-79` vs.
  `jsonl.rs:209-216`). This is the shape root CLAUDE.md's "Kept-in-sync
  copies are forbidden" rule flags — not something this doc's boundary lets
  it fix, but worth recording so the implementation lane that eventually
  adds a version gate here folds this consumer onto `jsonl::read_tail` at
  the same time rather than adding version logic to a second hand-rolled
  reader.

### Recommended placement

Not inside `jsonl.rs`'s existing functions. Instead: a small **versioned-header
helper**, a peer module in `tidepool-repr` (not a widening of `jsonl.rs`,
since its scope is correct as-is) built on the exact pattern
`serial/mod.rs`'s `strip_header`/`write_header` already prove out — magic-or-plain
version int + a typed `UnsupportedVersion`-style rejection — generic enough
that each of the four JSONL consumers writes it as their own first line, and
`Checkpoint` (which isn't JSONL) uses the same validate-on-load discipline
directly against its own top-level field, the way `generation` already sits
beside `state`/`compaction`/etc.

Concretely, each consumer's adoption point:

- **Harness log**: add a real version field to `LogHeader` and *enforce* it
  in `LogReader::open` — closing the existing gap where `harness_version` is
  carried but not checked. Natural moment to also fold `LogReader`/`EventIter`
  onto `jsonl::read_tail` (see above), though that code change is outside
  this doc's boundary.
- **`JsonlObserver` transcript**: add a header line where none exists today.
  Lower risk than the other three: this stream is explicitly best-effort
  (`SyncPolicy::None`) and has no first-party reader in the codebase to
  break.
- **Worktree `EventJournal`**: add a header line. `EventJournal` is
  single-owner (`&mut self` exclusive, journal.rs's own doc at 76-78) so the
  header is written once at file creation, same as `LogWriter::create`.
- **Handlers `JournalHandler`**: add a header line **per segment**, not per
  run — PRD 20's segmented design already treats each segment file as
  independently readable/foldable (`resume.rs:11-22,47-51`), and a version
  concept that only applies at the run level would need to reach into the
  lease/fold machinery in `tidepool-harness::selfharness::resume`, which
  sits above this crate in the dependency graph. Stamping per segment keeps
  the version check at the same layer that already owns segment framing.
- **`Checkpoint`**: add a top-level version field, sibling to `generation`.

### The `Checkpoint.state: Json` blob is a second, independent versioning problem

This is the finding that most directly explains why `26080349` needed a
reset even in principle. `Checkpoint.state` is `serde_json::Value`
(`persistence.rs:154-155`) — opaque to every byte of Rust in
`tidepool-harness`. `RunSummary`/`lastAnswer` never appears in
`persistence.rs`, `log/mod.rs`, or anywhere else this doc's boundary
covers; it lived entirely inside the recursive-companion harness's own
Haskell `State` type (`harness-dogfooding/recursive-companion/HarnessTypes.hs`,
per the `26080349` diff). Stamping and gating the `Checkpoint` **envelope**
(generation/compaction/harness_source/iteration/ask_id_high_water/
pending_operator_input) solves nothing for a change entirely inside `state`
— the envelope's own version number has no visibility into what shape the
opaque blob it's carrying is in.

So there are genuinely two versioning concerns stacked on top of each
other: the Rust-owned `Checkpoint` envelope (this design can version and
gate directly, in `tidepool-harness`), and each AUTHORED harness's own
`State` shape (owned by the harness author, in Haskell, invisible to Rust
by design — the whole point of `state: Json` being opaque). A migration
hook for the second one is real new work with no existing analog anywhere
in the surveyed code; Section 3 proposes a default and flags the real
alternative as an open question rather than deciding it here.

---

## 3. Migration-ladder shape

**One version counter per artifact kind, not one global counter.** This
mirrors how repr already scopes `VERSION_MAJOR`/`VERSION_MINOR` to "the
`CoreExpr`/metadata wire format" specifically — not a repo-wide version.
The kinds, each evolving independently:

1. `Checkpoint`'s Rust-owned envelope (`tidepool-harness`)
2. Each authored harness's own `State` blob (per-harness, see below)
3. Harness log `Event` (`tidepool-harness::log`)
4. Worktree `RepositoryEvent` journal rows (`tidepool-worktree::journal`)
5. Handlers journal `kind`/`payload` rows (`tidepool-handlers::handlers::journal`)
6. Selfharness transcript `Event` (`tidepool-harness::selfharness::observer`)

**The ladder itself.** For a given artifact kind with `CURRENT_VERSION`, a
table of small transform functions indexed by the version they start from:

```rust
type Migration = fn(serde_json::Value) -> Result<serde_json::Value, MigrationError>;
const MIGRATIONS: &[Migration] = &[migrate_v1_to_v2, migrate_v2_to_v3, /* ... */];
```

A reader loads the stamped version `V`, then folds `(V..CURRENT_VERSION)`
over `MIGRATIONS`, applying each step once, before final typed
deserialization into the current Rust struct/enum. This is the standard
composed-ladder idiom (the same shape database migration tools use): `N`
versions cost `N-1` small, independently-testable steps, never one
from-any-version-to-current function that grows quadratically messy.
This is genuinely new machinery — nothing in the surveyed code does this
today; the closest existing pattern (`Checkpoint`'s `#[serde(default)]`
fields) only handles pure addition, which is exactly the case that does
*not* need a migration step at all (see the floor discussion below —
additive changes stay free, same as CBOR's minor bumps).

**Where the functions live, per kind:**

- **`Checkpoint` envelope** — beside `Checkpoint` itself, in
  `tidepool-harness::selfharness::persistence` (e.g. a `migrate` submodule).
  Only `tidepool-harness` owns this struct's shape.
- **JSONL row kinds (3, 4, 5, 6)** — beside each kind's own reader
  (`tidepool-harness::log`, `tidepool-worktree::journal`,
  `tidepool-handlers::handlers::journal`, the selfharness observer/transcript
  reader once one exists), applied per-line before final typed decode. This
  is also where the dead-variant question resolves: `Event::SnapshotFrozen`/
  `BranchInvocation` are kept today purely for wire compatibility
  (`log/mod.rs:172-213`, explicit "NO LONGER EMITTED... kept for wire
  compatibility" doc comments). A migration ladder makes it possible to
  actually delete such variants (rewrite them forward into whatever survives
  at migration time) — but doesn't force the issue; tolerating a cheap dead
  arm forever remains legitimate when nothing is gained by removing it.
- **Per-harness `State` blob (kind 2)** — genuinely unresolved by anything
  in the surveyed code, and the two live options trade off differently
  enough that this doc flags it as Open Question 1 rather than picking
  silently:
  - *Rust-side JSON surgery*: a hand-written `fn(Value) -> Value` per
    harness per bump, living wherever `tidepool-harness` reads a
    checkpoint's `state` field back out. Doesn't require understanding the
    harness's Haskell types — the `26080349` rename is pure structural
    surgery (`state["lastAnswer"] = state["lastRun"]?["runAnswer"]; state.remove("lastRun")`)
    with no semantic content beyond moving JSON around. Cheap, but the
    transform's shape now has to be kept in sync BY HAND with the Haskell
    `State` type it targets, in a different language, with no shared
    formatter checking the two agree (the same hazard `tidepool-repr/CLAUDE.md`
    already names for the CBOR header's dual hardcoding).
  - *Haskell-authored typed migration*: each harness ships a
    `migrateState :: Int -> Value -> Either Text Value` (or similar) the
    Rust driver invokes via extract+eval before decoding `State`, keeping
    the transform typed and co-located with the type it targets — closer
    to this repo's "Haskell expands, Rust collapses" split (root CLAUDE.md),
    but adds a compile+eval round trip to every restore that has pending
    migrations, and needs its own calling convention designed from scratch.

  This doc's recommendation: start with Rust-side JSON surgery for the
  first real migration (it directly answers the `26080349` case with no new
  calling convention), and revisit the Haskell-authored option only if
  per-harness `State` schemas turn out to churn often enough that hand-kept
  Rust/Haskell shape agreement becomes its own maintenance burden.

---

## 4. Old-corpus replay test plan

**Model, inverted from repr's.** `tidepool-repr/tests/golden_wire_contract.rs`
+ `haskell/regen-corpus.sh` pin a corpus that gets **regenerated** on every
major bump, because `.cbor` fixtures are reproducible build artifacts. A
persistence corpus is the opposite: it must **never be regenerated** once a
fixture is frozen — a `Checkpoint`/journal fixture stands in for a real
operator's on-disk state, and regenerating it after a bump would just
produce the new shape, silently deleting the very test case that proves the
migration works. The corpus grows by exactly one frozen fixture per version
bump, forever, and every fixture must keep passing (through however many
composed migration steps that implies) until the floor advances past it
(Section 6).

**Location.** `tidepool-harness/tests/fixtures/persistence-corpus/`, one
subdirectory per artifact kind (checkpoint, harness-log, worktree-journal,
handlers-journal, selfharness-transcript), one committed file per version
that ever existed for that kind.

**Test.** A single file, e.g.
`tidepool-harness/tests/persistence_migration_corpus.rs`, that walks each
kind's fixture directory programmatically (so a new fixture needs no new
test function — matches the "one substrate, many assertions" wall-time rule
in root CLAUDE.md, though it applies more loosely here than to the
GHC-compiling test families it was written for: these fixtures are plain
JSON/JSONL with no `tidepool-extract` compile in the loop, so per-fixture
cost is negligible and the constraint that actually matters is "don't
duplicate the walking logic per kind," not "don't fork a compile"). For each
fixture: run it through the versioned reader, assert the migration chain
applies without error, and assert specific known-good fields land at their
new post-migration name/value — not merely "it parses," since a migration
that silently drops a field would still parse cleanly into the current
struct's defaults.

**Bootstrapping the first entries.** At the moment the version stamp lands
(before any real migration exists), freeze today's shape as the floor
version for each kind by running the existing construction paths once and
committing the output instead of writing it to a tempdir — e.g.
`persistence.rs`'s own `checkpoint(generation)` test helper
(`persistence.rs:429-441`) or a real one-cycle harness run for the
`Checkpoint` corpus; a short real transcript for the harness-log/worktree/
handlers corpora. Every subsequent bump adds exactly one more fixture: the
shape immediately before that bump lands, captured the same way, plus the
one migration function that turns it into the next version.

---

## 5. What it unblocks

- **`RunSummary → lastAnswer`** (`26080349`) — shipped only via checkpoint
  reset, explicitly because a redeploy window was already scheduled
  alongside it. With the `State`-blob
  migration hook from Section 3, this exact class of change (hoist a nested
  field, drop the wrapper) could ship as an ordinary Rust-side JSON-surgery
  migration with no reset.
- **Any non-additive `Checkpoint` edit** — currently frozen outright (see
  Problem statement). Once the envelope carries a
  version and a migration ladder exists, a restructuring edit (e.g.
  reshaping `compaction: Option<String>` into a richer type, or finally
  retiring `pending_operator_input`'s deserialize-only legacy status —
  `persistence.rs:172-184`) becomes an ordinary versioned change.
- **Any non-additive `Event` edit** — same freeze, same unblock. Specifically
  enables actually deleting `Event::SnapshotFrozen`/`BranchInvocation`
  (`log/mod.rs:172-213`) if the operator ever wants to, rather than
  carrying them forever for wire compatibility with no gate distinguishing
  "old log using this variant" from "new log that should never see it
  again."
- **Journal `kind`/vocabulary changes** for worktree `RepositoryEvent` and
  handlers' dev-tree `split`/`outcome`/`replan`/`rebase`/`escalation` kinds
  — worth noting these are *already* partly tolerant by construction, since
  `resume.rs:126-129` documents the handlers journal's payload as opaque
  `serde_json::Value` end to end, interpreted only by the authoring harness.
  Versioning mainly helps the cases that opacity doesn't cover: a `kind`
  string itself being renamed/retired, or `last_by_kind_key`
  (`tidepool-handlers/src/handlers/journal.rs:256-262`) needing to fold an
  old payload shape a current harness no longer emits.

**Rollout order.** Stamp first: land version fields + header stamps +
typed-rejection reading on all six kinds, with the ladder machinery wired
but empty (nothing to migrate yet — every existing fixture is version 1
reading as version 1). Only once that's in place does the first real
migration (`RunSummary → lastAnswer`, or whatever's next queued) land as an
ordinary, no-longer-frozen change.

---

## 6. Floor policy

**An artifact older than the ladder's floor is a loud, typed refusal —
never a silent reset, never a best-effort guess.** This continues a
discipline the codebase already states explicitly for the failure case one
level up: `load_checkpoint`'s own doc comment
(`persistence.rs:306-309`) — "A file that exists but fails to parse is a
typed [`PersistenceError`], never a silent reset to `None`." A version below
the floor is the same shape of failure and should get the same treatment,
extended with the version numbers involved: a new typed variant per
consumer's existing error enum (`PersistenceError` for `Checkpoint`,
`WorktreeError` for the worktree journal, `JournalLoadError` for the
handlers journal, `log::ReadError` for the harness log), e.g.
`PersistenceError::BelowFloor { found: u32, floor: u32, path: PathBuf }`.

**What the operator sees.** Follow the existing precedent for an
operator-facing refusal message —
`PersistenceError::LiveLeaseHeld`'s error text
(`persistence.rs:121-130`) is a full paragraph naming the exact pid and the
exact remedy env var, not a terse code. A below-floor rejection should do
the same: name the artifact's path, the version found, the floor this build
still supports, and the two real remedies — archive or delete the artifact
and start a fresh run, or run an older `tidepool-selfharness` build old
enough to still read it (mirroring `MissingHeader`'s "stale fixtures/caches
must be regenerated, not tolerated" wording, adapted to state that genuinely
can't be regenerated: it must be started over, explicitly, not silently).

**Floor advancement is a deliberate, rare, human decision** — pruning old
migration rungs once they're judged not worth carrying, never automatic
garbage collection triggered by a bump or a corpus-size threshold. This is
consistent with treating it as exactly the kind of scope decision "wide
scope hygiene sweeps are policy" memory covers elsewhere: legitimate to do,
but as an explicit, named sweep the operator signs off on, not a side
effect of an unrelated change.

---

## Open questions for the operator

1. **Where does a harness-authored `State` blob's migration function
   live** — Rust-side JSON surgery (Section 3's default recommendation, no
   new calling convention, but hand-kept in sync with Haskell types across a
   language boundary with no shared formatter) or a Haskell-authored typed
   `migrateState` invoked via extract+eval at restore (typed, co-located,
   matches the repo's Haskell/Rust split, but is new machinery with its own
   calling convention to design)? This doc defaults to Rust-side surgery for
   the first migration and flags revisiting only if `State` schemas turn out
   to churn often.
   **ANSWERED (operator, 2026-08-24): Rust-side JSON surgery** — "json
   surgery is fine, idk how long-term State will be anyway vs some other
   mechanism." The caveat is part of the answer: `State`'s own longevity is
   uncertain, which argues further against bespoke migration machinery.
2. **Should `LogReader`/`EventIter` be folded onto `jsonl::read_tail` as
   part of landing the version stamp**, closing the existing "one
   mechanism, one home" gap where the harness log's read side hand-rolls
   torn-tail tolerance that already exists in `tidepool-repr::jsonl`
   (`log/reader.rs:58-79` vs. `jsonl.rs:209-216`)? This doc's boundary is
   design-only and doesn't decide implementation sequencing, but the version
   header's natural landing spot is inside a real `read_tail` call, so doing
   both in the same lane avoids touching this reader twice.
   **DECIDED (root, 2026-08-24, per this doc's own lean; operator informed):
   yes — fold it in the same lane that lands the stamp.**
3. **Per-artifact-kind version counters (this doc's recommendation, mirroring
   how CBOR scopes its own version to one wire format) vs. one global
   persistence version** — the per-kind approach means six independent
   counters/ladders (`Checkpoint` envelope, N per-harness `State` blobs,
   harness log `Event`, worktree `RepositoryEvent`, handlers `kind`/`payload`,
   selfharness transcript `Event`) to maintain rather than one, and is worth
   the operator's explicit sign-off given the bookkeeping multiplies.
   **ANSWERED (operator, 2026-08-24): per-kind counters, as recommended.**
4. **Does the handlers journal need a version header per segment** (this
   doc's recommendation, since PRD 20 already treats each segment as
   independently readable/foldable — `resume.rs:11-22,47-51`) **or would a
   run-level version work better**, given segments are a newer and less
   battle-tested mechanism than the other three consumers? Flagged rather
   than decided because a run-level version would need to reach into
   `tidepool-harness::selfharness::resume`'s lease/fold machinery, which
   sits above `tidepool-handlers` in the dependency graph — a real
   cross-crate design choice, not a mechanical default.
   **DECIDED (root, 2026-08-24, per this doc's recommendation; operator
   informed): per-segment header — segments stay independently foldable.**
