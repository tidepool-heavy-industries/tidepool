input
-- TIDEPOOL-ITEM --
:type startWorkers
-- TIDEPOOL-ITEM --
:info WorkerStartResult
-- TIDEPOOL-ITEM --
waveAssignment <- pure ("inspect one focused boundary" :: Text)
-- TIDEPOOL-ITEM --
:bindings
-- TIDEPOOL-ITEM --
(firstStart, secondStart) <- do
  starts <- startWorkers
    [ worker "review-1" waveAssignment
    , worker "review-2" "inspect a disjoint boundary"
    ]
  case starts of
    [first, second] -> pure (first, second)
    _ -> error "worker batch did not preserve its two-result shape"
-- TIDEPOOL-ITEM --
firstWorkers <- pure [firstStart, secondStart]
