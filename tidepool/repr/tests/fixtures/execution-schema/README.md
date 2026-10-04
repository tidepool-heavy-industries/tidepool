# Execution-schema test fixtures

`tidepool-test-data` constructs current typed representations and encodes them
with its test-only TPSTG/TPGRP codec. Decoder and linker refusals mutate one field
of a valid control. These fixtures establish structural contracts only.

The generated `m3_vertical_prepared`, `freer_resume_prepared` and
`freer_retention_prepared` build products come from the real Haskell producer.
Runtime interop tests decode those products, encode their current typed
representations and compare the decoded semantics. Resident checked-cell tests
compile fresh producer and consumer source to test retained values and calls
through production admission, publication and generation ownership.
