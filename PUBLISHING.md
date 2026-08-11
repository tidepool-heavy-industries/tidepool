# Publishing to crates.io

## Publish Order

Crates must be published in dependency order. Wait for each crate to appear on crates.io before publishing the next.

```
1.  tidepool-bignum
2.  tidepool-extract-cmd
3.  tidepool-repr
4.  tidepool-eval
5.  tidepool-bridge
6.  tidepool-bridge-derive
7.  tidepool-effect
8.  tidepool-heap
9.  tidepool-codegen
10. tidepool-bridge-effects
11. tidepool-runtime
12. tidepool-mcp
13. tidepool-worktree
14. tidepool-agent
15. tidepool-handlers
16. tidepool-harness
17. tidepool-macro
18. tidepool-optimize
19. tidepool (binary)
20. tidepool-lsp (binary)
21. tidepool-repl (binary)
22. tidepool-web (binary)
```

This order is derived from the workspace dependency graph (`cargo metadata`,
normal deps only) — re-derive it after adding a crate or changing inter-crate
dependencies rather than editing the list by hand.

`tidepool-testing` and the two example crates (`tidepool-guess`, `tidepool-tide`)
have `publish = false` — crates.io ignores them. Everything else in the workspace
publishes, including the four binaries at the end of the list (`tidepool-web`
depends on `tidepool-harness`, which publishes earlier as a library).

## Dry Run

```bash
cargo publish --dry-run -p tidepool-repr
cargo publish --dry-run -p tidepool-eval
# ... etc
```

## Publish

```bash
cargo publish -p tidepool-repr
# wait for it to appear on crates.io
cargo publish -p tidepool-eval
# ... continue in order
```

## Cachix Binary Cache

Push Nix build artifacts to the `tidepool` Cachix cache for both Linux x86_64 and macOS aarch64.

### Setup

```bash
# Install cachix (if not present)
nix-env -iA cachix -f https://cachix.org/api/v1/install
# or: nix profile install nixpkgs#cachix

# Auth (needs token from https://app.cachix.org)
cachix authtoken <TOKEN>
```

### Push

```bash
# Build and push tidepool-extract
nix build .#tidepool-extract
cachix push tidepool $(nix build .#tidepool-extract --print-out-paths)

# Also push the dev shell closure
nix build .#devShells.$(nix eval --raw 'nixpkgs#system').default
cachix push tidepool $(nix build .#devShells.$(nix eval --raw 'nixpkgs#system').default --print-out-paths)
```

Run on both Linux x86_64 and macOS aarch64 to populate the cache for both architectures.
