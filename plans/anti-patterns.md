# Gemini Worker Anti-Patterns (Base)

These rules apply to ALL workers in the tidepool project. TLs must include this file in every worker's read_first list. Individual specs may add domain-specific anti-patterns.

---

## Code Style

- **No `todo!()` stubs.** Every match arm must be implemented. If you can't implement a variant, return a clear error, not a stub.
- **No escape hatches.** No `Other(String)`, `Raw(Vec<u8>)`, `Unknown(Box<dyn Any>)` variants. If a type doesn't cover your case, that's a spec problem — stop and report it, don't invent a workaround.
- **No stream-of-consciousness comments.** Doc comments only. Comments describe what IS, not your thought process.
- **No unnecessary dependencies.** If a dependency isn't listed in the spec, don't add it. If you need something, use std or what's already in Cargo.toml.

## Type Discipline

- **Use EXACT type signatures from the scaffold/decisions.** Do not rename types, variants, or fields. If the spec says `EvalError::NoMatchingAlt`, that's the name. Not `NoMatch`, not `MissingAlt`.
- **Do not change module structure.** Files listed in the spec are exhaustive. Don't create new modules unless the spec says to.
- **Do not add type parameters, trait bounds, or generics beyond what's specified.** If the spec says `fn eval(expr: &CoreExpr) -> Result<Value, EvalError>`, that's the signature.

## Architecture

- **Do not make architectural decisions.** If something feels like it needs a design choice (new trait, different data structure, alternative algorithm), describe the gap in your PR body. Do not guess. The TL makes architectural decisions.
- **Do not refactor existing code.** Only modify files and functions listed in your spec. If you see something that "should" be cleaned up, leave it alone.
- **Do not over-engineer.** If the spec says "3 files, ~200 lines," that's the scope. Don't build a framework.

## Testing

- **Every test the spec lists must exist and pass.** Don't skip tests. Don't mark them `#[ignore]`.
- **Test names must describe the property being tested**, not the implementation. `test_lambda_identity` not `test_eval_case_3`.

## Completion

- **When done, run the verify command from your spec.** If it fails, fix the issue. Do not report success with failing tests.
- **Call `notify_parent` with status="success" only when verify passes.** If you can't get tests green after reasonable effort, call `notify_parent` with status="failure" and describe what's broken.
