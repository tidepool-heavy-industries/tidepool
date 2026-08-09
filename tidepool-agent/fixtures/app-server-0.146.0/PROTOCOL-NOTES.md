# Protocol truth — app-server 0.146.0 / codex-codes 0.146.4

PRD 18 phase 2 (`plans/post-restart/agent-lanes/dev-adapter-bringup.md`).
Answered entirely offline: from this directory's committed JSON Schema, the
pinned `codex-codes` 0.146.4 crate's generated types
(`~/.cargo/registry/src/*/codex-codes-0.146.4/src/protocol_generated/`), and
the `openai/codex` CLI source at git tag `rust-v0.146.0` (the tag matching
`codex --version` → `codex-cli 0.146.0`). No app-server process was started
and no ChatGPT token was spent producing this file.

## 1. Where does `dynamicTools` attach?

**`ThreadStartParams.dynamicTools`** — a top-level field of the `thread/start`
request, sibling to `cwd`/`config`/`model`/etc. **Not** nested inside the open
`config` object. This confirms the PRD's assumption that dynamic tools are
thread-scoped, frozen at thread creation, not renegotiable per turn.

Source (`codex-rs/app-server-protocol/src/protocol/v2/thread.rs` @
`rust-v0.146.0`, lines 127–133):

```rust
#[experimental("thread/start.dynamicTools")]
#[serde(
    default,
    deserialize_with = "codex_protocol::dynamic_tools::deserialize_dynamic_tool_specs"
)]
#[ts(optional = nullable)]
pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
```

**The pinned `codex-codes` 0.146.4 crate does not expose this field, or the
`DynamicToolSpec`/`DynamicToolFunctionSpec`/`DynamicToolNamespaceSpec`/
`DynamicToolNamespaceTool` types at all.** Its generated
`ThreadStartParams` (`src/protocol_generated/types.rs:7301`) has only
`approvalPolicy, approvalsReviewer, baseInstructions, config, cwd,
developerInstructions, ephemeral, model, modelProvider, personality, sandbox,
serviceName, serviceTier, sessionStartSource, threadSource` — no
`dynamicTools`. This is schema-generation drift, not a version-pin mismatch:
`generate-json-schema` (and whatever process produced the crate's generated
types) drops fields marked `#[experimental(...)]` unless the experimental
surface is explicitly included, so an experimental-gated field never reaches
either the committed JSON Schema (`v2/ThreadStartParams.json` in this
directory, confirmed by grep — zero `$ref` to `DynamicToolSpec` from any
reachable property) or the crate's generated bindings, even though the wire
protocol accepts it.

**Consequence for the adapter:** `tidepool-agent`'s `backend::codex` module
must hand-roll `DynamicToolSpec`/`DynamicToolFunctionSpec`/
`DynamicToolNamespaceSpec`/`DynamicToolNamespaceTool` locally (shapes below,
straight from `codex-rs/protocol/src/dynamic_tools.rs` @ `rust-v0.146.0`) and
send `thread/start` through `codex-codes`'s generic escape hatch —
`AppServerClient::request<P: Serialize, R: DeserializeOwned>(&mut self,
method: &str, params: &P)` (`src/client_async.rs:192`) — with a params value
that layers `dynamicTools` alongside the typed `ThreadStartParams` fields,
rather than through the crate's typed `thread_start()` helper, which cannot
express this field.

Wire shape (function tool):

```json
{"type": "function", "name": "ask_parent", "description": "...", "inputSchema": {...}, "deferLoading": false}
```

Namespace form (wraps a tool list; `deferLoading` omitted when `false` —
`skip_serializing_if` on both):

```json
{"type": "namespace", "name": "...", "description": "...", "tools": [{"type": "function", ...}]}
```

## 2. Experimental-APIs opt-in

**`InitializeParams.capabilities.experimentalApi: bool`**, set at `initialize`
time. Source (`codex-rs/app-server-protocol/src/protocol/v1.rs` @
`rust-v0.146.0`, `InitializeCapabilities`):

```rust
pub struct InitializeCapabilities {
    #[serde(default)]
    pub experimental_api: bool,   // wire: experimentalApi
    ...
}
```

Enforcement (`codex-rs/app-server/src/message_processor.rs:817-820`):

```rust
if let Some(reason) = codex_request.experimental_reason()
    && !session.experimental_api_enabled()
{
    return Err(invalid_request(experimental_required_message(reason)));
}
```

`experimental_required_message` renders `"<reason> requires experimentalApi
capability"` — the reason for `thread/start.dynamicTools` is exactly the
string in the `#[experimental(...)]` attribute above, so an unauthorized
attempt to set `dynamicTools` without opting in fails with a specific,
attributable JSON-RPC error rather than silently dropping the field.

`InitializeCapabilities.experimental_api` **is** present in `codex-codes`
0.146.4 (`experimental_api: Option<bool>`, wire `experimentalApi`) — this
part of the handshake needs no hand-rolling.

**`ExperimentalFeatureListParams`/`ExperimentalFeatureEnablementSetParams` are
a different mechanism** — a named feature-flag admin API (`codex features
list`/`enable`/`disable` at the protocol level, e.g. `apps`, `memories`,
`multi_agent`), unrelated to the per-request `experimentalApi` capability
gate. Confirmed by `codex features list`: no `dynamic_tools` (or similarly
named) entry exists among the ~100 named features, so dynamic tools are not
reachable through that surface either. Do not conflate the two.

## 3. Tool-error shape for `item/tool/call`

Confirmed: **`success: false` with `contentItems`, not a JSON-RPC error.**
Source (`codex-rs/app-server/src/dynamic_tools.rs` @ `rust-v0.146.0`,
`fallback_response`, used for every decode/transport failure path):

```rust
fn fallback_response(message: &str) -> (DynamicToolCallResponse, Option<String>) {
    (
        DynamicToolCallResponse {
            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                text: message.to_string(),
            }],
            success: false,
        },
        Some(message.to_string()),
    )
}
```

`DynamicToolCallParams`/`DynamicToolCallResponse` are **not**
`#[experimental]`-gated (no attribute on the struct or its fields in
`codex-rs/app-server-protocol/src/protocol/v2/item.rs:1534-1552`) — only the
*declaration* surface (`dynamicTools` on `thread/start`) is gated, not the
resulting `item/tool/call` traffic. Both types are present, unmodified, in
`codex-codes` 0.146.4.

## 4. `outputSchema` on `TurnStartParams`

**`TurnStartParams.outputSchema: Option<JsonValue>`** — arbitrary JSON Schema,
not gated (`codex-rs/app-server-protocol/src/protocol/v2/turn.rs:143-146`,
no `#[experimental]` attribute):

```rust
/// Optional JSON Schema used to constrain the final assistant message for
/// this turn.
#[ts(optional = nullable)]
pub output_schema: Option<JsonValue>,
```

Present in `codex-codes` 0.146.4's generated `TurnStartParams`
(`src/protocol_generated/types.rs:7797`, `rename = "outputSchema"`) — no
hand-rolling needed for this one.

## Summary: what needs hand-rolled types vs. what codex-codes covers

| Surface | codex-codes 0.146.4 | Action |
|---|---|---|
| `initialize` + `capabilities.experimentalApi` | ✅ typed | use as-is |
| `thread/start` (all fields except `dynamicTools`) | ✅ typed | use as-is |
| `thread/start.dynamicTools` + `DynamicToolSpec`/`DynamicToolFunctionSpec`/`DynamicToolNamespaceSpec`/`DynamicToolNamespaceTool` | ❌ absent | hand-roll in `backend::codex`, send via raw `request()` |
| `item/tool/call` (`DynamicToolCallParams`/`Response`) | ✅ typed | use as-is |
| `turn/start.outputSchema` | ✅ typed | use as-is |

The patch-level skew this directory's README calls out (CLI `0.146.0` vs.
crate `0.146.4`) is not the cause of this gap — the gap is that the crate's
type generation drops fields the schema itself omits for experimental gating,
at every version. A future crate bump will not fix this; the field needs
hand-rolling for as long as `dynamicTools` stays experimental.
