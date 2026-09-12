# GHC 9.12 matcher matrix

Executed:

```text
bash scripts/dev-shell.sh bash -c \
  'cd haskell && cabal exec -- runghc -XOverloadedStrings -isrc \
   -package=ghc -package=process \
   ../plans/notebook-cells/lookup/probes/MatcherMatrix.hs'
```

Each tuple is `(query,candidate)` followed by
`(tcMatchTy q c, tcMatchTy c q, tcUnifyTy q c)` for the full sigma type and
then its `tcSplitSigmaTy` body:

```text
poly/exact       (T,T,T) (T,T,T)
poly/concrete    (F,F,F) (T,F,T)
wild/concrete    (F,F,F) (T,F,T)
num/num          (T,T,T) (T,T,T)
num/plain        (F,F,F) (T,T,T)
plain/num        (F,F,F) (T,T,T)
rank/exact       (T,T,T) (T,T,T)
rank/monomorphic (F,F,F) (F,F,F)
```

Consequences:

- full-sigma matching cannot provide the intended ordinary compatibility
  results;
- body matching supplies the tested compatibility results;
- body matching alone erases the only evidence about predicates;
- nested foralls remain discriminating;
- `tcUnifyTy` adds no useful distinction in this matrix.

The implemented release rule is exact `eqType` over the full type, otherwise
symmetric body `tcMatchTy` only when both predicate sets are empty. `usable`
names this bounded compatibility search, not Haskell subsumption. It is
intentionally conservative: constrained non-exact matches await real predicate
entailment rather than being mislabeled usable. The nested-forall rows are
observations for these fixtures, not a higher-rank subsumption contract.
