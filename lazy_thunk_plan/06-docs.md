# WS-Final: Documentation Updates

After implementation is complete and tests pass.

## CLAUDE.md

### Dangerous Patterns section (~line 131)

Remove "Infinite lists" from dangerous patterns or cross it off:

```markdown
- **~~Infinite lists~~**: `repeat x`, `iterate f x`, `cycle xs`, `[0..]` now
  work via lazy thunks for Con fields. `zipWith f xs [0..]` works too.
```

Keep the `zipWith` note updated since that was the motivating case.

### Adding new Prelude functions section (~line 158)

Remove infinite list crash note from this section.

## MEMORY.md

### Known Bugs section

Update the "Eager Con field evaluation" bullet:

```markdown
- **~~Eager Con field evaluation~~**: Fixed. Non-trivial Con fields are now
  thunkified (lazy). Infinite list producers work. LetRec knot-tying for
  data recursion (`let xs = 1 : xs`) unchanged.
```

### Add new entry about thunk implementation

```markdown
## Lazy Thunks for Con Fields
- Non-trivial Con field expressions compiled as thunks (TAG_THUNK = 1)
- Thunk entry = separate Cranelift function, self-contained
- heap_force handles TAG_THUNK: blackhole → call entry → write indirection
- GC tracing already implemented in for_each_pointer_field (gc/raw.rs)
- Pure thunks only (no update frames, no exception thunks, no atomics)
- Cheapness gate: Var/Lit/Lam evaluated eagerly, App/Case/PrimOp thunkified
```

## README.md

No changes needed — the README describes the MCP server interface, not
internal codegen details.
