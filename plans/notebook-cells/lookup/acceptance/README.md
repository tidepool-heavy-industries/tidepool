# Lookup acceptance fixture

These files are ready for the coordinator-owned hosted-tool harness. They do
not add a second harness or edit shared registration.

The future `documentation_tests` consumer should:

1. add `LookupAmbiguousA.hs` and `LookupAmbiguousB.hs` to the inspection include
   path and import both unqualified in the tested actor preamble;
2. execute `setup-cell.hs` through the hosted `haskell` tool;
3. invoke hosted `lookup` with `request.json`;
4. apply every assertion in `assertions.json` to the structured response,
   preserving result order;
5. execute `use-returned-name-cell.hs`, proving that the returned
   `awaitSettled` name is callable in the next cell;
6. settle the worker and execute `observe-settlement-cell.hs`.

The two deliberately bad type queries must reject independently. They must not
turn the lookup invocation into a tool-level failure or hide either neighboring
success. `choose` is genuinely ambiguous because the acceptance preamble imports
two distinct exported values with that occurrence name.

The fixtures use no colon commands. They assume the agreed notebook contract:
each `.hs` file is one cell, declarations/statements share that cell, and
bindings persist into the next file.
