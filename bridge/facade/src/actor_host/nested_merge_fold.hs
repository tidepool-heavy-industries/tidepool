evidenceOf :: ResponseResult a -> (WorktreeReceipt, GitOid)
evidenceOf result = case responseWorktree result of { WorktreeObserved receipt _ submission -> (receipt, case submittedHead submission of { OnBranch _ oid -> oid; Detached oid -> oid }); _ -> error "expected worktree evidence" }
mergeObserved :: WorktreeHandle -> Text -> ResponseResult a -> Eff CodingEffects (Either WorktreeError MergeOutcome)
mergeObserved target message result = let (receipt, source) = evidenceOf result in tryMerge MergeRequest { mergeSourceHead = source, mergeSourceWorktree = worktreeId receipt, mergeSourceBranch = Just (branch receipt), mergeTargetWorktree = worktreeId target, mergeMessage = message, mergeAdvance = Nothing }
nestedObserved <- pollWatch nestedReady
let nestedResults = case nestedObserved of { WatchReady values -> values; _ -> error "expected ready nested watch" }
targetResult <- boundWorktree
let targetTree = case targetResult of { Right value -> value; Left _ -> error "expected bound scaffold tree" }
mergeImplementation <- mergeObserved targetTree "merge nested implementation" (fst nestedResults)
mergeVerification <- mergeObserved targetTree "merge nested verification" (snd nestedResults)
respond (ScaffoldReport "folded")
