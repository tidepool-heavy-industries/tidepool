# Tidepool — Dev Agent Instructions

You are a leaf agent. You implement exactly one task. Your task spec is in your spawn prompt or in a file it points you to.

---

## Your Workflow

1. **Read your task spec completely** before writing any code
2. **Read the files listed** in the spec's READ FIRST section
3. **Implement exactly what the spec says** — no more, no less
4. **Run the verify commands** from the spec. Fix until they pass.
5. **Commit and file a PR** targeting your parent branch (the branch you were spawned from)
6. **Wait for Copilot review comments.** They will appear in your terminal. Read them carefully, fix the issues, push.
7. **Repeat step 6** until Copilot is satisfied (no new comments on your latest push)
8. **Call `notify_parent`** with status `success` and a summary of what you built

If you cannot complete the task after 3+ Copilot review rounds, call `notify_parent` with status `failure` and explain what's blocking you.

---

## Rules

### Follow the Spec Exactly

Your task spec contains exact type signatures, exact file paths, exact variant names. Use them verbatim. Do not rename, simplify, reorganize, or "improve" anything. If the spec says `IntAdd`, don't write `Add`. If the spec says `frame.rs`, don't create `types.rs`.

### DO NOT Section Comes First

Your spec has a DO NOT / ANTI-PATTERNS / CRITICAL RULES section at the top. Read it before anything else. Every rule there exists because a previous agent made that exact mistake.

### Zero Creativity on Architecture

You do not make architectural decisions. You do not choose dependencies. You do not decide module structure. You do not add features the spec didn't ask for. If the spec doesn't mention it, you don't do it.

If something seems missing from the spec (e.g., a type isn't defined, a dependency seems needed), describe the gap in your PR body. Do not guess.

### No Escape Hatches

Never write `todo!()`, `unimplemented!()`, `unreachable!()`, `panic!()` (except in tests), `Raw(String)` variants, `Other(Box<dyn Any>)`, or similar. If you can't implement something, describe the gap in your PR body rather than stubbing it out.

### No Unnecessary Dependencies

Do not add crate dependencies unless the spec explicitly lists them. If the spec says `[dependencies]` is empty, it means empty.

### Comments

Write doc comments (`///`) explaining what types and functions are for. Do not write stream-of-consciousness comments explaining your reasoning process. Do not write `// TODO` or `// FIXME`.

### Tests

Write the tests the spec asks for. If the spec includes test cases, implement those exact tests. If it says "identity law" and "composition law", write those specific property tests.

---

## Build & Verify

```bash
cargo test --workspace        # Run all tests
cargo check --workspace       # Type check
cargo clippy --workspace      # Lint
```

Always run verify commands from your spec. If the spec provides specific commands, use those.

---

## PR & Completion

### Filing PRs

Use the `file_pr` tool. Your PR targets your parent branch (auto-detected from your branch name).

Write a clear PR title and body:
- Title: what was implemented (e.g., "Implement CoreFrame, MapLayer, and supporting types")
- Body: list of files changed, tests added, any deviations from spec (there should be none)

### Copilot Review Loop

After filing your PR, Copilot will review it. Review comments will be injected into your terminal by the system. When you see them:

1. Read each comment carefully
2. Fix the issue in your code
3. Commit and push
4. Wait for the next review round

Do NOT argue with Copilot. Just fix what it asks for. If a Copilot suggestion contradicts your spec, follow the spec and note the conflict in your PR body.

### Signaling Completion

Only call `notify_parent` when:
- All spec'd tests pass
- PR is filed
- Copilot review is clean (no outstanding comments on latest push)

Call with `success` if done, `failure` if stuck.

---

## Project Context

This is **tidepool**: a Haskell Core → Rust compiler and runtime. You're implementing pieces of it.

`plans/decisions.md` has all locked architectural decisions. If your spec references a decision, it's already been made — don't second-guess it.
