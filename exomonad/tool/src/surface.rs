//! What changed between two model-visible tool surfaces.
//!
//! An actor's declared tool surface is registered once with the attached agent
//! application and served read-only for the life of that registration, so a
//! recompiled tool record may replace implementations but must declare the same
//! surface. This module answers whether two surfaces are the same, and when
//! they are not, what a model would see differently.
//!
//! Everything a model reads is compared: the set of tools, each one's
//! description, input and output schema and kind, and the order they are
//! presented in. Two schemas that mean the same thing but serialize differently
//! are reported as a rendering change, because the request text differs even
//! though the contract does not.

use crate::{HostedTool, ToolKind};

/// One model-visible field of a tool declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolField {
    Description,
    InputSchema,
    OutputSchema,
    Kind,
    /// The schemas mean the same thing but do not serialize identically.
    SchemaRendering,
}

impl ToolField {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Description => "description",
            Self::InputSchema => "input schema",
            Self::OutputSchema => "output schema",
            Self::Kind => "kind",
            Self::SchemaRendering => "schema rendering",
        }
    }
}

/// One difference between an active surface and a candidate one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceChange {
    Added {
        name: String,
    },
    Removed {
        name: String,
    },
    Changed {
        name: String,
        fields: Vec<ToolField>,
    },
    Moved {
        name: String,
        from: usize,
        to: usize,
    },
}

impl SurfaceChange {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Added { name }
            | Self::Removed { name }
            | Self::Changed { name, .. }
            | Self::Moved { name, .. } => name,
        }
    }

    /// One line naming this difference, for a refusal a model reads.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Added { name } => {
                format!("{name}: declared by the candidate and not by the active surface")
            }
            Self::Removed { name } => {
                format!("{name}: declared by the active surface and not by the candidate")
            }
            Self::Changed { name, fields } => {
                let fields: Vec<&str> = fields.iter().map(|field| field.label()).collect();
                format!("{name}: {} changed", fields.join(", "))
            }
            Self::Moved { name, from, to } => {
                format!("{name}: presented at position {from}, now at position {to}")
            }
        }
    }
}

fn kind_of(tool: &HostedTool) -> ToolKind {
    match tool {
        HostedTool::Custom(_) => ToolKind::Raw,
        HostedTool::Function(declaration) => declaration.kind,
    }
}

fn input_schema(tool: &HostedTool) -> Option<&serde_json::Value> {
    match tool {
        HostedTool::Custom(_) => None,
        HostedTool::Function(declaration) => Some(&declaration.input_schema),
    }
}

fn output_schema(tool: &HostedTool) -> Option<&serde_json::Value> {
    match tool {
        HostedTool::Custom(_) => None,
        HostedTool::Function(declaration) => declaration.output_schema.as_ref(),
    }
}

/// True when two schema slots differ in the text a model would receive, having
/// already been found equal as values.
fn renders_differently(
    active: Option<&serde_json::Value>,
    candidate: Option<&serde_json::Value>,
) -> bool {
    match (active, candidate) {
        (Some(active), Some(candidate)) => {
            let (active, candidate) = (active.to_string(), candidate.to_string());
            active != candidate
        }
        _ => false,
    }
}

fn compare_one(active: &HostedTool, candidate: &HostedTool) -> Vec<ToolField> {
    let mut fields = Vec::new();
    if active.description() != candidate.description() {
        fields.push(ToolField::Description);
    }
    if kind_of(active) != kind_of(candidate) {
        fields.push(ToolField::Kind);
    }
    let (active_input, candidate_input) = (input_schema(active), input_schema(candidate));
    let (active_output, candidate_output) = (output_schema(active), output_schema(candidate));
    if active_input != candidate_input {
        fields.push(ToolField::InputSchema);
    }
    if active_output != candidate_output {
        fields.push(ToolField::OutputSchema);
    }
    if !fields.contains(&ToolField::InputSchema)
        && !fields.contains(&ToolField::OutputSchema)
        && (renders_differently(active_input, candidate_input)
            || renders_differently(active_output, candidate_output))
    {
        fields.push(ToolField::SchemaRendering);
    }
    fields
}

/// Every difference a model would see between the active surface and a
/// candidate one, in the active surface's order, then the candidate's
/// additions. An empty result means the candidate declares exactly what is
/// already registered, so its implementations may be swapped in.
#[must_use]
pub fn compare_surfaces(active: &[HostedTool], candidate: &[HostedTool]) -> Vec<SurfaceChange> {
    let mut changes = Vec::new();
    for (position, tool) in active.iter().enumerate() {
        let Some((found_at, counterpart)) = candidate
            .iter()
            .enumerate()
            .find(|(_, other)| other.name() == tool.name())
        else {
            changes.push(SurfaceChange::Removed {
                name: tool.name().to_owned(),
            });
            continue;
        };
        let fields = compare_one(tool, counterpart);
        if !fields.is_empty() {
            changes.push(SurfaceChange::Changed {
                name: tool.name().to_owned(),
                fields,
            });
        }
        if found_at != position {
            changes.push(SurfaceChange::Moved {
                name: tool.name().to_owned(),
                from: position,
                to: found_at,
            });
        }
    }
    for tool in candidate {
        if !active.iter().any(|other| other.name() == tool.name()) {
            changes.push(SurfaceChange::Added {
                name: tool.name().to_owned(),
            });
        }
    }
    changes
}

/// The refusal a model reads when a candidate declares a different surface.
#[must_use]
pub fn describe_changes(changes: &[SurfaceChange]) -> String {
    changes
        .iter()
        .map(SurfaceChange::describe)
        .collect::<Vec<String>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CustomToolDeclaration, ToolDeclaration};

    fn function(name: &str, description: &str, input: serde_json::Value) -> HostedTool {
        HostedTool::Function(ToolDeclaration {
            name: name.into(),
            description: description.into(),
            input_schema: input,
            output_schema: None,
            kind: ToolKind::Call,
        })
    }

    fn surface() -> Vec<HostedTool> {
        vec![
            function(
                "check",
                "Run the check",
                serde_json::json!({"type": "object"}),
            ),
            HostedTool::Custom(CustomToolDeclaration {
                name: "bash".into(),
                description: "Run a command".into(),
            }),
        ]
    }

    #[test]
    fn a_rebuilt_record_declaring_the_same_surface_compares_equal() {
        assert!(compare_surfaces(&surface(), &surface()).is_empty());
    }

    #[test]
    fn an_edited_description_is_a_change_a_model_would_see() {
        let mut candidate = surface();
        candidate[0] = function(
            "check",
            "Run the check and explain it",
            serde_json::json!({"type": "object"}),
        );
        let changes = compare_surfaces(&surface(), &candidate);
        assert_eq!(
            changes,
            vec![SurfaceChange::Changed {
                name: "check".into(),
                fields: vec![ToolField::Description],
            }]
        );
        assert_eq!(changes[0].describe(), "check: description changed");
    }

    #[test]
    fn an_added_argument_is_an_input_schema_change() {
        let mut candidate = surface();
        candidate[0] = function(
            "check",
            "Run the check",
            serde_json::json!({"type": "object", "properties": {"scope": {"type": "string"}}}),
        );
        assert_eq!(
            compare_surfaces(&surface(), &candidate),
            vec![SurfaceChange::Changed {
                name: "check".into(),
                fields: vec![ToolField::InputSchema],
            }]
        );
    }

    #[test]
    fn a_tool_gained_and_a_tool_lost_are_reported_by_name() {
        let candidate = vec![
            surface()[0].clone(),
            function(
                "review",
                "Review a diff",
                serde_json::json!({"type": "object"}),
            ),
        ];
        let changes = compare_surfaces(&surface(), &candidate);
        assert_eq!(
            changes,
            vec![
                SurfaceChange::Removed {
                    name: "bash".into()
                },
                SurfaceChange::Added {
                    name: "review".into()
                },
            ]
        );
    }

    #[test]
    fn the_same_tools_in_another_order_are_a_change() {
        let mut candidate = surface();
        candidate.reverse();
        let changes = compare_surfaces(&surface(), &candidate);
        assert_eq!(
            changes,
            vec![
                SurfaceChange::Moved {
                    name: "check".into(),
                    from: 0,
                    to: 1,
                },
                SurfaceChange::Moved {
                    name: "bash".into(),
                    from: 1,
                    to: 0,
                },
            ]
        );
    }

    #[test]
    fn a_raw_tool_that_became_a_function_changes_its_kind() {
        let mut candidate = surface();
        candidate[1] = function(
            "bash",
            "Run a command",
            serde_json::json!({"type": "object"}),
        );
        let changes = compare_surfaces(&surface(), &candidate);
        let SurfaceChange::Changed { fields, .. } = &changes[0] else {
            panic!("expected a changed tool, got {changes:?}");
        };
        assert!(fields.contains(&ToolField::Kind));
        assert!(fields.contains(&ToolField::InputSchema));
    }

    #[test]
    fn a_body_only_edit_leaves_the_surface_untouched() {
        // Nothing about an implementation reaches a declaration, so a record
        // rebuilt from edited handler bodies compares equal field by field.
        let active = surface();
        let candidate = surface();
        for (active, candidate) in active.iter().zip(candidate.iter()) {
            assert!(compare_one(active, candidate).is_empty());
        }
    }
}
