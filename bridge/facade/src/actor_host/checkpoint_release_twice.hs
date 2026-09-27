Just seed <- R.call (readSeed (R.client seedStore)) ()
first <- releaseCheckpoint seed
again <- releaseCheckpoint seed
first == Right () && again == Right ()
