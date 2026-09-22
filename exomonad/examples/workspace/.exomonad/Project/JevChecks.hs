{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- Deterministic checks for the shipped programs' pure decision mechanics.
-- These do not exercise a Jev request: recipe sessions currently install no
-- Jev backend, so model-settled outcomes need a separate scripted seam.
module Project.JevChecks (investigation, review, reflex) where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

import Project.Investigate
import Project.Reflex
import Project.Review (riskCountAt)

investigation :: Member RecipeCheck effects => Eff effects ()
investigation = do
  let nonExhaustive = Text.unlines
        [ "error[E0004]: non-exhaustive patterns: `None` not covered"
        , " --> src/main.rs:136:67"
        , "  |"
        , "error[E0004]: non-exhaustive patterns: `None` not covered"
        , " --> src/main.rs:148:31"
        , "  |"
        , "error[E0004]: non-exhaustive patterns: `None` not covered"
        , " --> src/app.rs:72:9"
        , "  |"
        , "error[E0004]: non-exhaustive patterns: `None` not covered"
        , " --> src/app.rs:96:17"
        , "  |"
        , "error[E0004]: non-exhaustive patterns: `None` not covered"
        , " --> src/app.rs:121:13"
        ]
      groups = groupDiagnostics (splitDiagnostics nonExhaustive)
      arity = splitDiagnostics (Text.unlines
        [ "error[E0061]: this function takes 2 arguments but 1 argument was supplied"
        , " --> src/main.rs:83:9"
        , "  |"
        , "note: function defined here"
        , " --> src/store.rs:59:1"
        ])
      lint = splitDiagnostics (Text.unlines
        [ "error: field assignment outside of initializer for an instance created with Default::default()"
        , " --> src/widget.rs:14:9"
        , "  |"
        , "note: this diagnostic is in a test body"
        , " --> src/widget.rs:31:1"
        ])
  check "five repeated non-exhaustive diagnostics group under one cause"
    (case groups of
      [group] -> groupSize group == 5 && groupCode group == "E0004"
      _ -> False)
  check "wrong-arity input retains the definition as a labeled secondary site"
    (case arity of
      [diagnostic] ->
        diagCode diagnostic == "E0061"
          && ("note: function defined here", "src/store.rs:59:1") `elem` diagSites diagnostic
      _ -> False)
  check "lint input retains the test-body note instead of requiring a symbol"
    (case lint of
      [diagnostic] ->
        diagCode diagnostic == ""
          && ("note: this diagnostic is in a test body", "src/widget.rs:31:1") `elem` diagSites diagnostic
      _ -> False)
  check "the ambiguity fixture stays inside the policy's undecided band"
    (mustChangeUnclear defaultInvestigationPolicy < 0.51
      && 0.51 < mustChangeFloor defaultInvestigationPolicy)

review :: Member RecipeCheck effects => Eff effects ()
review = do
  check "review risk count excludes scores at or below its Noul floor"
    (riskCountAt 0.5 [("high", 0.8), ("at-floor", 0.5), ("below", 0.2)] == 1)
  check "review risk count handles an empty judgment set"
    (riskCountAt 0.5 [] == 0)

reflex :: Member RecipeCheck effects => Eff effects ()
reflex = do
  let compilerFailure = "error[E0004]: non-exhaustive patterns: `None` not covered"
      known = reflexFor 101 compilerFailure
      successful = reflexFor 0 compilerFailure
      unknown = reflexFor 101 "a captured command failed without a known marker"
  check "known E0004 failure selects the non-exhaustive repair path"
    (fmap (\entry -> (reflexClass entry, reflexNext entry)) known
      == Just ("non_exhaustive_match", LlmPatch))
  check "exit zero wins over diagnostic-looking output"
    (fmap (\entry -> (reflexClass entry, reflexNext entry)) successful
      == Just ("green", Proceed))
  check "unknown failure remains unclassified for the Jev fallback"
    (unknown == Nothing)
