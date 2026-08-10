# Lane 1 live leg — how a human runs the one real coupled spawn

**Nothing in this document is run by an agent.** The live leg spends the
operator's ChatGPT tokens, so it is a deliberate, manually-triggered run with
receipts, never wired into a suite or a battery tier (standing rule, Inanna
2026-08-09; `inheritance-for-agent-core.md` §9). Every committed test in
`tidepool-agent` runs against fixtures or the mock backend; the three live
`#[ignore]`d tests in `backend::codex::process` stay ignored.

The subject of this document is `tidepool-agent/examples/live_one_cycle.rs`:
one coupled spawn (worktree + binding + thread + one turn) against the real
`codex app-server`, printing a receipt whose every field is checkable against
disk or the backend.

---

## Invocation

```bash
cargo run -p tidepool-agent --example live_one_cycle
```

Nothing else. No flags, no environment variables, no arguments. It exits `0`
on success and nonzero on any failure — including an isolation regression,
which it treats as a failure even if the turn itself succeeded.

## Preconditions

Check all four BEFORE running. A failure caused by a missing precondition still
spent tokens if it got as far as `turn/start`.

1. **Codex CLI 0.146.0 on `PATH`** — `codex --version` must print
   `codex-cli 0.146.0`. The adapter is pinned to that CLI against
   `codex-codes` `=0.146.4`; dynamic tools are an experimental surface, so a
   version skew is a fixture-regeneration event, not a lockfile refresh
   (`inheritance-for-agent-core.md` §2).
2. **Live ChatGPT auth in the real `~/.codex`** — the example runs against the
   operator's own Codex home on purpose. Proving that a normal worker run does
   not mutate it is the point of the isolation check, not something to route
   around. **Never** copy or rewrite credentials into an isolated `CODEX_HOME`
   (PRD 18 forbids it).
3. **Network reachability** to the model backend.
4. **A clean-enough box** — the example writes only into one fresh temp
   directory tree and reads `~/.codex`. It does not touch this repository.

## The model policy, and why `gpt-5.6-terra` is banned

Runtime-resolved, never a hardcoded slug (Inanna, 2026-08-09, superseding the
earlier hardcoded form). `ModelPolicy::CheapPlumbing` queries `model/list`
once per backend and takes the first of:

| order | slug |
|---|---|
| 1 | `gpt-5.4-mini` |
| 2 | `gpt-5.6-luna` |

and **fails loudly if neither is offered**, naming what the catalogue actually
contained. The list is an **allowlist**, not a denylist: a model that is not on
it cannot be selected however the server's catalogue changes. That is the
mechanism by which `gpt-5.6-terra` is unreachable — there is no "skip terra"
branch that a renamed slug could slip past.

Why the ban: `gpt-5.6-terra` is the expensive tier. The lane-1 vertical is
plumbing — one small synthetic task, no reasoning content of interest — so
spending that tier on it buys nothing and burns the operator's budget.

**The fixture is not a counterexample.** `fixtures/app-server-0.146.0/
phase4-live-turn.jsonl` was recorded on `gpt-5.6-terra` before this policy
existed. It is **protocol truth** (frame shapes, field names, ordering) and
never a model choice or a behavioural baseline. A cheap-plumbing run will not
reproduce that transcript turn-for-turn; a weaker model may need a blunter
prompt, which is why the example's task is written as flatly as it is.

The receipt records the **exact resolved slug**, never the tier name — a
receipt saying "cheap plumbing" is not checkable.

## What the run does

1. Snapshots `~/.codex`'s config surface (`ConfigSnapshot`, `isolation.rs`):
   sha256 of `config.toml` / `auth.json`, the `installation_id`, and the
   top-level listing.
2. Builds a throwaway substrate under one temp directory — a `source/` git repo
   (`git init`, one commit, pinned identity) plus sibling `registry/`,
   `worktrees/`, and `bindings/` roots. **The temp tree is kept on disk** and
   its path is printed: the binding table and the managed worktree *are* the
   receipt, and a receipt deleted on exit is not checkable.
3. Runs ONE `CoupledSpawner::spawn_one_cycle` against `CodexOneCycleBackend`
   with an ephemeral thread, **no dynamic tools**, and an output schema of
   `{"result": string}`, asking the worker to emit one specific word.
4. Prints the receipt, the payload, the activity, and the binding table read
   straight off disk.
5. Re-snapshots `~/.codex` and compares, per-field.

`cwd` appears at `turn/start` and **never** at `thread/start` — that split is
what avoids Codex's project-trust write into `config.toml` (PRD 18 acceptance
criterion 11). It is structural: the request type used for `thread/start` has
no `cwd` field, and a unit test pins the serialized shape.

## Receipts — what to paste back

Paste these lines **verbatim**. They are the evidence; a summary of them is not.

| line | why it matters |
|---|---|
| `RESOLVED MODEL:  <slug>` | the policy's whole point — must be `gpt-5.4-mini` or `gpt-5.6-luna` |
| `thread id:       <uuid>` | the backend accepted an ephemeral thread |
| `turn id:         <id>` | one turn actually ran on it |
| `worktree id:` + `binding ref:` | the coupling: one managed worktree, one agent, bound under that exact ref |
| the `--- payload ---` block | `Structured` / `Unstructured` / `Absent`, and the JSON if structured |
| the `--- binding table on disk ---` JSON | the binding's FINAL state, read from the file, not from memory |
| `isolation PASS: …` **or** the three `before=/after=` lines | criterion 11, per-field, not a bare boolean |
| the `scratch root (kept on disk):` path | so anything above can be re-checked afterwards |

A success run ends with `PASS: one-cycle live leg completed with receipts
above.` and exit code 0.

### Known gaps in the receipt (lane 1, deliberate)

- **No token/usage counters.** Usage arrives on `thread/tokenUsage/updated`
  notifications, which the driver records into its frame log but discards from
  the turn outcome. Surfacing it needs an event-projection layer, which is a
  later lane's design — not a field to bolt onto lane 1.
- **Activity is usually empty.** The projection reads `turn.items` from the
  terminal `turn/completed` frame and maps only command executions and file
  changes. In the phase-4 transcript that frame carried a single
  `agentMessage` item, so `none reported` is the expected shape for a
  message-only plumbing turn, not a bug.
- **The payload is not type-checked here.** `Structured` means "the terminal
  message parsed as JSON". Decoding it against a caller's Haskell result type
  happens on the Haskell side, and a decode failure there is a typed error,
  never a success.

## Stop-and-hold

**Stop. Do not re-run. Report what you saw.** Any of:

- **An auth prompt or a login/device-code flow.** The run is supposed to use
  the existing live auth silently. Being asked to authenticate means the
  credential state is not what this leg assumes.
- **`isolation FAIL`, or any `before=`/`after=` pair that differs.** The
  operator's Codex config changed. That is PRD 18 criterion 11 regressing, and
  the exact `config.toml` before/after hashes are the finding — capture them
  before anything else touches `~/.codex`.
- **A new top-level file in `~/.codex`** that is not a `-wal`/`-shm` sidecar of
  a database that already existed (the checker already excludes those).
- **A rate-limit wall**, an account/billing error, or anything mentioning
  quota. Do not wait it out and retry; report it.
- **`ProtocolRejected` mentioning `experimentalApi`, or any `thread/start`
  rejection.** That is a version-skew signal against the 0.146.0 / 0.146.4 pin,
  not a transient.
- **`no cheap-plumbing model available`.** The catalogue changed. The error
  names what WAS offered — that list is the finding. Do NOT edit
  `CHEAP_PLUMBING_PREFERENCE` to make the run go; escalate the catalogue change.
- **An orphaned-process warning on shutdown.** Check for a stray
  `codex app-server` before doing anything else.

## The ONE-attempt rule

Once `turn/start` has been sent, tokens are spent. **One attempt per go.** On
failure: capture the full output (it is all on stdout/stderr), capture the
kept scratch path, and report. Do not loop, do not "try once more to see if
it was flaky", do not tweak the prompt and re-run without a fresh go.

The example prints this rule on every failure exit, and it is the same rule
the phase-4 live test carries (`process.rs`, condition 3).

Failures that are safe to re-run *only* because they cost nothing: anything
that fails before `turn/start`, i.e. a precondition failure, a `thread/start`
rejection, or a `model/list` failure. When in doubt, treat it as
token-spending and report instead.

## Cleanup

The scratch tree is kept on purpose. Once the receipts have been read and
recorded, remove it:

```bash
rm -rf <the scratch root path the run printed>
```

Nothing else needs cleaning: the run never writes into this repository, and
the ephemeral thread is not persisted server-side.
