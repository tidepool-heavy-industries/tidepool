//! The Exec effect — PRD 22's first migrated effect.
//!
//! Chosen because it is the smallest effect present in registries 1, 2 AND 4:
//! the macro DSL, the bridged `Proc` record, and the harness's constructor-name
//! hole classifier. That triangle is where the silent-misroute bug class the
//! PRD exists to close actually lives. See the scaffold doc §2.
//!
//! It is also where the no-raw-Haskell rule gets its first real test. All three
//! Exec helpers use the old grammar's `raw` escape hatch, but none of them is
//! irregular: `run` is exactly the point-free form and `runIn` exactly the
//! applied one. They are `raw` because the old helper grammar required at least
//! one doc line, and `runIn`/`runArgv` carry no doc comment. The hatch was
//! standing in for a missing one-line affordance — here, an EMPTY `doc` slice.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody,
    OuterEffect, RustBinding, Verb,
};

/// `Proc` — the bridged result record. Its Haskell declaration is NOT generated
/// from this schema: it is single-sourced from the Rust struct in
/// `tidepool-bridge-effects` through the `CoreRecord` derive, into the committed
/// `haskell/lib/Tidepool/Records/Bridged.hs`. Folding that mechanism into the
/// schema is a later lane; here the type is referenced by name only.
fn proc() -> HsType {
    HsType::Named("Proc")
}

/// The Exec effect, completely.
#[must_use]
pub fn exec() -> Effect {
    Effect {
        name: "Exec",
        handler: "ExecHandler",
        req_enum: "ExecReq",
        decl_fn: "exec_decl",
        description: &["Run shell commands and capture output."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: false,
        extra_imports: &[
            "import qualified Tidepool.Shell as Shell",
            "import Tidepool.Shell (sh)",
            "import qualified Tidepool.Cargo as Cargo",
        ],
        type_defs: Vec::new(),
        // #335 typed-failure ADT. A nonzero EXIT is NOT a failure here — `run`
        // still returns a Proc with its exitCode on nonzero exit; `Left` is
        // only for a spawn failure or a bad/escaping working directory.
        errors: Some(ErrorAdt {
            name: "ExecError",
            variants: vec![
                ErrorVariant {
                    ctor: "ExecSpawn",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "the process could not be spawned",
                },
                ErrorVariant {
                    ctor: "ExecBadDir",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "working directory is invalid or escapes the sandbox",
                },
            ],
        }),
        verbs: vec![
            Verb {
                ctor: "Run",
                method: "exec_run",
                args: vec![Arg {
                    name: "cmd",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                }],
                ret: proc(),
                errors: Some("ExecError"),
                handling: HandlingClass::OuterDispatch(OuterEffect::Exec),
                extract: None,
            },
            Verb {
                ctor: "RunIn",
                method: "exec_run_in",
                args: vec![
                    Arg {
                        name: "dir",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "cmd",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                ],
                ret: proc(),
                errors: Some("ExecError"),
                handling: HandlingClass::OuterDispatch(OuterEffect::Exec),
                extract: None,
            },
            // Shell-free exec: argv list, no sh -c. Safe with metachars ($1, globs).
            Verb {
                ctor: "RunArgv",
                method: "exec_run_argv",
                args: vec![Arg {
                    name: "argv",
                    ty: HsType::list(HsType::Text),
                    rust: RustBinding::Derived,
                }],
                ret: proc(),
                errors: Some("ExecError"),
                handling: HandlingClass::OuterDispatch(OuterEffect::Exec),
                extract: None,
            },
        ],
        helpers: vec![
            Helper {
                name: "run",
                ctor: "Run",
                doc: &[
                    "Run a shell command; returns a `Proc` record {exitCode, stdout, stderr}",
                    "(use `ok p` for the zero-exit check). Failure is TYPED (#335): `Left",
                    "(ExecSpawn _)` when the process can't be spawned, `Left (ExecBadDir _)`",
                    "for `runIn` with a bad/escaping directory. A nonzero EXIT is NOT a",
                    "failure — inspect `p.exitCode`. Natural spelling: `Right p <- run cmd`.",
                ],
                body: HelperBody::Pointfree,
            },
            Helper {
                name: "runIn",
                ctor: "RunIn",
                doc: &[],
                body: HelperBody::Applied(&["dir", "cmd"]),
            },
            Helper {
                name: "runArgv",
                ctor: "RunArgv",
                doc: &[],
                body: HelperBody::Pointfree,
            },
        ],
    }
}
