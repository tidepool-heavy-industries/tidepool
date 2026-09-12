{
  description = "tidepool - compile freer-simple effect stacks into Cranelift-backed state machines";

  nixConfig = {
    extra-substituters = [ "https://tidepool.cachix.org" ];
    extra-trusted-public-keys = [
      "tidepool.cachix.org-1:jnYeaWymP+9/MeAECROfi4+/l7X1ilkOqM5Nrr5Lo1w="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Codex owns its own locked compiler/package graph. Do not force it onto
    # Tidepool's Rust overlay: the two workspaces intentionally have distinct
    # MSRV/toolchain timelines.
    codex.url = "github:inanna-malick/codex/fc8e158d582d9f28767a94a76ccb45976e845977";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      codex,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        # Overlay: rebuild GHC 9.12 with fat interface files for boot libraries.
        # -fwrite-if-simplified-core writes ALL Core (including workers, loop-breakers)
        # into mi_extra_decls in .hi files, bypassing unfolding heuristics entirely.
        # -fexpose-all-unfoldings + high threshold retained as secondary defense.
        # Targets: ghc-internal (stdlib impl) + ghc-bignum (Integer/Natural).
        # base is just re-exports; ghc-prim has no Haskell Core.
        ghcInternalOverlay =
          final: prev:
          let
            patchedGhc =
              (prev.haskell.compiler.ghc912.override {
                # Native (pure-Haskell) ghc-bignum backend: Integer/Natural ops desugar
                # to pure Core over Word#/ByteArray# primops (no __gmpn_*/integer_gmp_*
                # FFI), which the JIT compiles directly — correct by construction.
                enableNativeBignum = true;
              }).overrideAttrs
                (old: {
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
          in
          {
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

        overlays = [
          (import rust-overlay)
          ghcInternalOverlay
        ];
        pkgs = import nixpkgs { inherit system overlays; };
        # rust-toolchain.toml is the single source of truth for the Rust
        # version + components; the flake reads it rather than pinning
        # `stable.latest` (which drifts silently on every flake.lock update).
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        tidepoolRustPlatform = pkgs.makeRustPlatform {
          cargo = rust;
          rustc = rust;
        };
        # One Haskell package universe for both development and the deployed
        # extractor. The worker loads Tidepool modules at runtime, so a bare
        # compiler is not a usable development toolchain even when it can
        # compile the worker executable itself.
        hsPkgs = pkgs.haskell.packages.ghc912.override {
          overrides = self': super': {
            mkDerivation =
              args:
              super'.mkDerivation (
                args
                // {
                  configureFlags = (args.configureFlags or [ ]) ++ [
                    "--ghc-options=-fwrite-if-simplified-core"
                    "--ghc-options=-fexpose-all-unfoldings"
                  ];
                }
              );
            freer-simple =
              (pkgs.haskell.lib.unmarkBroken (pkgs.haskell.lib.doJailbreak super'.freer-simple)).overrideAttrs
                (old: {
                  postPatch = (old.postPatch or "") + ''
                    sed -i 's/instance (MonadBase b m, LastMember m effs) => MonadBase b (Eff effs)/instance (MonadBase b m, LastMember m effs, Applicative b, Monad b) => MonadBase b (Eff effs)/' src/Control/Monad/Freer/Internal.hs
                  '';
                });
          };
        };
        ghcEnv = hsPkgs.ghcWithPackages (
          ps: with ps; [
            freer-simple
            lens
            errors
            witherable
            safe
            random
            splitmix
          ]
        );
        interactiveCodex = codex.packages.${system}.default;
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            pkgs.pkg-config
          ];
          buildInputs = [
            rust
            ghcEnv
            pkgs.cabal-install
            pkgs.openssl
            pkgs.jq
            pkgs.just
            pkgs.bubblewrap
            # No sccache here, deliberately. It IS active for every build on
            # this box, but via `build.rustc-wrapper` in ~/.cargo/config.toml
            # (host-global, outside this flake), naming an absolute store path
            # — which is already an exact pin, tighter than a version string.
            # This shell used to add `pkgs.sccache` alongside it; under the
            # current flake.lock that resolves to 0.14.0 while the live wrapper
            # is 0.16.0, so it put a DIFFERENT sccache client on PATH than the
            # one doing the caching. Every agent worktree on this box shares
            # one sccache server; a second client version reaching it is at
            # best redundant.
            pkgs.cargo-nextest
          ];

          shellHook = ''
            export TIDEPOOL_GHC_LIBDIR="$(ghc --print-libdir)"
            echo "tidepool dev shell"
            echo "  Rust: $(rustc --version)"
            echo "  GHC:  $(ghc --version)"
            echo "  sccache (rustc-wrapper, from ~/.cargo/config.toml): $(sccache --version 2>/dev/null || echo 'not on PATH')"
          '';
        };

        # The private Codex is selected by absolute path, not added to PATH.
        # Ordinary shells and the operator's CODEX_HOME remain untouched.
        devShells.shoal = pkgs.mkShell {
          inputsFrom = [ self.devShells.${system}.default ];
          packages = [
            pkgs.git
            pkgs.tmux
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.systemd ];
          TIDEPOOL_INTERACTIVE_CODEX_BIN = "${interactiveCodex}/bin/codex";
          TIDEPOOL_SHOAL_CODEX_CLOSURE = "${interactiveCodex}";
          TIDEPOOL_SHOAL_NIX_STORE_BIN = "${pkgs.nix}/bin/nix-store";
          shellHook = ''
            export TIDEPOOL_GHC_LIBDIR="$(ghc --print-libdir)"
            echo "shoal dev shell"
            echo "  interactive agent: $TIDEPOOL_INTERACTIVE_CODEX_BIN"
          '';
        };

        packages.tidepool-extract =
          let
            # The overlay already wires patchedGhc into pkgs.haskell.packages.ghc912,
            # so this package set has fat interfaces AND rebuilds all deps from source.
            #
            # freer-simple 1.2.1.2 needs a patch for GHC 9.12: MonadBase instance
            # requires explicit Applicative+Monad constraints due to superclass changes.
            # Every library package must be built with fat interface flags so that
            # mi_extra_decls is populated. Without this, the fat interface fallback
            # hits the PIT panic for non-boot-library packages (containers, aeson, etc.).
            # Fidelity tests exercise the worker protocol directly. The build
            # sandbox has no cabal, so point their worker override at the
            # executable this same derivation just produced.
            harness =
              pkgs.haskell.lib.overrideCabal (hsPkgs.callCabal2nix "tidepool-extract" ./haskell { })
                (old: {
                  preCheck = (old.preCheck or "") + ''
                    export TIDEPOOL_EXTRACT_WORKER="$PWD/dist/build/tidepool-extract-bin/tidepool-extract-bin"
                  '';
                });
            # This crate has an independent minimal lockfile so Nix vendors
            # its actual graph rather than the whole workspace graph.
            frontend = pkgs.rustPlatform.buildRustPackage {
              pname = "tidepool-extract-frontend";
              version = "0.1.0";
              src = ./tidepool-extract-cmd;
              cargoLock.lockFile = ./tidepool-extract-cmd/Cargo.lock;
              # The daemon integration test needs the separately packaged GHC
              # worker; the final wrapper is exercised by the repository battery.
              cargoTestFlags = [ "--lib" ];
            };
          in
          pkgs.runCommand "tidepool-extract" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
            mkdir -p "$out/bin"
            makeWrapper ${frontend}/bin/tidepool-extract "$out/bin/tidepool-extract" \
              --prefix PATH : ${ghcEnv}/bin \
              --set TIDEPOOL_EXTRACT_WORKER ${harness}/bin/tidepool-extract-bin
            # Shoal locates the pair before retaining the frontend for a run.
            ln -s ${harness}/bin/tidepool-extract-bin "$out/bin/tidepool-extract-bin"
          '';

        packages.shoal-unwrapped = tidepoolRustPlatform.buildRustPackage {
          pname = "shoal-unwrapped";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [
            "-p"
            "tidepool"
            "--bin"
            "shoal"
          ];
          cargoInstallFlags = [
            "-p"
            "tidepool"
            "--bin"
            "shoal"
          ];
          doCheck = false;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];
        };

        packages.shoal = pkgs.symlinkJoin {
          name = "shoal";
          paths = [ self.packages.${system}.shoal-unwrapped ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            wrapProgram "$out/bin/shoal" \
              --prefix PATH : ${
                pkgs.lib.makeBinPath ([
                  self.packages.${system}.tidepool-extract
                  pkgs.bubblewrap
                  pkgs.coreutils
                  pkgs.git
                  pkgs.tmux
                ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.systemd ])
              } \
              --set TIDEPOOL_EXTRACT "${self.packages.${system}.tidepool-extract}/bin/tidepool-extract" \
              --set TIDEPOOL_INTERACTIVE_CODEX_BIN "${interactiveCodex}/bin/codex" \
              --set TIDEPOOL_SHOAL_CODEX_CLOSURE "${interactiveCodex}" \
              --set TIDEPOOL_SHOAL_NIX_STORE_BIN "${pkgs.nix}/bin/nix-store"
          '';
        };

        apps.shoal = flake-utils.lib.mkApp {
          drv = self.packages.${system}.shoal;
        };

        packages.default = self.packages.${system}.tidepool-extract;

        formatter = pkgs.nixfmt;

        checks = {
          # Fail-loud replacement for the ghcInternalOverlay postPatch's silent
          # `if [ -d "$dir" ]` / `if [ -d "$lib" ]` guards (flake.nix, above):
          # if GHC ever renames/moves one of these boot-library directories,
          # the patch would otherwise skip injecting the fat-interface flags
          # for it with no error. This check lives OUTSIDE the patchedGhc
          # derivation on purpose — it builds over the plain GHC *source*
          # tarball (cheap fetch, no compile), so it can assert loudly without
          # touching postPatch/mkDerivation and changing their derivation
          # hashes (that would trigger a from-source GHC rebuild).
          #
          # The directory list is duplicated rather than shared with
          # postPatch's `for dir in ...` / `for lib in ...` lines: sharing it
          # would mean editing that string, which is exactly the hash-changing
          # edit this check exists to avoid. Keep both lists in sync by hand;
          # each side comments at the other.
          #
          # `knownMissing`: the patch's second loop names `libraries/Cabal-syntax`,
          # but in the GHC 9.12.2 tree that directory lives at
          # `libraries/Cabal/Cabal-syntax` (nested under Cabal, not a sibling
          # under libraries/), so the postPatch `-d` guard silently skips it —
          # Cabal-syntax never gets the fat-interface OPTIONS_GHC. Correcting
          # the path in postPatch changes the patched-GHC derivation hash, so
          # it isn't done here; it rides the next deliberate lock bump (which
          # rebuilds GHC anyway) instead of costing a from-source rebuild today.
          # One-line fix for that future bump:
          #   for lib in ... libraries/Cabal libraries/Cabal/Cabal-syntax libraries/text
          # (replace the bare `libraries/Cabal-syntax` entry with the nested path).
          #
          # The assertion below is self-retiring: it fails if Cabal-syntax ever
          # starts existing at the top-level name the patch actually uses — that
          # would mean the patch entry has gone live again and this exception
          # (and the one-line fix above) should be deleted.
          ghc-boot-library-dirs = pkgs.runCommand "ghc-boot-library-dirs-check" { } ''
            src=${pkgs.haskell.compiler.ghc912.src}

            # Mirrors the two `for` loops in ghcInternalOverlay's postPatch above,
            # minus the one entry tracked in knownMissing below.
            expected="libraries/ghc-internal/src libraries/ghc-bignum/src libraries/ghc-prim \
                  libraries/containers libraries/bytestring libraries/array \
                  libraries/deepseq libraries/directory libraries/filepath \
                  libraries/process libraries/unix libraries/parsec \
                  libraries/mtl libraries/transformers libraries/stm \
                  libraries/template-haskell libraries/binary \
                  libraries/exceptions libraries/time libraries/hpc \
                  libraries/Cabal libraries/text"

            missing=""
            for d in $expected; do
              if [ ! -d "$src/$d" ]; then
                missing="$missing $d"
              fi
            done

            if [ -n "$missing" ]; then
              echo "ghcInternalOverlay's postPatch loops over these boot-library" >&2
              echo "directories, but they no longer exist in $src:" >&2
              echo "$missing" >&2
              exit 1
            fi

            # knownMissing: libraries/Cabal-syntax (patch's path) -> real path
            # libraries/Cabal/Cabal-syntax. Assert the real path still exists
            # (the gap is real, not stale) and the patch's path still does NOT
            # exist (if it now does, the exception above is out of date).
            if [ ! -d "$src/libraries/Cabal/Cabal-syntax" ]; then
              echo "knownMissing entry libraries/Cabal-syntax (real path" >&2
              echo "libraries/Cabal/Cabal-syntax) is stale: the real path no" >&2
              echo "longer exists either. Update the known-gap entry." >&2
              exit 1
            fi
            if [ -d "$src/libraries/Cabal-syntax" ]; then
              echo "libraries/Cabal-syntax now exists at the top level -" >&2
              echo "the postPatch loop's entry for it is live again. Delete" >&2
              echo "the knownMissing exception for it in this check." >&2
              exit 1
            fi

            touch $out
          '';

          # Covers extractor construction as part of `nix flake check`.
          tidepool-extract = self.packages.${system}.tidepool-extract;

          # Deterministic and model-free. The first private Codex build can be
          # substantial, so use this targeted check during iteration.
          codex-host-tools-contract = pkgs.runCommand "codex-host-tools-contract" { } ''
            ${interactiveCodex}/bin/codex --version
            test -x ${interactiveCodex}/bin/codex-code-mode-host
            ${interactiveCodex}/bin/codex-code-mode-host --help
            ${interactiveCodex}/bin/codex --help | grep --fixed-strings -- '--host-dynamic-tools-socket'
            ${interactiveCodex}/bin/codex fork --help | grep --fixed-strings -- '--destination-local'
            ${interactiveCodex}/bin/codex fork --help | grep --fixed-strings -- '--after-call'
            ${interactiveCodex}/bin/codex queue --help | grep --fixed-strings -- '--thread'
            ${interactiveCodex}/bin/codex queue --help | grep --fixed-strings -- '--message'
            ${interactiveCodex}/bin/codex app-server --help | grep --fixed-strings -- '--controller-token-file'
            ${interactiveCodex}/bin/codex observe --help | grep --fixed-strings -- '--remote'
            ${interactiveCodex}/bin/codex archive --help
            touch "$out"
          '';

          shoal = self.packages.${system}.shoal;
        };
      }
    );
}
