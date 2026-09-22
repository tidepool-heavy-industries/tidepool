#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../companion_scope_trees.rs"]
mod companion_scope_trees;
#[path = "../selfharness_budget.rs"]
mod selfharness_budget;
#[path = "../selfharness_compaction.rs"]
mod selfharness_compaction;
#[path = "../selfharness_compaction_fixes.rs"]
mod selfharness_compaction_fixes;
#[path = "../selfharness_context_window.rs"]
mod selfharness_context_window;
#[path = "../selfharness_framing.rs"]
mod selfharness_framing;
#[path = "../selfharness_lifecycle.rs"]
mod selfharness_lifecycle;
#[path = "../selfharness_spine.rs"]
mod selfharness_spine;
