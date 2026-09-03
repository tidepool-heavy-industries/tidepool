replayed <- collectWorkers [firstHandle]
-- TIDEPOOL-ITEM --
acks <- acknowledgeWorkers
  [ WorkerAcknowledgementRequest firstHandle Reviewed ]
-- TIDEPOOL-ITEM --
afterAck <- collectWorkers [firstHandle]
-- TIDEPOOL-ITEM --
do
  if null
       [ ()
       | later <- sessionInput.sessionContext.workerWakes
       , earlier <- context1.workerWakes
       , later.wakeEvent == earlier.wakeEvent
       ]
    then complete (pure ())
    else error "a consumed worker wake replayed into a later activation"
