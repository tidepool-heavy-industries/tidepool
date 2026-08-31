//! The `AskUser` suspension.
//!
//! Two constructors ride this one GADT: `AskUserWith spec` (a typed form,
//! routed to the human operator) and `NoteWith text` (a non-blocking display
//! line on the same GADT, sibling constructor). `spec`'s `Value` is a JSON
//! payload deserialized into a `FormShape` by
//! `tidepool-harness::selfharness::operator` — this schema only recognizes
//! the constructor and hands back the raw payload, same as every other
//! suspending effect.
//!
//! No real `tidepool-handlers` handler — harness-serviced only (same
//! convention as `Ask`/`RunLlmTurn`/`Finalize`/`Fork`'s own module docs); only
//! `decl_rs`/`suspension_req_rs` consume this definition. Both `askUserRaw` and
//! `noteRaw` are thin, single-verb `send` wrappers, so — unlike
//! `Ask`/`RunLlmTurn`/`Finalize`/`Fork`/`Green` — this effect's WHOLE surface
//! is representable by [`crate::schema::HelperBody`]'s existing reviewed
//! shapes, and it is fully migrated: in [`crate::effects::all`], its hand
//! copy deleted from `tidepool-mcp/src/effect_defs.rs`.
//!
//! The typed surface a caller writes is `askUser @T` (`Tidepool.Form`, which
//! derives the form from `T`'s own `Generic` representation) and `note ::
//! Text -> M ()`; `askUserRaw`/`noteRaw` here are the raw escape hatches
//! those build on.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, Polymorphism, RustBinding, Verb,
};

/// The `AskUser` suspension (both its constructors).
#[must_use]
pub fn ask_user() -> Effect {
    Effect {
        name: "AskUser",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "AskUserHandler",
        handler_module: "ask_user",
        req_enum: "AskUserReq",
        // NOT snake_case("AskUser") ("ask_user_decl") — this is the existing
        // public function name every caller already uses
        // (`tidepool_mcp::askuser_decl`); the flip must not move it.
        decl_fn: "askuser_decl",
        prompt_card: Some(&[
            "`choose :: [(Text, a)] -> M a` — labeled decision from (label, value) pairs; ",
            "ALWAYS prefer it for a decision, the label is the only text the operator sees. ",
            "`chooseMany :: [(Text, a)] -> M [a]` — pick a subset.\n",
            "`askUser @T :: M T` — form derived from `T`'s own shape: a record's fields ",
            "become named inputs, a SUM's constructors become the choices (nullary ",
            "constructors are direct options; a payload constructor is a selectable branch ",
            "with its fields), or a primitive (`Text`/`Int`/`Bool`); a bad submission ",
            "re-prompts internally, no `Either` to unwrap: `d <- askUser @Deploy`, ",
            "`k <- askUser @NextStep` then `case k of ...` to sequence follow-ups. A type ",
            "you define for this needs `deriving (Generic, FromJSON)`. Prefer RECORD syntax ",
            "for a sum's payload constructors — `Other { detail :: Text }` shows the ",
            "operator a real label, where `Other Text` only shows a generic `Contents` ",
            "field; a constructor with SEVERAL positional fields is rejected outright (no ",
            "names to key each input by), so record syntax is required once there is more ",
            "than one field.\n",
            "`note \"...\" :: M ()` — non-blocking narration to the operator's feed; call it ",
            "BEFORE presenting a form to explain what you're about to ask and why (it never ",
            "costs a turn).",
        ]),
        description: &[
            "Present a typed form to a HUMAN OPERATOR and block until they submit. ",
            "`askUser @T` presents a human form and returns `T`. Define `T` using ordinary ",
            "records and constructors, derive `Generic` and `FromJSON` for it and any ",
            "nested custom types, and end fields in `Text`, `Int`, `Double`, or `Bool`. ",
            "Constructors are choices, record fields are named inputs, and `Maybe a` is ",
            "optional; a bad submission re-prompts internally, so there is no `Either` to ",
            "unwrap. The type IS the form — there is no second description of the shape to ",
            "drift from the answer type, and the submission is ordinary JSON read back by ",
            "that same generic `FromJSON`:\n",
            "  data Env = Development | Staging | Production deriving (Generic, FromJSON)\n",
            "  data Deploy = Deploy { service :: Text, env :: Env, replicas :: Int, note :: Maybe Text } deriving (Generic, FromJSON)\n",
            "  d <- askUser @Deploy   -- then read fields with record-dot: d.service, d.env\n",
            "For alternatives that ",
            "exist only as runtime values, `choose :: [(Text, a)] -> M a` and ",
            "`chooseMany :: [(Text, a)] -> M [a]` take (label, value) pairs. ",
            "`askUserRaw :: Value -> M Value` is the raw escape hatch these are ",
            "built on, carrying the form spec as JSON directly. `note :: Text -> M ()` ",
            "posts markdown-ish text to the operator's feed WITHOUT blocking — use it to ",
            "explain what you are about to ask and why, before presenting a form.",
        ],
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Form"],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AskUserWith",
                method: "ask_user_with",
                args: vec![Arg {
                    name: "spec",
                    ty: HsType::Value,
                    rust: RustBinding::CoreValue,
                }],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::AskUserForm,
                extract: None,
            },
            Verb {
                ctor: "NoteWith",
                method: "note_with",
                args: vec![Arg {
                    name: "text",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Note,
                extract: None,
            },
        ],
        helpers: vec![
            Helper {
                name: "askUserRaw",
                ctor: Some("AskUserWith"),
                substrate: false,
                doc: &[],
                body: HelperBody::Applied(&["spec"]),
            },
            Helper {
                name: "noteRaw",
                ctor: Some("NoteWith"),
                substrate: false,
                doc: &[],
                body: HelperBody::Applied(&["text"]),
            },
        ],
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
