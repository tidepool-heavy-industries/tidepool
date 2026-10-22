stagedSeed <- segmentRecord 11 >> pure (41 :: Int)
data StagedPlainMarker = StagedPlainMarker
stagedPlain <- segmentRecord 12 >> pure (Carrier.carrierBaseline stagedSeed)
data StagedTextMarker = StagedTextMarker
stagedText <- segmentRecord 13 >> pure (Carrier.lateCarrierTextLength "forty-one")
segmentRecordRendered (fst (StageDisplay.workbenchDisplay stagedText))
segmentRecord (stagedPlain + stagedText)
stagedText
