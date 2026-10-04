//! ModelCall control JSON. Tool arguments and returned values remain opaque;
//! declaration metadata reuses exomonad-tool; continuation envelopes are closed.
use crate::gen::GeneratedFile;

#[derive(Clone)]
enum Ty {
    Text,
    Int,
    Count,
    Bool,
    Value,
    /// Existing transport-neutral declaration metadata owns the Rust codec.
    /// Haskell passes the compiled Agent.Contract projection as JSON.
    ToolDeclarations,
    Named(&'static str),
    Maybe(Box<Ty>),
    List(Box<Ty>),
}
impl Ty {
    fn rust(&self) -> String {
        match self {
            Self::Text => "String".into(),
            Self::Int => "i64".into(),
            Self::Count => "u64".into(),
            Self::Bool => "bool".into(),
            Self::Value => "serde_json::Value".into(),
            Self::ToolDeclarations => "Vec<exomonad_tool::ToolDeclaration>".into(),
            Self::Named(n) => (*n).into(),
            Self::Maybe(t) => format!("Option<{}>", t.rust()),
            Self::List(t) => format!("Vec<{}>", t.rust()),
        }
    }
    fn hs(&self) -> String {
        match self {
            Self::Text => "Text".into(),
            Self::Int | Self::Count => "Int".into(),
            Self::Bool => "Bool".into(),
            Self::Value | Self::ToolDeclarations => "Value".into(),
            Self::Named(n) => (*n).into(),
            Self::Maybe(t) => format!("(Maybe {})", t.hs()),
            Self::List(t) => format!("[{}]", t.hs()),
        }
    }
}
struct Field {
    name: &'static str,
    ty: Ty,
}
struct Variant {
    ctor: &'static str,
    tag: &'static str,
    fields: Vec<Field>,
}
enum Shape {
    Record(Vec<Field>),
    Sum(Vec<Variant>),
    Enumeration(Vec<(&'static str, &'static str)>),
}
struct Decl {
    name: &'static str,
    shape: Shape,
}
fn f(name: &'static str, ty: Ty) -> Field {
    Field { name, ty }
}
fn variant(ctor: &'static str, tag: &'static str, fields: Vec<Field>) -> Variant {
    Variant { ctor, tag, fields }
}
fn schema() -> Vec<Decl> {
    use Ty::*;
    let optional = |ty| Maybe(Box::new(ty));
    let list = |ty| List(Box::new(ty));
    vec![
        Decl {
            name: "ModelEffortEnvelope",
            shape: Shape::Enumeration(vec![
                ("ModelLowEffort", "low"),
                ("ModelMediumEffort", "medium"),
                ("ModelHighEffort", "high"),
            ]),
        },
        Decl {
            name: "ModelLimitEnvelope",
            shape: Shape::Enumeration(vec![
                ("ModelRequestsLimit", "requests"),
                ("ModelToolsLimit", "tools"),
                ("ModelTokensLimit", "reported_tokens"),
                ("ModelDeadlineLimit", "deadline"),
            ]),
        },
        Decl {
            name: "ModelRequestLimits",
            shape: Shape::Record(vec![
                f("requests", optional(Count)),
                f("tools", optional(Count)),
                f("reported_tokens", optional(Count)),
                f("seconds", optional(Count)),
            ]),
        },
        Decl {
            name: "ModelRequestEnvelope",
            shape: Shape::Record(vec![
                f("instructions", Text),
                f("input", Text),
                f("model", optional(Text)),
                f("effort", optional(Named("ModelEffortEnvelope"))),
                f("limits", Named("ModelRequestLimits")),
                f("tools", ToolDeclarations),
                f("result_schema", optional(Value)),
                f("after_tool", Bool),
            ]),
        },
        Decl {
            name: "ModelUsageEnvelope",
            shape: Shape::Record(vec![
                f("requests", Count),
                f("tools", Count),
                f("reported_tokens", Count),
                f("unknown_usage_requests", Count),
            ]),
        },
        Decl {
            name: "ModelOutcomeEnvelope",
            shape: Shape::Sum(vec![
                variant("ModelTextOutcome", "text", vec![f("value", Text)]),
                variant("ModelTypedOutcome", "typed", vec![f("value", Value)]),
                variant("ModelFailedOutcome", "failed", vec![f("value", Text)]),
                variant("ModelCancelledOutcome", "cancelled", vec![]),
                variant(
                    "ModelExhaustedOutcome",
                    "exhausted",
                    vec![f("value", Named("ModelLimitEnvelope"))],
                ),
            ]),
        },
        Decl {
            name: "ModelReceiptEnvelope",
            shape: Shape::Record(vec![
                f("invocation_id", Text),
                f("parent_cell", Text),
                f("requests", list(Text)),
                f("counts", Named("ModelUsageEnvelope")),
                f("cell_counts", Named("ModelUsageEnvelope")),
                f("outcome", Named("ModelOutcomeEnvelope")),
            ]),
        },
        Decl {
            name: "ModelControlStep",
            shape: Shape::Sum(vec![
                variant(
                    "ModelCallback",
                    "callback",
                    vec![
                        f("invocation", Text),
                        f("call_id", Text),
                        f("name", Text),
                        f("arguments", Value),
                    ],
                ),
                variant(
                    "ModelHook",
                    "hook",
                    vec![
                        f("invocation", Text),
                        f("operation", Text),
                        f("name", Text),
                        f("arguments", Value),
                        f("handle", Text),
                        f("ordinal", Int),
                        f("value", Value),
                        f("output", Text),
                    ],
                ),
                variant(
                    "ModelFinished",
                    "finished",
                    vec![f("receipt", Named("ModelReceiptEnvelope"))],
                ),
            ]),
        },
        Decl {
            name: "ModelAnnotationEnvelope",
            shape: Shape::Sum(vec![
                variant("ModelNoAnnotation", "none", vec![]),
                variant("ModelAbstained", "abstained", vec![f("reason", Text)]),
                variant("ModelAnnotated", "annotated", vec![f("text", Text)]),
                variant(
                    "ModelPruned",
                    "pruned",
                    vec![f("handle", Text), f("text", Text)],
                ),
            ]),
        },
    ]
}

#[must_use]
pub fn generated_files() -> Vec<GeneratedFile> {
    let mut rust = String::from("//! ModelCall controls — generated by tidepool-protocol.\n");
    let mut hs = String::from("{-# LANGUAGE OverloadedStrings #-}\n-- Generated by tidepool-protocol.\nmodule Tidepool.Internal.ModelControl where\nimport Prelude\nimport Data.Text (Text)\nimport Tidepool.Aeson\nimport qualified Tidepool.Aeson.KeyMap as KM\n\ncontrolObject :: [Text] -> (Object -> Result a) -> Value -> Result a\ncontrolObject allowed parse = withObject \"ModelCall control\" $ \\o ->\n  if all (`elem` allowed) (map KM.toText (KM.keys o)) then parse o else Error \"unknown ModelCall control field\"\n\ncontrolCount :: Int -> Result Int\ncontrolCount count = if count < 0 then Error \"negative ModelCall count\" else pure count\n\n");
    for decl in schema() {
        rust.push_str("#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]\n");
        match decl.shape {
            Shape::Enumeration(variants) => {
                rust.push_str(&format!("pub enum {} {{\n", decl.name));
                hs.push_str(&format!(
                    "data {} = {} deriving (Show, Eq)\n",
                    decl.name,
                    variants
                        .iter()
                        .map(|(ctor, _)| *ctor)
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
                hs.push_str(&format!("instance ToJSON {} where\n", decl.name));
                for (ctor, tag) in &variants {
                    rust.push_str(&format!("    #[serde(rename = \"{tag}\")]\n    {ctor},\n"));
                    hs.push_str(&format!("  toJSON {ctor} = toJSON (\"{tag}\" :: Text)\n"));
                }
                rust.push_str("}\n\n");
                hs.push_str(&format!("instance FromJSON {} where\n  parseJSON = withText \"{}\" $ \\value -> case value of\n",decl.name,decl.name));
                for (ctor, tag) in &variants {
                    hs.push_str(&format!("    \"{tag}\" -> pure {ctor}\n"));
                }
                hs.push_str("    _ -> Error \"unknown ModelCall control value\"\n\n");
            }
            Shape::Record(fields) => {
                rust.push_str(&format!(
                    "#[serde(deny_unknown_fields)]\npub struct {} {{\n",
                    decl.name
                ));
                for field in &fields {
                    rust.push_str(&format!("    pub {}: {},\n", field.name, field.ty.rust()));
                }
                rust.push_str("}\n\n");
                hs.push_str(&format!(
                    "data {} = {} {} deriving (Show, Eq)\n",
                    decl.name,
                    decl.name,
                    fields
                        .iter()
                        .map(|f| f.ty.hs())
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                emit_hs_codec(&mut hs, decl.name, None, decl.name, &fields);
            }
            Shape::Sum(variants) => {
                rust.push_str(&format!(
                    "#[serde(tag = \"kind\", deny_unknown_fields)]\npub enum {} {{\n",
                    decl.name
                ));
                let constructors: Vec<_> = variants
                    .iter()
                    .map(|v| {
                        format!(
                            "{} {}",
                            v.ctor,
                            v.fields
                                .iter()
                                .map(|f| f.ty.hs())
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    })
                    .collect();
                hs.push_str(&format!(
                    "data {} = {} deriving (Show, Eq)\n",
                    decl.name,
                    constructors.join(" | ")
                ));
                hs.push_str(&format!("instance ToJSON {} where\n", decl.name));
                for v in &variants {
                    rust.push_str(&format!("    #[serde(rename = \"{}\")]\n", v.tag));
                    if v.fields.is_empty() {
                        rust.push_str(&format!("    {},\n", v.ctor));
                    } else {
                        let fields = v
                            .fields
                            .iter()
                            .map(|field| format!("{}: {}", field.name, field.ty.rust()))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let compact = format!("    {} {{ {fields} }},\n", v.ctor);
                        if fields.len() <= 60 && compact.trim_end().len() <= 100 {
                            rust.push_str(&compact);
                        } else {
                            rust.push_str(&format!("    {} {{\n", v.ctor));
                            for field in &v.fields {
                                rust.push_str(&format!(
                                    "        {}: {},\n",
                                    field.name,
                                    field.ty.rust()
                                ));
                            }
                            rust.push_str("    },\n");
                        }
                    }
                    emit_hs_to_json(&mut hs, Some(v.tag), v.ctor, &v.fields);
                }
                rust.push_str("}\n\n");
                hs.push_str(&format!("instance FromJSON {} where\n  parseJSON = withObject \"{}\" $ \\o -> do\n    kind <- o .: \"kind\"\n    case (kind :: Text) of\n",decl.name,decl.name));
                for v in variants {
                    hs.push_str(&format!(
                        "      \"{}\" -> {}\n",
                        v.tag,
                        format!(
                            "controlObject {} (\\o -> {}) (Object o)",
                            hs_keys(Some("kind"), &v.fields),
                            hs_parse(v.ctor, &v.fields)
                        )
                    ));
                }
                hs.push_str("      _ -> Error \"unknown ModelCall control variant\"\n\n");
            }
        }
    }
    for source in [&mut rust, &mut hs] {
        while source.ends_with("\n\n") {
            source.pop();
        }
    }
    vec![
        GeneratedFile {
            path: "tidepool/bridge-effects/src/generated/model_control.rs".into(),
            contents: rust,
        },
        GeneratedFile {
            path: "bridge/haskell/lib/Tidepool/Internal/ModelControl.hs".into(),
            contents: hs,
        },
    ]
}
fn emit_hs_to_json(out: &mut String, tag: Option<&str>, ctor: &str, fields: &[Field]) {
    let vars: Vec<_> = (0..fields.len()).map(|i| format!("f{i}")).collect();
    let mut pairs: Vec<_> = fields
        .iter()
        .zip(&vars)
        .map(|(field, var)| format!("\"{}\" .= {var}", field.name))
        .collect();
    if let Some(tag) = tag {
        pairs.insert(0, format!("\"kind\" .= (\"{tag}\" :: Text)"));
    }
    out.push_str(&format!(
        "  toJSON ({} {}) = object [{}]\n",
        ctor,
        vars.join(" "),
        pairs.join(", ")
    ));
}
fn hs_parse(ctor: &str, fields: &[Field]) -> String {
    let mut expression = format!("pure {ctor}");
    for field in fields {
        let operator = if matches!(field.ty, Ty::Maybe(_)) {
            ".:?"
        } else {
            ".:"
        };
        let read = format!("o {operator} \"{}\"", field.name);
        let read = match &field.ty {
            Ty::Count => format!("({read}) >>= controlCount"),
            Ty::Maybe(inner) if matches!(**inner, Ty::Count) => {
                format!("({read}) >>= traverse controlCount")
            }
            _ => read,
        };
        expression.push_str(&format!(" <*> ({read})"));
    }
    expression
}
fn emit_hs_codec(out: &mut String, name: &str, tag: Option<&str>, ctor: &str, fields: &[Field]) {
    out.push_str(&format!("instance ToJSON {name} where\n"));
    emit_hs_to_json(out, tag, ctor, fields);
    out.push_str(&format!(
        "instance FromJSON {name} where\n  parseJSON = controlObject {} $ \\o -> {}\n\n",
        hs_keys(None, fields),
        hs_parse(ctor, fields)
    ));
}

fn hs_keys(tag: Option<&str>, fields: &[Field]) -> String {
    let mut keys: Vec<_> = fields
        .iter()
        .map(|field| format!("\"{}\"", field.name))
        .collect();
    if let Some(tag) = tag {
        keys.insert(0, format!("\"{tag}\""));
    }
    format!("[{}]", keys.join(", "))
}
