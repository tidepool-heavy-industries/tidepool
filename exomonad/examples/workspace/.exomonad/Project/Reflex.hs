{-# LANGUAGE OverloadedStrings #-}

-- The reflex table as typed data: the precedence-ordered list this project
-- uses to classify compiler, lint and test output without spending a model
-- turn. Ported entry for entry from .exomonad/reflex-table.json (v1,
-- 2026-09-17). Pure: no effects, no I/O, no Jev. `reflexFor` is the entry
-- point -- exit 0 is green before any matcher is consulted, and output that
-- matches nothing at all returns Nothing, which is the caller's signal to
-- hand `jevClasses` to Jev.
module Project.Reflex
  ( Section (..)
  , NextStep (..)
  , Matcher (..)
  , Reflex (..)
  , reflexTable
  , classify
  , reflexFor
  , matches
  , green
  , jevClasses
  , tableVocabulary
  ) where

import Data.Text (Text)
import qualified Data.Text as Text

-- Sections in precedence order. A rustfmt diff wins outright; environment
-- markers precede the code table because they explain the codes that follow.
data Section
  = Rustfmt
  | Environment
  | Rustc
  | RustcLint
  | Clippy
  | Ghc
  | Parse
  | TestRunner
  | Green
  deriving (Show, Eq)

-- The next-step vocabulary of the table, verbatim.
data NextStep
  = RunFmt      -- ^ run `cargo fmt` and rebuild
  | CargoFix    -- ^ apply machine-applicable suggestions, then re-run; on
                --   persistence fall through to LlmPatch
  | Rerun       -- ^ rerun once: locks, timeouts, network, build-script I/O
  | LlmPatch    -- ^ a model reads the code and edits
  | Escalate    -- ^ a person or reasoning model decides
  | Proceed     -- ^ nothing to do; the command was green
  deriving (Show, Eq)

-- Matchers are literal substrings. `AnyOf` is the table's `|` alternation;
-- `AllOf` is its two `.*` wildcards, each part matched independently.
data Matcher
  = Has Text
  | AnyOf [Matcher]
  | AllOf [Matcher]
  deriving (Show, Eq)

matches :: Matcher -> Text -> Bool
matches (Has needle) haystack = needle `Text.isInfixOf` haystack
matches (AnyOf alternatives) haystack = any (`matches` haystack) alternatives
matches (AllOf required) haystack = all (`matches` haystack) required

-- A lint prints hyphenated after `-D` and underscored in the lint name; the
-- table's note says match either form, so the matcher carries both.
lintName :: Text -> Matcher
lintName hyphenated = AnyOf [Has hyphenated, Has (Text.replace "-" "_" hyphenated)]

data Reflex = Reflex
  { reflexClass :: Text
  , reflexNext :: NextStep
  , reflexSection :: Section
  , reflexMatcher :: Matcher
  , reflexNote :: Maybe Text
  }

-- One line, because a ledger row holds it. The matcher and the note stay
-- reachable through the fields when a wake notice needs to quote them.
instance Show Reflex where
  show reflex = Text.unpack (reflexClass reflex)
    ++ " -> " ++ show (reflexNext reflex)
    ++ " (" ++ show (reflexSection reflex) ++ ")"

-- Exit 0 is green. This is the first rule and it is not a matcher: no
-- classification runs against the output of a command that succeeded.
green :: Reflex
green = Reflex "green" Proceed Green (Has "") (Just "exit 0 is green; the output is not classified")

-- | The whole reflex: the exit code decides first, then the table in order.
-- Nothing means no entry matched -- hand `jevClasses` to Jev.
reflexFor :: Int -> Text -> Maybe Reflex
reflexFor 0 _ = Just green
reflexFor _ output = classify output

-- | The table in precedence order: the first entry whose matcher hits wins.
classify :: Text -> Maybe Reflex
classify output = case [entry | entry <- reflexTable, matches (reflexMatcher entry) output] of
  entry : _ -> Just entry
  [] -> Nothing

reflexTable :: [Reflex]
reflexTable =
  [ Reflex "format_diff" RunFmt Rustfmt (Has "Diff in ") (Just "Precedence 1: a rustfmt diff is decided before any other marker is consulted.")
  , Reflex "build_lock" Rerun Environment (Has "Blocking waiting for file lock") Nothing
  , Reflex "build_script_failed" Rerun Environment (Has "failed to run custom build command") (Just "second failure -> escalate")
  , Reflex "link_failed" Escalate Environment (AnyOf [Has "linking with `cc` failed", Has "linker `cc` not found"]) Nothing
  , Reflex "network" Rerun Environment (AnyOf [Has "Connection refused", Has "Could not resolve host", Has "network failure", Has "failed to fetch"]) Nothing
  , Reflex "disk_full" Escalate Environment (Has "No space left on device") Nothing
  , Reflex "toolchain_missing" Escalate Environment (AnyOf [Has "command not found", AllOf [Has "No such file or directory: ", AnyOf [Has "cargo", Has "ghc", Has "cabal", Has "nix"]]]) Nothing
  , Reflex "dependency_resolution" Escalate Environment (AnyOf [Has "failed to select a version", Has "Could not resolve dependencies", AllOf [Has "could not find", Has "in registry"]]) Nothing
  , Reflex "type_mismatch" LlmPatch Rustc (Has "E0308") Nothing
  , Reflex "arity_mismatch" LlmPatch Rustc (Has "E0061") Nothing
  , Reflex "missing_field" LlmPatch Rustc (Has "E0063") Nothing
  , Reflex "unknown_field" LlmPatch Rustc (Has "E0560") Nothing
  , Reflex "unknown_field" LlmPatch Rustc (Has "E0609") Nothing
  , Reflex "non_exhaustive_match" LlmPatch Rustc (Has "E0004") Nothing
  , Reflex "trait_bound_unsatisfied" LlmPatch Rustc (Has "E0277") Nothing
  , Reflex "trait_bound_unsatisfied" LlmPatch Rustc (Has "E0369") (Just "binary op on a type without the impl")
  , Reflex "type_annotation_needed" LlmPatch Rustc (Has "E0282") Nothing
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0382") Nothing
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0499") Nothing
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0502") Nothing
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0505") Nothing
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0506") Nothing
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0507") Nothing
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0596") (Just "rustc suggests `mut`; machine-applicable, so try cargo_fix first")
  , Reflex "borrow_or_move" LlmPatch Rustc (Has "E0384") Nothing
  , Reflex "lifetime" LlmPatch Rustc (Has "E0597") Nothing
  , Reflex "lifetime" LlmPatch Rustc (Has "E0621") Nothing
  , Reflex "lifetime" LlmPatch Rustc (Has "E0106") Nothing
  , Reflex "unresolved_name" LlmPatch Rustc (Has "E0425") (Just "inspect the source and this diagnostic before choosing an import or definition repair")
  , Reflex "unresolved_name" LlmPatch Rustc (Has "E0433") (Just "inspect the source and this diagnostic before choosing an import or definition repair")
  , Reflex "unresolved_name" LlmPatch Rustc (Has "E0412") (Just "inspect the source and this diagnostic before choosing an import or definition repair")
  , Reflex "unresolved_import" LlmPatch Rustc (Has "E0432") (Just "the import line itself is wrong; a typo or a missing dependency")
  , Reflex "no_such_method" LlmPatch Rustc (Has "E0599") (Just "inspect the source and this diagnostic before choosing an import or definition repair")
  , Reflex "unresolved_name" LlmPatch Rustc (Has "E0423") Nothing
  , Reflex "visibility" LlmPatch Rustc (Has "E0603") Nothing
  , Reflex "visibility" LlmPatch Rustc (Has "E0616") Nothing
  , Reflex "type_mismatch" LlmPatch Rustc (Has "E0614") Nothing
  , Reflex "orphan_or_coherence" Escalate Rustc (Has "E0117") (Just "a design constraint, not a local edit")
  , Reflex "unstable_feature" Escalate Rustc (Has "E0658") Nothing
  , Reflex "unused_item" CargoFix RustcLint (lintName "unused-imports") (Just "Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "unused_item" CargoFix RustcLint (lintName "unused-mut") (Just "Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "unused_item" LlmPatch RustcLint (lintName "unused-variables") (Just "the underscore suggestion is MaybeIncorrect; deleting may hide a bug Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "unused_item" LlmPatch RustcLint (lintName "unused-assignments") (Just "Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "unused_item" LlmPatch RustcLint (lintName "dead-code") (Just "deleting a function is a judgment Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "unused_item" LlmPatch RustcLint (lintName "unreachable-code") (Just "Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "ignored_result" LlmPatch RustcLint (lintName "unused-must-use") (Just "Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "naming" CargoFix RustcLint (lintName "non-snake-case") (Just "Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "unused_item" CargoFix RustcLint (lintName "unused-features") (Just "Lint names print as `error:` with no E-code under `-D warnings`. Match either the hyphenated form printed after `-D` or the underscored form inside `#[warn(...)]`; normalize to one form before lookup.")
  , Reflex "clippy_lint" CargoFix Clippy (Has "clippy::") (Just "Any `clippy::<lint>` name: run `cargo clippy --fix --allow-dirty`, rebuild, repeat up to 3 times because a fix can surface new lints; if a lint persists, llm_patch. There is no allowlist -- only MachineApplicable suggestions are applied, so the fix tool is the allowlist. Verified applied by fix: clippy::needless_return, clippy::map_clone, clippy::clone_on_copy, clippy::useless_vec, clippy::len_zero. Verified surfaced after fix: clippy::iter_cloned_collect, clippy::const_is_empty.")
  , Reflex "type_mismatch" LlmPatch Ghc (Has "GHC-83865") (Just "also kind mismatch GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "type_mismatch" LlmPatch Ghc (Has "GHC-25897") (Just "rigid type variable or occurs check GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "instance_resolution" LlmPatch Ghc (Has "GHC-39999") (Just "no instance, ambiguous type variable, or a function applied to too few args surfacing as Show (a -> b) GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "instance_resolution" LlmPatch Ghc (Has "GHC-43085") (Just "overlapping instances GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "not_in_scope" LlmPatch Ghc (Has "GHC-88464") (Just "variable not in scope; qualified name or a message saying `Perhaps you want to add ... to the import list` points toward an import, but inspect the source and this diagnostic before choosing an import or definition repair GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "not_in_scope" LlmPatch Ghc (Has "GHC-76037") (Just "inspect the source and this diagnostic before choosing an import or definition repair GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "missing_module" LlmPatch Ghc (Has "GHC-87110") (Just "cabal dependency or exposed-modules change; a build-file edit GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "syntax_error" LlmPatch Ghc (Has "GHC-58481") (Just "GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "duplicate_definition" LlmPatch Ghc (Has "GHC-29916") (Just "GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "arity_mismatch" LlmPatch Ghc (Has "GHC-27346") (Just "constructor pattern arity GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "missing_field" LlmPatch Ghc (Has "GHC-95909") (Just "GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "missing_main" LlmPatch Ghc (Has "GHC-67120") (Just "GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "missing_method" LlmPatch Ghc (Has "GHC-06201") (Just "GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "unused_item" LlmPatch Ghc (Has "GHC-66111") (Just "unused import under -Werror; GHC prints the exact import to remove, a mechanical edit but no fix tool applies it GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "unused_item" LlmPatch Ghc (Has "GHC-40910") (Just "unused local bind GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "naming" LlmPatch Ghc (Has "GHC-63397") (Just "shadowing GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "non_exhaustive_match" LlmPatch Ghc (Has "GHC-62161") (Just "GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "missing_signature" LlmPatch Ghc (Has "GHC-38417") (Just "GHC prints the inferred signature; mechanical but no fix tool GHC codes print as [GHC-NNNNN]. GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.")
  , Reflex "syntax_error" LlmPatch Parse (AnyOf [Has "error: expected", Has "error: unexpected"]) (Just "rustc parse errors carry NO E-code: `error: expected `;`, found keyword `let``. Match `error: expected` / `error: unexpected` before falling through to Jev.")
  , Reflex "test_timed_out" Rerun TestRunner (Has "TIMEOUT [") (Just "second timeout -> escalate nextest and libtest output has no codes; match these literal markers in order and the first match wins.")
  , Reflex "test_crashed" Escalate TestRunner (AnyOf [Has "SIGSEGV", Has "SIGABRT", Has "SIGKILL", Has "signal: "]) (Just "nextest and libtest output has no codes; match these literal markers in order and the first match wins.")
  , Reflex "test_assertion_failed" LlmPatch TestRunner (AnyOf [Has "assertion `left == right` failed", Has "assertion `left != right` failed", Has "assertion failed:"]) (Just "nextest and libtest output has no codes; match these literal markers in order and the first match wins.")
  , Reflex "test_panicked" LlmPatch TestRunner (Has "panicked at") (Just "unwrap on None, index out of bounds, unreachable, explicit panic, or assert!(cond, msg) which prints only msg nextest and libtest output has no codes; match these literal markers in order and the first match wins.")
  , Reflex "test_failed_uncategorized" LlmPatch TestRunner (AnyOf [Has "FAIL [", Has "test result: FAILED"]) (Just "nextest and libtest output has no codes; match these literal markers in order and the first match wins.")
  ]

-- | The final fallback: output with no code, no lint name, no test marker
-- and no environment marker. These eleven criteria are what a Jev choice
-- offers; the caller builds the alternatives from this list.
jevClasses :: [(Text, Text)]
jevClasses =
  [ ("syntax_error", "The compiler could not parse the source: `expected ... found ...`, `unexpected token`, `parse error on input`. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("type_mismatch", "The compiler accepted the syntax but a value's type does not match what its position requires, including wrong argument counts and missing or unknown fields. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("name_resolution", "A name, module, type, trait, or method could not be found in scope. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("ownership_or_lifetime", "A borrow, move, mutability, or lifetime rule was violated. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("instance_or_trait", "A required instance or trait implementation is missing, ambiguous, or overlapping. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("unused_or_style_lint", "A warning promoted to error about an unused item, naming, or style; the code would otherwise compile. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("test_failure", "The program compiled and a test reported a failure, panic, or assertion. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("test_hang_or_crash", "A test timed out or the process died from a signal. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("build_environment", "Tooling, network, disk, locks, build scripts, linking, or dependency resolution failed; not a source-code error. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("multiple_unrelated", "Several errors of different kinds whose first one is not clearly the cause. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  , ("other", "None of the above. Final fallback: no code, no lint name, no test marker, no environment marker. Hand these eleven criteria to Jev as the Choice map; Jev selects the class and a next_step from the vocabulary in the $vocabulary entry.")
  ]

-- | The table's own description of itself, for a wake notice that has to
-- say where a classification came from.
tableVocabulary :: Text
tableVocabulary = "Reflex table v1, 2026-09-17, flattened from tidepool-jev plans/jev/reflex_table.json. Entries are in precedence order: take the FIRST entry whose matcher hits the output. Sections in order: rustfmt (a rustfmt diff wins outright), environment (matched anywhere; these precede the code table because they explain the codes that follow them), rustc / rustc_lint / clippy / ghc (an error code or lint name in the FIRST error), parse (rustc parse errors carry no code; match them before falling through to Jev), test_runner (nextest and libtest output has no codes; first literal marker wins), and jev (output with no code, no lint name, no test marker and no environment marker -- hand the Choice criteria map below to Jev, which picks both a class and a next_step). next_step vocabulary: cargo_fix = apply machine-applicable suggestions (`cargo fix --allow-dirty` or `cargo clippy --fix --allow-dirty`), then re-run; if the diagnostic persists, fall through to llm_patch. run_fmt = run `cargo fmt` and rebuild. rerun = rerun the same command once; only for locks, timeouts, network, or a build script that failed on I/O. llm_patch = a model reads the code and edits, including any deterministic panic or assertion in code under test -- this is also where a name-resolution diagnostic lands, since the code alone does not establish that adding an import is the repair. escalate = a person or reasoning model decides; the fix changes a design, a dependency, or the environment. `verified` means the code was reproduced by compiling with rustc 1.93.0 / GHC 9.12.2."
