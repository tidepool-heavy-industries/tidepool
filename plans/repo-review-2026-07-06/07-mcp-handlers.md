# 07 — tidepool-mcp + tidepool-handlers + tidepool facade

Server-surface drift and defensive-layer gaps. Context for severity calls:
eval code has unrestricted shell via Exec, so the Http/Fs/Git guards are
ACCIDENT-PREVENTION, not a security boundary — but they exist to close exactly
these gaps, and typed-error contracts (the API is the prompt) should hold.

## ANTI-PATTERNS

- Do NOT hand-roll anything that `base_effects!`
  (`tidepool-mcp/src/effect_defs.rs`) can derive — that drift IS finding F1.
- Do NOT change effect stack order/membership casually — it's the locked
  single source; `--debug` must DERIVE from it, not re-declare it.

## READ FIRST

- `tidepool-mcp/CLAUDE.md`, `tidepool-handlers/CLAUDE.md`
- `tidepool/src/stack.rs` — `run_base` vs `run_debug`
- `tidepool-mcp/src/effect_defs.rs` — `base_effects!`, `helper_text!`
  (:102-138), `standard_decls`

---

## F1 (MODERATE): `--debug` stack drifted from the single source — Git/Time silently missing, Meta introspection lies

**Where:** `tidepool/src/stack.rs:18-40`. `run_base` derives its handler HList
from `base_effects!` (tags 0–8 incl. Git/Time); `run_debug` HAND-ROLLS
`[Console, KV, Fs, Http, Exec, Lsp, Meta, Llm]` and was never updated when
Git/Time were added. Three concrete failures in `tidepool --debug`:

1. **Missing effects:** server decls derive from the ACTUAL stack
   (`H::collect_decls()`), so `gitLog`/`gitStatus`/`getCurrentTime` are not in
   the generated `Tidepool.Effects` — every eval using them fails to compile,
   silently diverging from the base server.
2. **Meta misreports:** `effect_names` is built from `standard_decls()` (which
   INCLUDES Git/Time) with `decls.insert(decls.len() - 2, meta_decl())` — the
   `// before Llm, Ask` comment is stale; neither position matches Meta's real
   tag (6). `metaEffects` — the debug introspection verb itself — reports
   Git/Time as present and misplaces Meta.
3. **`metaHelp` returns junk:** `helper_sigs` takes `h.lines().next()` per
   helper, but every generated helper now STARTS with a `-- |` doc comment
   (`helper_text!`), so the sig list is mostly comment fragments.

**Fix:** derive the debug HList from `base_effects!` via a callback appending
`MetaHandler` (exactly how `build_base_stack` works); build
`effect_names`/`helper_sigs` from the actual stack's `collect_decls()`;
extract sigs by first non-comment line.
**Verify:** `tidepool --debug` test asserting `gitStatus` compiles and
`metaEffects` output == actual stack order.

## F2 (MODERATE): `timeout_secs` tool-schema doc is wrong (the API is the prompt)

**Where:** `tidepool-mcp/src/lib.rs:108-115` — the JSON-schema description
says "Default 120; clamped to [1, 600]". Reality: `EVAL_TIMEOUT_SECS = 600`
(:42), `MAX_EVAL_TIMEOUT_SECS = 1800` (:48); CLAUDE.md documents 600/1800.
A model believing the cap is 600 won't request 1800 for a long build and eats
an avoidable pause/resume cycle.
**Fix:** interpolate the constants into the doc string so it can't drift again.

## F3 (MODERATE, defensive layer): HTTP SSRF guard — two concrete bypasses

**Where:** `tidepool-handlers/src/handlers/http.rs:26-66, 98-124`.

1. **Redirects skip validation entirely:** `validate_url` runs only on the
   initial URL; ureq 2 follows up to 5 redirects by default. A public URL that
   302s to `http://localhost/…` or `http://169.254.169.254/…` is followed.
   Fix: agent with `redirects(0)`; loop with re-validation (or surface 3xx as
   data).
2. **IPv6 checks only loopback/unspecified:** `http://[::ffff:127.0.0.1]/`
   (IPv4-mapped) reaches loopback; ULA `fc00::/7` and link-local `fe80::/10`
   pass. Fix: `ip.to_ipv4_mapped()` against the v4 rules + the v6 private
   ranges.

(DNS names resolving to internal IPs remain unchecked — out of scope unless a
resolver hook is added; noted, not required.)

## F4 (LOW/MOD): crash-log write/read paths disagree — panic forensics never surface

**Where:** `tidepool/src/main.rs:58-60` writes panic dumps to
`~/.tidepool/crash.log`; the Crashed-outcome forensics reader
(`tidepool-mcp/src/server.rs:416`) reads CWD-relative `.tidepool/crash.log`,
which matches only the JIT signal handler's path
(`tidepool-codegen/src/signal_safety.rs:351-353`). After a Rust-side panic,
"Recent Crash Log Entries" is silently absent (unless CWD == home).
**Fix:** point the panic hook at `<cwd>/.tidepool/crash.log` to match the
signal handler, or have the reader check both.

## F5 (LOW): `..`-containing glob patterns silently return `Ok([])`

**Where:** `tidepool-handlers/src/handlers/fs.rs:115-117`. Empty SUCCESS for
any pattern containing `..`, while absolute patterns get typed `FsSandbox` and
missing roots get loud `FsNotFound` (the crate's own "a typo'd root must not
look like a clean no-match" rationale, :126). Also false-positives on
legitimate names (`glob "notes/v1..v2.diff"` → `[]`).
**Fix:** `FsError::FsSandbox("'..' not allowed in glob patterns")`.

## F6 (LOW): git verbs accept flag-shaped revspecs (no `--` separator)

**Where:** `tidepool-handlers/src/handlers/git.rs:160-192`. `rev` passed as a
bare positional: `gitDiffStat "--output=/tmp/x"` makes git write an arbitrary
file OUTSIDE the Fs sandbox; any `-`-prefixed rev parses as an option instead
of the typed `GitBadRevspec`.
**Fix:** reject revs starting with `-`, and/or `git diff --numstat <rev> --`.

## F7 (LOW): KV store silently reset on backing-file read error, then overwritten

**Where:** `tidepool-handlers/src/handlers/kv.rs:39`. `Err(_) =>
HashMap::new()` on `read_to_string` failure — no warning (invalid JSON DOES
warn), and the next `flush` OVERWRITES the file. A transient EACCES at startup
wipes persisted KV.
**Fix:** `tracing::warn!` at minimum; refuse to flush over a file that existed
but couldn't be read.

## Doc drift

- `tidepool-mcp/CLAUDE.md:129-138` — "Structural search" section still
  documents `hsDef`/`hsSig`/`rsFn`/`rHas`/`rInside`, cut with the SG effect
  (f1a480e6). Only `grepGlob` survives. Delete/rewrite (no-scar-tissue).
  NOTE: `tidepool-repl/CLAUDE.md` also references "structural search
  (`sgFind`)" patterns — sweep it in the same pass.
- `tidepool/src/lib.rs:8-9` — crate doc lists handlers "Console, KV, Fs, HTTP,
  Exec, Lsp, and Meta"; missing Llm, Git, Time.

## Opportunities

- Dedupe `exec_run_argv` (`exec.rs:94-129`) — duplicates `run_command`'s 2MB
  truncation + Proc assembly verbatim; extract
  `fn proc_from_output(output: Output) -> Proc`.
- `StartError::Busy` inconsistency (`server.rs:199-206`): Overloaded →
  `CallToolResult::error` (retryable, model-visible) but Busy →
  `McpError::internal_error` (protocol error). Make both tool-level for a
  clean retry signal.
- `with_prelude` (`server.rs:779-783`) re-derives `H::collect_decls()` +
  `ask_decl()`, duplicating `new()` — stash the decls once.
- `fs_metadata`/`fs_hash` fold permission errors into "absent" (`None`) —
  consistent with absence-is-data, but a CAS loop meeting EACCES gets a
  confusing conflict; consider a deliberate distinction.
- ~25 clippy warnings workspace-wide (style tier: `if let` vs single-arm
  `match`, `contains()` vs `iter().any()`, doc-list indentation), concentrated
  in tidepool-mcp + test files — `cargo clippy --fix` sweep.

## Verified clean — do NOT re-audit

The main.rs → prelude/setup/stack refactor (e4660ee9) is PURE code motion —
diffed line-by-line: secrets loading, prelude resolution, degraded-server
fallback, signal-handler install order, config layering, KV-path selection all
byte-preserved. Fs sandbox (`resolve`, `expand_glob`): canonicalize-both +
`starts_with` per call; deepest-existing-ancestor reconstruction catches `..`
and symlink escapes for not-yet-existing write targets (adversarial paths
traced; the one oddity is a wrong-but-INSIDE-sandbox reconstruction when a
`..` path's prefix doesn't exist — not an escape). #335 typed-error contracts
hold across Exec/Fs/Http/Git/Llm/Lsp (acceptance tests pin each). Concurrency:
`write_generated_modules` (process lock + unique-tmp atomic rename),
`lib_isolate` memo, `CapturedOutput`, KV Arc-shared store across per-eval
clones all sound; per-eval `LlmHandler` clone deliberately resets the budget.
effect_defs single-source: decl↔Req↔dispatch pairing closed by construction;
the ONLY consumer still hand-rolling is `run_debug` (F1).

## DONE CRITERIA

- [ ] F1 derived-not-declared; debug-stack test green
- [ ] F2 constants interpolated into schema doc
- [ ] F3 redirects+IPv6 closed with unit tests on `validate_url`
- [ ] F4–F7 fixed with typed-error tests
- [ ] Doc drift swept (both CLAUDE.md files + lib.rs)
- [ ] `cargo nextest run --ignore-default-filter -p tidepool-mcp -p tidepool-handlers` green
