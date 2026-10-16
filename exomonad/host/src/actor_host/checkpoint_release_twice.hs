Just seed <- R.call (readSeed (R.client seedStore)) ()
first <- releaseCheckpoint seed
again <- releaseCheckpoint seed
display (first == Right () && again == Right ())
