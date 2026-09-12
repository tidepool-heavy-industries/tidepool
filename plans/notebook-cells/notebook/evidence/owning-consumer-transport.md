# Owning-consumer nominal transport

Source: `a92dd7685818ab6412f24a0a5620b0d8f34e5293`.
GHC: 9.12.2.

## Checked observations

`InferenceProbe.hs` now typechecks `ResponseCellProbe.hs` against Tidepool's
actual `Response` type, traverses the post-zonk typechecked AST, and prints:

```text
response :: Response G
response :: Response G
```

`response` is a monadic binder whose result is fixed only by the later
`consume :: Response G -> Int`. Thus the exact response-shaped pin is
harvestable; it is not inferred from rendered API documentation.

Command:

```bash
bash scripts/dev-shell.sh bash -c '
  ghc -package ghc \
    plans/notebook-cells/notebook/evidence/InferenceProbe.hs \
    -o /tmp/inference-probe
  effects="$(find /home/inanna/.cache/tidepool/effects \
    -mindepth 1 -maxdepth 1 -type d -printf "%T@ %p\n" |
    sort -nr | sed -n "1s/^[^ ]* //p")"
  /tmp/inference-probe "$(ghc --print-libdir)" \
    plans/notebook-cells/notebook/evidence/ResponseCellProbe.hs \
    ResponseCellProbe response haskell/lib "$effects"
'
```

The live resident actor consumer then exercised its real `Lib.G`/`Response`
path:

```haskell
data TransportG = OldG Int deriving Show
oldScore (OldG n) = n

oldResponse <- unfold ... (child @TransportG ...)
-- child settled with OldG 17
oldScore oldG
-- 17

data TransportG = NewG Text deriving Show
newScore (NewG value) = T.length value

newResponse <- request @TransportG retainedActor ...
-- retained actor settled with NewG "shadow"
newScore newG
-- 6

oldScore oldG
-- 17

newScore oldG
-- rejected before execution:
-- expected G10.TransportG, actual G9.TransportG
```

The receipt identifies the old and new heads as
`Tidepool.Session.Lib.G9.TransportG` and
`Tidepool.Session.Lib.G10.TransportG`. This proves the existing consumer
preserves exact installed identity through response settlement and shadowing,
and rejects the mismatched consumer. The reverse mismatch,
`oldScore newG`, was also rejected with the same two exact heads. The
new-generation reply was delivered by a typed request to the retained Astra
actor after two unrelated new-actor admissions timed out; it returned
`NewG "shadow"` and `newScore newG` returned `6`.

Cleanup of the old throwaway child was later reported degraded: its native
process/socket/worktree custody remain retained because exact termination was
not established. This does not alter its already-settled typed result, but it is
reported separately and is not described as successful cleanup.

## Result and coordinator obligation

The two halves needed by Stage 1 are individually checked:

1. GHC harvests the exact downstream-fixed `Response G`.
2. The resident consumer executes an explicitly pinned `Response G` against
   the installed head and retains old/new nominal separation.

The automatic join is still absent, and this worker may not edit its
coordinator-owned seam. Therefore dependent notebook fanout remains gated.
The smallest patch obligation for the coordinator is:

```text
CheckedBinderPin
  binder: stable cell item/span + binder
  type syntax
  nominal references:
    ExistingHead(module, incarnation)
    SameCellDecl(export key)
```

GHC owns extraction, complete traversal, relocation, and rendering. Rust
carries the opaque checked pin and maps each `SameCellDecl` only after the
declaration group has installed as the exact `Lib.G<n>` generation. Before
any installation/effect, preflight must reject escaping skolems/metavariables,
unnameable references, incomplete nominal maps, or a pin that cannot be
rechecked. The staged compiler recompiles with this generated pin; its captured
binder type must validate against the relocated checked type modulo
alpha-renaming, including nested types, aliases, kind arguments, and
constraints—not merely top-level nominal heads.

Required coordinator-owned regression:

1. analyze a real cell and harvest `Response G`;
2. nonmutating preflight resolves its complete nominal map;
3. install the cell's `G`;
4. stage without an authored type application;
5. execute/settle and consume it;
6. shadow `G`, repeat, consume both with matching functions, and reject the
   cross-generation consumer before effects.

Until this passes, harvest-and-pin is feasible but transport has not passed the
approved unchanged-contract gate.
