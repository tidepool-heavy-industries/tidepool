# Publishing to crates.io

## Publish Order

Crates must be published in dependency order. Wait for each crate to appear on crates.io before publishing the next.

```
1.  tidepool-bignum
2.  tidepool-repr
3.  tidepool-eval
4.  tidepool-bridge
5.  tidepool-bridge-derive
6.  tidepool-effect
7.  tidepool-heap
8.  tidepool-codegen
9.  tidepool-bridge-effects
10. tidepool-runtime
11. tidepool-mcp
12. tidepool-handlers
13. tidepool-harness
14. tidepool-macro
15. tidepool-optimize
16. tidepool (binary)
17. tidepool-lsp (binary)
18. tidepool-repl (binary)
19. tidepool-web (binary)
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
