:{
do
  finished <- startActor deadActor ()
  _ <- awaitExit finished
  _ <- call finished DeadCall
  pure ()
:}
