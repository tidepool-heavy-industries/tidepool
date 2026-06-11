# Diagnostics Flow Reconnaissance

Map of GHC diagnostics flow from `tidepool-extract` through the Rust runtime to MCP eval results, with identifying patch points for improvements.

## 1. Flow Diagram

### Compile FAILURE Path
1. **GHC (Haskell):** `runPipeline` in `haskell/src/Tidepool/GhcPipeline.hs` executes the GHC API.
2. **Extraction Binary (`tidepool-extract`):** `haskell/app/Main.hs` catches `SourceError` (line 173). Detailed error text is printed to `stderr` by GHC's default log action.
3. **Rust Runtime:** `compile_haskell` in `tidepool-runtime/src/lib.rs` (line 140) captures `stderr` and returns `CompileError::ExtractFailed(String)` (line 157).
4. **MCP Eval Tool:** The eval thread in `tidepool-mcp/src/lib.rs` (line 2153) catches the error and sends `SessionMessage::Error`.
5. **Formatting:** `format_error_with_source` in `tidepool-mcp/src/lib.rs` (line 1064) appends the user code section from the full `source` string to the error message.
6. **Result:** The MCP client receives a `CallToolResult` with the formatted error.

### Compile SUCCESS Path
1. **Haskell:** `runPipeline` completes successfully.
2. **Extraction Binary:** `encodeMetadata` in `haskell/src/Tidepool/CborEncode.hs` (line 82) writes `meta.cbor`. Currently only `hasIO` is included in the warnings map.
3. **Rust Runtime:** `read_metadata` in `tidepool-repr/src/serial/read.rs` (line 80) parses `meta.cbor` into `MetaWarnings`.
4. **Rust Runtime:** `compile_haskell` (line 170) returns `(expr, table, warnings)`.
5. **Eval Tool:** `compile_and_run` in `tidepool-runtime/src/lib.rs` (line 186) checks `warnings.has_io` but otherwise **discards** the warnings.
6. **Result:** MCP result contains only the evaluated value, with no visibility into GHC warnings.

---

## 2. Patch Points

### (a) Surfacing Success-Path Warnings
Goal: Include GHC warnings (e.g., `-Wincomplete-patterns`, name shadowing) in successful eval results.

| Component | File:Line | Change Description |
|-----------|-----------|--------------------|
| **Haskell Pipeline** | `haskell/src/Tidepool/GhcPipeline.hs`:100 | Collect warnings from `HscEnv` after `load`. Add `prWarnings :: [Text]` to `PipelineResult`. |
| **Haskell Main** | `haskell/app/Main.hs`:153 | Pass collected warnings to `encodeMetadata`. |
| **Haskell CBOR** | `haskell/src/Tidepool/CborEncode.hs`:86 | Update `encodeMetadata` to include `warnings` field in the CBOR map. |
| **Rust Repr** | `tidepool-repr/src/serial/read.rs`:74 | Add `warnings: Vec<String>` to `MetaWarnings` struct and update `parse_warnings`. |
| **Rust Runtime** | `tidepool-runtime/src/render.rs`:14 | Add `warnings: Vec<String>` to `EvalResult` struct and its `new()` method. |
| **Rust Runtime** | `tidepool-runtime/src/lib.rs`:195 | Pass warnings from `compile_haskell` to `EvalResult::new()`. |
| **MCP Server** | `tidepool-mcp/src/lib.rs`:1370 | In `SessionMessage::Completed`, include warnings in the result string. |

**Risks:**
- **Warning Noise:** High-volume warnings could clutter LLM context. May need a flag or threshold.
- **CBOR Stability:** Adding a field to the map is backward compatible, but older runtimes will ignore it.

**Estimate:** Small (1-2 days).

---

## 3. Patch Points (b) Rebasing Compile-Error Line Numbers
Goal: Subtract the preamble/wrapper line offset from GHC error coordinates to point at user-written lines.

| Component | File:Line | Change Description |
|-----------|-----------|--------------------|
| **MCP Server** | `tidepool-mcp/src/lib.rs`:1004 | In `template_haskell`, calculate and return the line offsets for `helpers` and `code`. |
| **MCP Server** | `tidepool-mcp/src/lib.rs`:1064 | Update `format_error_with_source` to accept line offsets. |
| **MCP Server** | `tidepool-mcp/src/lib.rs`:1075 | Regex-replace `/Expr.hs:(\d+):` in error text with rebased line numbers based on offsets. |

**Rebase Arithmetic:**
- `Preamble Offset`: Line count of everything before `-- [user]\n`.
- `Code Offset`: Line count of everything before `__user =\n` + 1.
- Error at line $L$ in `Expr.hs`:
  - If $L > \text{Code Offset}$: Reported line $= L - \text{Code Offset} + 1$ (relative to `code` block).
  - If $L > \text{Preamble Offset}$ and $L < \text{Code Offset}$: Reported line $= L - \text{Preamble Offset} + 1$ (relative to `helpers` block).

**Risks:**
- **Indentation:** `template_haskell` indents the `code` block by 2 spaces. If GHC reports a column, it will be offset by 2.
- **Multi-error parsing:** Regex must be robust to GHC's multi-line error format (handles "at line X, column Y" and "Expr.hs:X:Y").

**Estimate:** Medium (2-3 days).

---

## 4. Line Count Analysis (Reference)

Current `build_preamble` (approximate lines):
- Pragmas: 1
- Module header: 1
- Standard imports: 11
- User library: 1 (optional)
- Prelude/Default: 2
- Pagination helpers: ~80
- Effect helpers: ~50
- **Total Preamble:** ~145 lines + `imports` count.

User `helpers` follow immediately after `-- [user]\n`.
`input` injection (if any) adds `2 + lines(json)` lines.
`__user =` adds 1 line.
`code` follows.
