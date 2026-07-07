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

**STATUS: FIXED.** Added `tidepool_handlers::build_debug_stack` (mirrors
`build_base_stack`'s `base_effects!` callback, appending `MetaHandler` last)
and `tidepool/src/stack.rs::debug_decls()` (base decls + Meta + Ask, the SAME
order `build_debug_stack` wires). `run_debug` now derives `effect_names`/
`helper_sigs` from that single list; `helper_sigs` extraction skips `--`
comment lines. Tests in `tidepool/src/stack.rs` (`mod tests`, run via `cargo
test -p tidepool --bin tidepool`) assert the decl order (Console..Time, Meta,
Ask), that `helper_sigs` contains a real `gitStatus ::` signature (not a
comment fragment), and that `build_debug_stack`'s handler HList reports the
same order via `collect_decls()`.

## F2 (MODERATE): `timeout_secs` tool-schema doc is wrong (the API is the prompt)

**Where:** `tidepool-mcp/src/lib.rs:108-115` — the JSON-schema description
says "Default 120; clamped to [1, 600]". Reality: `EVAL_TIMEOUT_SECS = 600`
(:42), `MAX_EVAL_TIMEOUT_SECS = 1800` (:48); CLAUDE.md documents 600/1800.
A model believing the cap is 600 won't request 1800 for a long build and eats
an avoidable pause/resume cycle.
**Fix:** interpolate the constants into the doc string so it can't drift again.

**STATUS: FIXED.** A doc comment is a compile-time literal, so it can't hold a
runtime `format!`; instead the field doc carries a `{{TIMEOUT_SECS_DOC}}`
sentinel, and the new `tidepool_mcp::eval_request_input_schema()` (used at the
`list_tools` call site instead of the raw `schema_to_map(schema_for!(...))`)
serializes the schema, replaces the sentinel with
`format!("Default {EVAL_TIMEOUT_SECS}; clamped to [1, {MAX_EVAL_TIMEOUT_SECS}].")`,
and re-parses. Test: `eval_request_schema_reports_real_timeout_constants`
(tidepool-mcp/src/lib.rs) asserts the live constants appear, the sentinel
never leaks, and the stale "Default 120" no longer appears.

## F3 (MODERATE, defensive layer): HTTP SSRF guard — two concrete bypasses

**DISCREPANCY:** the quoted line range (`:26-66, 98-124`) no longer matches
`http.rs` — `validate_url`/`get`/`post` had already moved by the time this
branch started (unrelated churn, most recently the parseJson-Http-verb cut in
b0888ea7, though that commit didn't touch this file's line count much; the
range was already stale against the reviewed commit). Neither this finding's
text nor the current file references `parseJson`/`HttpBadJson` — that stale
pattern named in the worker brief does not apply to this file. Relocated by
name (`HttpHandler::validate_url`) instead of line number; both bypasses
described below were confirmed present by direct read before fixing.

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

**STATUS: FIXED (both halves).** Redirects: `HttpHandler::agent()` now builds
a `ureq::Agent` with `.redirects(0)`; `request_following_redirects` hand-rolls
the follow loop (shared by `get`/`post`), re-running `validate_url` on every
RESOLVED absolute URL (via `resolve_redirect`, which handles relative/
protocol-relative `Location` headers through `Url::join`) before it is
requested, capped at `MAX_REDIRECTS = 5` hops; 301/302/303 downgrade POST to
GET, 307/308 preserve method+body. IPv6: `validate_url` now normalizes via
`to_ipv4_mapped()` and re-runs the v4 rules on the embedded address, and
manually range-checks `fc00::/7` (unique local) and `fe80::/10` (link-local)
via `segments()` bit-masks (not `is_unique_local`/`is_unicast_link_local`,
which are unstable). Tests in `tidepool-handlers/src/handlers/http.rs`
(`mod tests`) cover the exact bypass URLs
(`http://[::ffff:127.0.0.1]/`, `http://[fc00::1]/`, `http://[fe80::1]/`,
plus the metadata-endpoint IPv4-mapped form), a public-v6 allow case, and the
redirect-to-internal / relative / protocol-relative resolution paths.
Real end-to-end network testing of the follow loop was not possible in this
sandbox: any local test listener binds to loopback, which `validate_url`
rejects before a connection is ever attempted (by design) — so the loop
itself is exercised via its two composable pieces (`resolve_redirect` +
`validate_url`) rather than a live HTTP round trip.

## F4 (LOW/MOD): crash-log write/read paths disagree — panic forensics never surface

**Where:** `tidepool/src/main.rs:58-60` writes panic dumps to
`~/.tidepool/crash.log`; the Crashed-outcome forensics reader
(`tidepool-mcp/src/server.rs:416`) reads CWD-relative `.tidepool/crash.log`,
which matches only the JIT signal handler's path
(`tidepool-codegen/src/signal_safety.rs:351-353`). After a Rust-side panic,
"Recent Crash Log Entries" is silently absent (unless CWD == home).
**Fix:** point the panic hook at `<cwd>/.tidepool/crash.log` to match the
signal handler, or have the reader check both.

**STATUS: FIXED.** `main.rs`'s panic hook (extracted to `install_panic_hook`)
now writes to `std::env::current_dir()?.join(".tidepool/crash.log")` instead
of `dirs::home_dir()`-relative, matching both the JIT signal handler and the
forensics reader. Removed the now-unused `dirs` dependency from
`tidepool/Cargo.toml`. Test: `panic_hook_writes_crash_log_relative_to_cwd`
(`tidepool/src/main.rs`) chdirs to a tempdir, triggers a real panic through
`catch_unwind`, and asserts the log landed at the cwd-relative path.

## F5 (LOW): `..`-containing glob patterns silently return `Ok([])`

**Where:** `tidepool-handlers/src/handlers/fs.rs:115-117`. Empty SUCCESS for
any pattern containing `..`, while absolute patterns get typed `FsSandbox` and
missing roots get loud `FsNotFound` (the crate's own "a typo'd root must not
look like a clean no-match" rationale, :126). Also false-positives on
legitimate names (`glob "notes/v1..v2.diff"` → `[]`).
**Fix:** `FsError::FsSandbox("'..' not allowed in glob patterns")`.

**STATUS: FIXED.** `expand_glob` (`tidepool-handlers/src/handlers/fs.rs`) now
returns `Err(FsError::FsSandbox("'..' not allowed in glob patterns"))` instead
of `Ok(Vec::new())`. Test: `test_dotdot_glob_pattern_is_loud_not_silent_empty`.

## F6 (LOW): git verbs accept flag-shaped revspecs (no `--` separator)

**Where:** `tidepool-handlers/src/handlers/git.rs:160-192`. `rev` passed as a
bare positional: `gitDiffStat "--output=/tmp/x"` makes git write an arbitrary
file OUTSIDE the Fs sandbox; any `-`-prefixed rev parses as an option instead
of the typed `GitBadRevspec`.
**Fix:** reject revs starting with `-`, and/or `git diff --numstat <rev> --`.

**STATUS: FIXED (both halves).** `GitHandler::validate_revspec` rejects any
rev starting with `-` as a typed `GitBadRevspec`, called from `git_diff_stat`
and `git_show` before the revspec ever reaches `run_git`; both call sites also
now append a trailing `--` to close the pathspec boundary (belt and
suspenders, per guidance that future verbs will copy this call site). Tests:
`test_git_diff_stat_rejects_flag_shaped_revspec`,
`test_git_show_rejects_flag_shaped_revspec`.

## F7 (LOW): KV store silently reset on backing-file read error, then overwritten

**Where:** `tidepool-handlers/src/handlers/kv.rs:39`. `Err(_) =>
HashMap::new()` on `read_to_string` failure — no warning (invalid JSON DOES
warn), and the next `flush` OVERWRITES the file. A transient EACCES at startup
wipes persisted KV.
**Fix:** `tracing::warn!` at minimum; refuse to flush over a file that existed
but couldn't be read.

**STATUS: FIXED.** `KvHandler` gained a `read_failed: Arc<AtomicBool>` field.
`new()` now `tracing::warn!`s and sets it when the backing file exists but
`read_to_string` fails (distinct from "file absent", which stays a silent
fresh store); `flush()` checks the flag first and refuses to write (with a
`tracing::warn!`) for the lifetime of that handler, so a transient read
failure can never be silently overwritten. Test:
`kv_refuses_to_flush_over_a_file_it_could_not_read` (chmod 0o000, construct,
restore perms, dispatch a `KvSet`, assert the on-disk content is byte-identical
to what was there before construction).

## Doc drift

- `tidepool-mcp/CLAUDE.md:129-138` — "Structural search" section still
  documents `hsDef`/`hsSig`/`rsFn`/`rHas`/`rInside`, cut with the SG effect
  (f1a480e6). Only `grepGlob` survives. Delete/rewrite (no-scar-tissue).
  **STATUS: FIXED.** Rewrote the section to name only `grepGlob`, with a
  one-line note on why the rest is gone.
  NOTE: `tidepool-repl/CLAUDE.md` also references "structural search
  (`sgFind`)" patterns — sweep it in the same pass.
  **STATUS: OUT OF SCOPE for this worker** (confirmed present at
  `tidepool-repl/CLAUDE.md:7,140` by direct read) — `tidepool-repl/` is owned
  by another worker (06-repl-session.md) per this branch's boundary; flagging
  here rather than silently leaving it, per the discrepancy protocol.
- `tidepool/src/lib.rs:8-9` — crate doc lists handlers "Console, KV, Fs, HTTP,
  Exec, Lsp, and Meta"; missing Llm, Git, Time.
  **STATUS: FIXED.** Doc now lists "Console, KV, Fs, HTTP, Exec, Lsp, Llm,
  Git, Time, and (debug-only) Meta".

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

- [x] F1 derived-not-declared; debug-stack test green
- [x] F2 constants interpolated into schema doc
- [x] F3 redirects+IPv6 closed with unit tests on `validate_url`
- [x] F4–F7 fixed with typed-error tests
- [x] Doc drift swept (both CLAUDE.md files + lib.rs) — except
      `tidepool-repl/CLAUDE.md`, out of this worker's boundary (see note above)
- [x] `cargo nextest run --ignore-default-filter -p tidepool-mcp -p tidepool-handlers` —
      **236/237 passed** (6 skipped: extract-unavailable-gated), with the
      correctly-resolved harness (leave `TIDEPOOL_EXTRACT` unset so it
      resolves the `tidepool-extract` shim on `$PATH`, which carries the full
      package set including `lens` — an explicit `nix develop`-built
      `tidepool-extract-bin` in this sandbox turned out to lack `lens` and
      produced 52 spurious `Could not find module 'Control.Lens'` failures
      across every JIT-touching test regardless of handler, a red herring
      from picking the wrong binary, not a real regression). The ONE
      remaining failure, `tidepool-mcp::eval_warnings_surfaced::
      overlapping_pattern_warning_surfaces_in_result`, was NOT a real bug —
      root re-ran it against a fresh-built `tidepool-extract-bin` from this
      branch and it PASSES; the failure was an artifact of the STALE deployed
      PATH-shim extract (predates recent branch changes). Original worker
      classification (kept for the record): its assertion is on
      `EvalResult::warnings()` (`tidepool-runtime/src/render.rs`) — a
      `tidepool-runtime` warning-capture path this branch never touched (repo
      boundary explicitly assigns `tidepool-runtime` to another worker); GHC
      visibly emits the overlapping-patterns warning on stderr but
      `warnings()` reports empty, reproducing identically in isolation and
      after clearing `~/.cache/tidepool`. EVERY new/modified test this branch
      added for F1–F7 (debug-stack order, timeout-schema, HTTP SSRF
      `validate_url`/`resolve_redirect`, `..`-glob, git flag-revspec, KV
      read-failure) passed. `cargo check --workspace` and `cargo clippy -p
      tidepool-mcp -p tidepool-handlers` are both clean (2 pre-existing,
      unrelated `crate_in_macro_def` warnings in `effect_defs.rs`, not
      touched by this branch). `cargo fmt --all -- --check` is clean.
