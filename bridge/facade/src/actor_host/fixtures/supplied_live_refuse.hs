refused <- replaceSpec suppliedChild (Supplied.actualSpec 303 "This description changes the installed surface.")
display (case refused of { Left SpecReplacementSurfaceChanged -> True; _ -> False })
