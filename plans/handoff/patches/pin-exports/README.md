Shoal cell-pin export leaks: scratch drafts (not compiled or run)
=================================================================

Base: engine/stg-production-cutover working tree with the uncommitted
Record.hs re-index (`Message api`). Each patch passes `git apply --check`
against that tree on its own and in order.

  01-shoal-reexports.patch                  Shoal exports `Reply` (abstract) and
                                            `AgentStopControlOutcome (..)`
  02-ghcpipeline-cell-pin-probe-helpers.patch
                                            exports renderCellPinType; adds
                                            unresolvedCellPinNames and
                                            cellPinCandidateTypes (the pin
                                            renderer's qualification is shared,
                                            not copied)
  03-cell-pin-surface-wiring.patch          cabal test-suite stanza; actor_host
                                            SHOAL_CELL_IMPORTS const (the
                                            production workbench and the test
                                            now read one list); registers the
                                            test module
  04-cell-pin-surface-new-files.patch       haskell/test-cell-pin-surface/Main.hs,
                                            tidepool/src/actor_host/cell_pin_surface_tests.rs

Evidence
--------
  iface3/                   `ghc --show-iface` dumps of build-products d5aca08...
                            (built 08:39, after the 08:24 Record.hs re-index; it
                            shows `newtype Message api result`)
  scan_fixed.py             the prior scan.py plus one fix. `--show-iface` prints an
                            export whose parent is exported as `T|{C ...}`, and
                            scan.py kept the `|`, so it missed e.g. Tidepool.Effects'
                            export of AgentStopControlOutcome
  scan_pin.py               pin-relevant filter. Statement binders are
                            monomorphic, so only a value's body after its
                            context, and the fields of exported constructors,
                            can reach a pin. Synonym RHSs, class bodies and
                            contexts are ignored.
  shoal-pin-relevant.tsv    production Shoal cell scope (Shoal, R, Cmd, Command,
                            Actor, Prelude, Tidepool.Effects)
  shoal-only-pin-relevant.tsv  scope = `import Tidepool.Actors.Shoal` only
  eval-pin-relevant.tsv     eval scope (Prelude, Tidepool.Effects, Aeson)

Findings after the re-index
---------------------------
* The Message/Schema/Rep leak is gone. definition/sender/LocalEffects now name
  `Message api`, which Record exports. Workbench cells render it as `R.Message Api`.
  In the fresh ifaces `Schema` appears only in the contexts of `self` and
  `client` (`Rep (api Self) ~ Fields (Schema api) Self`). A binder
  instantiates that context away, so a pin never shows it.
* Still leaking, production scope: `acknowledgeCancellation :: Reply result -> ...`
  names Tidepool.Agent.Reply.Internal.Reply, which Shoal does not export.
* AgentStopControlOutcome (CleanupStepReceipt's CleanupStoppedActor field) is
  masked in the root workbench, because the preamble does
  `import Tidepool.Effects hiding (...)` and Tidepool.Effects exports it. It still
  leaks for any module that imports only Shoal.
* Also masked only by the workbench's extra imports (Shoal-only scope leaks):
    definition   -> Tidepool.Actor.Internal.EffectProfile, Tidepool.Actor.Record.Message
    finish, forwardingExit -> Tidepool.Actor.ActorExit
    lifecycle    -> Tidepool.Actor.Source.ActorLifecycle
    WorktreeError(..) -> Tidepool.Effects.Core.GitFailureReceipt
  Not patched; decide whether Shoal must stand alone.
* RootProtocol (ShoalDriver): not pin-relevant. rootDriver's body names the
  exported synonym RootEffects, and stabilizeEffectRows expands only synonyms
  whose expansion contains Eff.
* Eval surface: `sleep`/`Sleep` name Tidepool.Duration.Duration, which neither
  Tidepool.Prelude nor Tidepool.Effects exports. GFromJSON/GToJSON appear only
  in class bodies (not pin-relevant).

Name clashes for 01
-------------------
Across all 93 dumped modules only Tidepool.Actor.Record and
Tidepool.Agent.Reply(.Internal) export an occurrence named `Reply`. Shoal imports
Record with an explicit list that omits Record's `Reply`, so Shoal's `Reply` is
unambiguous. Workbench cells import Shoal unqualified and Record as `R`, so
`Reply` means the agent reply handle and `R.Reply` the protocol marker. Neither
spelling changes meaning, and before this change no cell could write an
unqualified `Reply`.
Residual risk: a cell or session declaration that defines its own `Reply`
and uses it unqualified becomes ambiguous. No tracked .hs file does.
AgentStopControlOutcome's constructors (AgentStoppedNow, AgentStopAlreadyStopped,
AgentStopUnavailable, AgentStopUnauthorized, AgentStopFailed) do not collide
with StopOutcome's (StoppedNow, AlreadyStopped, StopUnavailable, StopUnauthorized,
StopFailed). It is the same entity Tidepool.Effects exports, so importing both is
fine.

Regression test design
----------------------
Why not a cell-level probe: pins are harvested only for `<-` statement binders
(Binders.renderCellCheckSource adds `__tidepool_cell_pin_*` aliases for KBind
items only). Binding a constrained export such as `ack <- pure
acknowledgeCancellation` fails with an ambiguous `Member Replies effs`, so a
cell cannot probe the surface generically.

Probe: the GHC-API check in Haskell, reusing the production renderer.
  1. Compile a header module holding the production cell pragmas and imports
     through `runPipeline`, as the fidelity harness does.
  2. Build the pin NamePprCtx from `prTargetRdrEnv` with the same
     `mkNamePprCtx (PromTickCtx True True)` that capturedCellBinderPins uses.
  3. For every `modInfoExports` entry of each audited module, take
     `cellPinCandidateTypes` (value body after context, constructor fields,
     with stabilizeEffectRows). Report each tycon/class/promoted-con name
     where `queryQualifyName` answers NameNotInScope1/2. That is exactly when
     renderCellPinType prints an unimportable module-qualified name.
Home: a new cabal test-suite, `cell-pin-surface-test`. Its input comes from Rust
because Shoal depends on the generated Tidepool.Effects(.Core), which only
tidepool_mcp::ensure_effects_module materializes; a pure cabal suite cannot
build Shoal. The driver is an in-crate test in tidepool/src/actor_host, which
builds the header from the production consts (shoal_effect_declarations,
SHOAL_REPLACED_EFFECT_NAMES, DRIVER_MODULE, SHOAL_CELL_IMPORTS). It asserts the
probe prints nothing.
Run (draft):
  (cd haskell && cabal build cell-pin-surface-test)
  TIDEPOOL_CELL_PIN_SURFACE_PROBE=$(cd haskell && cabal list-bin cell-pin-surface-test) \
    just test-lib tidepool 'test(cell_pin_surface_tests)'
Expected: fails on acknowledgeCancellation before 01, passes after.

Known gaps
----------
* The Rust test returns early when the env var is unset, following
  TIDEPOOL_CELL_TEST_EXTRACT in turn.rs, so it is not in `just verify`.
  Gate enforcement needs scripts/verify.sh (not drafted) to build the probe and
  export the variable. The alternative is a typed extractor request in
  tidepool-extract-cmd, per the mechanism index.
* The driver header omits the default quasiquoter and tool imports
  (qualified Tidepool.Command.Tools, qualified Tidepool.Agent.Contract).
  Omitted imports can only add false positives, never hide a leak.
* GHC API details are unverified against 9.12: `findModule name Nothing`
  (matches Introspection.hs), `Session`/`reflectGhc`, and the four-field
  `FunTy`. Names under a CastTy kind coercion and kinds are skipped. If a
  rendered pin ever shows an explicit kind, extend `written`.
* An eval-surface driver is not drafted. It belongs in tidepool-mcp: header
  from build_preamble(standard_decls()), audit Tidepool.Prelude and
  Tidepool.Effects, and the Duration finding is expected until that is fixed.
