#!/usr/bin/env bash
# Bound daemon-side builds, which are not descendants of the calling command.
set -euo pipefail
case "${1:---check}" in
  --install)
    if [[ -e /etc/NIXOS ]]; then
      echo 'NixOS owns /etc/systemd. Set nix.settings.{max-jobs,cores} and systemd.services.nix-daemon.serviceConfig in configuration.nix, then rebuild.' >&2
      echo 'Use --runtime to apply the resource caps immediately without a rebuild.' >&2
      exit 2
    fi
    install -d -m 0755 /etc/systemd/system/nix-daemon.service.d
    cat > /etc/systemd/system/nix-daemon.service.d/shoal-resources.conf <<'UNIT'
[Service]
MemoryMax=8G
MemorySwapMax=1G
CPUQuota=200%
Environment="NIX_CONFIG=max-jobs = 1\ncores = 1"
UNIT
    systemctl daemon-reload
    systemctl set-property --runtime nix-daemon.service MemoryMax=8G MemorySwapMax=1G CPUQuota=200%
    echo 'Memory/CPU limits are live. Nix job/core defaults activate at the next daemon restart; do not restart over active builds.'
    ;;
  --runtime)
    systemctl set-property --runtime nix-daemon.service MemoryMax=8G MemorySwapMax=1G CPUQuota=200%
    ;;
  --check) ;;
  *) echo 'usage: shoal-nix-resources.sh [--check|--install|--runtime]' >&2; exit 2 ;;
esac
systemctl show nix-daemon.service --property=MemoryMax --property=MemorySwapMax --property=CPUQuotaPerSecUSec
[[ "$(systemctl show nix-daemon.service --property=MemoryMax --value)" == 8589934592 ]]
[[ "$(systemctl show nix-daemon.service --property=MemorySwapMax --value)" == 1073741824 ]]
[[ "$(systemctl show nix-daemon.service --property=CPUQuotaPerSecUSec --value)" == 2s ]]
