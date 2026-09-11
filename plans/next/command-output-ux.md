# Resident command output UX

Approved scope: ordinary multiline Bash, typed run/await results, complete stdout,
Text-native compositional display, bounded stream paging and qualified discovery.
The September 10 usability wave stopped before product fan-out. Retained feedback:
`plans/parallel-dogfood/next-wave/command-ux-{astra,sol}.md`; original planner
`28ec1c6eebbf00e7916b0955aeea52ac4e926d1c`. Product branches remain separate.

Implementation is on main and the native command-jobs continuation. No live
package changes or new swarm launch are part of this work. The next runner must
select a matched native/Tidepool pair: stream-end notification support is required
before terminal output can be reported with stable cursor positions.

Validation completed:
- Twelve low-level parser cases/matrices, independent of the Haskell stack.
- Five byte-window checks: positions, gaps, UTF-8, fragments, empty/live end,
  and non-consuming reads.
- Resident nested large Text rendering, strict stdout/JSON extraction, separate
  stderr completeness, qualified discovery, literal skill examples, retained-job
  cancellation/routing and recovery after a deliberate display failure without
  executing the command again.
- Native completion/stream ordering in both arrival orders and rejection of
  incomplete stream disposition; generated stable and experimental schemas.
- Rust/Haskell formatting and skill frontmatter validation.

Actual TUI acceptance passed in 181.44 s, exercising the real native/host binaries
with a scripted provider. Thirteen native boundary tests and scoped lint completed;
lint also reported preexisting warnings outside the changed mechanism. Native source
`8c5f5477f0a65cac1144614c75d26d4f2004b248` is pushed.
The matched runtime is Tidepool `61f40e8bd443fbf6f1aa002a9fe85a18d7a035b0`
with the native revision above. The frozen package and selection manifest are in
`/home/inanna/dev/tidepool/target/command-output-runner-20260910/`.
The wrapper selects immutable native, host, extractor and standard-library store
paths. The running recipe compiler's executable hash matches the packaged worker.

Remaining release gate: finish the updated workspace recipe checks. Coordination
assertions compare typed values inside the resident session, avoiding dependence
on the display layout. The command's own presentation fixtures test rendering
separately. No new swarm launch.
The TUI fixture must execute as the sole process in a fresh systemd scope with
`--user --scope --slice=swarm.slice --property=Delegate=yes`. Start the compiler
and test launcher outside that scope; delegating their shared cgroup is invalid.

## Additional UX review

Current guidance must distinguish command intent from resolved execution, viewing
an existing value from reading retained output from executing again, and sequential
`run` traversal from start-all/await-all. Native resolution is owned by
`app-server/src/request_processors/command_exec_processor.rs`: cwd and inherited
environment are resolved on launch, not while constructing Haskell values.

Deferred capabilities: a resolved execution receipt (without dumping inherited
secrets), explicit execution and cleanup deadlines, secret-aware retained history,
and a byte-output escape hatch. Design redaction across nested values/errors before
adding richer history; redaction must never alter execution bytes. Do not add
Git/Grep/Sed effects. File/event/artifact/diagnostic effects need demonstrated usage
and must extend the existing owners.

Conversation acceptance uses exact skill examples in the resident workbench plus
the actual TUI fixture: multiline quotation, retained pending/completed jobs,
nonzero output, nested Unicode, qualified discovery and paged output. Low-level
parser and byte-window matrices remain independent of the Haskell engine.

## Declaration latency evidence

The opt-in `command_description_latency_probe` runs five declarations/observations
in one resident actor without starting subprocesses. September 10 measurement:
argv-first 5.085 s; quote-first 11.352 s; quote-second 17.380 s; argv-second
15.449 s; reuse 0.816 s. This does not isolate the compiler's internal phases, but
it disproves attribution of the recurring delay solely to Bash execution or its
quoter: fresh argv declarations also cost seconds. Keep command reuse prominent;
do not introduce fragile Template Haskell name construction to optimize an
unproven cause. Detailed compiler optimization belongs with the engine owner.
