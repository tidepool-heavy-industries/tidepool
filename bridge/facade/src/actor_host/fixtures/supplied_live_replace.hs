replaced <- replaceSpec suppliedChild (Supplied.actualSpec 202 "The caller supplied this distinctive probe.")
display (case replaced of { Right () -> True; _ -> False })
