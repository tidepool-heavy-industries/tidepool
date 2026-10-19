//! Commands are descriptions; actor-owned jobs interpret them in the native workspace.
use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, HandlingClass, JsonInstance, Polymorphism, RecordField,
    RustBinding, SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

const DERIVES: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::FromHaskell,
    WireDerive::Clone,
    WireDerive::Debug,
    WireDerive::PartialEq,
    WireDerive::Eq,
    WireDerive::Serialize,
    WireDerive::Deserialize,
]);
fn named(name: &'static str) -> HsType {
    HsType::Named(name)
}
fn sum(name: &'static str, variants: Vec<(&'static str, Vec<HsType>)>) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Sum {
            variants: variants
                .into_iter()
                .map(|(ctor, fields)| SumVariant {
                    ctor,
                    fields: VariantFields::Positional(fields),
                    doc: &[],
                })
                .collect(),
        },
        json: JsonInstance::None,
        derives: DERIVES,
        domain: None,
        doc: &[],
    }
}
fn record(name: &'static str, fields: Vec<(&'static str, &'static str, HsType)>) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Record {
            fields: fields
                .into_iter()
                .map(|(hs_name, rust_name, ty)| RecordField {
                    hs_name,
                    rust_name,
                    ty,
                    doc: &[],
                })
                .collect(),
        },
        json: JsonInstance::None,
        derives: DERIVES,
        domain: None,
        doc: &[],
    }
}
fn verb(
    ctor: &'static str,
    method: &'static str,
    args: Vec<(&'static str, HsType, &'static str)>,
    ret: HsType,
) -> Verb {
    Verb {
        ctor,
        method,
        args: args
            .into_iter()
            .map(|(name, ty, rust)| Arg {
                name,
                ty,
                rust: RustBinding::Path(rust),
            })
            .collect(),
        ret,
        errors: None,
        handling: HandlingClass::Actor,
    }
}

pub fn commands() -> Effect {
    Effect {
        name: "Commands",
        authored_surface: AuthoredSurface::OPAQUE,
        handler: "CommandsDecodeHandler",
        handler_module: "commands",
        req_enum: "CommandsReq",
        decl_fn: "commands_decl",
        description: &[
            "Native command jobs are retained values for composing process execution, output inspection and continuation under weighted memory admission. ",
            "Use `Tidepool.Command` (`Cmd`) to build command values, set arguments, directory, environment and resource requirements, then start or run them explicitly. ",
            "Keep the returned `Cmd.Job` when later code needs observation, cancellation, stream paging or routing; reconstructing a command and running it creates another execution. ",
            "Distinguish invocation-owned execution from actor-owned work that survives the cell, and bounded observation from completion notification. ",
            "Compose checks and output selection with Haskell, or ask Jev about semantic evidence before choosing a prepared continuation. ",
            "Launch refusal, nonzero exit, input acknowledgment, terminal outcome and cleanup are separate facts. Preserve partial-output and retention-gap evidence when interpreting results. ",
            "The hosted bash/write_stdin/read_output/cancel_command tools provide direct calls; load `exomonad-command` for authored composition.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[
            "import qualified Tidepool.Command as Cmd",
            "import Tidepool.Command (bash, withMemory, Memory(..))",
        ],
        type_defs: vec![
            sum(
                "CommandInput",
                vec![
                    ("ClosedInput", vec![]),
                    ("PipeInput", vec![]),
                    ("TerminalInput", vec![]),
                ],
            ),
            sum(
                "CommandSourceCapture",
                vec![("NoCapture", vec![]), ("CaptureBeforeStart", vec![])],
            ),
            record(
                "CommandSpec",
                vec![
                    ("commandArgv", "argv", HsType::list(HsType::Text)),
                    ("commandDirectory", "directory", HsType::maybe(HsType::Text)),
                    (
                        "commandEnvironment",
                        "environment",
                        HsType::list(HsType::Tuple(vec![HsType::Text, HsType::Text])),
                    ),
                    ("commandMemory", "memory", HsType::Int),
                    ("commandInput", "input", named("CommandInput")),
                    (
                        "commandSourceCapture",
                        "source_capture",
                        named("CommandSourceCapture"),
                    ),
                ],
            ),
            sum(
                "CommandOutcome",
                vec![
                    ("CommandExited", vec![HsType::Int]),
                    ("CommandSignalled", vec![HsType::Int]),
                    ("CommandOutOfMemory", vec![HsType::Int]),
                    ("CommandCancelled", vec![]),
                    ("CommandFailed", vec![HsType::Text]),
                    ("CommandUnconfirmed", vec![HsType::Text]),
                ],
            ),
            sum(
                "CommandCleanup",
                vec![
                    ("CommandClean", vec![]),
                    ("CommandRetained", vec![]),
                    ("CommandCleanupUnknown", vec![HsType::Text]),
                ],
            ),
            record(
                "CommandResult",
                vec![
                    ("commandOutcome", "outcome", named("CommandOutcome")),
                    ("commandCleanup", "cleanup", named("CommandCleanup")),
                ],
            ),
            sum(
                "CommandStatus",
                vec![
                    ("CommandQueued", vec![]),
                    ("CommandStarting", vec![]),
                    ("CommandRunning", vec![]),
                    ("CommandStopping", vec![]),
                    ("CommandFinished", vec![named("CommandResult")]),
                ],
            ),
            sum(
                "CommandStream",
                vec![("Stdout", vec![]), ("Stderr", vec![])],
            ),
            sum(
                "CommandPosition",
                vec![
                    ("OutputBeginning", vec![]),
                    ("OutputTail", vec![]),
                    ("OutputOffset", vec![HsType::Int]),
                    ("OutputSlice", vec![HsType::Int, HsType::Int]),
                ],
            ),
            record(
                "CommandPage",
                vec![
                    ("outputText", "text", HsType::Text),
                    ("outputStart", "start", HsType::Int),
                    ("outputEnd", "end", HsType::Int),
                    ("outputAvailableEnd", "available_end", HsType::Int),
                    ("outputRetainedStart", "retained_start", HsType::Int),
                    ("outputLostBytes", "lost_bytes", HsType::Int),
                    ("outputFinished", "finished", HsType::Bool),
                    ("outputLossy", "lossy", HsType::Bool),
                    ("outputLeadingFragment", "leading_fragment", HsType::Bool),
                    ("outputTrailingFragment", "trailing_fragment", HsType::Bool),
                ],
            ),
            record(
                "CommandOutput",
                vec![
                    ("commandStdout", "stdout", named("CommandPage")),
                    ("commandStderr", "stderr", named("CommandPage")),
                ],
            ),
            record(
                "CommandObservation",
                vec![
                    ("observedCommandResult", "result", named("CommandResult")),
                    (
                        "observedCommandOutput",
                        "output",
                        HsType::either(named("CommandError"), named("CommandOutput")),
                    ),
                ],
            ),
            record(
                "CommandSource",
                vec![
                    ("sourceDirectory", "directory", HsType::Text),
                    ("sourceCommit", "commit", HsType::maybe(HsType::Text)),
                    ("sourceDirty", "dirty", HsType::Bool),
                ],
            ),
            record(
                "CommandReport",
                vec![
                    ("reportCommand", "command", HsType::list(HsType::Text)),
                    (
                        "reportSource",
                        "source",
                        HsType::maybe(named("CommandSource")),
                    ),
                    ("reportResult", "result", named("CommandResult")),
                    ("reportOutputComplete", "output_complete", HsType::Bool),
                    ("reportTail", "tail", HsType::Text),
                ],
            ),
            sum(
                "CommandPresentation",
                vec![
                    ("CommandVisible", vec![HsType::Text, HsType::Int]),
                    ("CommandQuiet", vec![]),
                ],
            ),
            sum(
                "CommandError",
                vec![
                    ("CommandUnavailable", vec![HsType::Text]),
                    ("CommandJobNotFound", vec![]),
                    ("CommandInvalid", vec![HsType::Text]),
                    ("CommandUnauthorized", vec![]),
                    ("CommandOutputPending", vec![]),
                    ("CommandInputRejected", vec![HsType::Text]),
                    ("CommandInputAcceptedCloseUnconfirmed", vec![HsType::Text]),
                ],
            ),
        ],
        external_types: &[crate::schema::ExternalType {
            haskell_name: "WorkerLifetime", rust_wire: "crate::WorkerLifetime", core_module: None,
        }],
        errors: None,
        verbs: vec![
            verb(
                "CommandStartWith",
                "command_start_with",
                vec![(
                    "spec",
                    named("CommandSpec"),
                    "tidepool_bridge_effects::CommandSpec",
                )],
                HsType::either(named("CommandError"), HsType::Text),
            ),
            verb(
                "CommandStartOwnedWith", "command_start_owned_with",
                vec![
                    ("spec", named("CommandSpec"), "tidepool_bridge_effects::CommandSpec"),
                    ("lifetime", named("WorkerLifetime"), "crate::WorkerLifetime"),
                ],
                HsType::either(named("CommandError"), HsType::Text),
            ),
            verb(
                "CommandRetainWith", "command_retain_with",
                vec![("job", HsType::Text, "String"), ("lifetime", named("WorkerLifetime"), "crate::WorkerLifetime")],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
            verb(
                "CommandBackgroundWith",
                "command_background_with",
                vec![(
                    "spec",
                    named("CommandSpec"),
                    "tidepool_bridge_effects::CommandSpec",
                )],
                HsType::either(named("CommandError"), HsType::Text),
            ),
            verb(
                "CommandStatusWith",
                "command_status_with",
                vec![("job", HsType::Text, "String")],
                HsType::either(named("CommandError"), named("CommandStatus")),
            ),
            verb(
                "CommandAwaitWith",
                "command_await_with",
                vec![
                    ("job", HsType::Text, "String"),
                    ("milliseconds", HsType::Int, "i64"),
                ],
                HsType::either(named("CommandError"), named("CommandStatus")),
            ),
            verb(
                "CommandAwaitAndNotifyWith",
                "command_await_and_notify_with",
                vec![
                    ("job", HsType::Text, "String"),
                    ("milliseconds", HsType::Int, "i64"),
                ],
                HsType::either(named("CommandError"), named("CommandStatus")),
            ),
            verb(
                "CommandWaitWith",
                "command_wait_with",
                vec![("job", HsType::Text, "String")],
                HsType::either(named("CommandError"), named("CommandObservation")),
            ),
            verb(
                "CommandPresentWith",
                "command_present_with",
                vec![
                    ("job", HsType::Text, "String"),
                    (
                        "presentation",
                        named("CommandPresentation"),
                        "tidepool_bridge_effects::CommandPresentation",
                    ),
                ],
                HsType::Unit,
            ),
            verb(
                "CommandRetainJobWith",
                "command_retain_job_with",
                vec![("job", HsType::Text, "String")],
                HsType::either(named("CommandError"), HsType::Text),
            ),
            verb(
                "CommandOutputWith",
                "command_output_with",
                vec![
                    ("job", HsType::Text, "String"),
                    ("bytes", HsType::Int, "i64"),
                ],
                HsType::either(named("CommandError"), named("CommandOutput")),
            ),
            verb(
                "CommandReadWith",
                "command_read_with",
                vec![
                    ("job", HsType::Text, "String"),
                    (
                        "stream",
                        named("CommandStream"),
                        "tidepool_bridge_effects::CommandStream",
                    ),
                    (
                        "position",
                        named("CommandPosition"),
                        "tidepool_bridge_effects::CommandPosition",
                    ),
                ],
                HsType::either(named("CommandError"), named("CommandPage")),
            ),
            verb(
                "CommandInputWith",
                "command_input_with",
                vec![
                    ("job", HsType::Text, "String"),
                    ("text", HsType::Text, "String"),
                ],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
            verb(
                "CommandFinishInputWith",
                "command_finish_input_with",
                vec![
                    ("job", HsType::Text, "String"),
                    ("text", HsType::Text, "String"),
                ],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
            verb(
                "CommandCloseInputWith",
                "command_close_input_with",
                vec![("job", HsType::Text, "String")],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
            verb(
                "CommandResizeWith",
                "command_resize_with",
                vec![
                    ("job", HsType::Text, "String"),
                    ("rows", HsType::Int, "i64"),
                    ("columns", HsType::Int, "i64"),
                ],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
            verb(
                "CommandDetachWith",
                "command_detach_with",
                vec![("job", HsType::Text, "String")],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
            verb(
                "CommandCancelWith",
                "command_cancel_with",
                vec![("job", HsType::Text, "String")],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
        ],
        helpers: vec![],
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}

/// Curated authored companion surface; signatures are data, never scraped source.
pub struct CompanionOperation {
    pub name: &'static str,
    pub arguments: Vec<HsType>,
    pub result: HsType,
    pub effectful: bool,
    pub contract: &'static [&'static str],
    pub source: &'static str,
    pub examples: &'static [&'static str],
}
impl CompanionOperation {
    pub fn signature(&self) -> String {
        let ty = if self.effectful {
            crate::hs::render_member_signature(&self.arguments, "Commands", &self.result)
        } else {
            self.arguments
                .iter()
                .rev()
                .fold(self.result.clone(), |ret, arg| {
                    HsType::func(arg.clone(), ret)
                })
                .render()
        };
        format!("Cmd.{} :: {}", self.name, ty)
    }
}

pub fn companion_operations() -> Vec<CompanionOperation> {
    let command = named("Cmd.Command");
    let job = named("Cmd.Job");
    let result = named("Cmd.RunResult");
    let output = HsType::either(named("Cmd.OutputIssue"), HsType::Text);
    let mut operations = Vec::new();
    let mut add = |name, arguments, result, effectful, contract| {
        operations.push(CompanionOperation {
            name,
            arguments,
            result,
            effectful,
            contract,
            source: "bridge/haskell/lib/Tidepool/Command.hs",
            examples: &[],
        })
    };
    add(
        "argv",
        vec![HsType::list(HsType::Text)],
        command.clone(),
        false,
        &["Describe a command with exact argv; execution is explicit."],
    );
    add(
        "bashCommand",
        vec![HsType::Text],
        command.clone(),
        false,
        &["Describe a bash script; execution is explicit."],
    );
    add(
        "withMemory",
        vec![named("Cmd.Memory"), command.clone()],
        command.clone(),
        false,
        &["Set positive weighted admission memory using Cmd.MiB or Cmd.GiB."],
    );
    add(
        "inDirectory",
        vec![HsType::Text, command.clone()],
        command.clone(),
        false,
        &["Set the command working directory."],
    );
    add(
        "withEnvironment",
        vec![
            HsType::list(HsType::Tuple(vec![HsType::Text, HsType::Text])),
            command.clone(),
        ],
        command.clone(),
        false,
        &["Add environment overrides."],
    );
    add(
        "run",
        vec![command.clone()],
        result.clone(),
        true,
        &["Start and await completion; retain the result for output and terminal evidence."],
    );
    add(
        "start",
        vec![command],
        job.clone(),
        true,
        &["Start once and retain the Job for later observation or cancellation."],
    );
    add(
        "await",
        vec![job.clone()],
        result.clone(),
        true,
        &["Await an existing job without starting another execution."],
    );
    add(
        "job",
        vec![result.clone()],
        job.clone(),
        false,
        &["Recover the retained job from a completed result."],
    );
    add(
        "stdout",
        vec![result.clone()],
        output.clone(),
        false,
        &["Inspect captured stdout; Left preserves output gaps or incomplete capture."],
    );
    add(
        "failure",
        vec![result],
        HsType::maybe(HsType::Text),
        false,
        &["Inspect terminal failure separately from output decoding."],
    );
    add(
        "readStdout",
        vec![job.clone()],
        output,
        true,
        &["Read retained stdout, preserving partial-output issues."],
    );
    add(
        "observe",
        vec![named("Cmd.Observation"), job.clone()],
        named("Cmd.CommandStatus"),
        true,
        &["Observe with a finite wait and output budget; observation does not set job lifetime."],
    );
    add(
        "output",
        vec![job.clone()],
        named("Cmd.OutputPage"),
        true,
        &["Open retained output with paging continuation."],
    );
    add(
        "pageText",
        vec![named("Cmd.OutputPage")],
        HsType::Text,
        false,
        &["Inspect text on one output page."],
    );
    add(
        "next",
        vec![named("Cmd.OutputPage")],
        named("Cmd.OutputPage"),
        true,
        &["Read the next output page."],
    );
    add(
        "cancel",
        vec![job],
        HsType::Unit,
        true,
        &["Request cancellation; inspect subsequent status for terminal and cleanup evidence."],
    );
    for operation in &mut operations {
        if operation.name == "run" {
            operation.examples = &[COMMAND_COMPOSITION_EXAMPLE];
        }
        if matches!(
            operation.name,
            "argv" | "bashCommand" | "withMemory" | "inDirectory" | "withEnvironment"
        ) {
            operation.source = "bridge/haskell/lib/Tidepool/Command/Types.hs";
        }
    }
    operations
}

/// Compiled verbatim by the authored command surface contract fixture.
pub const COMMAND_COMPOSITION_EXAMPLE: &str = "do\n  result <- Cmd.run (Cmd.withMemory (Cmd.MiB 256) (Cmd.argv [\"printf\", \"hello\\n\"]))\n  pure (Cmd.failure result, Cmd.stdout result)";
