# Prepared `Address` observation, first slice: implementation plan (read-only, not built)

All 74 corpus Address rows point into a program's pinned literal bytes (19
`sat.N` locals, 55 `$trModule*`/`$tc*` tops whose `TrNameS` fields hold
`Addr#`); words are written as `storage.as_ptr()` (run.rs:288-294,
invocation.rs:119-125), so each observes as the whole literal.

## Edit order: static_bytes -> observe -> forcing -> invocation -> machine
1. `static_bytes.rs`: `by_address: Vec<PinnedLiteral { storage, logical_len }>`
   built in `new` with `logical_len = key.len()` (test storage may lack a NUL;
   never derive from `storage.len()`). `c_string_len`/`read_range_offset`
   use `.storage`. New `logical_suffix(address) -> Option<&[u8]>`: owner via
   the same `partition_point`; `Some(storage[offset..logical_len])` only when
   `offset <= logical_len` (end gives empty; NUL or beyond gives None).
2. `observe.rs`: `AddressOrigin { Null, Unauthenticated }` (exported beside
   `ObservationFailure`, prepared_program.rs:43); variant
   `Address { origin }`. A `ByteArray` origin needs a ledger lookup that does
   not read the payload; not available yet, so byte-array addresses report
   `Unauthenticated` in this slice. `ObservationHeap.pools: Vec<&'a PinnedBytes>`
   via `new_with_registry_and_starts(.., pools)`. `expand` arm (replacing :459):
   0 gives `Null`; first pool with a suffix gives `charge_bytes(len)` then
   `Leaf(Lit(LitString(suffix)))`; else `Unauthenticated`. `inspect_outer`
   (machine.rs:869) unchanged (slice 2).
3. `forcing::observe_results` (152) and `current_heap` (266) take
   `pools: &[Arc<PinnedBytes>]`.
4. One-shot: `invocation.rs:300` passes `slice::from_ref(&self.program.bytes)`.
5. `PreparedMachine.pools: Vec<Arc<PinnedBytes>>` pushed beside `statics`
   (machine.rs:657), threaded through `InstalledProgram::run_entry`
   (1535/1269) and `observation_heap` (947/969).
Check: `just test-lib tidepool-codegen 'test(prepared_program::)'`.

## Tests
- `static_bytes.rs`: `logical_suffix_is_bounded_by_the_literal_not_its_terminator`
  (base, interior, end-empty, NUL and storage end None, embedded NUL
  `b"ab\0tail"`, 0, `usize::MAX`, cross-allocation).
- `observe.rs`: `address_results_observe_pinned_literal_suffixes` (base,
  interior +2, budget exactly `1 + len` passes, `len` fails);
  `address_outside_every_pool_is_refused_unread` (second pool hit; nursery and
  `Vec<u8>` addresses give `Unauthenticated`); constructor with an Address field
  gives `Con(_, [LitString])`.
- Update `cyclic_constructors_exhaust_budget_and_reject_unobservable_shapes`
  (1091-1094) and `constructor_child_errors_are_reported_in_source_order`
  (1125-1128) to expect `Address { origin: Null }`.
- Add a one-shot `HeapRhs::Bytes` test in `bytes_tests.rs` and a two-program
  `PreparedMachine` test proving the pool union.

## Corpus effect and risks
- 74 rows should move to execution passes (628 toward 702); floors in
  `scripts/prepared-corpus.sh` (162-163) still hold; raise afterwards.
  Comparison stays 216 (rows have no oracle).
- `$tc*` TyCon rows may then fail on `$krep`/`KindRep` fields (function
  observation or budget); a `plusAddr#` past the literal end gives
  `Unauthenticated`.
- The pool union is sound only while programs are never retired; remove
  entries with `statics` when retirement lands.
