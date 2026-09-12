# Engine M5 checked join (r7)

## Source and ownership

- Shared M4/M5 scaffold: `30011003ed70f45ba61b67689bbec440c361af24`.
- Parent scaffold retained as `50af291ba56a846b9c99622cfb26c6ce85da6de2`.
- Descriptor allocation/marshalling: `67084c5750bbd38a1697c45db03ce2b1ccc35467`.
- Roots/descriptor tracing: `aad98ebe88803963ad309370000c05e31e0201ac`.
- Mixed PAP/thunk/code-root lifetime stress: `a19c24709353e71662ced40a3cf3dd216c9398b6`.

`StorageLayout` remains the physical payload authority. `ObjectDescriptor`
retains allocation extent, header state, entry metadata, and exact managed slot
offsets. `DescriptorRegistry` owns descriptor identity across moving collection;
it is explicitly not a second reachability/root registry. The existing runtime
frame/global roots remain the only collection roots.

## Integrated consumer

`cheney_copy_registered` is the real Cheney consumer for mixed descriptor and
legacy objects. It uses one shared copying implementation with the legacy
`cheney_copy`, moves descriptor identity when objects move, traces descriptor
objects only through `for_each_trace_slot`, falls back to the existing tag scanner
for legacy objects, and retires unreachable from-space descriptor registrations.
All registered extents are validated before the first forwarding pointer is
installed.

The codegen integration constructs and marshals a descriptor-owned object, roots
that object, performs a real moving copy, reloads both managed slots, preserves a
pointer-looking `Address`, and observes descriptor relocation. Separate checks
cover pre-forwarding refusal of a truncated collection range and retirement of an
unreachable descriptor.

## Checks on the joined content

- `just test-lib tidepool-codegen 'test(registered_)'`: 4 passed, 144 skipped.
- `just test-lib tidepool-codegen 'test(registered_) | test(descriptor_marshalling_) | test(missing_root_registry_) | test(barrier_remembers_)'`: 6 passed, 141 skipped (before the final shared-copy refactor; the directly invalidated registered tests were rerun afterward).
- `just test-lib tidepool-heap 'test(test_transitive_chain) | test(test_diamond_sharing) | test(test_dead_objects_not_copied)'`: 5 passed, 19 skipped.
- `just test-lib tidepool-heap 'test(descriptor_trace_)'`: 2 passed, 22 skipped.
- `just test-target tidepool-codegen resident 'test(pap_void_prefix_and_code_global_survive_collection_then_retire)'`: 1 passed, 62 skipped.
- Rust formatting for the two changed Rust files and `git diff --check`: passed.

## Remaining gates

- M4 must wire generated `LinkedProgram` allocation to
  `emit_descriptor_alloc_fast_path`, `marshal_descriptor_object`, registration,
  and `cheney_copy_registered`; this M5 join supplies and executes the owning
  heap path but does not claim the separate M4 native emitter is cut over.
- Final M6 must execute the combined M4/M5 path with the sole M2 root registry,
  then recheck the A7 join after schema/profile invalidation and production
  cutover.
- Checked host is x86_64. Native aarch64 remains unavailable and unverified.
