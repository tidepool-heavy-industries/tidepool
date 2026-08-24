# tidepool-bridge-effects — single-source bridged wire records

**Charter.** Belongs: the shared bridged Rust↔Haskell wire record types (e.g.
`GitCommit`/`GitStatusEntry`/`GitFileDelta`), single-sourced here — a LOW
crate — so both real handlers (`tidepool-handlers`) and test mocks
(`tidepool-testing`, low-crate integration tests) import the same struct
without a dependency cycle. Does NOT belong: the handler logic that uses
these records (`tidepool-handlers`), the derive machinery
(`tidepool-bridge-derive`).
