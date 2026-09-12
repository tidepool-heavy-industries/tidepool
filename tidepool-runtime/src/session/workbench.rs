//! Frontend-neutral mechanics for a resident Haskell workbench.
//!
//! This module owns the pieces every resident frontend needs before it can
//! apply its own policy: source-item classification, meta-command tokenization,
//! and prefix-preserving ordered execution. It deliberately does not know
//! about MCP response shapes, actor conversations, effect settlement, or
//! presentation.

use std::future::Future;

use pest::Parser;
use pest_derive::Parser;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    assemble_bind_module, assemble_display_expression_module, assemble_opaque_expression_module,
    insert_preamble_imports, ExpressionLift, TemplateSelector, TurnTemplate, DECL_TEMPLATE_SOURCE,
};

#[derive(Parser)]
#[grammar = "session/ghci_input.pest"]
struct GhciScriptParser;

/// One independently executed unit in a GHCi-style script payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhciInputUnit {
    /// One nonblank top-level Haskell input line.
    Code { source: String, line: usize },
    /// One reserved, colon-prefixed workbench command.
    Command {
        source: String,
        line: usize,
        command: MetaCommandLine,
    },
    /// One multiline Haskell input delimited by exact `:{` and `:}` lines.
    Block {
        source: String,
        start_line: usize,
        end_line: usize,
    },
}

impl GhciInputUnit {
    #[must_use]
    pub fn source(&self) -> &str {
        match self {
            Self::Code { source, .. }
            | Self::Command { source, .. }
            | Self::Block { source, .. } => source,
        }
    }

    #[must_use]
    pub fn kind(&self) -> GhciInputKind {
        match self {
            Self::Command { .. } => GhciInputKind::Command,
            Self::Code { .. } | Self::Block { .. } => GhciInputKind::Code,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhciInputKind {
    Code,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GhciInputError {
    #[error("line {line}: unexpected `:}}` without a matching `:{{`")]
    UnexpectedBlockClose { line: usize },
    #[error("line {line}: nested `:{{` is not supported; close the current GHCi input unit first")]
    NestedBlockOpen { line: usize },
    #[error("line {line}: unterminated `:{{` GHCi input unit; add a `:}}` line")]
    UnterminatedBlock { line: usize },
    #[error("could not parse GHCi input: {0}")]
    Grammar(String),
}

/// Parse one custom-tool payload as a small GHCi-style script.
///
/// The grammar reserves colon-prefixed lines for workbench commands, treats
/// every other nonblank line as Haskell, joins more-indented continuation
/// lines to their preceding Haskell unit, and recognizes `:{` / `:}` as one
/// explicit multiline unit. Block contents retain their exact decoded text;
/// the delimiters themselves do not become Haskell source.
pub fn parse_ghci_input(source: &str) -> Result<Vec<GhciInputUnit>, GhciInputError> {
    let script = GhciScriptParser::parse(Rule::script, source)
        .map_err(|error| GhciInputError::Grammar(error.to_string()))?
        .next()
        .ok_or_else(|| GhciInputError::Grammar("parser returned no script".into()))?;
    let mut units = Vec::new();
    for pair in script.into_inner() {
        let line = pair.as_span().start_pos().line_col().0;
        match pair.as_rule() {
            Rule::code_unit => {
                let source = pair
                    .into_inner()
                    .find(|part| part.as_rule() == Rule::code_source)
                    .ok_or_else(|| GhciInputError::Grammar("code unit contained no source".into()))?
                    .as_str()
                    .to_owned();
                let indent = leading_indent(&source);
                let extends_previous = matches!(
                    units.last(),
                    Some(GhciInputUnit::Code { source: previous, .. })
                        if indent > leading_indent(previous)
                );
                if extends_previous {
                    let Some(GhciInputUnit::Code {
                        source: previous, ..
                    }) = units.last_mut()
                    else {
                        unreachable!("extends_previous only matches a code unit");
                    };
                    previous.push('\n');
                    previous.push_str(&source);
                } else {
                    units.push(GhciInputUnit::Code { source, line });
                }
            }
            Rule::command_unit => {
                let command_pair = pair
                    .into_inner()
                    .find(|part| part.as_rule() == Rule::command)
                    .ok_or_else(|| {
                        GhciInputError::Grammar("command unit contained no command".into())
                    })?;
                let command = meta_command_from_pair(command_pair.clone())?;
                units.push(GhciInputUnit::Command {
                    source: command_pair.as_str().to_owned(),
                    line,
                    command,
                });
            }
            Rule::multiline_unit => {
                let mut body = None;
                let mut end_line = line;
                for part in pair.into_inner() {
                    match part.as_rule() {
                        Rule::block_body => {
                            if let Some(nested) = part
                                .clone()
                                .into_inner()
                                .find(|row| row.as_rule() == Rule::nested_block_open)
                            {
                                return Err(GhciInputError::NestedBlockOpen {
                                    line: nested.as_span().start_pos().line_col().0,
                                });
                            }
                            body = Some(strip_one_line_ending(part.as_str()).to_owned());
                        }
                        Rule::block_close => {
                            end_line = part.as_span().start_pos().line_col().0;
                        }
                        _ => {}
                    }
                }
                units.push(GhciInputUnit::Block {
                    source: body.unwrap_or_default(),
                    start_line: line,
                    end_line,
                });
            }
            Rule::stray_block_close => {
                return Err(GhciInputError::UnexpectedBlockClose { line });
            }
            Rule::unterminated_multiline_unit => {
                return Err(GhciInputError::UnterminatedBlock { line });
            }
            Rule::invalid_colon_unit => {
                return Err(GhciInputError::Grammar(format!(
                    "line {line}: colon-prefixed input is reserved for GHCi commands"
                )));
            }
            Rule::EOI => {}
            _ => unreachable!("script exposes only complete input units"),
        }
    }
    Ok(units)
}

fn strip_one_line_ending(source: &str) -> &str {
    source
        .strip_suffix("\r\n")
        .or_else(|| source.strip_suffix('\n'))
        .unwrap_or(source)
}

fn leading_indent(source: &str) -> usize {
    source
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .fold(0, |column, character| {
            if character == '\t' {
                column + (8 - column % 8)
            } else {
                column + 1
            }
        })
}

fn meta_command_from_pair(
    pair: pest::iterators::Pair<'_, Rule>,
) -> Result<MetaCommandLine, GhciInputError> {
    let mut name = None;
    let mut arguments = String::new();
    for part in pair.into_inner() {
        match part.as_rule() {
            Rule::command_name => name = Some(part.as_str().to_owned()),
            Rule::command_arguments => arguments = part.as_str().trim().to_owned(),
            _ => {}
        }
    }
    Ok(MetaCommandLine {
        name: name.ok_or_else(|| GhciInputError::Grammar("empty workbench command".into()))?,
        arguments,
    })
}

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
                tidepool_eval::shapes::parse_decimal_token(&value.to_string());
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkbenchRequest {
    /// GHCi-capable items run in source order. Execution stops at the first
    /// rejected or suspended item while preserving earlier commits.
    pub items: Vec<String>,
    /// Optional structured payload mounted as @input :: Aeson.Value@.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    /// Request the frontend's expanded diagnostic receipt when supported.
    #[serde(default)]
    pub verbose: Option<bool>,
    /// Parser-owned classification for raw GHCi scripts. Structured callers
    /// omit it and retain the historical per-item classifier.
    #[serde(skip)]
    #[schemars(skip)]
    input_kinds: Vec<GhciInputKind>,
    /// Runtime-minted identity for one exact hosted tool call.
    ///
    /// This is not accepted from JSON. The transport owner derives it from
    /// authenticated call coordinates before actor dispatch, allowing the
    /// actor to return a committed receipt when that exact call is retried.
    #[serde(skip)]
    #[schemars(skip)]
    execution_id: Option<WorkbenchExecutionId>,
    /// Trusted transport coordinates; never accepted from authored JSON.
    #[serde(skip)]
    #[schemars(skip)]
    fork_boundary: Option<WorkbenchForkBoundary>,
    /// Trusted named-handler selection; arguments are data, never Haskell source.
    #[serde(skip)]
    #[schemars(skip)]
    tool_call: Option<WorkbenchToolCall>,
    /// Raw notebook cell awaiting GHC split/classify/preflight in the owning
    /// actor session.
    #[serde(skip)]
    #[schemars(skip)]
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
            input_kinds: Vec::new(),
            execution_id: None,
            fork_boundary: None,
            tool_call: Some(WorkbenchToolCall { name, arguments }),
            cell_source: None,
        }
    }

    pub fn tool_call(&self) -> Option<&WorkbenchToolCall> {
        self.tool_call.as_ref()
    }

    pub fn from_ghci_input(source: &str) -> Result<Self, GhciInputError> {
        let units = parse_ghci_input(source)?;
        Ok(Self {
            items: units.iter().map(|unit| unit.source().to_owned()).collect(),
            input: None,
            verbose: None,
            input_kinds: units.iter().map(GhciInputUnit::kind).collect(),
            execution_id: None,
            fork_boundary: None,
            tool_call: None,
            cell_source: None,
        })
    }

    #[must_use]
    pub fn from_cell_input(source: &str) -> Self {
        Self {
            items: Vec::new(),
            input: None,
            verbose: None,
            input_kinds: Vec::new(),
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
        self.input_kinds = vec![GhciInputKind::Code; items.len()];
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

    #[must_use]
    pub fn input_kind(&self, index: usize) -> GhciInputKind {
        self.input_kinds.get(index).copied().unwrap_or_else(|| {
            if self
                .items
                .get(index)
                .is_some_and(|source| source.trim_start().starts_with(':'))
            {
                GhciInputKind::Command
            } else {
                GhciInputKind::Code
            }
        })
    }

    #[must_use]
    pub fn item_is_observational(&self, index: usize) -> bool {
        self.input_kind(index) == GhciInputKind::Command
            && self.items.get(index).is_some_and(|source| {
                MetaCommandLine::parse(source).is_ok_and(|command| command.is_observational())
            })
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchItemReceipt {
    pub index: usize,
    pub status: WorkbenchItemStatus,
    pub output: String,
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
    pub(crate) fn declaration(name: String, type_query: String) -> Self {
        Self {
            name,
            type_display: None,
            kind: WorkbenchBindingKind::Declaration,
            type_query: Some(type_query),
        }
    }

    pub(crate) fn materialized(name: String, type_display: Option<String>) -> Self {
        Self {
            name,
            type_display,
            kind: WorkbenchBindingKind::MaterializedValue,
            type_query: None,
        }
    }

    #[must_use]
    pub fn type_query(&self) -> Option<&str> {
        self.type_query.as_deref()
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
        let normalized = if trimmed.starts_with(':') {
            trimmed.to_owned()
        } else {
            format!(":{trimmed}")
        };
        let root = GhciScriptParser::parse(Rule::standalone_command, &normalized)
            .map_err(|error| format!("invalid workbench command: {error}"))?
            .next()
            .ok_or_else(|| "invalid workbench command: parser returned no command".to_string())?;
        let command = root
            .into_inner()
            .find(|pair| pair.as_rule() == Rule::command)
            .ok_or_else(|| "invalid workbench command: missing command".to_string())?;
        meta_command_from_pair(command).map_err(|error| error.to_string())
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
#[must_use]
pub fn resident_cell_check_template(preamble: &str, effect_stack: &str, imports: &str) -> String {
    let preamble = insert_preamble_imports(
        &insert_preamble_imports(preamble, imports),
        "qualified GHC.TypeError as TidepoolWorkbenchTypeError",
    );
    format!(
        "{preamble}\n\
         class TidepoolCellPure value\n\
         instance {{-# OVERLAPPABLE #-}} TidepoolCellPure value\n\
         instance {{-# OVERLAPPING #-}} TidepoolWorkbenchTypeError.Unsatisfiable \
           ('TidepoolWorkbenchTypeError.Text \"an Eff action must use the current workbench effect row\") \
           => TidepoolCellPure (Eff effects value)\n\
         class TidepoolCellExpression value where\n\
           __tidepoolCellExpression :: value -> Eff {effect_stack} ()\n\
         instance {{-# OVERLAPPING #-}} TidepoolCellExpression (Eff {effect_stack} value) where\n\
           __tidepoolCellExpression action = action >> pure ()\n\
         instance {{-# OVERLAPPABLE #-}} TidepoolCellPure value => TidepoolCellExpression value where\n\
           __tidepoolCellExpression _ = pure ()\n\
         {{CELL_DECLS}}\n\
         __tidepool_cell_check = do {{\n\
         {{CELL_BODY}}\n\
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
    fn hosted_coordinates_cannot_be_supplied_by_authored_json() {
        let authored: WorkbenchRequest = serde_json::from_value(serde_json::json!({
            "items": ["pure ()"],
            "execution_id": "forged-execution",
            "fork_boundary": {"thread_id": "another-parent", "call_id": "another-call"},
            "tool_call": {"name": "bash", "arguments": "forged input"}
        }))
        .unwrap();
        assert!(authored.execution_id().is_none());
        assert!(authored.fork_boundary().is_none());
        assert!(authored.tool_call().is_none());

        let trusted = authored.with_fork_boundary(WorkbenchForkBoundary {
            thread_id: "parent-thread".into(),
            call_id: "hosted-call".into(),
        });
        assert_eq!(trusted.fork_boundary().unwrap().call_id, "hosted-call");
        let serialized = serde_json::to_value(trusted).unwrap();
        assert!(serialized.get("fork_boundary").is_none());
        assert!(serialized.get("execution_id").is_none());
        let schema = serde_json::to_value(schemars::schema_for!(WorkbenchRequest)).unwrap();
        assert!(schema["properties"].get("fork_boundary").is_none());
        assert!(schema["properties"].get("execution_id").is_none());
        assert!(schema["properties"].get("tool_call").is_none());
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
        let wire = serde_json::to_value(&request).unwrap();
        assert!(wire.get("tool_call").is_none());
        let decoded: WorkbenchRequest = serde_json::from_value(wire).unwrap();
        assert!(decoded.tool_call().is_none());
        assert_ne!(request, decoded);
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
    fn ghci_script_parses_lines_and_exact_multiline_units() {
        assert_eq!(
            parse_ghci_input(
                ":info ActionFailure\r\n\r\n:{\r\ndata Example\r\n  = First\r\n  | 第二\r\n:}\r\n:type Example\r\n"
            )
            .unwrap(),
            vec![
                GhciInputUnit::Command {
                    source: ":info ActionFailure".into(),
                    line: 1,
                    command: MetaCommandLine {
                        name: "info".into(),
                        arguments: "ActionFailure".into(),
                    },
                },
                GhciInputUnit::Block {
                    source: "data Example\r\n  = First\r\n  | 第二".into(),
                    start_line: 3,
                    end_line: 7,
                },
                GhciInputUnit::Command {
                    source: ":type Example".into(),
                    line: 8,
                    command: MetaCommandLine {
                        name: "type".into(),
                        arguments: "Example".into(),
                    },
                },
            ]
        );
    }

    #[test]
    fn ghci_script_preserves_multiline_quasiquotes_and_following_units() {
        let quotation = include_str!("fixtures/multiline-command.hs").trim_end_matches('\n');
        let input = format!("{quotation}\nresult <- Cmd.run command\n:info Cmd.RunResult\n");
        let units = parse_ghci_input(&input).unwrap();
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].source(), quotation);
        assert_eq!(units[1].source(), "result <- Cmd.run command");
        assert_eq!(units[2].source(), ":info Cmd.RunResult");
    }

    #[test]
    fn ghci_script_ignores_quotation_openers_in_haskell_lexical_islands() {
        let input = "let text = \"[bash|\"\n-- [bash|\nlet char = '['\n{- [bash| {- nested -} -}\nnext <- pure text\n";
        let units = parse_ghci_input(input).unwrap();
        assert_eq!(units.len(), 5);
        assert_eq!(units[4].source(), "next <- pure text");
    }

    #[test]
    fn ghci_script_quasiquote_boundary_matrix() {
        let quotations = [
            "[bash|\nprintf '%s' \"$HOME\"\n|]",
            "[Cmd.bash|\n# comment\n\ntrue\n|]",
            "[bash|printf 'a'|] <> [bash|\nprintf 'b'\n|]",
            "[bash|\ncat <<-END\n\ttabs stay tabs\n\tEND\n|]",
            "[bash|\nprintf '%s' '[notAnotherQuote|'\n|]",
        ];
        for quotation in quotations {
            for newline in ["\n", "\r\n"] {
                let declaration = format!("let command = {quotation}").replace('\n', newline);
                let input =
                    format!("{declaration}{newline}result <- Cmd.run command{newline}:type result");
                let units = parse_ghci_input(&input).unwrap();
                assert_eq!(units.len(), 3, "{input:?}");
                assert_eq!(units[0].source(), declaration, "quotation bytes changed");
                assert_eq!(units[1].source(), "result <- Cmd.run command");
                assert_eq!(units[2].source(), ":type result");
                assert!(matches!(&units[1], GhciInputUnit::Code { line, .. }
                    if *line == declaration.lines().count() + 1));
            }
        }
    }

    #[test]
    fn ghci_script_unclosed_quasiquote_keeps_suffix_for_ghc_rejection() {
        let input = "let command = [bash|\n:info literal\nrunDangerousEffect\n";
        let units = parse_ghci_input(input).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].source(), input);
    }

    #[test]
    fn ghci_script_explicit_block_preserves_quoted_block_delimiters() {
        let quotation = include_str!("fixtures/multiline-command.hs").trim_end_matches('\n');
        let input = format!(":{{\n{quotation}\n:}}\nnext <- pure ()");
        let units = parse_ghci_input(&input).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].source(), quotation);
        assert!(matches!(&units[0], GhciInputUnit::Block { .. }));
        assert_eq!(units[1].source(), "next <- pure ()");
    }

    #[test]
    fn ghci_script_escaped_strings_and_nested_comments_do_not_open_quotes() {
        for source in [
            r#"let text = "escaped \" [bash| still string""#,
            "let primed' = '\\'' -- [bash| not code",
            "{- outer {- [bash| -} still a comment -} let x = 1",
            "let bracket = '['",
        ] {
            let input = format!("{source}\nnext <- pure ()");
            let units = parse_ghci_input(&input).unwrap();
            assert_eq!(units.len(), 2, "{input:?}");
            assert_eq!(units[0].source(), source);
            assert_eq!(units[1].source(), "next <- pure ()");
        }
    }

    #[test]
    fn ghci_script_groups_indented_layout_before_following_reply() {
        assert_eq!(
            parse_ghci_input(
                "let findings =\n  [ \"first\"\n  , \"second\"\n  ]\nrespond findings\n"
            )
            .unwrap(),
            vec![
                GhciInputUnit::Code {
                    source: "let findings =\n  [ \"first\"\n  , \"second\"\n  ]".into(),
                    line: 1,
                },
                GhciInputUnit::Code {
                    source: "respond findings".into(),
                    line: 5,
                },
            ]
        );
    }

    #[test]
    fn ghci_script_keeps_same_indent_haskell_as_separate_units() {
        assert_eq!(
            parse_ghci_input("  first = 1\n  second = 2\n").unwrap(),
            vec![
                GhciInputUnit::Code {
                    source: "  first = 1".into(),
                    line: 1,
                },
                GhciInputUnit::Code {
                    source: "  second = 2".into(),
                    line: 2,
                },
            ]
        );
    }

    #[test]
    fn ghci_script_compares_indentation_at_tab_stops() {
        assert_eq!(
            parse_ghci_input("          first = 1\n   \tsecond = 2\n").unwrap(),
            vec![
                GhciInputUnit::Code {
                    source: "          first = 1".into(),
                    line: 1,
                },
                GhciInputUnit::Code {
                    source: "   \tsecond = 2".into(),
                    line: 2,
                },
            ]
        );
    }

    #[test]
    fn explicit_multiline_layout_still_precedes_following_reply() {
        assert_eq!(
            parse_ghci_input(
                ":{\nlet findings =\n  [ \"first\"\n  , \"second\"\n  ]\n:}\nrespond findings\n"
            )
            .unwrap(),
            vec![
                GhciInputUnit::Block {
                    source: "let findings =\n  [ \"first\"\n  , \"second\"\n  ]".into(),
                    start_line: 1,
                    end_line: 6,
                },
                GhciInputUnit::Code {
                    source: "respond findings".into(),
                    line: 7,
                },
            ]
        );
    }

    #[test]
    fn ghci_script_reports_structural_errors() {
        assert_eq!(
            parse_ghci_input(":}\n").unwrap_err(),
            GhciInputError::UnexpectedBlockClose { line: 1 }
        );
        assert_eq!(
            parse_ghci_input(":{\nx = 1\n").unwrap_err(),
            GhciInputError::UnterminatedBlock { line: 1 }
        );
        assert_eq!(
            parse_ghci_input(":{\n:{\n:}\n").unwrap_err(),
            GhciInputError::NestedBlockOpen { line: 2 }
        );
        assert!(parse_ghci_input(": not-a-command\n")
            .unwrap_err()
            .to_string()
            .contains("colon-prefixed input is reserved"));
    }

    #[test]
    fn ghci_grammar_classification_survives_into_the_workbench_request() {
        let request = WorkbenchRequest::from_ghci_input(
            "  value = 42\n:info value\n:{\ntext = \"embedded :} and :{ stay Haskell\"\n:}\n",
        )
        .unwrap();
        assert_eq!(request.items.len(), 3);
        assert_eq!(request.items[0], "  value = 42");
        assert_eq!(
            request.items[2],
            "text = \"embedded :} and :{ stay Haskell\""
        );
        assert_eq!(request.input_kind(0), GhciInputKind::Code);
        assert_eq!(request.input_kind(1), GhciInputKind::Command);
        assert_eq!(request.input_kind(2), GhciInputKind::Code);
        assert!(WorkbenchRequest::from_ghci_input(" \t\n  ")
            .unwrap()
            .items
            .is_empty());
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
            items: vec![WorkbenchItemReceipt {
                index: 0,
                status: WorkbenchItemStatus::Committed,
                output: "bound `answer`".into(),
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
        let blocks = parse_ghci_input("let findings =\n  [ missing\n  ]\nrespond findings\n")
            .unwrap()
            .into_iter()
            .map(|unit| unit.source().to_owned())
            .collect();
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
}
