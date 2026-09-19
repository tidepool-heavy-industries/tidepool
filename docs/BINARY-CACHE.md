# Binary cache

`flake.nix` declares a public Cachix substituter for prebuilt artifacts.
Coverage depends on which revisions have been published.
Whether you need to do anything depends on how Nix was installed:

```bash
nix config show trusted-users
```

- **Your username is listed** — this is what the Determinate Systems installer
  does — then nothing is needed. Accept the flake configuration when prompted
  and the cache is used.
- **Only `root` is listed**, the official multi-user installer's default, then
  one root action is needed once:

  ```bash
  sudo cachix use tidepool            # writes /etc/nix/nix.conf
  sudo cachix use tidepool --mode nixos   # on NixOS, writes /etc/nixos/cachix/
  ```

- **Single-user install** (no daemon), then `cachix use tidepool` without sudo.

A substituter writes into the shared `/nix/store`, so only root can authorize
one; a user who could add substituters and signing keys could hand every other
user on the machine an arbitrary binary. That is why a flake's own
`nixConfig` is ignored for untrusted users, and why no flag works around it.
On NixOS the least-privilege form is to permit rather than impose, leaving the
opt-in with the flake:

```nix
nix.settings.trusted-substituters = [ "https://tidepool.cachix.org" ];
nix.settings.trusted-public-keys = [
  "tidepool.cachix.org-1:jnYeaWymP+9/MeAECROfi4+/l7X1ilkOqM5Nrr5Lo1w="
];
```

