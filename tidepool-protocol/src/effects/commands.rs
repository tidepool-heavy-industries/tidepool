//! Commands are descriptions; actor-owned jobs interpret them in the native workspace.
use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, HandlingClass, JsonInstance, Polymorphism, RecordField,
    RustBinding, SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

const DERIVES: WireDerives = WireDerives(&[
    WireDerive::ToCore,
    WireDerive::FromCore,
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
        core_module: Some("Tidepool.Effects.Core"),
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
        core_module: Some("Tidepool.Effects.Core"),
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
        extract: None,
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
        description: &["Actor-owned native command jobs with weighted memory admission."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
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
                ],
            ),
            sum(
                "CommandOutcome",
                vec![
                    ("CommandExited", vec![HsType::Int]),
                    ("CommandSignalled", vec![HsType::Int]),
                    ("CommandOutOfMemory", vec![]),
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
                    ("observedCommandOutput", "output", named("CommandOutput")),
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
                    ("CommandInvalid", vec![HsType::Text]),
                    ("CommandUnauthorized", vec![]),
                    ("CommandOutputPending", vec![]),
                    ("CommandInputRejected", vec![HsType::Text]),
                    ("CommandInputAcceptedCloseUnconfirmed", vec![HsType::Text]),
                ],
            ),
        ],
        foreign_types: &[],
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
                "CommandForegroundWith",
                "command_foreground_with",
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
                "CommandCancelWith",
                "command_cancel_with",
                vec![("job", HsType::Text, "String")],
                HsType::either(named("CommandError"), HsType::Unit),
            ),
        ],
        helpers: vec![],
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
