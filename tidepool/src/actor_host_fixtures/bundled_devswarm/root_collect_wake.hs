let firstHandle = case workerHandleOf firstStart of
      Just handle -> handle
      Nothing -> error "first worker was not accepted"
-- TIDEPOOL-ITEM --
let context1 = sessionInput.sessionContext
-- TIDEPOOL-ITEM --
let context2 = sessionInput.sessionContext
-- TIDEPOOL-ITEM --
wakeCollections <- collectWorkerWakes context1.workerWakes
-- TIDEPOOL-ITEM --
do
  if map (\wake -> wake.wakeEvent) context1.workerWakes
       == map (\wake -> wake.wakeEvent) context2.workerWakes
    then complete (pure ())
    else error "session context changed within one activation"
