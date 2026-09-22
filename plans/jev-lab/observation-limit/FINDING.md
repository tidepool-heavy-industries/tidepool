# Observation budget and committed work

Status: fixed and retained as regression evidence. Binding and reflection must
keep committed values usable when rendering exceeds the observation budget.
The runtime handles budget exhaustion at `Bind` and `Project` while preserving
the retained value; bare observation still reports the display failure.

`bridge/facade/src/actor_host/observation_budget_tests.rs` covers both an
effectful result that still binds past the budget and reflected history larger
than the budget. This record preserves the defect class that those regressions
protect; raw session transcripts are not needed to understand the contract.
