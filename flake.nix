{
  description = "tidepool - compile freer-simple effect stacks into Cranelift-backed state machines";

  nixConfig = {
    extra-substituters = [ "https://tidepool.cachix.org" ];
    extra-trusted-public-keys = [ "tidepool.cachix.org-1:jnYeaWymP+9/MeAECROfi4+/l7X1ilkOqM5Nrr5Lo1w=" ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        # Overlay: rebuild GHC 9.12 with fat interface files for boot libraries.
        # -fwrite-if-simplified-core writes ALL Core (including workers, loop-breakers)
        # into mi_extra_decls in .hi files, bypassing unfolding heuristics entirely.
        # -fexpose-all-unfoldings + high threshold retained as secondary defense.
        # Targets: ghc-internal (stdlib impl) + ghc-bignum (Integer/Natural).
        # base is just re-exports; ghc-prim has no Haskell Core.
        ghcInternalOverlay = final: prev:
          let
            patchedGhc = prev.haskell.compiler.ghc912.overrideAttrs (old: {
              postPatch = (old.postPatch or "") + ''
                TIDEPOOL_GHC_OPTS="-fexpose-all-unfoldings -funfolding-creation-threshold=100000 -fwrite-if-simplified-core"

                # Inject fat interface flags via OPTIONS_GHC into boot libraries.
                # ghc-internal, ghc-bignum, ghc-prim: safe to prepend (no exotic extensions).
                for dir in libraries/ghc-internal/src libraries/ghc-bignum/src libraries/ghc-prim; do
                  if [ -d "$dir" ]; then
                    find "$dir" -name '*.hs' -exec sed -i "1s/^/{-# OPTIONS_GHC $TIDEPOOL_GHC_OPTS #-}\n/" {} +
                    echo "tidepool: injected OPTIONS_GHC into $dir"
                  fi
                done

                # For ALL other boot libraries (containers, bytestring, array, text, etc.),
                # inject OPTIONS_GHC AFTER existing pragmas by appending before the module line.
                # This avoids breaking files that start with {-# LANGUAGE MagicHash #-} etc.
                for lib in libraries/containers libraries/bytestring libraries/array \
                           libraries/deepseq libraries/directory libraries/filepath \
                           libraries/process libraries/unix libraries/parsec \
                           libraries/mtl libraries/transformers libraries/stm \
                           libraries/template-haskell libraries/binary \
                           libraries/exceptions libraries/time libraries/hpc \
                           libraries/Cabal libraries/Cabal-syntax libraries/text; do
                  if [ -d "$lib" ]; then
                    find "$lib" -name '*.hs' -exec sed -i '/^module /i {-# OPTIONS_GHC '"$TIDEPOOL_GHC_OPTS"' #-}' {} +
                    echo "tidepool: injected OPTIONS_GHC before module decl in $lib"
                  fi
                done
              '';
            });
          in {
            haskell = prev.haskell // {
              compiler = prev.haskell.compiler // {
                ghc912 = patchedGhc;
              };
              # Wire patched GHC into the package set so ALL Haskell deps
              # (freer-simple, etc.) are rebuilt from source against the new boot lib ABIs.
              # Both `ghc` and `buildHaskellPackages` must point to patchedGhc to avoid
              # mixing artifacts from the old ABI universe (causes "dependency doesn't exist").
              packages = prev.haskell.packages // {
                ghc912 = prev.haskell.packages.ghc912.override (old: {
                  ghc = patchedGhc;
                  buildHaskellPackages = old.buildHaskellPackages.override (_: {
                    ghc = patchedGhc;
                  });
                });
              };
            };
          };

        overlays = [ (import rust-overlay) ghcInternalOverlay ];
        pkgs = import nixpkgs { inherit system overlays; };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            pkgs.pkg-config
          ];
          buildInputs = [
            rust
            pkgs.haskell.compiler.ghc912
            pkgs.cabal-install
            pkgs.openssl
            self.packages.${system}.tidepool-extract
          ];

          shellHook = ''
            echo "tidepool dev shell"
            echo "  Rust: $(rustc --version)"
            echo "  GHC:  $(ghc --version)"
          '';
        };

        packages.tidepool-extract = let
          # The overlay already wires patchedGhc into pkgs.haskell.packages.ghc912,
          # so this package set has fat interfaces AND rebuilds all deps from source.
          #
          # freer-simple 1.2.1.2 needs a patch for GHC 9.12: MonadBase instance
          # requires explicit Applicative+Monad constraints due to superclass changes.
          # Every library package must be built with fat interface flags so that
          # mi_extra_decls is populated. Without this, the fat interface fallback
          # hits the PIT panic for non-boot-library packages (containers, aeson, etc.).
          # Override mkDerivation to inject flags globally into ALL packages.
          hsPkgs = pkgs.haskell.packages.ghc912.override {
            overrides = self': super': {
              mkDerivation = args: super'.mkDerivation (args // {
                configureFlags = (args.configureFlags or []) ++ [
                  "--ghc-options=-fwrite-if-simplified-core"
                  "--ghc-options=-fexpose-all-unfoldings"
                ];
              });
              freer-simple = (pkgs.haskell.lib.unmarkBroken (
                pkgs.haskell.lib.doJailbreak super'.freer-simple
              )).overrideAttrs (old: {
                postPatch = (old.postPatch or "") + ''
                  sed -i 's/instance (MonadBase b m, LastMember m effs) => MonadBase b (Eff effs)/instance (MonadBase b m, LastMember m effs, Applicative b, Monad b) => MonadBase b (Eff effs)/' src/Control/Monad/Freer/Internal.hs
                '';
              });
            };
          };
          ghcEnv = hsPkgs.ghcWithPackages (ps: with ps; [
            freer-simple
            lens
          ]);
          harness = hsPkgs.callCabal2nix "tidepool-harness" ./haskell {};
        in pkgs.writeShellScriptBin "tidepool-extract" ''
          export PATH="${ghcEnv}/bin:$PATH"
          exec ${harness}/bin/tidepool-extract-bin "$@"
        '';

        packages.default = self.packages.${system}.tidepool-extract;
      }
    );
}
