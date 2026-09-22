//! Shared builders for the fenced Haskell snippets a `ReplayProvider`-scripted
//! test hands the harness — the `RunLLMTurn`/`Fork`/`resume` call SYNTAX these
//! suites hardcode as literal strings under test.
//!
//! # Why this exists, and what it does NOT fix
//!
//! `ReplayProvider` (order-keyed record-replay — see its own doc) is already
//! the best available mechanism for `acceptance_fanout.rs`/`golden_path.rs`:
//! `ModelProvider::complete` carries no site/node identity to match on (only
//! `messages` and `max_tokens` ride the wire), and these suites are single-threaded and
//! deterministic, so serving replies strictly in call order is not a
//! text-coupling bug — there is nothing to key on BUT order.
//!
//! The real, observed pain (commit 79cd3adc,
//! `rename(harness): returnControl* -> runLLMTurn* across all layers`, which
//! touched 10 lines in `acceptance_fanout.rs` and 8 in `golden_path.rs` for
//! one mechanical verb rename) is that every scripted reply is a literal
//! Haskell source STRING naming the verb directly — `returnControlFanout`/
//! `runLLMTurnFanout` appears once per call site rather than once, period.
//! These builders centralize the call SYNTAX so a future verb rename is an
//! edit here, not N edits scattered across every scripted reply.
//!
//! **This cannot reach zero-fixture-edits on a verb rename** — unlike a
//! provider matching key, there is no available indirection that lets a
//! rename in the real Haskell API avoid touching test source at all: the
//! scripted reply IS Haskell source under test, and its correctness is
//! inherently coupled to the real verb name. Touching production code to
//! expose the verb name as a Rust constant these builders could read is out
//! of scope (test-side only). What these builders buy is a rename touching
//! ONE definition instead of every call site — genuinely smaller blast
//! radius, not full immunity.

#![allow(dead_code)]

/// Fence a Haskell block: ```` ```haskell\n<block>\n``` ````.
pub fn haskell(block: &str) -> String {
    format!("```haskell\n{block}\n```")
}

/// `resume <expr>` — the one spelling a corrective-retry or a plain fork/
/// fanout child's answer uses to resume a suspended continuation.
pub fn resume_call(expr: &str) -> String {
    haskell(&format!("resume {expr}"))
}

/// `Right <bind> <- runLLMTurnFork @<ty> "<prompt>"` — the ONE call-syntax
/// definition for a plain fork.
pub fn fork_bind(bind: &str, ty: &str, prompt: &str) -> String {
    format!("Right {bind} <- runLLMTurnFork @{ty} \"{prompt}\"")
}

/// `<bind> <- mapM liftEither =<< runLLMTurnFanout @<ty> [<prompts>]` — the
/// ONE call-syntax definition for a fanout over `prompts`.
pub fn fanout_bind(bind: &str, ty: &str, prompts: &[&str]) -> String {
    let list = prompts
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{bind} <- mapM liftEither =<< runLLMTurnFanout @{ty} [{list}]")
}
