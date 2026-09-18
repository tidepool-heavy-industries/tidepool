//! Frontend-neutral mechanics for a resident Haskell workbench.
//!
//! This module owns the pieces every resident frontend needs before it can
//! apply its own policy: compiler-prepared cell requests, operator command tokenization,
//! and prefix-preserving ordered execution. It deliberately does not know
//! about MCP response shapes, actor conversations, effect settlement, or
//! presentation.

use std::future::Future;

use schemars::JsonSchema;
use serde::Serialize;

use super::turn::CellSourceSpan;
use super::{
    assemble_bind_module, assemble_display_expression_module, assemble_opaque_expression_module,
    insert_preamble_imports, ExpressionLift, TemplateSelector, TurnTemplate, DECL_TEMPLATE_SOURCE,
};

/// Normalize the one extra JSON-string layer some MCP clients apply to a
/// structured tool argument. Plain strings remain strings unless they parse
/// as a complete JSON object, array, or quoted JSON string. Numeric and
/// boolean-looking strings remain text.
#[must_use]
pub fn normalize_workbench_input(value: &serde_json::Value) -> serde_json::Value {
    let serde_json::Value::String(encoded) = value else {
        return value.clone();
    };
    match serde_json::from_str::<serde_json::Value>(encoded) {
        Ok(
            decoded @ (serde_json::Value::Object(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::String(_)),
        ) => decoded,
        _ => value.clone(),
    }
}

/// Render the optional transport input as the canonical @input ::
/// Aeson.Value@ source binding used by every resident workbench frontend.
#[must_use]
pub fn workbench_input_binding(input: Option<&serde_json::Value>) -> String {
    input.map_or_else(String::new, |value| {
        format!(
            "input :: Aeson.Value\ninput = {}\n\n",
            workbench_json_to_haskell(value)
        )
    })
}

pub fn escape_workbench_haskell_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\t' => output.push_str("\\t"),
            '\r' => output.push_str("\\r"),
            character if character.is_control() => {
                output.push_str(&format!("\\x{:x}\\&", character as u32));
            }
            character => output.push(character),
        }
    }
    output
}

pub fn workbench_json_to_haskell(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "Aeson.Null".into(),
        serde_json::Value::Bool(value) => {
            format!("Aeson.Bool {}", if *value { "True" } else { "False" })
        }
        serde_json::Value::Number(value) => {
            let (coefficient, exponent) =
                tidepool_bridge::shapes::parse_decimal_token(&value.to_string());
            format!("Aeson.Number (Aeson.scientific ({coefficient}) ({exponent}))")
        }
        serde_json::Value::String(value) => {
            format!(
                "Aeson.String \"{}\"",
                escape_workbench_haskell_string(value)
            )
        }
        serde_json::Value::Array(values) => format!(
            "toJSON [{}]",
            values
                .iter()
                .map(workbench_json_to_haskell)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        serde_json::Value::Object(values) => format!(
            "object [{}]",
            values
                .iter()
                .map(|(key, value)| format!(
                    "\"{}\" .= {}",
                    escape_workbench_haskell_string(key),
                    workbench_json_to_haskell(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Exact hosted invocation coordinates supplied by the trusted transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchForkBoundary {
    pub thread_id: String,
    pub call_id: String,
}

/// One ordered request against a persistent Haskell workbench.
///
/// MCP, provider-native fenced execution, tests, and other frontends share
/// item sequencing and input mounting through this transport-neutral value.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkbenchRequest {
    /// Compiler-prepared cell items or one trusted hosted tool invocation.
    pub items: Vec<String>,
    /// Optional structured payload mounted as @input :: Aeson.Value@.
    pub input: Option<serde_json::Value>,
    /// Request the frontend's expanded diagnostic receipt when supported.
    pub verbose: Option<bool>,
    /// Runtime-minted identity for one exact hosted tool call.
    ///
    /// This is not accepted from JSON. The transport owner derives it from
    /// authenticated call coordinates before actor dispatch, allowing the
    /// actor to return a committed receipt when that exact call is retried.
    execution_id: Option<WorkbenchExecutionId>,
    /// Trusted transport coordinates; never accepted from authored JSON.
    fork_boundary: Option<WorkbenchForkBoundary>,
    /// Trusted named-handler selection; arguments are data, never Haskell source.
    tool_call: Option<WorkbenchToolCall>,
    /// Raw notebook cell awaiting GHC split/classify/preflight in the owning
    /// actor session.
    cell_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbenchToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl WorkbenchRequest {
    pub fn for_tool(name: String, arguments: serde_json::Value) -> Self {
        Self {
            items: vec![name.clone()],
            input: None,
            verbose: None,
            execution_id: None,
            fork_boundary: None,
            tool_call: Some(WorkbenchToolCall { name, arguments }),
            cell_source: None,
        }
    }

    pub fn tool_call(&self) -> Option<&WorkbenchToolCall> {
        self.tool_call.as_ref()
    }

    #[must_use]
    pub fn from_cell_input(source: &str) -> Self {
        Self {
            items: Vec::new(),
            input: None,
            verbose: None,
            execution_id: None,
            fork_boundary: None,
            tool_call: None,
            cell_source: Some(source.to_owned()),
        }
    }

    #[must_use]
    pub fn cell_source(&self) -> Option<&str> {
        self.cell_source.as_deref()
    }

    pub fn install_cell_items(&mut self, items: Vec<String>) {
        self.items = items;
        self.cell_source = None;
    }

    #[must_use]
    pub fn with_execution_id(mut self, execution_id: WorkbenchExecutionId) -> Self {
        self.execution_id = Some(execution_id);
        self
    }

    #[must_use]
    pub fn execution_id(&self) -> Option<&WorkbenchExecutionId> {
        self.execution_id.as_ref()
    }

    #[must_use]
    pub fn with_fork_boundary(mut self, boundary: WorkbenchForkBoundary) -> Self {
        self.fork_boundary = Some(boundary);
        self
    }

    #[must_use]
    pub fn fork_boundary(&self) -> Option<&WorkbenchForkBoundary> {
        self.fork_boundary.as_ref()
    }
}

/// Opaque, stable identity of one authenticated hosted workbench call.
///
/// The transport hashes structured execution coordinates into this value; no
/// behavior parses the rendered digest back into authority or control flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct WorkbenchExecutionId(String);

impl WorkbenchExecutionId {
    #[must_use]
    pub fn from_digest(digest: [u8; 16]) -> Self {
        Self(format!("exec-{}", hex_bytes(&digest)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkbenchExecutionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        rendered.push(HEX[usize::from(byte >> 4)] as char);
        rendered.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    rendered
}

/// Stable coordinate of one effect boundary within a hosted input unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchOperationId {
    pub execution: WorkbenchExecutionId,
    pub input_unit_index: usize,
    pub effect_ordinal: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchOperationDisposition {
    /// The owner reserved state whose publication is coupled to the input
    /// unit's commit boundary (currently an applicative unfold frontier).
    Prepared,
    Committed,
    Rejected,
    /// The effect owner failed after dispatch without proving whether its
    /// mutation crossed the commit point. Retrying the enclosing hosted call
    /// returns this receipt rather than guessing and running the unit again.
    Unknown,
}

/// Which layer produced an item's failure, kept as data instead of leaving a
/// reader to infer it from `output`'s prose.
///
/// `Compile` never reached an effect at all: the cell or declaration was
/// rejected before anything ran. `Effect` covers an effect that itself
/// failed, or a unit that ended before every effect it started crossed its
/// commit point (an incomplete fork-group admission, a handler failure).
/// `Observation` is narrower and more reassuring than either: every effect
/// this unit ran already committed, and only materializing the result
/// afterward failed — a bound name, if any, is sound and the failure is
/// about display, not about what happened.
///
/// `None` on the receipt (not a variant here) covers an ordinary
/// program-language fault that never reached either boundary (a pattern
/// match failure, a case trap), and any failure this classification does
/// not yet cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchFailureLayer {
    Compile,
    Effect,
    Observation,
}

/// One effect boundary observed while evaluating an input unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchOperationReceipt {
    pub id: WorkbenchOperationId,
    /// Open effect rows make the operation vocabulary extensible. This name
    /// is diagnostic metadata only and never drives behavior.
    pub effect: String,
    pub disposition: WorkbenchOperationDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchTerminalTransfer {
    CommandBackgrounded,
    ReplyAccepted,
    CancellationAcknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchItemStatus {
    Committed,
    /// Execution ended before returning; recovery bindings may still be installed.
    Stopped,
    Diagnostic,
    Rejected,
    NotRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchCellItemKind {
    Declaration,
    Statement,
    Expression,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchCellSourceItem {
    pub ordinal: usize,
    pub kind: WorkbenchCellItemKind,
    pub span: CellSourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchItemReceipt {
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WorkbenchCellItemKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<CellSourceSpan>,
    /// Original cell items represented by this execution step. Declaration
    /// groups retain one entry per authored declaration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_items: Vec<WorkbenchCellSourceItem>,
    pub status: WorkbenchItemStatus,
    pub output: String,
    /// The compiler diagnostics behind this unit's `output` and `warnings`,
    /// kept as data: severity, the coordinate the rendered header shows, and
    /// the message body. Populated on the compile-rejection paths (cell check
    /// and declaration validation); empty for a unit that committed, was
    /// never run, or failed for a reason that is not a GHC diagnostics report
    /// at all.
    ///
    /// This never replaces `output`, and reading `output` is unaffected by
    /// its presence. It exists so a reader that needs the span, the severity,
    /// or the message on its own does not have to recover them by parsing
    /// text the compiler already handed over structured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<crate::diag::StructuredDiagnostic>,
    /// Which layer produced this item's failure, when `status` reports one
    /// (`Rejected`, or `Stopped`/`Diagnostic` for a mid-run fault). `None`
    /// for a committed/not-run item, and for a failure this classification
    /// does not cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_layer: Option<WorkbenchFailureLayer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Names installed into the persistent lexical environment by this unit.
    /// This is authoritative metadata; clients need not scrape transcript
    /// strings such as `[bound x]` or declaration summaries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub installed_bindings: Vec<String>,
    /// Effect boundaries completed before this unit returned or failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<WorkbenchOperationReceipt>,
    /// Accepted terminal transfer, when this unit intentionally cannot return
    /// a Haskell value to its caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_transfer: Option<WorkbenchTerminalTransfer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchRunStatus {
    Backgrounded,
    Committed,
    Rejected,
    Replied,
    RequestCancelled,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchResponse {
    pub status: WorkbenchRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub items: Vec<WorkbenchItemReceipt>,
    pub next_index: usize,
    pub total: usize,
}

/// One term-level name visible in a persistent Haskell workbench.
///
/// Declaration-backed values have no stored type display and carry the exact
/// GHC expression to inspect. Materialized values already retain their
/// compiler-produced type. Neither case requires forcing the live value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchBinding {
    pub name: String,
    pub type_display: Option<String>,
    pub kind: WorkbenchBindingKind,
    type_query: Option<String>,
    /// The session generation (`DeclLog` turn, or the `Val` module's own
    /// generation) whose commit currently defines this name — the same
    /// identity `current_declarations_in`/`current_binding_in` already
    /// track for latest-wins shadowing. `None` only if the caller built this
    /// value without going through [`Self::declaration`]/[`Self::materialized`]
    /// plus [`Self::with_generation`].
    defining_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchBindingKind {
    Declaration,
    MaterializedValue,
}

impl WorkbenchBindingKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::MaterializedValue => "live value",
        }
    }
}

impl WorkbenchBinding {
    /// Public so a downstream crate's status-rendering tests (there is no
    /// invariant here to protect: this is a plain data constructor) can
    /// build a value without a live session.
    #[must_use]
    pub fn declaration(name: String, type_query: String) -> Self {
        Self {
            name,
            type_display: None,
            kind: WorkbenchBindingKind::Declaration,
            type_query: Some(type_query),
            defining_generation: None,
        }
    }

    /// Public for the same reason as [`Self::declaration`].
    #[must_use]
    pub fn materialized(name: String, type_display: Option<String>) -> Self {
        Self {
            name,
            type_display,
            kind: WorkbenchBindingKind::MaterializedValue,
            type_query: None,
            defining_generation: None,
        }
    }

    /// Attach the generation whose commit currently defines this binding.
    #[must_use]
    pub fn with_generation(mut self, generation: Option<u64>) -> Self {
        self.defining_generation = generation;
        self
    }

    #[must_use]
    pub fn type_query(&self) -> Option<&str> {
        self.type_query.as_deref()
    }

    /// The defining session generation, when known. This is the "cell
    /// execution" identity a what-is-live status view renders beside a
    /// binding: the same latest-wins turn `current_declarations_in`/
    /// `current_binding_in` already resolve, not a newly tracked id.
    #[must_use]
    pub fn defining_generation(&self) -> Option<u64> {
        self.defining_generation
    }
}

/// One tokenized `:command`. Frontends interpret the name and arguments they
/// own; tokenization itself has one implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaCommandLine {
    pub name: String,
    pub arguments: String,
}

/// The discovery commands shared by every persistent Haskell workbench.
///
/// Frontends may add operational commands of their own, but these names and
/// argument contracts are part of the GHCi-shaped language rather than an MCP
/// or actor adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkbenchDiscovery {
    Type(String),
    Info(String),
    ShowImports,
    Browse {
        module: Option<String>,
        expanded: bool,
    },
    Bindings,
    Recovery,
    Doc(String),
}

impl MetaCommandLine {
    /// Parse a command with an optional leading colon.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let command = trimmed.strip_prefix(':').unwrap_or(trimmed);
        if command.is_empty() || command.starts_with([' ', '\t']) || command.contains(['\r', '\n'])
        {
            return Err("invalid workbench command: expected one command line".into());
        }
        let boundary = command.find([' ', '\t']).unwrap_or(command.len());
        Ok(Self {
            name: command[..boundary].to_owned(),
            arguments: command[boundary..].trim().to_owned(),
        })
    }

    /// Interpret this line when it names the common discovery subset.
    pub fn discovery(&self) -> Result<Option<WorkbenchDiscovery>, String> {
        let required = |command: &str| {
            if self.arguments.is_empty() {
                Err(format!(":{command} requires an argument"))
            } else {
                Ok(self.arguments.clone())
            }
        };
        match self.name.as_str() {
            "t" | "type" => required("type").map(WorkbenchDiscovery::Type).map(Some),
            "i" | "info" => required("info").map(WorkbenchDiscovery::Info).map(Some),
            "show" => match self.arguments.as_str() {
                "imports" => Ok(Some(WorkbenchDiscovery::ShowImports)),
                "" => Err(":show requires an argument (supported: :show imports)".into()),
                unsupported => Err(format!(
                    ":show does not support `{unsupported}` (supported: :show imports)"
                )),
            },
            "browse" | "browse!" => {
                let expanded = self.name == "browse!";
                let module = (!self.arguments.is_empty()).then(|| self.arguments.clone());
                if module
                    .as_deref()
                    .is_some_and(|module| module.starts_with('*'))
                {
                    Err(
                        ":browse *Module is unavailable because actor policy modules are compiled"
                            .into(),
                    )
                } else if module
                    .as_deref()
                    .is_some_and(|module| module.split_whitespace().count() != 1)
                {
                    Err(":browse accepts at most one module name".into())
                } else {
                    Ok(Some(WorkbenchDiscovery::Browse { module, expanded }))
                }
            }
            "bindings" | "b" => {
                if self.arguments.is_empty() {
                    Ok(Some(WorkbenchDiscovery::Bindings))
                } else {
                    Err(":bindings does not accept arguments".into())
                }
            }
            "recovery" => {
                if self.arguments.is_empty() {
                    Ok(Some(WorkbenchDiscovery::Recovery))
                } else {
                    Err(":recovery does not accept arguments".into())
                }
            }
            "doc" => required("doc").map(WorkbenchDiscovery::Doc).map(Some),
            _ => Ok(None),
        }
    }

    /// Whether failure of this command is itself only an observation.
    ///
    /// A misspelled name or unsupported argument must not prevent later
    /// independent inspection from running. Commands outside this closed set
    /// may acquire mutation semantics in a frontend and therefore retain the
    /// ordinary reject-and-stop boundary.
    #[must_use]
    pub fn is_observational(&self) -> bool {
        matches!(
            self.name.as_str(),
            "status"
                | "status!"
                | "lineage"
                | "trace"
                | "t"
                | "type"
                | "i"
                | "info"
                | "show"
                | "browse"
                | "browse!"
                | "bindings"
                | "b"
                | "recovery"
                | "doc"
        )
    }
}

/// The policy-free lexical shape of one workbench item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkbenchItem {
    /// A declaration form whose leading keyword makes it unambiguous.
    Declaration(String),
    /// Haskell requiring GHC classification as a declaration, bind, or
    /// expression.
    Haskell(String),
    /// A tokenized meta-command interpreted by the consuming frontend.
    Command(MetaCommandLine),
}

/// Perform only the lexical classification that is stable across resident
/// frontends. GHC remains authoritative for ambiguous Haskell.
pub fn classify_workbench_item(source: &str) -> Result<WorkbenchItem, String> {
    let source = source.trim();
    if source.is_empty() {
        return Ok(WorkbenchItem::Declaration(String::new()));
    }
    if source.starts_with(':') {
        return MetaCommandLine::parse(source).map(WorkbenchItem::Command);
    }

    const DECLARATION_PREFIXES: &[&str] = &[
        "data ",
        "newtype ",
        "type ",
        "class ",
        "instance ",
        "infixl ",
        "infixr ",
        "infix ",
        "foreign ",
        "import ",
        "default ",
        "{-# ",
    ];
    if DECLARATION_PREFIXES
        .iter()
        .any(|prefix| source.starts_with(prefix))
    {
        Ok(WorkbenchItem::Declaration(source.to_string()))
    } else {
        Ok(WorkbenchItem::Haskell(source.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Pre-GHC source-order detection (stage one only — NOT source-order
// execution, which is a later experiment).
//
// A notebook cell is split into units and its declaration-kind units are
// hoisted above its statement-kind units once GHC assembles the checked
// module, so a declaration written after a statement cannot see that
// statement's bindings. Left alone this either fails with a confusing GHC
// error, or — worse — silently resolves the same bare name to something else
// already in scope (an import). [`detect_hoisted_declaration_collision`]
// catches the shape lexically, before the cell ever reaches GHC.
//
// This is a SOURCE-LEVEL APPROXIMATION, not a real parse, and deliberately
// not built on `tidepool_repr::free_vars`: that engine computes free
// variables over `CoreExpr`, GHC's own post-typecheck Core, which does not
// exist yet at this point in the pipeline (a raw notebook cell is exactly
// what GHC has not seen). The approximation below is biased throughout
// toward a MISSED detection over a FALSE rejection: ambiguous shapes (a
// pattern-binding LHS, an operator definition, anything inside a
// pragma/import/data/class/instance header) fall through to `Other` and are
// never flagged.
// ---------------------------------------------------------------------------

/// A pre-GHC-detected hazard: a cell's declaration unit references a name
/// that an EARLIER statement unit in the same cell binds. See
/// [`detect_hoisted_declaration_collision`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOrderCollision {
    pub declaration_name: String,
    pub declaration_line: usize,
    pub binder_name: String,
    pub statement_line: usize,
}

impl SourceOrderCollision {
    /// Model-facing rejection text: names the declaration, the binding it
    /// appears to want, why it can't see it, and the two ways to fix it.
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "declaration `{decl}` (line {decl_line}) uses `{binder}`, but `{binder}` is bound \
             by an earlier statement in this cell (line {stmt_line}). This cell's declarations \
             are hoisted above its statements, so `{decl}` cannot see `{binder}` there. If that \
             is what this failure is about, put `{decl}`'s declaration \
             before the statement that binds `{binder}`, or write it as `let {decl} = ...` \
             inside a statement instead of a top-level declaration.",
            decl = self.declaration_name,
            decl_line = self.declaration_line,
            binder = self.binder_name,
            stmt_line = self.statement_line,
        )
    }
}

/// Detect a same-cell declaration/statement source-order hazard by lexical
/// scan, without invoking GHC. Returns the FIRST collision found in source
/// order (a declaration's free name reaching an earlier statement's binder);
/// later collisions in the same cell are left for the next round after the
/// first is fixed.
#[must_use]
pub fn detect_hoisted_declaration_collision(cell_source: &str) -> Option<SourceOrderCollision> {
    let units = split_source_units(cell_source);
    let mut statement_binders: Vec<(&str, usize)> = Vec::new();
    for unit in &units {
        match classify_source_unit(&unit.text) {
            SourceUnitShape::Statement { binders } => {
                for binder in binders {
                    statement_binders.push((binder, unit.start_line));
                }
            }
            SourceUnitShape::Declaration { name, params, body } => {
                let bound_locally: std::collections::HashSet<&str> = params
                    .into_iter()
                    .chain(std::iter::once(name))
                    .chain(locally_rebound_names(body))
                    .collect();
                for free_name in lowercase_identifier_tokens(body) {
                    if bound_locally.contains(free_name) {
                        continue;
                    }
                    if let Some(&(binder, statement_line)) = statement_binders
                        .iter()
                        .find(|(binder, _)| *binder == free_name)
                    {
                        return Some(SourceOrderCollision {
                            declaration_name: name.to_string(),
                            declaration_line: unit.start_line,
                            binder_name: binder.to_string(),
                            statement_line,
                        });
                    }
                }
            }
            SourceUnitShape::Other => {}
        }
    }
    None
}

/// One line-based approximation of a cell's top-level lexical unit: starts at
/// column 1, and gathers any immediately following indented lines as
/// continuations. This is NOT GHC's own layout algorithm — the compiler's
/// real split happens only during the whole-cell check (`CellAnalysisItem`,
/// in `super::turn`) — but for cells written the way this harness's models
/// actually write them (one statement/declaration per column-1 line) it
/// matches exactly, and where it doesn't, the failure mode is to merge lines
/// into one bigger unit rather than invent a boundary, so no unit is ever
/// split in a way that could create a false collision.
struct SourceUnit {
    start_line: usize,
    text: String,
}

fn split_source_units(cell_source: &str) -> Vec<SourceUnit> {
    let mut units: Vec<SourceUnit> = Vec::new();
    for (offset, raw_line) in cell_source.lines().enumerate() {
        let line_no = offset + 1;
        let without_comment = strip_line_comment(raw_line);
        if without_comment.trim().is_empty() {
            continue;
        }
        let is_top_level = !raw_line.starts_with(' ') && !raw_line.starts_with('\t');
        if is_top_level || units.is_empty() {
            units.push(SourceUnit {
                start_line: line_no,
                text: without_comment.trim_end().to_string(),
            });
        } else {
            let Some(last) = units.last_mut() else {
                continue;
            };
            last.text.push('\n');
            last.text.push_str(without_comment.trim_end());
        }
    }
    units
}

/// Cut a `--` line comment, respecting a double-quoted string literal so a
/// `--` inside one is never mistaken for a comment marker.
fn strip_line_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = line.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '-' if chars.peek().map(|&(_, next)| next) == Some('-') => return &line[..idx],
            _ => {}
        }
    }
    line
}

/// The policy-free lexical shape of one source unit, for
/// [`detect_hoisted_declaration_collision`] only — deliberately separate from
/// [`WorkbenchItem`], which is the real (GHC-authoritative-pending)
/// classification every frontend uses to dispatch a unit; this one exists
/// only to drive a conservative, local, pre-GHC heuristic.
enum SourceUnitShape<'a> {
    /// A monadic bind (`pat <- expr`) or a `let pat = expr` statement. Not
    /// hoisted — sequential, so a later unit CAN see its binder(s).
    Statement { binders: Vec<&'a str> },
    /// A top-level function/pattern binding (`name args.. = body`). Hoisted
    /// above every statement once GHC assembles the checked cell.
    Declaration {
        name: &'a str,
        params: Vec<&'a str>,
        body: &'a str,
    },
    /// A pragma/import/data/class/instance header, a bare expression
    /// statement (no binder), or any LHS shape ambiguous enough that
    /// misreading it risks a false rejection (e.g. a tuple or constructor
    /// pattern binding). Not interesting to this check.
    Other,
}

/// Mirrors `classify_workbench_item`'s `DECLARATION_PREFIXES` list, kept as
/// its own copy so this heuristic stays in its own region of the file.
const CONSERVATIVE_DECLARATION_PREFIXES: &[&str] = &[
    "data ", "newtype ", "type ", "class ", "instance ", "infixl ", "infixr ", "infix ",
    "foreign ", "import ", "default ", "{-# ",
];

fn classify_source_unit(text: &str) -> SourceUnitShape<'_> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return SourceUnitShape::Other;
    }
    if CONSERVATIVE_DECLARATION_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return SourceUnitShape::Other;
    }
    if let Some(rest) = trimmed.strip_prefix("let ") {
        let pattern = split_at_top_level_eq(rest).map_or(rest, |(lhs, _)| lhs);
        return SourceUnitShape::Statement {
            binders: lowercase_identifier_tokens(pattern),
        };
    }
    // Only a bind on the unit's own first line makes it a statement. A `<-`
    // further down belongs to a nested `do` inside a declaration's body, and
    // reading it as this unit's bind would both misclassify the declaration
    // and publish its head as a statement binder — which a later declaration
    // referring to that helper would then collide with.
    let first_line = trimmed.split_once('\n').map_or(trimmed, |(line, _)| line);
    if let Some((pattern, _)) = split_at_top_level(first_line, "<-") {
        return SourceUnitShape::Statement {
            binders: lowercase_identifier_tokens(pattern),
        };
    }
    if let Some((head, body)) = split_at_top_level_eq(trimmed) {
        let head_trimmed = head.trim_start();
        if head_trimmed.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
            let mut head_tokens = lowercase_identifier_tokens(head).into_iter();
            if let Some(name) = head_tokens.next() {
                let params: Vec<&str> = head_tokens.collect();
                return SourceUnitShape::Declaration { name, params, body };
            }
        }
    }
    SourceUnitShape::Other
}

/// Find the first occurrence of `needle` at paren/bracket/brace nesting
/// depth 0 and outside a string literal.
fn split_at_top_level<'a>(text: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut idx = 0usize;
    while idx < text.len() {
        let Some(c) = text[idx..].chars().next() else {
            break;
        };
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            idx += c.len_utf8();
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if depth == 0 && text[idx..].starts_with(needle) {
            return Some((&text[..idx], &text[idx + needle.len()..]));
        }
        idx += c.len_utf8();
    }
    None
}

/// Like [`split_at_top_level`] specialized to a bare assignment `=`: skips
/// `==`, `/=`, `<=`, `>=`, and `=>` so a comparison or constraint arrow is
/// never mistaken for a binding.
fn split_at_top_level_eq(text: &str) -> Option<(&str, &str)> {
    let indices: Vec<(usize, char)> = text.char_indices().collect();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (position, &(byte_idx, c)) in indices.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                let prev = position.checked_sub(1).map(|prior| indices[prior].1);
                let next = indices.get(position + 1).map(|&(_, c)| c);
                let composite = matches!(prev, Some('=' | '/' | '<' | '>' | '!'))
                    || matches!(next, Some('=' | '>'));
                if !composite {
                    let after = byte_idx + c.len_utf8();
                    return Some((&text[..byte_idx], &text[after..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Every name a declaration's own body rebinds, so a reference to one of them
/// is local rather than a reach back at an earlier statement's binder.
///
/// A body may introduce names four ways, and all of them shadow: `let x = …`,
/// a `where` clause's own equations, a lambda's parameters, and a nested `do`
/// block's `x <- …`. None of these is visible to the unit classifier, which
/// sees only the declaration's head. Missing them is what turns an ordinary
/// reuse of a short name like `task` into a rejection of valid source, so this
/// is deliberately generous: a name that appears bound anywhere in the body is
/// treated as bound throughout it. Over-collecting here can only withhold a
/// rejection, never invent one, which is the bias this detector wants.
fn locally_rebound_names(body: &str) -> Vec<&str> {
    let mut bound = Vec::new();
    for (offset, _) in body.match_indices("let ") {
        let rest = &body[offset + "let ".len()..];
        let pattern = split_at_top_level_eq(rest).map_or(rest, |(lhs, _)| lhs);
        bound.extend(lowercase_identifier_tokens(pattern));
    }
    for (offset, _) in body.match_indices("where") {
        bound.extend(lowercase_identifier_tokens(&body[offset + "where".len()..]));
    }
    for (offset, _) in body.match_indices('\\') {
        let rest = &body[offset + 1..];
        if let Some((parameters, _)) = rest.split_once("->") {
            bound.extend(lowercase_identifier_tokens(parameters));
        }
    }
    for (offset, _) in body.match_indices("<-") {
        let preceding = &body[..offset];
        let pattern = preceding
            .rfind('\n')
            .map_or(preceding, |line_start| &preceding[line_start + 1..]);
        bound.extend(lowercase_identifier_tokens(pattern));
    }
    bound
}

/// Every lowercase-leading identifier "root" referenced in `text`, skipping
/// string-literal contents and Haskell keywords. For a dotted run whose
/// first segment starts uppercase (a qualified reference, e.g. `Cmd.run`,
/// `Control.Lens.previews`), the run names nothing local and contributes
/// nothing; otherwise the run's first segment is the candidate — covering
/// both a bare name (`previews`) and the base of record-dot access
/// (`job.exitCode` contributes `job`). This is a lexical approximation, not
/// a parse: good enough to bias toward flagging a real collision without
/// inventing one that isn't syntactically there.
fn lowercase_identifier_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut idx = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while idx < text.len() {
        let Some(c) = text[idx..].chars().next() else {
            break;
        };
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            idx += c.len_utf8();
            continue;
        }
        if c == '"' {
            in_string = true;
            idx += c.len_utf8();
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let first_start = idx;
            let first_end = consume_identifier(text, first_start);
            let first_segment = &text[first_start..first_end];
            let starts_upper = first_segment.starts_with(|ch: char| ch.is_ascii_uppercase());
            let mut run_end = first_end;
            while text[run_end..].starts_with('.') {
                let after_dot = run_end + 1;
                let Some(next_char) = text.get(after_dot..).and_then(|s| s.chars().next()) else {
                    break;
                };
                if !(next_char.is_ascii_alphabetic() || next_char == '_') {
                    break;
                }
                run_end = consume_identifier(text, after_dot);
            }
            if !starts_upper && first_segment != "_" && !is_haskell_keyword(first_segment) {
                tokens.push(first_segment);
            }
            idx = run_end;
            continue;
        }
        idx += c.len_utf8();
    }
    tokens
}

fn consume_identifier(text: &str, start: usize) -> usize {
    let mut end = start;
    for c in text[start..].chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '\'' {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn is_haskell_keyword(token: &str) -> bool {
    matches!(
        token,
        "do" | "let"
            | "in"
            | "if"
            | "then"
            | "else"
            | "case"
            | "of"
            | "where"
            | "import"
            | "module"
            | "instance"
            | "class"
            | "data"
            | "type"
            | "newtype"
            | "deriving"
            | "infixl"
            | "infixr"
            | "infix"
            | "foreign"
            | "default"
            | "qualified"
            | "as"
            | "hiding"
            | "mdo"
            | "rec"
    )
}

// ---------------------------------------------------------------------------
// End pre-GHC source-order detection.
// ---------------------------------------------------------------------------

/// Build the canonical templates for a resident actor workbench. GHC selects
/// declaration, bind, or expression. Expressions first try the two
/// single-evaluation Haskell-display lifts, then opaque-display counterparts
/// for values without a rendering instance. Frontends may choose how to label
/// those typed outcomes, but should not grow another source assembly path.
#[must_use]
pub fn resident_workbench_templates(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
) -> Vec<TurnTemplate> {
    let preamble = insert_preamble_imports(preamble, imports);
    vec![
        TurnTemplate {
            kind: TemplateSelector::Decl,
            source: DECL_TEMPLATE_SOURCE.to_string(),
        },
        TurnTemplate {
            kind: TemplateSelector::Bind,
            source: assemble_bind_module(
                &preamble,
                "",
                "__result",
                effect_stack,
                "{{TURN_STMT}}",
                "({{BINDERS}})",
                false,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::BindDiscard,
            source: assemble_bind_module(
                &preamble,
                "",
                "__result",
                effect_stack,
                "{{TURN_STMT}}",
                "()",
                false,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_display_expression_module(
                &preamble,
                "__result",
                effect_stack,
                "{{TURN}}",
                ExpressionLift::Effectful,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_display_expression_module(
                &preamble,
                "__result",
                effect_stack,
                "{{TURN}}",
                ExpressionLift::Pure,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_opaque_expression_module(
                &preamble,
                "__result",
                effect_stack,
                "{{TURN}}",
                ExpressionLift::Effectful,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_opaque_expression_module(
                &preamble,
                "__result",
                effect_stack,
                "{{TURN}}",
                ExpressionLift::Pure,
            ),
        },
    ]
}

/// Runtime-authored module template for GHC's whole-cell preflight. The
/// compiler worker only fills the declaration/body placeholders after its own
/// lexer and parser classify the submitted source.
///
/// `__tidepoolInEffectRow` is the same no-op-at-runtime pin used by the
/// per-unit expression templates ([`assemble_expression_module`] and
/// friends, in `super::turn`) — same name, same
/// `Eff effect_stack value -> Eff effect_stack value` type in every module
/// that might see it. `TidepoolCellExpression`'s two instance heads cannot
/// themselves prefer the effectful reading of an ambiguous final expression
/// (GHC always resolves the overlap toward the unconditionally-matching bare
/// `value` head once forced to choose — the opposite of what a cell whose
/// final unit is `pure <expr>` wants), so
/// [`super::turn::check_cell_preferring_effectful`] retries a failed check
/// once with the cell's final expression wrapped in this pin, which settles
/// the ambiguity outright rather than adjudicating it here.
#[must_use]
pub fn resident_cell_check_template(preamble: &str, effect_stack: &str, imports: &str) -> String {
    let preamble = insert_preamble_imports(
        &insert_preamble_imports(preamble, imports),
        "qualified GHC.TypeError as TidepoolWorkbenchTypeError",
    );
    let preamble = insert_preamble_imports(&preamble, "{{CELL_IMPORTS}}")
        .replace("import {{CELL_IMPORTS}}", "{{CELL_IMPORTS}}")
        .replacen("\nmodule ", "\n{{CELL_PRAGMAS}}\nmodule ", 1);
    format!(
        "{preamble}\n\
         __tidepoolInEffectRow :: Eff {effect_stack} value -> Eff {effect_stack} value\n\
         __tidepoolInEffectRow = id\n\
         class TidepoolCellPure value\n\
         instance {{-# OVERLAPPABLE #-}} TidepoolCellPure value\n\
         instance {{-# OVERLAPPING #-}} TidepoolWorkbenchTypeError.Unsatisfiable \
           ('TidepoolWorkbenchTypeError.Text \"an Eff action must use the current workbench effect row\") \
           => TidepoolCellPure (Eff effects value)\n\
         class TidepoolCellExpression value where {{ \
           __tidepoolCellExpression :: value -> Eff {effect_stack} () }}\n\
         instance {{-# OVERLAPPING #-}} (effects ~ {effect_stack}) => TidepoolCellExpression (Eff effects value) where {{ \
           __tidepoolCellExpression action = action >> pure () }}\n\
         instance {{-# OVERLAPPABLE #-}} TidepoolCellPure value => TidepoolCellExpression value where {{ \
           __tidepoolCellExpression _ = pure () }}\n\
         {{{{CELL_DECLS}}}}\n\
         __tidepool_cell_check :: Eff {effect_stack} ()\n\
         __tidepool_cell_check = do {{\n\
         {{{{CELL_BODY}}}}\n\
         ; pure () }}\n"
    )
}

/// Suspension-safe cursor over one ordered unit of work.
///
/// A consumer commits only the current item. Leaving it uncommitted is the
/// stop/park operation, so the successful prefix and never-run suffix cannot
/// drift apart while the cursor is stored and later resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkSequence<Item, Committed> {
    items: Vec<Item>,
    committed: Vec<Committed>,
}

impl<Item, Committed> WorkSequence<Item, Committed> {
    #[must_use]
    pub fn new(items: Vec<Item>) -> Self {
        let capacity = items.len();
        Self {
            items,
            committed: Vec::with_capacity(capacity),
        }
    }

    #[must_use]
    pub fn position(&self) -> usize {
        self.committed.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.position())
    }

    #[must_use]
    pub fn item(&self, index: usize) -> Option<&Item> {
        self.items.get(index)
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    #[must_use]
    pub fn committed(&self) -> &[Committed] {
        &self.committed
    }

    pub fn committed_mut(&mut self) -> &mut [Committed] {
        &mut self.committed
    }

    /// Commit the current item and advance. Callers invoke this only after
    /// obtaining [`Self::current`]; the returned zero-based index lets them
    /// attach presentation metadata without maintaining another cursor.
    pub fn commit_next(&mut self, output: Committed) -> usize {
        let index = self.position();
        self.committed.push(output);
        index
    }

    #[must_use]
    pub fn into_committed(self) -> Vec<Committed> {
        self.committed
    }
}

/// One already-parsed runnable block and its position in an assistant
/// response. Parsing belongs to `tidepool-model-output`; this type begins the
/// execution contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBlock {
    pub ordinal: usize,
    pub total: usize,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedBlock<T> {
    pub block: ParsedBlock,
    pub output: T,
}

/// A block either commits and permits the next block to run, or parks/settles
/// the sequence with a caller-defined terminal value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockExecution<Committed, Stopped> {
    Committed(Committed),
    Stopped(Stopped),
}

/// Prefix-preserving result of one assistant response's ordered Haskell
/// sequence. On stop or failure, blocks after `block` were never invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockSequenceOutcome<Committed, Stopped, Error> {
    Completed {
        committed: Vec<CommittedBlock<Committed>>,
    },
    Stopped {
        committed: Vec<CommittedBlock<Committed>>,
        block: ParsedBlock,
        outcome: Stopped,
    },
    Failed {
        committed: Vec<CommittedBlock<Committed>>,
        block: ParsedBlock,
        error: Error,
    },
}

/// Execute parsed blocks strictly in source order.
pub async fn run_block_sequence<Committed, Stopped, Error, Run, RunFuture>(
    blocks: Vec<String>,
    mut run: Run,
) -> BlockSequenceOutcome<Committed, Stopped, Error>
where
    Run: FnMut(ParsedBlock) -> RunFuture,
    RunFuture: Future<Output = Result<BlockExecution<Committed, Stopped>, Error>>,
{
    let total = blocks.len();
    let blocks = blocks
        .into_iter()
        .enumerate()
        .map(|(index, source)| ParsedBlock {
            ordinal: index + 1,
            total,
            source,
        })
        .collect();
    let mut sequence = WorkSequence::new(blocks);

    while let Some(block) = sequence.current().cloned() {
        match run(block.clone()).await {
            Ok(BlockExecution::Committed(output)) => {
                sequence.commit_next(CommittedBlock { block, output });
            }
            Ok(BlockExecution::Stopped(outcome)) => {
                return BlockSequenceOutcome::Stopped {
                    committed: sequence.into_committed(),
                    block,
                    outcome,
                };
            }
            Err(error) => {
                return BlockSequenceOutcome::Failed {
                    committed: sequence.into_committed(),
                    block,
                    error,
                };
            }
        }
    }
    BlockSequenceOutcome::Completed {
        committed: sequence.into_committed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_template_preserves_worker_placeholders() {
        let template = resident_cell_check_template("module CellCheck where\n", "ActorEffects", "");
        assert_eq!(template.matches("{{CELL_DECLS}}").count(), 1);
        assert_eq!(template.matches("{{CELL_BODY}}").count(), 1);
    }

    /// The cell's own pragmas are ADDITIVE. `{{CELL_PRAGMAS}}` is filled only
    /// from the submitted cell's header, so the session dialect and the
    /// preamble's `default (...)` declaration are the workbench's, not the
    /// cell's, and both must survive template assembly — otherwise every
    /// checked cell typechecks under weaker defaulting than the turn it is
    /// checking.
    #[test]
    fn the_session_dialect_and_default_declaration_reach_every_checked_cell() {
        let preamble = format!(
            "{}\nmodule Expr where\nimport Tidepool.Prelude\ndefault (Int, Double, Text)\n",
            crate::session::EVAL_PRAGMAS,
        );
        let template = resident_cell_check_template(&preamble, "ActorEffects", "");

        let pragmas = template
            .find("{{CELL_PRAGMAS}}")
            .expect("the cell's own pragmas have a placeholder");
        let dialect = template
            .find("ExtendedDefaultRules")
            .expect("the session dialect reaches the checked cell");
        let module = template
            .find("\nmodule ")
            .expect("the template keeps a module header");
        assert!(
            dialect < pragmas && pragmas < module,
            "the session LANGUAGE block must precede the cell's own pragmas, and both \
             must precede the module header"
        );
        assert!(
            template.contains("default (Int, Double, Text)"),
            "the preamble's default declaration must survive into the checked cell"
        );
    }

    #[test]
    fn cell_source_and_tool_arguments_have_distinct_replay_identity() {
        let cell = WorkbenchRequest::from_cell_input("pure ()");
        assert!(cell.items.is_empty());
        assert_eq!(cell.cell_source(), Some("pure ()"));
        assert!(cell.tool_call().is_none());
        assert!(cell.execution_id().is_none());
        assert!(cell.fork_boundary().is_none());
        assert_ne!(cell, WorkbenchRequest::from_cell_input("pure 1"));
        assert_ne!(
            cell,
            WorkbenchRequest::for_tool("pure ()".into(), serde_json::Value::Null)
        );
    }

    #[test]
    fn named_tool_arguments_are_data_and_part_of_replay_identity() {
        let script = "cat <<'EOF'\n\":} [bash| λ |]\"\nEOF\n";
        let request = WorkbenchRequest::for_tool("bash".into(), script.into());
        assert_eq!(request.items, ["bash"]);
        assert_eq!(request.tool_call().unwrap().arguments, script);
        assert_eq!(request, request.clone());
        assert_ne!(
            request,
            WorkbenchRequest::for_tool("bash".into(), "other".into())
        );
        assert_ne!(
            request,
            WorkbenchRequest::for_tool("other".into(), script.into())
        );
    }

    #[test]
    fn classifies_only_stable_lexical_shapes() {
        assert_eq!(
            classify_workbench_item(" data X = X ").unwrap(),
            WorkbenchItem::Declaration("data X = X".into())
        );
        assert_eq!(
            classify_workbench_item("answer = 42").unwrap(),
            WorkbenchItem::Haskell("answer = 42".into())
        );
        assert_eq!(
            classify_workbench_item(":type answer").unwrap(),
            WorkbenchItem::Command(MetaCommandLine {
                name: "type".into(),
                arguments: "answer".into(),
            })
        );
    }

    #[test]
    fn discovery_commands_have_one_shared_argument_contract() {
        assert_eq!(
            MetaCommandLine::parse(":type startActor")
                .unwrap()
                .discovery()
                .unwrap(),
            Some(WorkbenchDiscovery::Type("startActor".into()))
        );
        assert_eq!(
            MetaCommandLine::parse(":bindings")
                .unwrap()
                .discovery()
                .unwrap(),
            Some(WorkbenchDiscovery::Bindings)
        );
        assert_eq!(
            MetaCommandLine::parse(":recovery")
                .unwrap()
                .discovery()
                .unwrap(),
            Some(WorkbenchDiscovery::Recovery)
        );
        assert_eq!(
            MetaCommandLine::parse(":show imports")
                .unwrap()
                .discovery()
                .unwrap(),
            Some(WorkbenchDiscovery::ShowImports)
        );
        assert_eq!(
            MetaCommandLine::parse(":show modules")
                .unwrap()
                .discovery()
                .unwrap_err(),
            ":show does not support `modules` (supported: :show imports)"
        );
        assert_eq!(
            MetaCommandLine::parse(":show")
                .unwrap()
                .discovery()
                .unwrap_err(),
            ":show requires an argument (supported: :show imports)"
        );
        assert!(MetaCommandLine::parse(":info")
            .unwrap()
            .discovery()
            .is_err());
        assert_eq!(
            MetaCommandLine::parse(":browse! Tidepool.Actors.Shoal")
                .unwrap()
                .discovery()
                .unwrap(),
            Some(WorkbenchDiscovery::Browse {
                module: Some("Tidepool.Actors.Shoal".into()),
                expanded: true,
            })
        );
        assert!(MetaCommandLine::parse(":browse *Tidepool.Actors.Shoal")
            .unwrap()
            .discovery()
            .is_err());
        for command in [
            ":type missing",
            ":info missing",
            ":browse Missing",
            ":bindings",
            ":recovery",
            ":show modules",
            ":status",
            ":doc request",
        ] {
            assert!(
                MetaCommandLine::parse(command).unwrap().is_observational(),
                "{command}"
            );
        }
        assert!(!MetaCommandLine::parse(":set -XGADTs")
            .unwrap()
            .is_observational());
    }

    #[test]
    fn cursor_retains_prefix_without_consuming_parked_item() {
        let mut sequence = WorkSequence::new(vec!["a", "park", "later"]);
        assert_eq!(sequence.current(), Some(&"a"));
        assert_eq!(sequence.commit_next(1), 0);
        assert_eq!(sequence.current(), Some(&"park"));
        assert_eq!(sequence.committed(), &[1]);
        assert_eq!(sequence.items()[sequence.position()..], ["park", "later"]);
    }

    #[test]
    fn input_normalization_and_source_are_frontend_neutral() {
        let encoded = serde_json::Value::String("{\"name\":\"shoal\"}".into());
        let normalized = normalize_workbench_input(&encoded);
        assert_eq!(normalized, serde_json::json!({"name": "shoal"}));
        assert_eq!(
            workbench_input_binding(Some(&normalized)),
            "input :: Aeson.Value\ninput = object [\"name\" .= Aeson.String \"shoal\"]\n\n"
        );
        assert_eq!(
            normalize_workbench_input(&serde_json::Value::String("42".into())),
            serde_json::Value::String("42".into())
        );
    }

    #[test]
    fn resident_expression_templates_prefer_haskell_display_then_fall_back() {
        let templates = resident_workbench_templates("module Expr where\n", "ActorEffects", "");
        let expressions = templates
            .iter()
            .filter(|template| template.kind == TemplateSelector::Expr)
            .collect::<Vec<_>>();

        assert_eq!(expressions.len(), 4);
        assert!(expressions[0].source.contains("pack (show __value)"));
        assert!(expressions[1]
            .source
            .contains("pack (show __workbenchValue)"));
        assert!(!expressions[2].source.contains("pack (show"));
        assert!(!expressions[3].source.contains("pack (show"));
        assert!(expressions[2].source.contains("pack \"<opaque value>\""));
        assert!(expressions[3].source.contains("pack \"<opaque value>\""));
    }

    #[test]
    fn response_statuses_are_closed_schema_backed_values() {
        let execution = WorkbenchExecutionId::from_digest([3; 16]);
        let response = WorkbenchResponse {
            status: WorkbenchRunStatus::Replied,
            summary: None,
            items: vec![WorkbenchItemReceipt {
                index: 0,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: WorkbenchItemStatus::Committed,
                output: "bound `answer`".into(),
                diagnostics: Vec::new(),
                failure_layer: None,
                warnings: Vec::new(),
                installed_bindings: vec!["answer".into()],
                operations: vec![WorkbenchOperationReceipt {
                    id: WorkbenchOperationId {
                        execution,
                        input_unit_index: 0,
                        effect_ordinal: 0,
                    },
                    effect: "reply".into(),
                    disposition: WorkbenchOperationDisposition::Committed,
                }],
                terminal_transfer: Some(WorkbenchTerminalTransfer::ReplyAccepted),
            }],
            next_index: 1,
            total: 1,
        };
        let encoded = serde_json::to_value(response).unwrap();
        assert_eq!(encoded["status"], "replied");
        assert_eq!(encoded["items"][0]["status"], "committed");
        assert_eq!(encoded["items"][0]["installedBindings"][0], "answer");
        assert_eq!(encoded["items"][0]["operations"][0]["effect"], "reply");
        assert_eq!(
            encoded["items"][0]["operations"][0]["disposition"],
            "committed"
        );
        assert_eq!(encoded["items"][0]["terminalTransfer"], "replyAccepted");
        assert_eq!(encoded["nextIndex"], 1);
        // A committed unit has nothing to say structurally, and says nothing:
        // the field is absent rather than an empty array.
        assert!(encoded["items"][0].get("diagnostics").is_none());
    }

    /// The receipt is schema-backed for the model-facing tool description.
    /// The added structured field must appear in that schema, including its
    /// representable-unknown location case — a schema that omitted it would
    /// tell the model the field cannot be there.
    #[test]
    fn receipt_schema_describes_the_structured_diagnostics_field() {
        let schema = serde_json::to_value(schemars::schema_for!(WorkbenchItemReceipt))
            .expect("the receipt schema serializes");
        let text = schema.to_string();
        assert!(text.contains("diagnostics"), "{schema:#}");
        assert!(text.contains("authored"), "{schema:#}");
        assert!(text.contains("unlocated"), "{schema:#}");
    }

    #[tokio::test]
    async fn stop_preserves_prefix_and_never_runs_suffix() {
        let outcome = run_block_sequence(
            vec!["a".into(), "park".into(), "must-not-run".into()],
            |block| async move {
                if block.source == "park" {
                    Ok::<_, ()>(BlockExecution::Stopped("suspended"))
                } else {
                    Ok(BlockExecution::Committed(block.source.clone()))
                }
            },
        )
        .await;
        assert!(matches!(
            outcome,
            BlockSequenceOutcome::Stopped {
                committed,
                block: ParsedBlock { ordinal: 2, .. },
                outcome: "suspended",
            } if committed.len() == 1
        ));
    }

    #[tokio::test]
    async fn failed_layout_unit_never_invokes_following_reply() {
        let blocks = vec![
            "let findings =\n  [ missing\n  ]".into(),
            "respond findings".into(),
        ];
        let mut invoked = Vec::new();
        let outcome = run_block_sequence(blocks, |block| {
            invoked.push(block.source.clone());
            async move {
                if block.ordinal == 1 {
                    Err("parse failure")
                } else {
                    Ok(BlockExecution::<(), ()>::Committed(()))
                }
            }
        })
        .await;
        assert!(matches!(
            outcome,
            BlockSequenceOutcome::Failed {
                block: ParsedBlock { ordinal: 1, .. },
                error: "parse failure",
                ..
            }
        ));
        assert_eq!(
            invoked,
            ["let findings =\n  [ missing\n  ]"],
            "a failed definition must not execute the later reply"
        );
    }

    // -- pre-GHC source-order detection (Wave 4 / proposal 5, stage one) --

    #[test]
    fn declaration_after_statement_referencing_its_binder_is_rejected() {
        let cell = "job <- Cmd.run \"ls\"\nresultText = describe job\n";
        let hit = detect_hoisted_declaration_collision(cell)
            .expect("a declaration reaching an earlier statement's binder must be flagged");
        assert_eq!(hit.declaration_name, "resultText");
        assert_eq!(hit.declaration_line, 2);
        assert_eq!(hit.binder_name, "job");
        assert_eq!(hit.statement_line, 1);
        let message = hit.message();
        assert!(message.contains("resultText"), "{message}");
        assert!(message.contains("job"), "{message}");
        assert!(message.contains("hoisted"), "{message}");
        assert!(message.contains("let resultText"), "{message}");
    }

    /// The same declaration, moved before the statement it referenced, is
    /// accepted: `detect_hoisted_declaration_collision` only ever flags a
    /// free name reaching a statement that is textually EARLIER — moving the
    /// declaration first removes that earlier statement entirely.
    #[test]
    fn declaration_placed_first_is_accepted() {
        let cell = "resultText = describe job\njob <- Cmd.run \"ls\"\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// Reusing a short name inside a declaration's own `let` is ordinary
    /// Haskell and reaches nothing outside the declaration, so it is accepted
    /// even when an earlier statement happens to bind the same name.
    #[test]
    fn a_name_rebound_by_the_declarations_own_let_is_not_a_collision() {
        let cell = "task <- pickTask\nsummarize xs = let task = clean xs in go task\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// The same, for a lambda parameter.
    #[test]
    fn a_name_rebound_by_a_lambda_parameter_is_not_a_collision() {
        let cell = "task <- pickTask\nsummarize = map (\\task -> describe task)\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// The same, for a `where` equation, which binds for the whole
    /// declaration and is invisible to the head-only classifier.
    #[test]
    fn a_name_rebound_by_a_where_equation_is_not_a_collision() {
        let cell = "task <- pickTask\nsummarize xs = describe task\n  where task = head xs\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// A nested `do` block's own bind shadows too.
    #[test]
    fn a_name_rebound_by_a_nested_bind_is_not_a_collision() {
        let cell = "task <- pickTask\nrunAll = do\n  task <- nextTask\n  describe task\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// A declaration whose body is a `do` block is still a declaration. Were
    /// its nested bind read as this unit's own, the helper's name would enter
    /// scope as a statement binder and the next declaration to call it would
    /// be rejected for using it.
    #[test]
    fn a_declaration_whose_body_is_a_do_block_binds_nothing_for_later_units() {
        let cell = "runAll = do\n  task <- nextTask\n  describe task\nreport = summarize runAll\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// A declaration whose only reachable name is an import (never a same-
    /// cell statement binder) is accepted — this detector never reasons
    /// about imports at all, only about earlier same-cell statement binders.
    #[test]
    fn declaration_resolving_only_to_an_import_is_accepted() {
        let cell = "import Control.Lens (previews)\n\nsummary = previews id someList\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// The silent-shadow case: `previews` is bound by an earlier statement
    /// AND separately importable. The declaration referencing it is still
    /// rejected — silently binding to the import instead would be worse.
    #[test]
    fn silent_shadow_by_an_in_scope_import_is_still_rejected() {
        let cell = "import Control.Lens (previews)\n\nprevious <- computePreviews\nsummary = previous\n";
        let hit = detect_hoisted_declaration_collision(cell)
            .expect("a same-named import must not suppress the rejection");
        assert_eq!(hit.declaration_name, "summary");
        assert_eq!(hit.binder_name, "previous");
        assert_eq!(hit.statement_line, 3);
        assert_eq!(hit.declaration_line, 4);
    }

    /// A declaration's own parameter is a local binding, not a free
    /// reference — even when it shares a name with an earlier statement's
    /// binder, this must not be flagged (a false rejection is worse than a
    /// missed one).
    #[test]
    fn declarations_own_parameter_shadows_an_earlier_statement_binder() {
        let cell = "job <- Cmd.run \"ls\"\ndescribeJob job = show job\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }

    /// A bare expression statement binds no name, so a later declaration
    /// referencing an unrelated free name is accepted.
    #[test]
    fn expression_statement_binds_nothing_so_a_later_declaration_is_accepted() {
        let cell = "putStrLn \"starting\"\nsummary = describe otherThing\n";
        assert_eq!(detect_hoisted_declaration_collision(cell), None);
    }
}
