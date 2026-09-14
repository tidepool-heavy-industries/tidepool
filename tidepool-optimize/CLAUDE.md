# tidepool-optimize — Core-to-Core optimization passes

**Charter.** Belongs: optimization passes over `CoreExpr` — beta reduction,
case reduction, dead code elimination, inlining, occurrence analysis, partial
evaluation. Does NOT belong: evaluation (JIT-only, `tidepool-codegen`), the
`CoreExpr` IR itself (`tidepool-repr`).
