# Embedded browser assets

The Exomonad distribution packages the harness operator UI from the same exact
`exomonad-harness` revision used by `bridge/facade/Cargo.toml` and
`Cargo.lock`. `nix flake check` validates the Cargo manifest, Cargo lockfile,
facade dependency, and flake source pin as one provenance chain before accepting
the packaged assets.

The assets are built from that revision's `web/` package and
`package-lock.json` with Node 24 from the harness web shell's pinned nixpkgs
revision `bfc1b8a4574108ceef22f02bafcf6611380c100d`.

Build the immutable asset output with:

```sh
nix build .#exomonad-embedded-assets
```

The `exomonad` package includes the same output under
`share/exomonad/web`. Its wrapper and the `exomonad` development shell set
`EXOMONAD_EMBEDDED_ASSET_ROOT` to that store path. An explicit `asset_root` in
the embedded launch configuration continues to override this default and is
validated before the server starts. Omitting it uses the packaged assets.
