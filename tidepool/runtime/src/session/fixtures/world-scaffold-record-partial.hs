data MergeRequest = MergeRequest { mergeSourceHead :: Int, mergeSourceWorktree :: Int }
request = MergeRequest { mergeSourceHead = Prior.oldHead Prior.old }
