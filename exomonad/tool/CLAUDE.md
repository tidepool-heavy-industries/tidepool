# exomonad-tool

Owns transport-neutral raw tool declarations, the admitted `EndpointPlan`
execution algebra, and comparison of active and candidate surfaces through
`surface::compare_surfaces`. Admission consumes external kind, implementation
and scheduling metadata; provider declarations project the admitted operation.
Installed actor endpoints retain this plan with their issuing installation and
compiler row. Runtime callers must not reconstruct an implementation from the
provider projection.

A spec reload must be refused when the declared surface changes; compare the
surface projections, then swap implementation owners when they match. Focused
checks use the counted `just test-lib exomonad-tool` runner.
