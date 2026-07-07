# Plans

## Active plan: repo-review-2026-07-06

Full-repo bug hunt (7 parallel reviewers + verification pass, 2026-07-06 on
`ghci-session` @ `cc90fe07`). ~60 verified findings across every crate, written
up as self-contained fix specs under `repo-review-2026-07-06/`. Each file is
independently workable by a fresh session; cross-file dependencies are called
out inline.

**All file:line references are as of commit `cc90fe07`.** Re-locate by the
quoted code/identifiers if the tree has moved.

### Provenance & confidence (how much to trust each finding)

Three evidence tiers, marked inline in the files:

- **EXECUTED REPRO / REPRODUCED LIVE** — a program was run and the wrong
  behavior observed (PartialEval Lam/Join in 04; non-ASCII String bugs in 02,
  reproduced against the live eval server). Highest confidence; the repro
  is embedded in the plan file.
- **VERIFIED BY (DIRECT) READ** — the coordinating session re-read the exact
  code and confirmed the mechanism (01 findings 1a/2/4, 03 F1, 06 F1, and
  others so marked). High confidence in the mechanism; the downstream blast
  radius is traced-but-not-executed.
- **Unmarked** — traced by a dedicated review agent that was required to
  articulate a concrete failure path before reporting, but not independently
  re-verified. Treat as "confirm mechanism on arrival, then fix" — if the
  code contradicts the finding, trust the code and note the discrepancy in
  this README.

Every file also carries a **"Verified clean — do NOT re-audit"** section:
subsystems that were adversarially traced and held up. Don't re-burn effort
there; do re-check a clean claim if your fix touches its assumptions.

### Coverage caveats (what this review did NOT deeply cover)

- `examples/guess/`, `examples/tide/` — only repo-hygiene level (09).
- `notes/`, `tmp/` — out of scope (and `tmp/` is protected scratch; leave it).
- `.tidepool/lib/ExploreProbe.hs` — assigned but no findings reported; treat
  as lightly reviewed, not verified clean.
- DNS-rebinding-class SSRF (07 F3) explicitly noted as out of scope.
- The review ran on `ghci-session` @ `cc90fe07` with the then-untracked files
  present; work merged afterward is unreviewed.

### Working these plans

- Files 01–09 are **conflict-free by construction** (disjoint crates/paths,
  cross-file dependencies called out inline) — safe to run as parallel
  workers/worktrees, one branch per file. Exceptions worth sequencing:
  03 F5 + 05 F2 share the qualified-name constants (coordinate or same
  worker); 04 F4 (shadowing generator) and 08 F1 (deep-force compare) gate
  each other's usefulness — land pass fixes before generator mode.
- Each file is spec-shaped (ANTI-PATTERNS → READ FIRST → findings → DONE
  CRITERIA) and self-contained; a worker needs only its file + the repo.
- Fix + red test land together; update the file's checkboxes and the Status
  list below as you go. If a finding turns out wrong, mark it so in the file
  (don't silently delete) — the discrepancy is signal.

### Fix sequencing (recommended order)

| Tier | File | Theme | Why this order |
|------|------|-------|----------------|
| 1 | [01-gc-memory-safety.md](repo-review-2026-07-06/01-gc-memory-safety.md) | GC rooting/init holes in codegen+heap | The plausible sources of rare nondeterministic heap corruption in long repl sessions |
| 2 | [02-haskell-stdlib.md](repo-review-2026-07-06/02-haskell-stdlib.md) | Silent wrong answers on everyday input | Two live-reproduced bugs (non-ASCII strings); small fixes, big fluency payoff |
| 3 | [03-engine-runtime-macro-bridge.md](repo-review-2026-07-06/03-engine-runtime-macro-bridge.md) | Availability + diagnostics | CancelHandle loss → permanent Overloaded; false StackOverflow ceiling (in 01) pairs with this |
| 4 | [04-optimizer-shadowing.md](repo-review-2026-07-06/04-optimizer-shadowing.md) | Shadowing soundness + test blindness | Two executed-repro miscompiles; one hardening commit covers the class |
| 5 | [05-repr-wire-eval.md](repo-review-2026-07-06/05-repr-wire-eval.md) | Wire-format + oracle robustness | Untrusted-input hangs/aborts; oracle divergence gaps |
| 6 | [06-repl-session.md](repo-review-2026-07-06/06-repl-session.md) | Repl response/session correctness | Data-loss in block responses; doc/code contract splits |
| 7 | [07-mcp-handlers.md](repo-review-2026-07-06/07-mcp-handlers.md) | Server surface + sandbox hygiene | `--debug` stack drift; SSRF guard gaps; typed-error contract holes |
| 8 | [08-test-infra.md](repo-review-2026-07-06/08-test-infra.md) | Tests that can't catch what they claim | Fix BEFORE relying on the suites to verify tiers 1–5 |
| 9 | [09-hygiene-docs-misc.md](repo-review-2026-07-06/09-hygiene-docs-misc.md) | Doc drift, dead code, scripts | no-scar-tissue sweep; mostly deletions |

Tier 8 is a soft prerequisite for tiers 1/4/5: several of the suites that
would verify those fixes currently mask the exact failure modes being fixed
(thunk-skipping compare, no-shadowing generator, panic-tolerant primop
routing). Read 08 before trusting a green run as evidence.

### Build/verify basics (every fix session needs these)

```bash
nix develop                              # Rust + GHC 9.12
cargo check --workspace
scripts/battery.sh                       # full suite (builds TIDEPOOL_EXTRACT if unset)
cargo nextest run                        # quick tier: pure-Rust crates only
cargo nextest run --ignore-default-filter -p <ghc-heavy-crate> -E 'test(name)'
```

Changed `haskell/`? Follow `haskell/CLAUDE.md` rebuild + deploy steps
(`scripts/redeploy.sh`). `scripts/deploy.sh` was a stale shadow of it and has
been deleted (tier 09).

### Status

- [ ] 01 GC / memory safety — ALL findings merged (1-5: 5ad34be4; M/L/doc/dead-code: 180f1791; L6 verified false); #34 sorted-fromList footgun retired (ea303d2e); remaining: final battery only (root close-out)
- [ ] 02 Haskell stdlib + extract — all findings merged (H tier 2760024b; M/LOW tier 7d5cba1e); remaining: root close-out only (final battery, redeploy, live H1/H2 spot-check) + RustSections.hs LOWs (untracked WIP, root-owned)
- [x] 03 Engine / runtime / macro / bridge — merged (50456e8c); freer_names consts exported for 05's F2 (in flight)
- [x] 04 Optimizer shadowing — merged (F1-F3 81e3b91f, F5/F6 fecd34b7, F4 acbd6f05; worker died pre-commit on F4, root verified 3/3 shadowing proptests + committed)
- [x] 05 Repr wire + eval oracle — merged (dc6bcbf2); freer_names single-sourced into tidepool-repr (effect re-exports). Residual follow-up: F7's typed rejection covers the untrusted decode_json_str boundary; parse_decimal_token's other callers (mcp/bridge) still silently zero an unparseable exponent — shared-signature fix deferred (see plan STATUS)
- [x] 06 Repl session — merged (51df6121; worker died pre-commit, root verified 189/189 + committed)
- [x] 07 MCP + handlers — merged (930b2ca4)
- [x] 08 Test infra — merged (8aca76b6); F5 + redeploy preflight finished by root in scripts/; battery 2720/2720 green (worker full run; root spot-checked the 14 ex-failing)
- [x] 09 Hygiene / docs / misc — merged (fed2a5df)

Mark a file's checkbox only when its own DONE CRITERIA section is satisfied.
