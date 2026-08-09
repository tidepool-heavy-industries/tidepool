# L5 — Exomonad integration decision record

**Lane:** L5 (worktree-wave, PRD 19). **Status:** mapping only — see the status
line at the end. Nothing in this document has been adopted, extracted,
vendored, or depended on. No `Cargo.toml` changed. No production code changed.

**Scope of this record:** the four PRD-named starting points — the dirty-tree
spawn refusal, `hooksock/`, `inbound.rs`, and abnormal-teardown worktree
preservation — mapped against what `tidepool-worktree` actually has today
(L1/L2/L3 stubs + frozen scaffold, since those lanes' receipts don't exist yet
at time of writing). Everything below was read from the actual files, not
inferred from names.

---

## 0. Path verification

All four PRD-cited paths exist verbatim in `~/dev/exomonad` as of this
reading (repo HEAD as checked out locally, 2026-08-08):

| PRD path | Exists? | Notes |
|---|---|---|
| `rust/exo/src/tools/spawn.rs` | Yes | 50KB. `require_clean_worktree` at line 106. |
| `rust/exo-node/src/hooksock/` | Yes | `mod.rs` + `client.rs` + `server.rs`, ~10KB combined. |
| `rust/exo-node/src/inbound.rs` | Yes | 1512 lines, 61KB. `watch`/`process_inbox` are the reliability core; the file also carries unrelated shutdown-cascade and dispatch logic (out of scope here). |

No path had moved; no successor search was needed. One adjacent fact worth
recording: `spawn.rs` is not the only place a worktree gets created or
destroyed in Exomonad. `rust/exomonad-core/src/services/agent_control/internal.rs`
(spawn-time RAII rollback) and `rust/exo/src/doctor.rs` (`exo doctor`,
audit + reclaim) are both load-bearing for §5 below and are cited there by
path.

---

## 1. Exact modules and contracts considered

### 1a. Dirty-tree spawn refusal — `rust/exo/src/tools/spawn.rs:103-121`

```rust
async fn require_clean_worktree<C: Git + Sync>(ctx: &C) -> CapResult<()> {
    let dirty = ctx.status_porcelain().await?;
    if dirty.is_empty() { Ok(()) } else { Err(CapError::invalid("worktree", ...)) }
}
```

Backed by `Git::status_porcelain` (`rust/exo-caps/src/git.rs:46`), implemented
in `rust/exo-runtime/src/git.rs:88-95` as a bare `git status --porcelain`
(no `-uall`, no `--ignored`, no submodule flags), split into non-empty lines
and returned verbatim so the refusal message can *name* the offending paths.
`require_clean_worktree` gates `spawn_dev` and `fork_wave` (worktree-creating
spawns) but explicitly not `spawn_worker` (inline, no worktree) — see
`spawn_worker_not_gated_on_dirty_worktree` test at line 873.

The underlying `git()` call site (`rust/exo-runtime/src/git.rs:203-230`) does
**no environment scrubbing** — no `GIT_DIR`/`GIT_INDEX_FILE`/`GIT_WORK_TREE`
removal, just `Command::new("git").current_dir(...).args(...)`.

### 1b. `hooksock/` — `rust/exo-node/src/hooksock/{mod,client,server}.rs`

Read in full (all three files, ~185 lines total). This is **not** a git hook.
It is the transport for *Claude Code's own* tool-use hooks (`PreToolUse`,
`SessionStart`), moved off the short-lived `exo hook` CLI process and onto the
long-lived per-agent sidecar so the hook decision can consult live runtime
state instead of re-bootstrapping. Contract, stated precisely (PRD requires
this precision — conflating it with a git-hook implementation would overstate
where Tidepool is):

- **Transport:** Unix domain socket, one per (run, pane) — path from
  `exo_caps::paths::hook_sock(home, run_id, own_pane)`, computed identically
  by client and server so they agree without a discovery step.
- **Payload bound:** server caps the read at 64KB (`stream.take(64*1024)`)
  before parsing, to bound memory on a malformed/malicious client.
- **Timeout:** server-side 2s read timeout on receiving the request
  (`tokio::time::timeout`); client-side 5s timeout on the whole round trip.
  Both are hard-coded constants, not configurable.
- **Permission model:** socket file created `0o600` immediately after bind,
  stale socket removed before rebind (`remove_file`, `NotFound` tolerated).
  Filesystem permissions are the entire access-control model — no token, no
  auth handshake.
- **Client/server split:** client (`client_request`, `resolve_hook_sock`) is
  the short-lived process; it writes the request, half-closes its write side
  (`shutdown()`) as the end-of-request signal, then reads to EOF for the
  response. Server (`serve`, `handle_conn`) accepts, spawns one task per
  connection, reads to EOF (relying on the client's half-close), decodes,
  dispatches to `RoleDef::pre_tool_use` against the **live** `ctx.runtime`
  (shared, mutable, long-lived — not a fresh instance per request), encodes
  the verdict, writes, shuts down.
- **Fail-open, not fail-closed:** the client's doc comment is explicit — "On
  any failure the caller must fail open — the sidecar being down means there
  are no tools to gate anyway." Any transport error (connect failure, decode
  failure, timeout) becomes an allow, never a block.
- **Payload shape is Claude-Code-specific:** the server's response is
  literally Claude Code's hook-output JSON (`{"continue": true, ...}`,
  `hookSpecificOutput`), and the request's `HookEvent` enum is Claude Code's
  hook taxonomy (`PreToolUse`, `SessionStart`).

### 1c. `inbound.rs` — `rust/exo-node/src/inbound.rs` (`watch`, `process_inbox`, `save_cursor`, lines 1-180 and 862-956 read in full)

An at-least-once, notify-plus-backstop inbox reader over a single append-only
JSONL file per node, with a sibling `.cursor` file as the durable read
position. Enumerated in §4 below, property by property, against Tidepool's
journal/monitor design.

### 1d. Abnormal-teardown worktree preservation — three sites, not one

The PRD names this as a single bullet; reading it turned up three distinct,
non-overlapping mechanisms and it matters that they're distinct:

- **Birth-time rollback** — `rust/exomonad-core/src/services/agent_control/internal.rs:1-70`,
  `SpawnRollback` (RAII guard). If a spawn fails *partway through* (e.g.
  worktree created but tmux pane launch fails), `Drop` fires a detached
  cleanup task **unless** `commit()` was called. This deletes the
  partially-created worktree. This is cleanup of a *never-successfully-born*
  agent, not preservation of a *crashed running* one — opposite lifecycle
  phase from what the PRD bullet is about.
- **Merge-time reclaim** — `rust/exo/src/tools/merge.rs`, `worktree_remove`
  (`rust/exo-caps/src/git.rs:64-69`): "force/reclaim semantics — uncommitted
  state in the worktree *directory* (dirty files, untracked artifacts) is
  discarded, but the branch ref is untouched, so committed work survives."
  Fires only after a *successful* merge folds the child's branch in.
- **`exo doctor`** — `rust/exo/src/doctor.rs:26-70` and `:381-395`. This is
  the actual evidence for the PRD's claim. `classify()` sorts every worktree
  under `.exo/worktrees/` into `Current` / `Merged` / `Unmerged` / `Live`
  (`Live` = a non-terminal entry in the children ledger, independent of git
  ancestry — a fresh child's branch sits at the fork point with zero commits,
  which a pure ancestry check alone would misread as "merged"). Only
  `Merged` is reclaimed by plain `--fix`; `Unmerged` requires the explicitly
  named `--include-unmerged` flag, documented in `main.rs:77` as "dangerous,
  use with caution"; `Live` is **never** reclaimed by doctor at all, "not
  even with `--include-unmerged`" (comment at doctor.rs:63) — tearing down a
  live agent is `merge`/`dismiss_worker`/shutdown's job, never doctor's.
  Branch deletion (`delete_branch`) only happens for paths doctor actually
  reclaimed (doctor.rs:381-385) — an unreclaimed worktree's branch is
  untouched.

So: an agent whose pane dies abnormally (crash, OOM, kill -9, network drop)
leaves an `Unmerged` worktree that nothing removes automatically. There is no
watchdog-driven GC keyed off pane death — the *absence* of such a path, plus
`doctor`'s explicit unmerged/dangerous gate, **is** the preservation
mechanism. What's reconstructable afterward: the committed branch history
(git ancestry + whatever the ledger recorded about the child), not
uncommitted/untracked bytes in the worktree directory — those are only ever
at risk from an operator's own explicit `--include-unmerged` run, never from
the crash itself.

---

## 2. Reuse / extract / adapt / reject, per component

### 2a. Dirty-tree refusal check → **REJECT reuse of the code; the shared strategic default is already independently validated**

`tidepool-worktree` (`git.rs::inspect::dirty_summary`, stubbed but with a
committed shape in `error.rs::DirtySummary`) is already a **strict superset**
of what `require_clean_worktree` checks:

| Checked by | Exomonad `require_clean_worktree` | Tidepool `dirty_summary` + `in_progress` |
|---|---|---|
| Staged/unstaged/untracked present | Yes, one flat list (`status --porcelain` lines) | Yes, split into `staged`/`unstaged`/`untracked` — needed because the snapshot path (L2) must *capture* different categories differently, not just refuse |
| Ignored files | Excluded implicitly (default `git status` behavior) | Excluded explicitly, and **counted** (`ignored_excluded: usize`) so an author can tell the exclusion happened |
| In-progress merge/rebase/cherry-pick/revert/bisect (`MERGE_HEAD`, `rebase-merge`, `CHERRY_PICK_HEAD`, ...) | **Not checked** — a spawn from a mid-rebase tree is refused only if it also happens to be dirty by `git status`, which a detached-HEAD, no-conflict rebase state might not trip | Checked explicitly by marker-file presence (`git.rs::inspect::in_progress`), and deliberately kept **out of** `SourceDirty` as its own `WorktreeError::SourceOperationInProgress` variant — see the type doc in `error.rs:13-21` for why collapsing it into "dirty" would give the wrong advice (committing mid-rebase is not the fix) |
| Dirty submodules | Not checked at all | Explicit `WorktreeError::DirtySubmoduleUnsupported` variant reserved (v1 refuses rather than half-capturing a gitlink) |
| Env-var git redirection (`GIT_DIR` etc. from caller's shell) | Not scrubbed — `Command::new("git")` inherits ambient env | Scrubbed at the one call site (`GitCli::run`) |

Exomonad's check is a one-shot boolean gate for a single use case (refuse a
spawn tool call); it was never trying to produce a structured summary an
author could branch on, or feed a snapshot capture step, so it has no reason
to split categories or check in-progress state. **Recommendation: reject
porting `require_clean_worktree` itself — Tidepool's stub already asks a
harder question and the PRD already commits to it.** What Exomonad's version
*does* validate, independently, is the strategic default: "refuse a
worktree-creating spawn from a dirty source because the child forks from a
commit and won't see uncommitted state" is exactly Tidepool's `RequireClean`
default, arrived at independently in a live production tool. That's evidence
the default is right, not a reason to import code.

One thing worth a human decision, not a code import: Exomonad's env-inheriting
`git()` call has never needed scrubbing because Exomonad never runs a
temporary-index operation against the source repo. Tidepool's snapshot lane
(L2) does, via `GitCli::with_env("GIT_INDEX_FILE", ...)` — so the scrub is
load-bearing *because* of a capability Exomonad doesn't have, not portable
practice from Exomonad. No action needed; noted so nobody reads the table
above as "Exomonad is careless here."

### 2b. `hooksock/` transport shape → **ADAPT the shape, later; not now**

**Recommendation: adapt (defer).** The wake-up-shape is genuinely strong and
worth deliberately re-implementing against Tidepool's own contract when the
hook-adapter deferred question (PRD deferred question 3) is actually picked
up — but there is nothing to extract *today* because the two systems answer
different questions:

- Exomonad's hooksock answers "should this Claude tool call be allowed" —
  synchronous, blocking, in the hot path of every tool use, one verdict per
  request, payload is a Claude Code hook JSON blob.
- Tidepool's future hook adapter (PRD: "a later managed-worktree hook adapter
  sends a local wake-up with worktree identity and optional hints") answers
  "something happened, go re-read git" — fire-and-forget, not blocking
  anything, payload is at most a worktree id and a hint, and the *reconciled
  git read* remains the only source of truth per `tidepool-worktree/CLAUDE.md`.

What transfers is the **pattern**, not the code: bounded-payload UDS,
hard timeouts on both legs, `0o600` + remove-before-bind, thin client that
fails open/fails harmlessly on any transport error, server processes against
live state rather than re-bootstrapping. All five of those properties are
answers to "how do you build a local, low-latency, low-trust-surface wake-up
channel," which is exactly Tidepool's future problem too. None of it is
Rust code Tidepool can literally call, since `hooksock`'s wire types
(`HookRequest`/`HookVerdict`/`HookEvent`) are Claude-Code-hook-shaped and the
socket path scheme is keyed by Exomonad's `run_id`/`own_pane` addressing.

**Cost of doing this later:** small if deferred as a from-scratch
implementation against this shape; the risk is only in *not* writing down the
shape now, since by the time deferred question 3 is picked up this record
may not be the thing whoever's doing it reads first.

### 2c. `inbound.rs` reliability properties → see §4 (per-property, not
one blanket verdict — some are already matched by design, some are worth
adopting, one is a considered rejection)

### 2d. Abnormal-teardown preservation → **REJECT importing any code; the evidence supports the existing stance as-is**

Nothing to extract: `exo doctor`'s classification logic is entangled with
Exomonad's children ledger, tmux pane liveness, and CLI flag surface, none of
which Tidepool has or wants. **Recommendation: no code action.** The value of
this component is purely evidentiary — see §5.

---

## 3. Ownership and versioning boundary

There is no shared crate to version *yet* because nothing above cleared the
extract bar (2a and 2d are pure-evidence rejections; 2b is an explicitly
deferred adaptation with no code today; 2c's adoptable properties in §4 are
each small enough to implement directly in `tidepool-worktree` rather than
factor out). If that changes — most likely for 2b, if the hook-adapter work
later decides Exomonad's UDS-transport crate (or a slice of `exo-caps`) is
worth literally sharing rather than re-implementing against the shape — the
boundary questions a human needs to answer before any `Cargo.toml` edit are:

1. **Where does shared code live?** A new standalone crate (e.g.
   `exo-hooksock` published independently of both trees) versus a
   Tidepool-side reimplementation that happens to match the shape. Given
   Exomonad and Tidepool are separate repos with independent release
   cadences and Tidepool's workspace has no path/git dependency on
   `~/dev/exomonad` today, a standalone crate is the only option that
   doesn't create a cross-repo build coupling — but someone has to own
   publishing it, and right now that's nobody.
2. **Who owns compatibility?** Exomonad's hook wire types
   (`HookRequest`/`HookVerdict`/`HookEvent`) are Claude-Code-hook-shaped by
   design; Tidepool's future wake-up payload is deliberately smaller
   (worktree id + hint). Sharing the transport without sharing the wire
   types means the "shared" surface is a handful of small functions
   (bind-with-perms, bounded-read-with-timeout, half-close framing) — worth
   asking whether that's even enough code to justify a crate boundary over
   copying ~40 lines with attribution.
3. **Version drift risk.** Exomonad is under active, fast-moving development
   (the `.exo/logs/` directory alone shows dozens of hooksock-adjacent
   sessions). A Tidepool dependency — even a vendored-with-attribution copy —
   needs an explicit "we do not track Exomonad's `main`" statement, or every
   Exomonad hooksock change becomes an implicit Tidepool obligation.

None of this is resolved here. It's the shape of the conversation a human
needs to have *if and when* 2b's "adapt, later" becomes "adapt, now."

---

## 4. `inbound.rs` reliability properties, mapped against Tidepool's journal/monitor

Exomonad's stated contract (`inbound.rs:1-13`): cursor = byte offset in a
sibling file; `notify`-crate watch coalesced through `tokio::sync::Notify`;
15s periodic backstop tick; read only up to the last `\n` (torn-line
protection); advance cursor **after** successful last-hop delivery, written
temp+rename; missing cursor on a fresh node starts at EOF, never replays.

| Property | Exomonad mechanism | Tidepool `EventJournal`/`WorktreeMonitor` | Verdict |
|---|---|---|---|
| Durable cursor, atomic write | `save_cursor`: write `.tmp`, `sync_all()`, `rename()` (`inbound.rs:167-175`) | `journal.rs::open` doc: "outside the source tree; a torn final row is skipped, not fatal" — same failure class (partial write on crash), currently a stub | **Already-matched by design, not yet by code.** Tidepool's chosen mechanism (tolerate a torn *trailing row* on read) is a different tactic than Exomonad's (make the *cursor* atomic via temp+rename) because the two are protecting different things — Exomonad's cursor is a tiny external pointer file, Tidepool's journal is the append target itself. Both converge on "a crash never corrupts the durable state past the last complete write." Worth explicitly deciding L3 uses temp+rename for cursor-equivalent state (`end_cursor()`'s backing store) too, since that's the same shape of problem, not a new one. |
| No replay for a fresh reader | Missing cursor file → start at current EOF | PRD + `journal.rs` module doc, stated as a hard invariant: "a subscription registered now begins at the journal's current end and never sees a row written before it" | **Already-matched**, and stated *more* strongly in Tidepool's design — Exomonad's is a per-process default (missing-cursor case only); Tidepool's is a protocol-level guarantee for every `withHandler` registration regardless of whether it's the first ever or the tenth. |
| Notify + periodic backstop | `notify`-crate filesystem watch on the parent directory, coalesced via `tokio::sync::Notify::notify_one()`, plus a 15s `tokio::time::interval` backstop specifically because a routing failure leaves the cursor unadvanced and only the backstop retries it if no later filesystem write ever wakes the watcher (`inbound.rs:127-129`) | `worktree-lanes/README.md`'s L3 line: "poll/reconcile, coalesced deltas, journal" — poll-based by design (PRD: "V1 starts with polling and reconciliation after observed coding-agent command activity... a later managed-worktree hook adapter sends a local wake-up... Polling remains the fallback") | **Worth adopting the shape, already planned in spirit.** Tidepool's ordering is inverted from Exomonad's (poll-first, wake-up later vs. Exomonad's watch-first, poll-as-backstop) because Tidepool's "event" is a git repository state, which has no equivalent of `notify`'s file-level granularity — but the *reason* for the backstop tick is identical (a wake-up channel can silently fail to fire; a periodic tick is the only wake-up source that can't). L3 should keep an unconditional poll tick even after the hook adapter exists, exactly as Exomonad kept its 15s tick even with a working `notify` watch. |
| Advance-after-success delivery, at-least-once, no dedup by id | `process_inbox` (`inbound.rs:926-951`): cursor advances only on `Ok(_)` from the handler; a handler `Err` leaves the cursor untouched and returns `Ok(false)` so the caller retries within 15s. Comment at 936-941 is explicit that redelivery is at-least-once *by design* and no code anywhere may treat a repeated entry id as a dedup key. | PRD's handler semantics are structured-concurrency, not queue-and-retry: "Handler failure fails the enclosing scope and triggers normal structured cleanup. It is never logged-and-forgotten." / "Queue overflow, source loss, or inability to drain before runtime deadline fails loudly; commits are never silently dropped." | **Not-applicable, and a deliberate divergence worth stating plainly rather than silently declining.** Exomonad's model assumes an unreliable last-hop delivery (tmux paste, external inbox) that legitimately needs blind retry. Tidepool's `withHandler` body runs in the resident's own effect row — a handler failure is *authored code* failing, and PRD 19 chose to fail the enclosing scope loudly rather than silently retry an authored closure that raised. Importing "retry silently, no dedup" here would contradict the PRD's explicit "never logged-and-forgotten" line. The one adoptable piece is narrower: reconciliation itself (`WorktreeMonitor::reconcile`, independent of any handler) should be safe to retry blindly, exactly like Exomonad's cursor-unadvanced retry — and it already is, by the doc comment at `monitor.rs:130-131`: "Idempotent: reconciling twice with no writer in between yields nothing the second time." So the reconciliation *step* gets Exomonad's retry-safety property already; the *dispatch-to-handler* step deliberately does not get Exomonad's retry-and-ignore-failure property, by PRD decision. |
| Torn-line / torn-write tolerance on read | `process_inbox` finds the last `\n` via `rposition` and only processes complete lines; a torn trailing write is silently deferred to the next pass (`inbound.rs:885-891`) | `journal.rs::open` doc: "a torn final row is skipped, not fatal" (currently `todo!`) | **Already-matched by design intent**, not yet implemented. Same tactic, independently arrived at (append-only text format, tolerate an incomplete trailing record). Nothing to adopt beyond confirming L3's eventual implementation actually does this — it's stated as an invariant already, not a gap. |

Net picture: three of five properties are already matched at the design-doc
level (durable-atomic-write intent, no-replay, torn-record tolerance); one
(notify+backstop) is a shape worth deliberately keeping even after a future
hook adapter exists, matching Exomonad's own choice to keep polling under a
working watch; one (retry-with-no-dedup at the delivery/handler layer) is a
considered rejection because PRD 19 chose different failure semantics on
purpose, with the narrower reconciliation-level idempotence already covered.

---

## 5. Tests/receipts adopted failure behavior would need

Nothing in §2/§4 crossed from "worth adopting" into "adopted" — no behavior
was actually imported — so there is no new test obligation from this lane
itself. Recorded here so the obligation isn't invented later without being
checked against what already exists:

- If L3 lands the atomic temp+rename write for its cursor-equivalent
  durable state (the §4 "durable cursor" row), it needs a same-shape test to
  Exomonad's own `test_cursor_durability_across_restart`
  (`inbound.rs:1444+`): write, simulate a crash between temp-write and
  rename, restart, confirm the pre-crash state is what's recovered — not a
  torn value.
- If L3 keeps an unconditional backstop poll tick after a future hook
  adapter lands (the §4 "notify + backstop" row), it needs a test that a
  *lost* wake-up (adapter fires, message dropped) still gets picked up by
  the next poll within the poll interval — mirroring the reasoning in
  Exomonad's own comment at `inbound.rs:127-129`, not the code.
- `WorktreeMonitor::reconcile`'s already-stated idempotence
  ("reconciling twice with no writer in between yields nothing the second
  time") needs its own acceptance test once L3 implements it — this was
  already an L3 obligation before this lane existed, listed here only
  because §4 leans on it as the reason the delivery-retry property is
  not-applicable.

No behavior from `hooksock/`, `require_clean_worktree`, or `exo doctor` was
adopted, so none of those need a Tidepool-local test from this lane.

---

## 6. Provider-neutrality gate

**Claim to check:** no Exomonad tmux, Claude-only, or global-process
assumption leaks into Tidepool's public Haskell surface. This is a gate, not
a checkbox, so here is what was actually checked rather than an assertion:

- **What's confirmed genuinely tmux/Claude-Code/global-process-shaped in the
  Exomonad code read for this lane**, so the gate has something concrete to
  check against:
  - `hooksock`'s socket address is keyed on `own_pane` (a tmux pane) and
    `run_id` (Exomonad's own run-scoping concept) — `hook_sock(home, run_id,
    own_pane)`.
  - `hooksock`'s response payload is literally Claude Code's `PreToolUse`/
    `SessionStart` hook JSON shape (`hookSpecificOutput`, `continue`).
  - `inbound.rs`'s last-hop dispatch explicitly branches on "Teams inbox or
    tmux paste" (module doc, line 17) — a delivery mechanism tied to a
    specific chat surface or a specific terminal multiplexer.
  - The whole `NodeContext`/sidecar model (`hooksock::serve`,
    `inbound::watch`, and an outbound loop all sharing one live, long-running
    `ctx.runtime`) assumes **one persistent OS process per agent instance**,
    continuously alive between turns. Tidepool's PRD 19 explicitly rejects
    this shape for its own handler lifetime model — "Closures live only in
    the current realm. A later resident cycle re-registers reactions from
    explicit `State` and stable worktree IDs" — precisely because Tidepool's
    resident is not a persistent process holding live subscriptions across
    cycles the way an Exomonad sidecar holds live hook state across an
    agent's whole run.
  - `exo doctor`'s liveness model is tmux-pane-existence plus a home-dir
    status file refreshed every 5s by a live sidecar (`STALE_RUN_THRESHOLD`
    in `doctor.rs:19-24`) — again, a persistent-process liveness signal.

- **What Tidepool's current surface actually contains:** the public
  vocabulary in the PRD (`WorktreeSpec`, `createWorktree`, `workspaceOf`,
  `commit`, `headChanged`, `withHandler`) and everything landed in
  `tidepool-worktree/` so far (`id.rs`, `error.rs`, `git.rs`, the `create.rs`/
  `registry.rs`/`binding.rs`/`monitor.rs`/`journal.rs` stubs) names nothing
  from tmux, nothing from Claude Code's hook taxonomy, and nothing that
  assumes a long-lived process per agent — `AgentRef` in `binding.rs` is
  explicitly "a string newtype and not a typed agent handle" precisely so
  the coupled-spawn seam (on hold, designed jointly with the agent lane) has
  nothing agent-shaped to leak from here. `GitCli`'s environment handling
  scrubs git-specific variables only; nothing tmux- or Claude-specific
  appears anywhere in the crate.
- **Where this could still go wrong, named so a human can watch for it
  rather than trusting this snapshot forever:** the hook-adapter deferred
  question (PRD deferred question 3) is the one future piece of work in PRD
  19 whose *shape* is directly inspired by `hooksock` (§2b). If whoever picks
  that up reaches for Exomonad's actual `HookEvent`/`HookRequest` types
  instead of designing Tidepool's own (worktree id + hint) payload, that is
  exactly how a Claude-Code-hook-shaped assumption would leak in. This
  record's recommendation in §2b — adapt the *shape*, not the *types* — is
  the guardrail; enforcing it is a human review question at that future PR,
  not something this document can guarantee today.

**Gate result: holds, as of everything read for this lane.** No leak found
in the code that exists. The one identified risk is prospective (a future
implementation choice at the deferred hook-adapter question), not a defect
in anything landed now.

---

## Open questions for Inanna / root

1. **§2b timing.** Is "adapt the hooksock shape when the hook-adapter
   deferred question (PRD deferred question 3) is picked up" the right
   sequencing, or should the shape be written down as a standalone Tidepool
   design note *now* (no code, just the contract table in §1b turned into a
   spec) so it doesn't have to be re-derived from this record later?
2. **§3 crate boundary.** If/when 2b's adaptation happens, is a standalone
   published crate (no dependency either direction between the Tidepool and
   Exomonad repos) the right shape, or is copying ~40 lines with attribution
   comment enough given how small the actually-shared surface is (bind+perms,
   bounded-read-with-timeout, half-close framing)? This record has no
   opinion strong enough to default one way.
3. **§4 backstop interval.** Exomonad's periodic backstop is a hard-coded
   15s. Tidepool's poll/reconcile interval isn't decided yet (L3 stub has no
   number). Worth deciding independently of Exomonad's constant — repository
   observation and inbox delivery have different urgency profiles — but
   flagging that 15s is the closest existing precedent in case there's no
   other reason to pick a different number.
4. **§5 confirmation.** Does root agree nothing here rises to "adopted
   behavior needing a test" yet, or is there a property in §4 that should be
   promoted from "worth adopting" to "adopt now" as part of L3's own work
   (in which case its test obligation is real today, not deferred)?

---

**AWAITING HUMAN REVIEW — nothing adopted.**
