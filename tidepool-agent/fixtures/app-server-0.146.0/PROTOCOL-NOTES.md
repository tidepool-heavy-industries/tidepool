# Protocol truth — app-server 0.146.0 / codex-codes 0.146.4

PRD 18 phase 2 (`plans/post-restart/agent-lanes/dev-adapter-bringup.md`).
Answered entirely offline: from this directory's committed JSON Schema, the
pinned `codex-codes` 0.146.4 crate's generated types
(`~/.cargo/registry/src/*/codex-codes-0.146.4/src/protocol_generated/`), and
the `openai/codex` CLI source at git tag `rust-v0.146.0` (the tag matching
`codex --version` → `codex-cli 0.146.0`). No app-server process was started
and no ChatGPT token was spent producing this file.

**Caveat that governs how to read every finding below: the generated JSON
Schema in this directory is not a complete picture of the protocol.**
`codex app-server generate-json-schema` (and whatever produced `codex-codes`'
generated types) silently drops fields marked `#[experimental(...)]` in the
CLI's own source — it does not mark them absent-but-gated, it omits them
entirely. Reading the schema (or the crate) as the full protocol surface
leads to the wrong conclusion that dynamic tools do not exist at 0.146.0.
They do; they're just not discoverable from the schema. Section 1 below is
the concrete case. Anywhere this document says a field or type is "present"
or "typed," that claim is sourced from the CLI's own Rust source at the
pinned tag, not from the schema alone — the schema was the starting point,
never the last word.

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

## Finding: what `codex-codes` is actually doing for us, on the critical path

Not a vendor-or-keep call — that's root's and the TL's, on evidence, not on
the fact that the dependency is already added. This is the evidence.

PRD 18's own rule: *"At the first material protocol or maintenance problem,
vendor the required client code or replace it with a small Tidepool-owned
Tokio stdio/JSONL adapter."* The dynamicTools gap above is arguably exactly
that kind of problem — it sits on the one experimental surface the whole
adapter exists to drive — so it's worth naming precisely what the dependency
buys before deciding whether it's still earning its keep.

**What `codex-codes` provides that this crate actually uses, on the vertical
core's critical path:**
- Process lifecycle: `AppServerBuilder` (binary resolution via `which`,
  argument construction, stdio piping) and version-check preflight
  (`check_codex_version_async`, harmless `codex --version` call, warn-only on
  skew — confirmed non-fatal even against a newer CLI).
- Raw framing: `RawAsyncClient` — newline-delimited JSON read/write over the
  child's stdio, with a 10MB stdout buffer and a background stderr drain so
  the app-server's ~200KB/s tracing output doesn't block it on a full pipe.
  This crate uses ONLY this raw layer for phase 3/4, not the higher-level
  `AsyncClient`, precisely because frame capture and the concurrent
  turn/tool-call/completion read loop needed hand-written control flow either
  way.
- JSON-RPC envelope types: `JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcError`/
  `JsonRpcNotification`/`JsonRpcMessage`/`RequestId` — thin, ~120 lines
  upstream (`src/jsonrpc.rs`), no correlation logic beyond the enum shape;
  this crate does its own id-matching in `Session::request`/`drive_turn`
  rather than using the crate's `AsyncClient::request` correlation loop
  (needed to, per the dynamicTools gap and the concurrent-read requirement
  above).
- Generated wire types for the STABLE surface: `InitializeParams`/
  `InitializeCapabilities`, all of `ThreadStartParams` except `dynamicTools`,
  all of `TurnStartParams` including `outputSchema`, `DynamicToolCallParams`/
  `DynamicToolCallResponse`, `Turn`/`ThreadItem`/`TurnStatus`,
  `TurnCompletedNotification`, `SandboxPolicy`, `UserInput`,
  `AbsolutePathBuf`. This is the bulk of what's actually load-bearing: several
  thousand lines of generated serde structs this crate did not write and does
  not want to maintain by hand.

**What it does NOT provide on the critical path:** any type or field on the
experimental surface (`dynamicTools` and its four supporting types, confirmed
above), and no correlation/dispatch logic this crate actually calls (built its
own instead, for reasons independent of the gap).

**Net:** on the vertical core specifically, `codex-codes` is buying transport
convenience (process spawn + raw framing + stderr handling) and a large body
of generated STABLE-surface types, at the cost of one gap on the one
experimental surface this adapter's reason for existing depends on — a gap
that requires hand-rolling four small types now and staying alert to it on
every future CLI bump, since it will not self-heal with a crate version bump.
Whether that trade is worth vendoring instead is root's call; this crate has
not needed anything from `codex-codes` beyond what's listed above.
