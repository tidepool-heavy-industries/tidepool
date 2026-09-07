//! Model-free checks of authored Shoal recipes. Installed only by `shoal check`.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

fn actor() -> HsType {
    HsType::Tuple(vec![HsType::Text, HsType::Int, HsType::Int])
}

fn verb(
    ctor: &'static str,
    method: &'static str,
    args: Vec<(&'static str, HsType)>,
    ret: HsType,
) -> Verb {
    Verb {
        ctor,
        method,
        args: args
            .into_iter()
            .map(|(name, ty)| Arg {
                name,
                ty,
                rust: if name == "actor" {
                    RustBinding::Path("(String, i64, i64)")
                } else {
                    RustBinding::Derived
                },
            })
            .collect(),
        ret,
        errors: None,
        handling: HandlingClass::RecipeCheck,
        extract: None,
    }
}

#[must_use]
pub fn recipe_check() -> Effect {
    Effect {
        name: "RecipeCheck",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "RecipeCheckHandler",
        handler_module: "recipe_check",
        req_enum: "RecipeCheckReq",
        decl_fn: "recipe_check_decl",
        description: &[
            "Isolated, model-free resident recipe checks; unavailable to ordinary actors.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: vec![],
        foreign_types: &[],
        errors: None,
        verbs: vec![
            verb("RecipeRoot", "root", vec![], actor()),
            verb(
                "RecipeTurn",
                "turn",
                vec![("actor", actor()), ("source", HsType::Text)],
                HsType::Text,
            ),
            verb(
                "RecipeActivation",
                "activation",
                vec![],
                HsType::Tuple(vec![
                    actor(),
                    HsType::Tuple(vec![
                        HsType::Text,
                        HsType::Text,
                        HsType::maybe(HsType::Text),
                    ]),
                ]),
            ),
            verb(
                "RecipeGit",
                "git",
                vec![
                    ("actor", actor()),
                    ("arguments", HsType::list(HsType::Text)),
                ],
                HsType::Text,
            ),
            verb(
                "RecipeWrite",
                "write",
                vec![
                    ("actor", actor()),
                    ("path", HsType::Text),
                    ("contents", HsType::Text),
                ],
                HsType::Unit,
            ),
            verb(
                "RecipeRead",
                "read",
                vec![("actor", actor()), ("path", HsType::Text)],
                HsType::Text,
            ),
            verb("RecipePresent", "present", vec![], HsType::Text),
            verb(
                "RecipeNotPresented",
                "not_presented",
                vec![("reason", HsType::Text)],
                HsType::Text,
            ),
            verb(
                "RecipeUnconfirmed",
                "unconfirmed",
                vec![("reason", HsType::Text)],
                HsType::Text,
            ),
            verb(
                "RecipeAssert",
                "assert",
                vec![("name", HsType::Text), ("holds", HsType::Bool)],
                HsType::Unit,
            ),
            verb("RecipeRestart", "restart", vec![], HsType::Text),
        ],
        helpers: vec![],
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
