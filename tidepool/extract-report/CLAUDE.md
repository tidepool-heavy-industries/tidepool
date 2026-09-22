# tidepool-extract-report

Owns the dependency-light wire types for compiler-worker stdout reports and completed-artifact integrity manifests. It does not own compilation policy, cache layout, publication, diagnostic rendering, or process-status interpretation; those stay with each consumer. Focused check: `cargo test -p tidepool-extract-report`.
