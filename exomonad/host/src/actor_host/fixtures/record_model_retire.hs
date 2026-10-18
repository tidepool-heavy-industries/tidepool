import qualified Tidepool.Actor as Actor
recordExit <- R.finish recordProbe
display (case recordExit of { Actor.Completed 2 -> True; _ -> False })
