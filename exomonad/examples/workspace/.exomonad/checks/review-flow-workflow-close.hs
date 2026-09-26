-- retainedInterviews comes from review-flow-workflow-interview-result, after
-- the owner has read and recorded each answer. Pending answers block cleanup.
case retainedInterviews of
  Nothing -> pure (inspectFull ("interview pending; cleanup not requested" :: Text))
  Just interviewReceipts | null interviewReceipts ->
    pure (inspectFull ("no reviewer interview; cleanup not requested" :: Text))
  Just interviewReceipts -> do
    current <- R.call (reviewSnapshot (R.client flow)) ()
    let terminal = case flowStage current of
          ReviewAccepted _ -> True
          ReviewStopped _ -> True
          _ -> False
    if terminal then do
      cleanupReceipt <- R.call (reviewCleanup (R.client flow)) ReviewCleanupOnce
      afterCleanup <- R.call (reviewSnapshot (R.client flow)) ()
      let releaseSteps = case cleanupReceipt of
            ReviewCleanupAttempted groups ->
              [(group, map cleanupReceiptSteps receipts) | (group, receipts) <- groups]
            _ -> []
      pure (inspectFull (show (map responseValue interviewReceipts, flowStage current,
        flowReviewRoutes current, cleanupReceipt, releaseSteps,
        flowCleanupResult afterCleanup)))
    else pure (inspectFull ("review pending; cleanup not requested" :: Text))
-- Inspect every CleanupReceipt step. StoppedReleasing requires its later host
-- release notice; a stopped actor or AlreadyStopped is not release evidence.
