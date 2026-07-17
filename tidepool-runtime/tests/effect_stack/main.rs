//! `effect_stack` suite — tests that exercise the MCP effect-stack preamble:
//! effect-returned-list folds, derived prompts across effect boundaries,
//! `showDouble`/pagination through the 10-effect stack, `Text.splitOn` across
//! the freer/MCP surfaces, `Value`-case matching, and the mock-stack lockstep
//! guard. Each former top-level test binary is absorbed here as one submodule;
//! nextest still runs every `#[test]` in its own process.

mod helpers;

mod effect_fold_regression;
mod effect_prompt_sigill;
mod mock_stack_lockstep;
mod show_double_10effect;
mod text_spliton;
mod value_case_match;
