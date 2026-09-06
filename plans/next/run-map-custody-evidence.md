# Recursive run-map launch failure

Direct observations in the live run-map actor, not historical inference:

- Lead source seed `936c707f55f38ac82eeeb74d63fa8c5eebc6469f`.
- `unfold` admitted reader and historical children from clean `boundHead`.
- Watches 7 (`reader-ready`) and 8 (`history-ready`) became ready without deliveries.
- `pollWatch readerReady` retained `readerResult`:
  `ReplyUnavailable (ResponseTargetFailed "actor cast failed: resident workbench execution failed: turn run failed: yield error: Haskell error: WorktreeUnauthorized (WorktreeId \"wt-ddb3c147-1550-4278-8b18-dd90a2f0a273\")")`.
- `pollWatch historyReady` retained `historyResult`:
  `ReplyUnavailable (ResponseTargetFailed "actor cast failed: resident workbench execution failed: turn run failed: yield error: Haskell error: WorktreeUnauthorized (WorktreeId \"wt-48a0d372-fe14-4430-9c79-1698c60946f9\")")`.

The failure boundary is admitted fork to unavailable response. No child command,
provider-start observation, or detailed source call stack was supplied by those
receipts; exact bootstrap phase is unknown. Do not infer a successful provider
launch or a specific race from this text. Both retained fork handles remain in
the lead scope; no retry, retirement, or worktree deletion was attempted.
Service/root own the production custody repair. Local implementation is partial
progress, not a substitute for fixing recursive launch.

## Firsthand context-efficiency notes

The inherited context looked like root even after Request 2 activation. One
`:status!` resolved actual actor identity/authority. Make the activated actor's
identity explicit alongside each assignment to avoid this diagnostic turn.
The core API guide sufficed for unfold/watch; no inventory was needed. Named
watch handles made wake handling two polls and direct inspection, without
rediscovering assignments. Failure receipts omit the denied operation and
bootstrap stage; adding structured denial context in the existing owner would
reduce source archaeology. No token, cache, or cost savings were measured.
