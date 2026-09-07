//! One-way actor notifications. Durable receipt ownership stays in the host inbox.
use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeShape, VariantFields, Verb, WireDerives,
};

fn address() -> HsType {
    HsType::Tuple(vec![HsType::Int, HsType::Int])
}
fn receipt() -> HsType {
    HsType::Tuple(vec![
        address(),
        HsType::Tuple(vec![
            address(),
            HsType::Tuple(vec![HsType::Text, HsType::Int]),
        ]),
    ])
}
fn variant(ctor: &'static str, fields: Vec<HsType>) -> SumVariant {
    SumVariant {
        ctor,
        fields: VariantFields::Positional(fields),
        doc: &[],
    }
}
fn sum(name: &'static str, variants: Vec<SumVariant>) -> TypeDef {
    TypeDef {
        name,
        wire_rust: None,
        shape: TypeShape::Sum { variants },
        json: JsonInstance::None,
        derives: WireDerives(&[]),
        domain: None,
        doc: &[],
    }
}

pub fn notifications() -> Effect {
    Effect {
        name: "Notifications", authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "NotificationsDecodeHandler", handler_module: "notifications", req_enum: "NotificationsReq", decl_fn: "notifications_decl",
        description: &["One-way actor notifications with inbox-backed delivery observations; no typed response obligation."], prompt_card: None,
        type_params: &[], default_row_args: &[], helpers_row_polymorphic: true, extra_imports: &[],
        type_defs: vec![
            sum("NotificationError", vec![variant("NotificationUnauthorized", vec![]), variant("NotificationUnavailable", vec![]), variant("NotificationInvalidReceipt", vec![]), variant("NotificationAdmissionUnconfirmed", vec![HsType::Text]), variant("NotificationStorageFailure", vec![HsType::Text])]),
            sum("NotificationState", vec![variant("NotificationAccepted", vec![]), variant("NotificationPresented", vec![]), variant("NotificationUnconfirmed", vec![])]),
        ], foreign_types: &[], errors: None,
        verbs: vec![
            Verb { ctor: "NotifyWith", method: "notify_with", args: vec![Arg { name: "target", ty: address(), rust: RustBinding::Path("(i64, i64)") }, Arg { name: "message", ty: HsType::Text, rust: RustBinding::Derived }], ret: HsType::either(HsType::Named("NotificationError"), receipt()), errors: None, handling: HandlingClass::Actor, extract: None },
            Verb { ctor: "PollNotificationWith", method: "poll_notification_with", args: vec![Arg { name: "receipt", ty: receipt(), rust: RustBinding::Path("((i64, i64), ((i64, i64), (String, i64)))") }], ret: HsType::either(HsType::Named("NotificationError"), HsType::Named("NotificationState")), errors: None, handling: HandlingClass::Actor, extract: None },
        ], helpers: vec![], polymorphism: Polymorphism::None, dispatched: false,
    }
}
