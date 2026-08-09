# Dev spec: turn-compile errors in USER coordinates, not template coordinates

When a model's Haskell turn fails to compile, the harness feeds the GHC error
back as a corrective user turn — that loop is the entire mechanism by which a
model repairs its own code. Today the error names a coordinate space the model
cannot see:

```
Expr.hs:62:5: error: Variable not in scope: _r
```

The model never wrote `Expr.hs`, never wrote line 62, and never wrote `_r`.
Its turn was a handful of lines; line 62 is inside the ~200-line generated
preamble. The model is being asked to fix code it cannot read, in a file it
does not know exists.

**This item is ONLY about the coordinate space.** Do not redesign the error
format, do not add explanatory prose, do not touch the retry loop.

## Read this first — the item is narrower than it looks

Two commits already moved this ground. Check both before writing code; if the
tip you are on already does what this spec asks, say so and stop.

- **`9c2b14ff`** (root, phase-b follow-up) added `render_compile_error` to
  `harness.rs` (~317). It fixed a REAL and different defect:
  `CompileError::Diagnostics`' `Display` reported only a COUNT
  ("Haskell compilation failed (1 diagnostic(s))"), so the corrective turn
  carried no information at all. That commit put severity, span, and message
  back. **It is not this item, and it is not wrong.**
- What it left: it formats `d.span` RAW — `{file}:{line}:{col}` — so the
  restored span is in template coordinates. Content fixed, coordinates not.
  That remainder is this item.

## Reuse the remapper; do not write a second one

`tidepool-mcp`'s eval path solved exactly this problem already and the
primitives live in a SHARED crate:

- `tidepool_runtime::diag::render_diagnostics(diags, &RenderOpts { anchor,
  label, user_lines, line_offset, col_indent, .. })`
- `tidepool_runtime::diag::extract_user_code_lines(source)`

See `tidepool-mcp/src/eval_prep.rs::format_error_with_source` (~530) for the
worked call. Its method:

1. Find the byte-exact marker where user code begins (there, `"__user = let {\n __b =\n"`).
2. `line_offset` = number of newlines before the user's first code line.
3. Hand that to `render_diagnostics`, which rebases every `anchor`-file span.

Your job is the same three steps against the HARNESS's turn template, whose
marker differs from the eval template's. **Find the harness's actual marker
empirically** — read the template builder that produced the source, do not
assume it matches eval's. Getting the marker wrong silently shifts every line
number by a constant, which is worse than not remapping (a wrong number looks
authoritative).

If the harness's compile path does not have the generated `source` available
at the error site, that is the real finding: report where it stops rather
than plumbing `source` through several layers. The threading cost decides
whether this item is cheap, exactly as with the synopsis item.

## Also: names the model cannot see

`_r`, `__user`, `__b`, `__anchor`, `result` are template-internal binders. A
diagnostic naming one is telling the model about plumbing. Where a message's
ONLY substantive content is such a name, that is a template defect worth
REPORTING (with the message, verbatim, in your handoff) — not papering over
with a find-and-replace. Do not rewrite GHC message text.

`__anchor` is load-bearing and recent (it fixed the ambiguous-`a0` defect);
if it starts appearing in user-facing diagnostics, that is a genuine
regression signal, so surface it rather than filtering it.

## Verify

- A turn whose user code fails on its FIRST line reports line 1, not the
  preamble-offset line. **This is the assertion that matters — mutation-close
  it** (break the offset by one, confirm red, restore).
- A multi-line user turn failing on its Nth line reports N.
- A non-`Diagnostics` `CompileError` variant (timeout, crash) still renders
  verbatim — those carry no GHC coordinates to remap. Root's existing
  early-return already does this; keep it.
- No template-internal binder appears in a remapped diagnostic for a turn that
  compiles the user's own names.

Standard tiers: `cargo check --workspace --all-targets`, `cargo fmt --all --
--check`, `cargo clippy --workspace`. Quick tier with the tests-RUN count.
GHC-heavy: `golden_path`, `acceptance_selfharness` (the retry loop is what
consumes these errors).

## Standing environment rules

```
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=<built tidepool-extract-bin>
export XDG_CACHE_HOME="$PWD/.cache"
```

The extract binary is SHARED and READ-ONLY — never rebuild it, never touch
`haskell/`. `XDG_CACHE_HOME` is mandatory before any test run and must be a
persistent per-worktree dir, not `mktemp -d`; verify it empirically (an
isolated cache appears in your worktree, `~/.cache/tidepool/selfharness/`
mtimes unchanged). A test run that writes the real user cache can clobber a
LIVE dogfood session — this has happened twice.

- Every GHC-heavy run through `/home/inanna/dev/tidepool/scripts/ghc-slots.sh
  run -- <cmd>` (absolute path). NEVER `exclusive` mode. Do not override
  `.config/nextest.toml`'s default-deny `ghc-heavy` group.
- Shard to ONE test per invocation. Gate on tests-RUN counts, NEVER exit
  codes.
- **Run GHC-heavy binaries DETACHED, not foreground.** This environment
  hard-kills background processes at ~380s, and a killed run is
  indistinguishable from a failure by exit status. Measured:
  `selfharness_lifecycle` needs ~596s, `acceptance_selfharness` ~210s. A
  short tests-RUN count means RE-RUN, not "failure".
- No LSP / rust-analyzer — `grep` and `Read` only.
- Scope kills to your own PID or worktree path; never a bare `pkill -f`.
- Never `git add -A`; never force-push; repo-root `tmp/` is protected; commit
  with `--no-verify`. Commit at every checkpoint.
- A flaky test never lands.

## Done criteria

- Turn-compile diagnostics report line numbers relative to the USER's code.
- The offset is mutation-closed.
- The shared `tidepool_runtime::diag` remapper is reused, not reimplemented.
- Non-`Diagnostics` variants still render verbatim.
- Any template-internal binder leaking into diagnostics is REPORTED, not
  filtered.
