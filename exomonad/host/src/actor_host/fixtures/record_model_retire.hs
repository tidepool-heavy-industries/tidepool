import qualified Tidepool.Actor as Actor
recordExit <- R.finish recordProbe
display (case recordExit of { Actor.Completed 22 -> True; _ -> False })
