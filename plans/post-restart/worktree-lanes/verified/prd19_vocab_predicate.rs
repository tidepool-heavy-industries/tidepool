//! The DISCRIMINATING gate for PRD 19's conditional `(<|>)` hiding under the
//! vocabulary/row split.
//!
//! `(<|>)` is a RepoEvent HELPER, and helper emission is ROW-GATED, so the
//! hiding term must track `in_row || helpers_row_polymorphic` — NOT mere
//! presence in the vocabulary. `vocab_effects` was tried, endorsed, and is
//! wrong: for a vocabulary-only RepoEvent it hides the Prelude's `(<|>)` while
//! emitting no replacement, costing `Alternative` for nothing.
//!
//! `mismatched_vocab_only_repoevent_does_not_hide_the_prelude_alternative` is
//! the gate that DISCRIMINATES between the two predicates — it is the only one
//! here that fails under the `vocab_effects` answer. The matched-pair gate
//! passes under either and is therefore no evidence for choosing.

use tidepool_mcp::EffectDecl;

const EVENT_HELPER: &str = "(<|>) :: Event a -> Event a -> Event a\nl <|> r = mergeEvents l r";

/// A RepoEvent stand-in carrying the `(<|>)` helper, row-CLOSED exactly as the
/// real declaration is.
fn repo_event() -> EffectDecl {
    EffectDecl {
        type_name: "RepoEvent",
        description: "test double",
        constructors: &["RepoEventDrain :: Int -> RepoEvent [Int]"],
        type_defs: &["data Event a = Event Int"],
        helpers: &[EVENT_HELPER],
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: false,
    }
}

fn console() -> EffectDecl {
    EffectDecl {
        type_name: "Console",
        description: "test double",
        constructors: &["Print :: Text -> Console ()"],
        type_defs: &[],
        helpers: &["say :: Text -> M ()\nsay = send . Print"],
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: false,
    }
}

fn gen(row: &[EffectDecl], vocab: &[EffectDecl]) -> String {
    tidepool_mcp::effects_module_source_with_vocab(row, vocab, &tidepool_mcp::RowArgs::default())
}

/// Matched pair: RepoEvent in BOTH. `(<|>)` is emitted, so the Prelude's must
/// be hidden. Passes under either predicate — included for completeness, NOT
/// as evidence for the choice.
#[test]
fn matched_repoevent_row_and_vocab_hides_the_prelude_alternative() {
    let src = gen(&[console(), repo_event()], &[console(), repo_event()]);
    assert!(
        src.contains(EVENT_HELPER),
        "precondition: RepoEvent is in the row, so its (<|>) helper must be emitted"
    );
    assert!(
        src.contains("import Tidepool.Prelude hiding (error, (<|>))"),
        "Event's (<|>) is in scope, so the Prelude's must be hidden or the PRD's own \
         example is an ambiguous occurrence"
    );
}

/// THE DISCRIMINATING GATE. RepoEvent is in the VOCABULARY but not the ROW, and
/// is row-CLOSED — so its helpers, including `(<|>)`, are NOT emitted. Hiding
/// the Prelude's here would remove `Alternative` and put nothing in its place.
///
/// This is the gate that FAILS under the superseded `vocab_effects` predicate.
#[test]
fn mismatched_vocab_only_repoevent_does_not_hide_the_prelude_alternative() {
    let src = gen(&[console()], &[console(), repo_event()]);
    assert!(
        !src.contains(EVENT_HELPER),
        "precondition: a row-closed helper must NOT be emitted for a vocabulary-only effect"
    );
    assert!(
        src.contains("import Tidepool.Prelude hiding (error)\n"),
        "no Event (<|>) is emitted here, so the Prelude's must stay — hiding it would cost \
         Alternative for nothing (this is what keying on vocab_effects gets wrong)"
    );
    assert!(
        !src.contains("hiding (error, (<|>))"),
        "vocabulary presence alone must not trigger the hiding"
    );
}

/// The vocabulary is emitted, so RepoEvent's GADT IS present even when its
/// helpers are not. This pins that the discriminating gate above is testing the
/// HELPER condition specifically, not merely "RepoEvent is absent".
#[test]
fn vocab_only_repoevent_still_emits_its_gadt() {
    let src = gen(&[console()], &[console(), repo_event()]);
    assert!(
        src.contains("data RepoEvent a where"),
        "a vocabulary-only effect must still be NAMEABLE — that is the whole point of the split"
    );
}

/// A row that is not a subset of the vocabulary is a construction error, which
/// is why only ONE mismatched pair exists. Pinned so the hazard space stays
/// documented rather than remembered.
#[test]
#[should_panic(expected = "vocabulary must be a superset")]
fn row_effect_absent_from_the_vocabulary_panics() {
    let _ = gen(&[console(), repo_event()], &[console()]);
}
