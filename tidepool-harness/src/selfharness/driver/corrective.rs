//! Corrective-retry text assembly and the compile-hint machinery a stuck
//! model round feeds back on: the fork-child failure corrective, the
//! decl-plane "types in scope" hint (and its unimported-`fork` detector),
//! and the answerer's system-framing suffix.

use super::display_ty;
use super::SelfHarnessDriver;
use super::*;
use crate::engine::{self, InvocationExit};
use crate::tree::NodeId;

/// The corrective when a fork child ends in `InvocationExit` — round
/// exhaustion, a non-answer ending, or its own provider call failing —
/// rather than finalizing: what happened, what survives, and the one
/// useful next step, mirroring [`fork_budget_refusal`]'s shape. Plain
/// composed-industry-terms language only (docs/GLOSSARY.md's prompt
/// rules): `path` names the child by its derived GUI/tree path, never an
/// internal identifier like `InvocationExit` or a constructor name.
pub(crate) fn fork_child_failure_corrective(
    path: &str,
    exit: &InvocationExit,
    ty_label: &str,
) -> String {
    let ty_disp = display_ty(ty_label);
    let what = match exit {
        InvocationExit::RoundsExhausted(_) => {
            "exhausted its model rounds without finalizing an answer"
        }
        InvocationExit::NotFinalized(_) => "ended its session without finalizing an answer",
        InvocationExit::Cancelled(_) => "was cancelled before it could finalize an answer",
        InvocationExit::RuntimeFailure(_) => "hit a runtime failure in its own session",
    };
    format!(
        "The forked child at {path} {what}. Its result is lost, and this block was \
         ABORTED (top-level declarations from earlier rounds persist; the aborted \
         block's bindings are lost). You can re-fork with an adjusted brief, or \
         proceed without that child's result: evaluate `finalize @{ty_disp} value`."
    )
}

/// The narrow answerer instruction appended after `render`'s output to form
/// the per-loop answerer session's system message. Scoped to the answerer's
/// surface — `askUser`, `fork`/`forkAll`, `finalize` — not the full eval
/// surface [`crate::engine::SYSTEM_FRAMING`] advertises. This is
/// belt-and-braces, not the enforcement mechanism: the scoped stack
/// ([`typed_request_agent_decls`]) is what makes any verb this framing omits fail to
/// compile.
///
/// The per-verb signatures/examples are NOT hand-narrated here: they fold
/// over `decls` — the answerer's ACTUAL configured compiling row
/// ([`Harness::cfg`]'s own `decls`, which is [`typed_request_agent_decls`]
/// widened to [`typed_request_agent_decls_with_delegate`] on the delegating
/// path) — via [`engine::available_effects_section`] — the same
/// [`tidepool_mcp::EffectDecl::prompt_card`]/`description` single source the
/// eval tool description is assembled from — so a row with a different
/// effect set gets a correspondingly different cheatsheet, sent ONCE per
/// loop in the system framing rather than re-narrated every hole. Passing
/// the row explicitly (rather than re-deriving the plain roster in here)
/// is what keeps this framing from omitting a delegating window's own
/// widened row.
pub(crate) fn typed_request_agent_framing_suffix(
    decls: &[tidepool_mcp::EffectDecl],
    fork_budget: u32,
    fork_subtree_cap: u32,
) -> String {
    format!(
        "---\n\
         You are the answering agent for a self-iterating harness loop. The system \
         context above is your working brief (it is re-rendered from the loop's durable \
         State each loop). Each request below asks you for ONE typed value.\n\
         \n\
         Your runnable output is fenced ```haskell blocks: every block in your reply \
         runs, in order, as one sequence — later blocks see earlier blocks' \
         declarations and bindings, so a `data` type declared in one block is usable \
         by `askUser`/`finalize` in the next block of the SAME reply. A value you \
         bind with `x <- …` persists into your NEXT round like GHCi, so you can \
         branch on it.\n\
         \n\
         THIS IS A MULTI-ROUND SESSION, NOT A ONE-SHOT. You have up to {} model \
         rounds (a reminder arrives at round {}), and the EXPECTED shape of a \
         non-trivial request is several of them: orient and define, fork a batch \
         of sub-answerers, read what came back, fork the next batch (or delegate \
         follow-up work) from what you learned, consult the operator where their \
         steer would genuinely change your answer — and only then finalize. A \
         one-round finalize on a question that deserved exploration is an \
         under-served request; keep going while each round is still improving \
         the answer, and finalize the moment one isn't. Rounds \
         accumulate: bindings and `let` helpers from earlier rounds stay in scope. \
         A block that is ONLY top-level declarations (type signatures, function \
         definitions, data types) persists beyond this agent session — for your \
         own later rounds, and for every session forked BENEATH you \
         (ancestry-scoped: descendants inherit your declarations, siblings never \
         do; the protocol above states the rule). The root session's \
         declarations persist for every later loop iteration: your growing \
         library. Define what you will want again.\n\
         \n\
         THE OPERATOR CANNOT INITIATE: they see your notes and the forms you \
         present, and between loop iterations they may attach a message that \
         arrives in your framing. If you want their input NOW, present a form \
         (`askUser`/`choose`); their silence during your session is structural, \
         not meaningful.\n\
         \n\
         {}\n\
         \n\
         Your block is a PROGRAM, not a single question: sequence several \
         consultations in one `do` block and branch on earlier answers with \
         ordinary `case`/`if` — each runs without another model round. Plan \
         the whole consultation up front when the branches are predictable; \
         end the round without finalizing only when an answer genuinely needs \
         fresh judgment. Bind results, then `finalize`.\n\
         \n\
         CONCURRENT DELEGATION: `Tidepool.Async` is the `Control.Concurrent.Async` \
         surface (`async`/`wait`/`waitCatch`/`waitEither`/`waitBoth`/`waitAny`/\
         `race`/`concurrently`/`mapConcurrently`) — your instincts for it apply. \
         It composes with `fork`: `async (fork @T brief)` parks the fork in a \
         green thread, so several forks can be outstanding before the first \
         `wait`:\n\
         \n\
         ```haskell\n\
         import Tidepool.Answerer.Fork (fork)\n\
         \n\
         do\n\
         \x20\x20ha <- async (fork @Plan \"design the schema\")\n\
         \x20\x20hb <- async (fork @Plan \"design the API\")\n\
         \x20\x20(a, b) <- waitBoth ha hb\n\
         \x20\x20finalize @Plan (mergePlans a b)\n\
         ```\n\
         \n\
         Spawn and wait in the SAME block — threads do not survive their block, \
         though their WAITED results (bound with `<-`) do. Batches compose two \
         ways: within one block, fork a batch, wait for it, fold the results in \
         ordinary Haskell, and fork the next batch from what you computed; or \
         one batch per round, ending the round after the waits so YOUR OWN \
         judgment (not just dataflow) shapes the next batch's briefs from the \
         bound results. This session may spawn at most {} fork children in total \
         (`fork` costs 1, `forkAll` its list length; direct and async forks draw \
         on the same pool), and the WHOLE tree of sessions under one request \
         shares a descendant budget of {} across all depths — so budget your \
         fan where the question genuinely splits, and prefer briefs a child can \
         answer without forking further. One past either budget is refused and \
         the block aborted.\n\
         \n\
         When you have the answer, COMMIT it by evaluating `finalize @T value`. This \
         ends the session and hands the typed value back to the loop. `T` is the type \
         named in the request. Do not call any other effect to answer; `finalize` is \
         how you resolve the request.",
        TYPED_REQUEST_AGENT_MAX_ROUNDS,
        TYPED_REQUEST_AGENT_NUDGE_ROUNDS,
        engine::available_effects_section(decls),
        fork_budget,
        fork_subtree_cap
    )
}

impl SelfHarnessDriver {
    /// The author-facing explanation appended to a compile-error retry when the
    /// pinned answer type is what failed to resolve.
    ///
    /// Pinning `finalize` to the hole's type means the turn cannot compile
    /// unless that type is importable by the answerer — so a harness whose
    /// author types live in the same module as `loop` fails here, every round,
    /// until the round cap. That must not read as a mysterious not-in-scope
    /// loop: say what was imported and what the author has to change.
    /// `node`'s CURRENTLY SET [`AnswerContract`] is the source of truth for
    /// what was actually imported (`Harness::answer_contract` — set by the
    /// same caller that pinned this turn), not a second copy of the same
    /// list threaded down separately.
    pub(crate) fn types_in_scope_hint(
        &self,
        node: NodeId,
        ty: &str,
        error: &str,
    ) -> Option<String> {
        if error.contains("Not in scope") && error.contains(ty) {
            let contract = self.agent.answer_contract(node);
            let imported = match contract.as_ref().map(|c| c.imports.as_slice()) {
                None | Some([]) => "no author modules are importable by this stack".to_string(),
                Some(mods) => format!("this turn imports {}", mods.join(", ")),
            };
            return Some(format!(
                "\n\nNOTE: `{ty}` is not in scope and {imported}. The answering stack \
                 cannot import the module that defines `loop` (its `runLLMTurn` is not \
                 in this effect row), so the harness author must move `{ty}` into a \
                 separate module that `loop`'s module imports."
            ));
        }
        // `fork`/`forkAll` unresolved — same "not in scope" GHC shape as the
        // type-name case above, but naming a VALUE (a plain identifier, not
        // `ty`), so it needs its own check rather than folding into the
        // `contains(ty)` branch above.
        if Self::error_names_unimported_fork(error) {
            return Some(
                "\n\nNOTE: `fork`/`forkAll` come from `Tidepool.Answerer.Fork` — add \
                 `import Tidepool.Answerer.Fork` to this block's imports."
                    .to_string(),
            );
        }
        None
    }

    /// Whether `error` — a GHC "not in scope" diagnostic — names `fork`/
    /// `forkAll` as the missing identifier. Tokenized on non-alphanumerics so
    /// this matches GHC's exact-name diagnostic (`Variable not in scope:
    /// fork`, whatever quoting marks GHC wraps the name in) regardless of
    /// case, without also firing on `forkSited`/`forkAllSited` (the internal
    /// head-swap targets a model should never be naming directly).
    pub(crate) fn error_names_unimported_fork(error: &str) -> bool {
        let lower = error.to_ascii_lowercase();
        lower.contains("not in scope")
            && lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|tok| tok == "fork" || tok == "forkall")
    }
}
