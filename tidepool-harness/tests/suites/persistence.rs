#[path = "../support/mod.rs"]
mod support;

// Each module remains a separate source file; nextest isolates each test process.
#[path = "../persistence_migration_corpus.rs"]
mod persistence_migration_corpus;
#[path = "../selfharness_fn_finalize_spike.rs"]
mod selfharness_fn_finalize_spike;
#[path = "../selfharness_persistence.rs"]
mod selfharness_persistence;
