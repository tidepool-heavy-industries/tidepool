# Pinned backend protocol fixtures

## `app-server-0.146.0/`

The complete Codex app-server protocol JSON Schema, generated from the pinned
CLI itself:

```bash
codex app-server generate-json-schema --out tidepool-agent/fixtures/app-server-0.146.0/
```

**Pinned versions this directory was generated against:**

| Component | Version | Source |
|---|---|---|
| Codex CLI | `0.146.0` | `codex --version` → `codex-cli 0.146.0` (operator's nix profile) |
| `codex-codes` | `0.146.4` | crates.io; the crate tracks CLI versions |

The CLI and the client crate are pinned **together**. Dynamic tools are an
experimental app-server surface, so a version bump means regenerating this
directory and re-running the adapter's compatibility tests — not refreshing a
lockfile. The patch-level skew between CLI `0.146.0` and crate `0.146.4` is
recorded deliberately: any protocol mismatch observed during bring-up is
attributed here first.

Generation is offline and side-effect-free: verified not to modify
`~/.codex` (top-level file size/mtime snapshot identical before and after), so
regenerating costs nothing and spends no ChatGPT tokens.

`v1/` and `v2/` hold the per-version request/response/notification schemas;
the two `codex_app_server_protocol*.schemas.json` files are the aggregate
bundles.
