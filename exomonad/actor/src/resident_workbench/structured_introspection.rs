//! Structured introspection projection: `ToHaskell` visitors for scope
//! queries, identifier/type info, and query errors surfaced by the
//! introspection inspection kinds, plus the typed answer enum they build.

use super::*;
use super::to_haskell::{actor_haskell_int, visit_core, visit_named};

fn visit_introspection_scope(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    scope: &tidepool_runtime::session::NameScope,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::NameScope;
    match scope {
        NameScope::Current => visit_core(table, visitor, "CurrentScope", |_| Ok(())),
        NameScope::PublicModule(module) => visit_core(table, visitor, "PublicModule", |visitor| {
            module.visit(table, visitor)
        }),
    }
}

fn visit_introspection_query(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    query: &tidepool_runtime::session::NameQuery,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::NameNamespace;
    visit_core(table, visitor, "NameQuery", |visitor| {
        visit_introspection_scope(table, visitor, &query.scope)?;
        visit_core(
            table,
            visitor,
            match query.namespace {
                NameNamespace::Any => "AnyName",
                NameNamespace::Value => "ValueName",
                NameNamespace::Type => "TypeName",
                NameNamespace::Constructor => "ConstructorName",
            },
            |_| Ok(()),
        )?;
        query.name.visit(table, visitor)
    })
}

fn visit_introspection_identifier(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    identifier: &tidepool_runtime::session::IdentifierRef,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::IdentifierNamespace;
    visit_core(table, visitor, "IdentifierRef", |visitor| {
        identifier.module.visit(table, visitor)?;
        identifier.name.visit(table, visitor)?;
        visit_core(
            table,
            visitor,
            match identifier.namespace {
                IdentifierNamespace::Value => "ValueIdentifier",
                IdentifierNamespace::Type => "TypeIdentifier",
                IdentifierNamespace::Constructor => "ConstructorIdentifier",
                IdentifierNamespace::Field => "FieldIdentifier",
            },
            |_| Ok(()),
        )
    })
}

fn visit_introspection_type_expression(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    ty: &tidepool_runtime::session::TypeExpression,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "TypeExpression", |visitor| {
        ty.canonical.visit(table, visitor)?;
        ty.variables.visit(table, visitor)?;
        ty.constraints.visit(table, visitor)
    })
}

fn visit_introspection_provenance(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    provenance: &tidepool_runtime::session::ScopeProvenance,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "ScopeProvenance", |visitor| {
        visit_introspection_scope(table, visitor, &provenance.scope)?;
        actor_haskell_int(provenance.generation, "scope generation")?.visit(table, visitor)?;
        provenance.fingerprint.visit(table, visitor)
    })
}

fn visit_introspection_field(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    field: &tidepool_runtime::session::FieldInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "FieldInfo", |visitor| {
        field.name.visit(table, visitor)?;
        visit_introspection_type_expression(table, visitor, &field.ty)
    })
}

fn visit_structural_list<T>(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    values: &[T],
    mut visit_value: impl FnMut(&T, &mut dyn HaskellVisitor) -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    let nil = tidepool_bridge::get_qualified(table, "GHC.Types.[]", 0)
        .ok_or_else(|| BridgeError::UnknownDataConName("[]".into()))?;
    let cons = tidepool_bridge::get_qualified(table, "GHC.Types.:", 2)
        .ok_or_else(|| BridgeError::UnknownDataConName(":".into()))?;
    for value in values {
        visitor.begin_constructor(cons, 2)?;
        visit_value(value, visitor)?;
    }
    visitor.begin_constructor(nil, 0)?;
    visitor.end_constructor()?;
    for _ in values {
        visitor.end_constructor()?;
    }
    Ok(())
}

fn visit_introspection_constructor(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    constructor: &tidepool_runtime::session::ConstructorInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "ConstructorInfo", |visitor| {
        visit_introspection_identifier(table, visitor, &constructor.identifier)?;
        visit_introspection_type_expression(table, visitor, &constructor.ty)?;
        visit_structural_list(table, visitor, &constructor.arguments, |ty, visitor| {
            visit_introspection_type_expression(table, visitor, ty)
        })?;
        visit_structural_list(table, visitor, &constructor.fields, |field, visitor| {
            visit_introspection_field(table, visitor, field)
        })
    })
}

fn visit_introspection_declaration(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    declaration: &tidepool_runtime::session::DeclarationInfo,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::DeclarationInfo;
    match declaration {
        DeclarationInfo::Value(ty) => visit_core(table, visitor, "ValueDeclaration", |visitor| {
            visit_introspection_type_expression(table, visitor, ty)
        }),
        DeclarationInfo::Data {
            parameters,
            constructors,
        } => visit_core(table, visitor, "DataDeclaration", |visitor| {
            parameters.visit(table, visitor)?;
            visit_structural_list(table, visitor, constructors, |constructor, visitor| {
                visit_introspection_constructor(table, visitor, constructor)
            })
        }),
        DeclarationInfo::Newtype {
            parameters,
            constructor,
        } => visit_core(table, visitor, "NewtypeDeclaration", |visitor| {
            parameters.visit(table, visitor)?;
            visit_introspection_constructor(table, visitor, constructor)
        }),
        DeclarationInfo::TypeSynonym { parameters, body } => {
            visit_core(table, visitor, "TypeSynonymDeclaration", |visitor| {
                parameters.visit(table, visitor)?;
                visit_introspection_type_expression(table, visitor, body)
            })
        }
        DeclarationInfo::Class {
            parameters,
            superclasses,
            methods,
        } => visit_core(table, visitor, "ClassDeclaration", |visitor| {
            parameters.visit(table, visitor)?;
            visit_structural_list(table, visitor, superclasses, |ty, visitor| {
                visit_introspection_type_expression(table, visitor, ty)
            })?;
            visit_structural_list(table, visitor, methods, |method, visitor| {
                visit_core(table, visitor, "ClassMethodInfo", |visitor| {
                    visit_introspection_identifier(table, visitor, &method.identifier)?;
                    visit_introspection_type_expression(table, visitor, &method.ty)
                })
            })
        }),
        DeclarationInfo::Constructor {
            parent,
            constructor,
        } => visit_core(table, visitor, "ConstructorDeclaration", |visitor| {
            visit_introspection_identifier(table, visitor, parent)?;
            visit_introspection_constructor(table, visitor, constructor)
        }),
        DeclarationInfo::RecordSelector { parent, ty } => {
            visit_core(table, visitor, "RecordSelectorDeclaration", |visitor| {
                visit_introspection_identifier(table, visitor, parent)?;
                visit_introspection_type_expression(table, visitor, ty)
            })
        }
    }
}

fn visit_optional_identifier(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    identifier: Option<&tidepool_runtime::session::IdentifierRef>,
) -> Result<(), BridgeError> {
    match identifier {
        Some(identifier) => visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
            visit_introspection_identifier(table, visitor, identifier)
        }),
        None => visit_named(table, visitor, "GHC.Maybe", "Nothing", |_| Ok(())),
    }
}

fn visit_introspection_info(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    info: &tidepool_runtime::session::IdentifierInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "IdentifierInfo", |visitor| {
        visit_introspection_identifier(table, visitor, &info.identifier)?;
        visit_introspection_declaration(table, visitor, &info.declaration)?;
        visit_optional_identifier(table, visitor, info.parent.as_ref())?;
        visit_introspection_provenance(table, visitor, &info.provenance)
    })
}

fn visit_introspection_type(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    info: &tidepool_runtime::session::TypeInfo,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "TypeInfo", |visitor| {
        visit_introspection_identifier(table, visitor, &info.identifier)?;
        visit_introspection_type_expression(table, visitor, &info.expression)?;
        visit_introspection_provenance(table, visitor, &info.provenance)
    })
}

fn visit_introspection_query_error(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    error: &tidepool_runtime::session::QueryError,
) -> Result<(), BridgeError> {
    use tidepool_runtime::session::QueryError;
    match error {
        QueryError::Unknown(query) => visit_core(table, visitor, "UnknownIdentifier", |visitor| {
            visit_introspection_query(table, visitor, query)
        }),
        QueryError::Ambiguous(query, candidates) => {
            visit_core(table, visitor, "AmbiguousIdentifier", |visitor| {
                visit_introspection_query(table, visitor, query)?;
                visit_structural_list(table, visitor, candidates, |candidate, visitor| {
                    visit_introspection_identifier(table, visitor, candidate)
                })
            })
        }
        QueryError::UnknownModule(module) => {
            visit_core(table, visitor, "UnknownModule", |visitor| {
                module.visit(table, visitor)
            })
        }
        QueryError::Unsupported(detail) => {
            visit_core(table, visitor, "UnsupportedDeclaration", |visitor| {
                detail.visit(table, visitor)
            })
        }
    }
}

pub(super) enum StructuredIntrospectionAnswer {
    ScopeChanged {
        before: tidepool_runtime::session::ScopeProvenance,
        after: tidepool_runtime::session::ScopeProvenance,
    },
    Info(Box<tidepool_runtime::session::IdentifierInfo>),
    Type(tidepool_runtime::session::TypeInfo),
    QueryError(tidepool_runtime::session::QueryError),
    CompilerUnavailable(String),
}

impl tidepool_bridge::sealed::ToHaskellSealed for StructuredIntrospectionAnswer {}
impl ToHaskell for StructuredIntrospectionAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let right = matches!(self, Self::Info(_) | Self::Type(_));
        visit_named(
            table,
            visitor,
            "Data.Either",
            if right { "Right" } else { "Left" },
            |visitor| match self {
                Self::ScopeChanged { before, after } => {
                    visit_core(table, visitor, "ScopeChanged", |visitor| {
                        visit_introspection_provenance(table, visitor, before)?;
                        visit_introspection_provenance(table, visitor, after)
                    })
                }
                Self::Info(info) => visit_introspection_info(table, visitor, info),
                Self::Type(info) => visit_introspection_type(table, visitor, info),
                Self::QueryError(error) => visit_introspection_query_error(table, visitor, error),
                Self::CompilerUnavailable(detail) => {
                    visit_core(table, visitor, "CompilerUnavailable", |visitor| {
                        detail.visit(table, visitor)
                    })
                }
            },
        )
    }
}
