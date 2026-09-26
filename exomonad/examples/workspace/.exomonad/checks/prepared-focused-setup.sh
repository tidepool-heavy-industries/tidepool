#!/usr/bin/env bash
set -euo pipefail
mkdir -p scripts
printf '#!/usr/bin/env bash\ntest -f .prepared-here || exit 88\nexec bash %q pass managed\n' "$1" > scripts/cargo-focused-test
chmod +x scripts/cargo-focused-test
