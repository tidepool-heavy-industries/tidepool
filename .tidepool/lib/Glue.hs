{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Semantic-glue verbs: tier-1 LLM judgment fused into deterministic
-- pipelines. Code does what the model shouldn't (iteration, exactness,
-- aggregation); the model does what code can't (semantic judgment);
-- `??` escalation pulls the caller in only on low confidence.
--
-- Born 2026-06-11, the first day of live tier-1 (gpt-4o-mini via the
-- genai consolidation + .tidepool/secrets key dropbox).
module Glue where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

-- | Semantic grep: regex for recall, tier-1 judgment for precision.
-- Each hit is judged against the intent with its matched line as
-- context; returns (relevant hits, dropped count). 3 or fewer hits
-- skip judgment (nothing to filter); at most 60 hits are judged
-- (one model call each — mind the per-eval LLM budget).
grepSift :: Text -> Text -> Text -> M ([(Text, Int, Text)], Int)
grepSift intent rx g = do
  hits <- grepGlob rx g
  if length hits <= 3
    then pure (hits, 0)
    else do
      (keep, dropped) <- sift yn render (take 60 hits)
      pure (keep, length dropped)
  where
    render (f, l, t) =
      "Is this code line plausibly relevant to: \"" <> intent <> "\"?\n"
        <> f <> ":" <> pack (show l) <> "  " <> stake 160 (strip t)

-- | Match an error message against the known-gotcha catalog
-- (compact mirror of CLAUDE.md "Known Limits", audited 2026-06-11).
-- Returns {gotcha, why, suggestion}; gotcha = "none-of-these" when no
-- pattern fits. Low confidence escalates to the caller with the
-- schema riding structurally.
diagnose :: Text -> M Value
diagnose err =
  obj schema ?? (catalog <> "\n\nERROR TO DIAGNOSE:\n" <> stake 2500 err)
  where
    schema = SObj
      [ ("gotcha", SEnum gotchaNames)
      , ("why", SStr)
      , ("suggestion", SStr)
      ]
    gotchaNames =
      [ "read-gmp-ffi"
      , "takeWhile-partial-application"
      , "cycle-unresolved-external"
      , "double-breakOn-case-trap"
      , "non-tail-recursion-overflow"
      , "constructor-tag-mismatch"
      , "jit-thread-crash"
      , "effect-error"
      , "none-of-these"
      ]
    catalog = intercalate "\n"
      [ "Known tidepool JIT gotchas (match the ERROR below to ONE):"
      , "- read-gmp-ffi: COMPILE error 'Unsupported FFI call: ...gmpn...' — `read`/`reads` pull GMP Integer ops. Fix: parseInt/parseDouble from the Prelude."
      , "- takeWhile-partial-application: no error, SILENTLY WRONG results — T.takeWhile/T.dropWhile partially applied. Fix: use the Prelude shadows, never the T. versions point-free."
      , "- cycle-unresolved-external: runtime 'unresolved variable VarId(0x...)' — `cycle` is an unresolved external. Fix: manual recursion."
      , "- double-breakOn-case-trap: 'case trap: tag mismatch' or 'apply_cont_heap: result con_tag ... neither Val nor E' — second T.breakOn on the sdrop of the first's remainder, esp. in cross-module M functions (#313 t11). Fix: inline the shape in one do-block."
      , "- non-tail-recursion-overflow: 'stack overflow (likely infinite list or unbounded recursion)' — non-tail recursion past ~10-20K frames. Fix: make it tail-recursive (TCO is unbounded)."
      , "- constructor-tag-mismatch: SIGILL / 'case trap' with no breakOn involved — a case hit a value shape no branch matches. Usually a compiler bug: report it."
      , "- jit-thread-crash: 'eval thread crashed (likely SIGILL... or SIGSEGV...)' — JIT compiler bug. Check .tidepool/crash.log; report with a minimal repro."
      , "- effect-error: 'effect dispatch error: handler error: ...' — an effect handler failed (missing file, network, abort). Not a JIT bug; fix the call."
      , "- none-of-these: anything else."
      ]
