# Tuple-pattern effect binding investigation

Inspected source baseline: `a3e661fe55c053f57e6cea6127d9f4619f1c0cd9`,
incorporated by fast-forward merge. Service's reported rejection is attributed;
the exact original rejected input/error is unavailable. It is not reproduced by
ordinary tuple binding. No parser or prompt change is justified by this evidence.

## Direct live resident probes

These calls used only `pure` and retained local values: no child admission,
project-resource effect, launcher or live-host replacement. Exact successful input:

```haskell
(tupleProbeLeft, tupleProbeRight) <- pure (1 :: Int, 2 :: Int)
inspectFull (tupleProbeLeft, tupleProbeRight)
```

Result: `bound tupleProbeLeft, tupleProbeRight`, `(1,2)`.

```haskell
:{
(tupleBlockLeft, tupleBlockRight) <-
  (,) <$> pure (3 :: Int) <*> pure (4 :: Int)
:}
tupleWhole <- (,) <$> pure (5 :: Int) <*> pure (6 :: Int)
let (tupleWholeLeft, tupleWholeRight) = tupleWhole
inspectFull (tupleBlockLeft, tupleBlockRight, tupleWholeLeft, tupleWholeRight)
```

Result: tuple binding, simple effect binding and pure destructuring all committed;
final output `(3,4,5,6)`. These demonstrate supported forms, not reproduction of
service's unknown expression/type problem.

Deliberately incomplete negative control (one line, no effect body):

```haskell
(tupleIncompleteLeft, tupleIncompleteRight) <-
```

Exact diagnostic:

```
<input unit 1>:1:45-47: error:
    parse error on input `<-'
  |
1 |  __value = __tidepoolPureWorkbenchValue $ (tupleIncompleteLeft, tupleIncompleteRight) <-
  |                                                                                       ^^
```

This local rejection is expected: outside `:{` / `:}`, each nonblank line is an
input unit. It is only a possible shape-sensitive explanation, NOT an attribution
of service's failure. No effects were submitted and no launch retry was made.

## Owning source and existing coverage

- `tidepool-runtime/src/session/workbench.rs::classify_workbench_item` leaves
  non-keyword Haskell to GHC; no simple-name restriction exists here.
- `haskell/src/Tidepool/Binders.hs::classifyTurn` prioritizes parsed binding
  statements and uses GHC `collectLStmtBinders`; tuple patterns are not special
  rejected syntax.
- `tidepool-actor/src/resident_workbench.rs` maps multiple binders to
  `run_projected_bind_with_sites`, rather than accepting only a single binder.
- `tidepool-runtime/src/session/turn.rs` already includes
  `multi_binder_tuple_bind` (`(a, b) <- pure (1, 2)`) and `discard_tuple_bind` in
  `turn_classification_corpus_old_and_new_path_agree`. Inspected, not rerun.
- `prompts/shoal/docs/workbench.md` explicitly documents multiline delimiters in
  its first two lines. Existing tuple examples agree with the observed API.

Live probes executed on the existing resident host/compiler, whose full build
identity was not established by checking out this source baseline. Thus the
source inspection at a3e661fe and live behavior are separate evidence, not a claim
that the live binary was rebuilt from a3e661fe. No test target changed, no native
battery ran, and no source fix or new regression was fabricated for a missing
reproduction. `git diff --check` passed for this report.

## Decision and context-use finding

Ordinary tuple-pattern effect binding is supported; an intentional general ban
or clear parser deficiency was not found. Classifying the historical rejection
as a parser bug versus call-specific type/layout error requires its exact input
and error, including delimiter boundaries. Preserve the successful workaround,
but do not rewrite the shared guide to forbid supported tuple patterns. If the
original receipt becomes available, use it to construct a pure reproduction
before any launch retry.

Qualitative cost: three small live calls plus targeted owner/fixture inspection
resolved the general syntax question. The missing original receipt prevents
specific diagnosis and creates avoidable speculative investigation. No quantified
context, token or cache saving is claimed.
