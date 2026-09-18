# Alpha release acceptance

This is the temporary acceptance checklist for the first public alpha. It keeps
release evidence separate from ordinary development checks. Delete it after the
release outcome and any lasting operator instructions have moved to their owners.

## Freeze the candidate

- Start from a committed, clean revision. Record `git rev-parse HEAD` and the
  intended tag before evaluating output paths. A dirty-tree output is diagnostic
  only; it is not the release identity.
- Run the repository's required focused checks and final gate according to
  `AGENTS.md`. Record exact commands and outcomes.
- Evaluate and build the public package from that revision:

  ```bash
  nix flake show
  nix build --print-out-paths .#shoal
  ```

  `.#shoal` wraps the matched Rust binary, extractor, patched Haskell toolchain,
  pinned Codex client, and runtime utilities. Record the returned store path and
  its closure with `nix-store -qR`.

## Publish and verify the complete closure

Publication requires explicit Cachix write authority and is a release action:

```bash
shoal_out=$(nix build --no-link --print-out-paths .#shoal)
cachix push tidepool "$shoal_out"
```

Do not infer coverage from `nix-cache-info`. Verify every closure member by its
exact store hash after the push:

```bash
nix-store -qR "$shoal_out" |
  while read -r path; do
    base=${path#/nix/store/}
    hash=${base%%-*}
    curl --fail --silent --show-error \
      --output /dev/null "https://tidepool.cachix.org/$hash.narinfo" ||
      { printf 'missing: %s\n' "$path" >&2; exit 1; }
  done
```

Record failures as missing closure paths. Endpoint reachability and top-level
presence alone do not prove that a clean user can substitute the closure.

## Clean-user smoke

Use a fresh Linux user or disposable VM with cgroup v2, systemd user services,
Nix, Bubblewrap, Git, and tmux. It must not share the builder's Nix store or
GitHub credentials. Configure cache trust as described in `README.md`, then:

1. Clone the exact release revision over public HTTPS with GitHub credential
   variables unset.
2. Run `nix flake show`, followed by
   `nix build --print-build-logs --print-out-paths .#shoal`. Record whether each
   path substituted or built; a successful warm-store build is not this proof.
3. Authenticate the pinned client selected by the flake, not an ambient
   `codex`, and check its status:

   ```bash
   nix develop .#shoal -c bash -lc \
     '"$TIDEPOOL_INTERACTIVE_CODEX_BIN" login'
   nix develop .#shoal -c bash -lc \
     '"$TIDEPOOL_INTERACTIVE_CODEX_BIN" login status'
   ```

4. Set `TYPESAFE_API_KEY` without printing it. Configure and inspect a finite
   `swarm.slice` using `docs/GETTING-STARTED.md`.
5. In a fresh throwaway Git repository, run `shoal new`, commit its generated
   package, then run `shoal check --workspace .` and start `shoal init`.
6. In the live session, execute a simple Haskell cell, a live Jev judgment, and
   one read-only child assignment. Observe the typed reply and retire the child;
   record cleanup state.
7. Stop the run and confirm that source Git state is intact. Preserve only
   bounded logs needed for acceptance evidence.

## Current evidence and open gates

At development revision `d3acfb699` with local runtime files present,
non-building evaluation on 2026-09-18 produced:

| Output | Evaluated path | Public narinfo |
|---|---|---|
| `tidepool-extract` | `/nix/store/yxk4kznlavikl1681mrs17ra06cgkl42-tidepool-extract` | HTTP 404 |
| `shoal-unwrapped` | `/nix/store/7as0sigv98da58gaillyjy2gch12wafs-shoal-unwrapped-0.1.0` | HTTP 404 |
| `shoal` | `/nix/store/xj6rl2ic8r74dkv19x8n7f93gs7m176i-shoal` | HTTP 404 |

The cache's `nix-cache-info` returned HTTP 200. These paths came from a dirty
working tree and are **not release paths**; their misses establish only that
endpoint availability is not closure coverage. No build, upload, tag, push, or
clean-user smoke was performed by this investigation.

Open release gates:

- select and commit the exact release revision;
- run the final checks and exact `.#shoal` build;
- publish and verify every member of that exact closure;
- complete the isolated clean-user smoke above;
- record live provider/after-turn acceptance separately;
- review the final diff and create/push the tag only after all evidence agrees.
