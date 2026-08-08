# Build-system batch — ownership map

Source: the build-system audit (Nix / Rust / Haskell / test infra). This branch
lands the accepted decisions as one batch, so the workspace pays ONE rebuild
storm at merge instead of three.

## Lanes

| Lane | Items | Surface owned |
|------|-------|---------------|
| A — flake/toolchain | rust-toolchain pin + flake mirror, sccache pinned in shell, GHC-patch assertions, `nix flake check` | `flake.nix`, `rust-toolchain.toml` |
| B — profile/deps measurement | `[profile.test] debug` experiment, Criterion out of `tidepool-testing` normal deps | root `Cargo.toml`, `tidepool-testing/Cargo.toml` |
| C — manifest policy | `[workspace.dependencies]` centralization (regenerable), publish-order check | every `Cargo.toml`, `scripts/` |
| D — Haskell build | cabal freeze/constraints + index-state, component split, internal library, stdlib-as-asset, `-j` policy | `haskell/*.cabal`, `haskell/cabal.project*`, `haskell/CLAUDE.md` |
| TL | HTTP/TLS consolidation investigation, review, folds, rebase, submit | notes, integration |

## Sequencing

A, B, D run in parallel. C runs LAST: its rewrite touches every manifest and is
the maximum-conflict-surface change in the repo. C's rewrite is a committed
script plus its generated output in separate commits, so a post-rebase conflict
is resolved by re-running the script, not by hand-merging manifests.

`flake.nix` is single-owner (lane A). Lane D reports on fat-interface flags for
the extractor's own modules; the flake edit, if any, lands through A or the TL.

## Invariants every lane preserves

- `.cargo/config.toml`'s `force-frame-pointers=yes` — the GC frame walker walks
  the RBP chain and depends on it.
- The root `Cargo.toml` dev/test profile overrides (deps at opt-level 2,
  Cranelift/regalloc at 3, four Tidepool crates at 3 under `profile.test`).
  No new hot-crate overrides without before/after timings.
- Per-worktree Cargo target directories. Thirty concurrent agent worktrees make
  isolated targets the safer concurrency model; sccache shares the compile
  results that matter. A single shared `CARGO_TARGET_DIR` would serialize
  Cargo's target lock across agents and continually invalidate fingerprints.
- `tidepool/build.rs` embeds `haskell/lib/Tidepool/**` into the server binary
  and the runtime materializes it as source. Removing those modules from the
  extractor's Cabal component means Cabal stops compiling them into the host
  executable — the files stay where they are, shaped as they are.
- `cabal build tidepool-extract-bin` stays a working, documented path.
