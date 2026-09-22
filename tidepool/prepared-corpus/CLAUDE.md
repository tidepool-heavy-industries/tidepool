# tidepool-prepared-corpus

Owns the compiler-derived prepared-STG corpus runner and its expected outcomes in `fixtures/prepared-corpus-expectations.json`; `just fixtures-check` relies on this crate to validate and execute corpus cases, while its per-item watchdog aborts suspected non-terminating cases with the item name. Focused check: `just fixtures-check`.
