{-# LANGUAGE QuasiQuotes #-}
let campaign = "interview-collect" :: CampaignLabel
let question = Question "source-choice" (DesignQuestion
      "plans/component.md" sourceHead "Choose a source" [] [] ["implementation"])
(expert, _updates) <- unfold (batch campaign "expert")
  (childWithProgress @WorkProgress @DesignAnswer
    (coding projectHead
      (assignment [label|source-expert|] (questionDetails question))))
let interviewItems = [AwaitAnswer question expert]
