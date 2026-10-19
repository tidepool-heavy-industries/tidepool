# Research and evidence index

## Exact source and present limitations

Plan base: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`.
Primary research base: `0fd622bf592f8f015a3795d6e52f36a109ec2f12`.
Historical timing source: `20d7ccdcf6ed13a5e0db7ffa89527224a95606ec`.

These are different revisions. The historical profile motivates the work; it
does not establish current performance. No compiler runs or new runtime
qualification were performed while writing this directory.

## Selection defect from the delivery peer

Authoritative handoff:
`/srv/swarm/evidence/inanna/persona-1895-launch-20261019/compiler-failure/HLS-REWRITE-HANDOFF.md`.

Confirmed mechanism: `RequestNativeAvailability` treated canonical interface
equality as sufficient to seed optional compiler roots, even when an explicit
Original selected another body. An unused historical implementation then pulled
its dependency closure into the request. The downstream strict namespace
validator correctly refused the manufactured conflict.

Repair donor `ec25a74a6be0c3249491c2465e2532463e17903d`, integrated equivalent
`d24b5b20b0`. Both focused tests failed on unfixed `30e31e93` and passed on
`509749f192`:

- `declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions`
- `declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts`

The repair distinguishes optional roots from exact dependencies. It does not
remove historical byte custody or relax genuine required-edge conflicts.
One reference oracle shared the broken filtering helper; another older test
expected the incorrect refusal. Preserve intended semantics, not those mistakes.

The stronger hosted selector is
`actor_host::startup_persona_acceptance_tests::persona_with_shared_workspace_keeps_lookup_and_later_cells_usable`
in `exomonad-host`. It was **running, not accepted**, at peer handoff. Evidence:
`/srv/swarm/evidence/inanna/admission-analysis-review-20261018/overnight-70a-strong-persona/`.
Its shared-source identity and startup-derived JSON/Jev support matter. Byte-equal
local copies do not establish the same source/custody history. Earlier hosted
failures also included assertions against the wrong presentation boundary;
typed result, model text and GUI output are distinct evidence.

## Research reports

All local reports are under
`/srv/swarm/evidence/inanna/perf-fork-map-audit-20261019/`.

| Report | Primary use |
| --- | --- |
| `input-offer.md` | P1 retained-value support fold and role/template scans |
| `admission-sealing.md` | P4 validation and P6 output-capture ownership |
| `runtime-consumers.md` | Runtime consumers and pre-P1 work |
| `worker-and-metrics.md` | Actual worker lifetime and timing boundaries |
| `incremental-research/incremental-validity.md` | Negative dependencies, full validation versus contextual reuse |
| `incremental-research/structural-sharing.md` | Physical interface duplication, arenas and package input work |
| `incremental-research/shared-warm-lifecycle.md` | Whole-flow ownership, exact worlds, failures and reload retirement |
| `incremental-research/shared-warm-feasibility.md` | GHC NameCache/HPT/EPS, lazy environments and executable epochs |
| `incremental-research/shared-warm-hls-review.md` | HLS comparison and checked-import experiment |
| `incremental-research/shared-warm-performance.md` | Historical decomposition, limits and sensitivity analysis |
| `incremental-research/shared-warm-performance-analysis.py` | Reproduction of historical arithmetic |
| `incremental-research/shared-warm-performance-data.json` | Retained derived timing data |
| `incremental-research/incremental-options.md` | Why a generic incremental framework is not the first change |
| `incremental-research/query-lifecycle.md` | Compiler operation ownership and request-independent facts |

The local reports contain more detail than a worker needs initially. Read the
assigned leaf and the report relevant to its owning seam; keep the whole-flow
contract visible when changing a shared boundary.

## Historical performance evidence

The measured scripted packaged history took 404.511 seconds: 364.156 in notebook
cells, 39.199 in startup, with the remaining 1.156 seconds elsewhere. It was a
13-phase generic/scripted Persona workload, not the user's authored live Persona.
All 28 physical requests used one already resident worker. The 27 retained queue
records reported zero milliseconds at their recorded resolution; that does not
establish physically zero queue time.

The three sibling host spans offer/admit/seal totalled
121.715/117.791/35.886 seconds. Their specific inner regions
retained-value fold/P4 items/P6 capture were
80.716/84.954/35.249 seconds. Do not add inner spans to enclosing spans.
Removing those three inner regions entirely would bound whole-history speedup
at about 1.99x; removing all worker service would bound it near 1.195x. Neither
is an achievable prediction by itself.

A representative 22.601-second warm cell had 17.179 seconds in those host stages
and a 5.422-second remainder. Illustrative sensitivity, not estimates:

| Change assumed | Resulting cell | Speedup |
| --- | ---: | ---: |
| 80% less host-stage time | 8.858s | 2.55x |
| 90% less host-stage time | 7.140s | 3.17x |
| 90% less host-stage time and half the remainder | 4.429s | 5.10x |

A tenfold warm-cell improvement also needs substantial reduction in remaining
frontend/runtime work. Startup has a different budget. Obtain a current baseline
before claiming that these proportions remain true.

## External primary sources and how they influence this plan

- [HLS rules at the inspected revision](https://github.com/haskell/haskell-language-server/blob/1b4b3c6bdd2bf8d1e1182e2e770f5dea9198db80/ghcide/src/Development/IDE/Core/Rules.hs)
  provide the comparison for concrete query products and selective retention.
- [HLS compilation](https://github.com/haskell/haskell-language-server/blob/1b4b3c6bdd2bf8d1e1182e2e770f5dea9198db80/ghcide/src/Development/IDE/Core/Compile.hs)
  and [session loading](https://github.com/haskell/haskell-language-server/blob/1b4b3c6bdd2bf8d1e1182e2e770f5dea9198db80/ghcide/session-loader/Development/IDE/Session.hs)
  show that compiler-session reuse and module checking remain separate concerns.
- [GHC module typechecking](https://github.com/ghc/ghc/blob/383be28ffdddf65b57b7b111bfc89808b4229ebc/compiler/GHC/Tc/Module.hs)
  owns import processing and interactive/module differences. It is the semantic
  reference for the frontend experiment, not a claim that retaining an HPT
  bypasses that work.
- [GHC NameCache](https://github.com/ghc/ghc/blob/383be28ffdddf65b57b7b111bfc89808b4229ebc/compiler/GHC/Types/Name/Cache.hs)
  and [interface hydration](https://github.com/ghc/ghc/blob/383be28ffdddf65b57b7b111bfc89808b4229ebc/compiler/GHC/IfaceToCore.hs)
  motivate explicit ownership of names and lazy checked environments.
- [Build Systems à la Carte](https://www.microsoft.com/en-us/research/wp-content/uploads/2018/03/build-systems-final.pdf)
  informs the separation of dependency discovery, reuse and scheduling.
- [Shake Before Building](https://ndmitchell.com/downloads/paper-shake_before_building-10_sep_2012.pdf)
  is background for dependency-driven rebuilds, not an adoption requirement.

HLS is an architectural influence, not evidence that its editor state model can
be transplanted into concurrent divergent actor worlds or used as execution
authority. Proposed Tidepool changes are design inferences from these sources
and the inspected production consumers.
