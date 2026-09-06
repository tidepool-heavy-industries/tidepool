# Root window/metadata and CLI integration

Accepted reviewed window/metadata candidate 0c5623f1 and review 1738b9a9,
integrated at c640be52b6f5e77fce683dd426ca650ae8277d2d. Added thin existing
Shoal CLI dispatch at 9374fbe80aaa02402744771fa32f3c24464aac38.

Root direct checks at 9374fbe8, inherited repository Nix shell:
- `just test-lib tidepool 'test(partial_map_) | test(run_map_)'`: 9 passed,
  99 skipped, 6.39s Cargo test build, 0.009s tests, daemon teardown confirmed.
- `cargo test -p tidepool --bin shoal -- --nocapture`: 2 passed, no exclusions.
- `cargo build -p tidepool --bin shoal --example run_map`: compiled both consumers.
- `python3 /tmp/root-run-map-cli/check.py`: 8 real CLI invocations passed:
  five windows, reversed and invalid-bound rejection, concise output. JSON excluded
  assignment sentinel; fixture/HOME files unchanged; empty PATH required no external
  launcher/provider. Script and output retained under /tmp/root-run-map-cli alongside
  reader-tests.log, cli-tests.log and build.log.
- Formatting/diff checks passed, checkout clean. Built shoal SHA256:
  c9a3cd5a094f9ff0a1def330160c0468bd920daae2aa24ad32a62daf21283d79.

This built the isolated actor-target binary, not a replacement/restart of the
running host. CLI review is separately assigned to the retained reviewer against
9374fbe8; root's new CLI seam is not yet independently accepted. Usage integration,
unrecorded evidence edges, status persistence migration and service/custody gates
remain separate obligations. No additional discovery/coordination saving measured.
