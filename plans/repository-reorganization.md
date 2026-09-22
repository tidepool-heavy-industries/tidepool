# Tidepool / Exomonad repository reorganization

This is the implementation map for the repository reorganization. It is
temporary coordination scaffolding and should be retired after the resulting
layout and ownership rules are reflected in the root and product guides.

Baseline: `65e3596cd`.

## Source roots

| Current path | Destination | Package naming |
|---|---|---|
| `tidepool-repr` | `tidepool/repr` | retain `tidepool-repr` |
| `tidepool-heap` | `tidepool/heap` | retain `tidepool-heap` |
| `tidepool-codegen` | `tidepool/codegen` | retain `tidepool-codegen` |
| `tidepool-bignum` | `tidepool/bignum` | retain `tidepool-bignum` |
| `tidepool-bridge` | `tidepool/bridge` | retain `tidepool-bridge`; this is value conversion, not the transitional root |
| `tidepool-bridge-derive` | `tidepool/bridge-derive` | retain `tidepool-bridge-derive` |
| `tidepool-bridge-effects` | `tidepool/bridge-effects` | retain `tidepool-bridge-effects` |
| `tidepool-effect` | `tidepool/effect` | retain `tidepool-effect` |
| `tidepool-extract-cmd` | `tidepool/extract-cmd` | retain `tidepool-extract-cmd` |
| `tidepool-extract-report` | `tidepool/extract-report` | retain `tidepool-extract-report` |
| `tidepool-toolchain` | `tidepool/toolchain` | retain `tidepool-toolchain` |
| `tidepool-runtime` | `tidepool/runtime` | retain `tidepool-runtime` |
| `tidepool-prepared-corpus` | `tidepool/prepared-corpus` | retain `tidepool-prepared-corpus` |
| `tidepool-actor` | `exomonad/actor` | rename to `exomonad-actor` |
| `tidepool-agent` | `exomonad/agent` | rename to `exomonad-agent` |
| `tidepool-model` | `exomonad/model` | rename to `exomonad-model` |
| `tidepool-model-output` | `exomonad/model-output` | rename to `exomonad-model-output` |
| `tidepool-node` | `exomonad/node` | rename to `exomonad-node` |
| `tidepool-tool` | `exomonad/tool` | rename to `exomonad-tool` |
| `tidepool-worktree` | `exomonad/worktree` | rename to `exomonad-worktree` |
| `tidepool-harness` | `exomonad/harness` | rename to `exomonad-harness`; remains historical/excluded |
| `tidepool-web` | `exomonad/web` | rename to `exomonad-web`; remains historical/excluded |
| `jev-integration` | `exomonad/jev-integration` | retain experimental package identity |
| `tidepool-protocol` | `bridge/protocol` | retain transitional package identity |
| `tidepool-mcp` | `bridge/mcp` | retain transitional package identity |
| `tidepool-handlers` | `bridge/handlers` | retain transitional package identity |
| `tidepool-atomic-write` | `bridge/atomic-write` | retain transitional package identity |
| `tidepool-testing` | `bridge/testing` | retain transitional package identity |
| `tidepool-test-data` | `bridge/test-data` | retain transitional package identity |
| `haskell` | `bridge/haskell` | retain intact until compiler and actor libraries can be separated cleanly |
| current `tidepool` facade crate | `bridge/facade` | retain `tidepool` package while mixed; split is deferred |

## Product source and workspace names

| Current path/name | Destination/name |
|---|---|
| `.shoal` tracked workspace | `.exomonad` |
| `.exo` | remove; no compatibility or migration |
| `examples/shoal-workspace` | `exomonad/examples/workspace` |
| `prompts/shoal` | `exomonad/prompts` |
| `harness-dogfooding` | `exomonad/harness-dogfooding` |
| `.agents/skills/shoal-*` | `.agents/skills/exomonad-*`, linked to the Exomonad workspace skills |
| Shoal CLI, identifiers, modules, variables and outputs | Exomonad equivalents |

The vendored `codex-shoal-protocol` package retains its upstream identity. Local
aliases and product-facing references use Exomonad terminology.

## Root ownership

The Cargo workspace and lockfile, Nix files, `justfile`, toolchain pins,
repository-wide contributor guidance, license/publishing files, `vendor`, and
shared build/verification scripts remain at the root. Product-specific scripts,
documentation, examples, prompts, plans, experiments and fixtures move below
their product root. Mixed documentation remains at the root until it can be
split without losing repository-wide context.

Ignored `.shoal`, `.tidepool`, build, compiler and run artifacts are preserved.
The user explicitly authorized removing `.exo` and its ignored runtime data.
