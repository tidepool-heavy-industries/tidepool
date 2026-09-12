Send raw Haskell as a notebook cell. GHC checks it before effects. Declarations
are mutually recursive and visible to statements; later statements see earlier
bindings. Declarations and bindings persist. Typecheck rejection runs no effects.
Runtime failure retains its prefix and stops the suffix. An exact retry returns
its receipt. Imports persist; leading pragmas are cell-local. No colon commands.

Read the activation; use `inspectFull sessionInput` for omitted prose. Hosted
`lookup` answers names, `::type` searches callable names, and `doc` finds guides.
`status` has `summary`, `detailed`, `recovery`, `lineage`, `trace`, and `bindings`.

`unfold` starts children after the cell returns. `request` queues new work;
`updateRequest` clarifies an owned active response; `respond` settles it. Register
a watch when waiting, then end the model turn. Failed provider turns leave requests
pending; inspect before steering or retirement. Use shell tools for repository work.
