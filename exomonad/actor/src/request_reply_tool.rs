//! Issuer-held compiled reply decoder, retained by one native request draft.

use std::sync::Arc;
use tidepool_runtime::session::RootCustody;

pub(crate) const REPLY_TOOL: &str = "reply";

pub(crate) struct ReplyToolDefinition {
    pub(crate) installation: serde_json::Value,
    pub(crate) session: tidepool_repr::SessionId,
    pub(crate) installer: Arc<RootCustody>,
}

/// One mounted request's decoder and exact recipient. Cloning preserves the
/// original request even when the actor presents another queued activation.
pub struct RequestReplyActivation {
    actor: crate::ActorRef,
    request: crate::RequestId,
    pub(crate) lease: crate::InstalledToolLease,
    schema: harness_schema::FunctionToolSchema,
    declarations: Arc<[exomonad_tool::HostedTool]>,
    settled: std::sync::atomic::AtomicBool,
    host_attachment: std::sync::OnceLock<Arc<dyn std::any::Any + Send + Sync>>,
}

impl RequestReplyActivation {
    pub(crate) fn new(
        actor: crate::ActorRef,
        request: crate::RequestId,
        lease: crate::InstalledToolLease,
    ) -> Result<Self, crate::ResidentActorWorkbenchError> {
        let mut declarations = lease
            .tools()
            .expect("reply lease retains compiled tools")
            .declaration_snapshot()
            .to_vec();
        let [exomonad_tool::HostedTool::Function(declaration)] = declarations.as_mut_slice() else {
            return Err(crate::ResidentActorWorkbenchError::ActorProtocol(
                "reply installation lost its sole compiled function endpoint".into(),
            ));
        };
        let schema = harness_schema::FunctionToolSchema::new(declaration.input_schema.clone())
            .map_err(|error| {
                crate::ResidentActorWorkbenchError::ActorProtocol(error.to_string())
            })?;
        declaration.input_schema = schema.parameters().clone();
        Ok(Self {
            actor,
            request,
            lease,
            schema,
            declarations: declarations.into(),
            settled: false.into(),
            host_attachment: Default::default(),
        })
    }
    /// One immutable host adapter value shared by all provider snapshots of this
    /// activation. It carries no native reply authority and is not recovered.
    pub fn host_attachment<T: std::any::Any + Send + Sync>(
        &self,
        create: impl FnOnce() -> T,
    ) -> Result<Arc<T>, String> {
        self.host_attachment
            .get_or_init(|| Arc::new(create()))
            .clone()
            .downcast::<T>()
            .map_err(|_| "request activation already belongs to a different host adapter".into())
    }
    pub fn actor(&self) -> crate::ActorRef {
        self.actor
    }
    pub fn request(&self) -> crate::RequestId {
        self.request
    }
    pub fn is_settled(&self) -> bool {
        self.settled.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) fn mark_settled(&self) {
        self.settled
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn begin_reply(
        &self,
        registry: &crate::request::RequestRegistry,
        binding: &dyn crate::HostedContextBinding,
        mut native_terminal: impl FnMut(&exomonad_tool::ToolInvocationContext) -> bool,
    ) -> Result<crate::request::RequestReplyClaim, RequestReplyAdmissionError> {
        let mut accepted = None;
        binding.admit_request_reply(self, &mut |pending| {
            if self.is_settled() {
                return Err(RequestReplyAdmissionError::Refused(
                    crate::ReplyError::AlreadySettled,
                ));
            }
            let pending = pending
                .iter()
                .filter(|operation| !native_terminal(&operation.invocation))
                .cloned()
                .collect::<Vec<_>>();
            if !pending.is_empty() {
                return Err(RequestReplyAdmissionError::Pending(pending));
            }
            let claim = registry
                .begin_reply(self.actor, self.request)
                .map_err(RequestReplyAdmissionError::Refused)?;
            self.mark_settled();
            accepted = Some(claim);
            Ok(())
        })?;
        accepted.ok_or_else(|| {
            RequestReplyAdmissionError::Unavailable(
                "host accepted reply admission without claiming its request".into(),
            )
        })
    }

    pub(crate) fn select(
        self: &Arc<Self>,
        name: &str,
    ) -> Option<crate::resident_workbench::InstalledToolSelection> {
        let mut selected = self.lease.select(name)?;
        selected.reply_activation = Some(self.clone());
        Some(selected)
    }
    pub(crate) fn declarations(&self) -> Arc<[exomonad_tool::HostedTool]> {
        self.declarations.clone()
    }

    /// Validate the provider wire representation before its single projection
    /// into the ordinary compiled Haskell handler's argument representation.
    pub(crate) fn admit_wire_arguments(
        &self,
        wire: serde_json::Value,
    ) -> Result<ReplyHostArguments, crate::ResidentToolError> {
        ReplyHostArguments::admit(&self.schema, wire)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedInvocationRole {
    Domain,
    Observation,
    Settlement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedInvocationState {
    Queued,
    Running,
    Unconfirmed,
}

#[derive(Debug, Clone)]
pub struct HostedPendingInvocation {
    pub invocation: exomonad_tool::ToolInvocationContext,
    pub name: String,
    pub state: HostedInvocationState,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestReplyAdmissionError {
    #[error(
        "request-owned operations remain pending: {0:?}; inspect status and use yield before retrying reply"
    )]
    Pending(Vec<HostedPendingInvocation>),
    #[error("reply rejected: {0:?}")]
    Refused(crate::ReplyError),
    #[error("reply admission unavailable: {0}")]
    Unavailable(String),
}

/// Only the retained activation schema can construct checked handler input.
pub(crate) struct ReplyHostArguments(serde_json::Value);
impl ReplyHostArguments {
    fn admit(schema: &harness_schema::FunctionToolSchema, wire: serde_json::Value) -> Result<Self, crate::ResidentToolError> {
        harness_schema::validate_value(&wire, schema.parameters(), "$")
            .map_err(|error| crate::ResidentToolError::InvalidInvocation(error.to_string()))?;
        schema.decode_arguments(wire).map(Self)
            .map_err(|error| crate::ResidentToolError::InvalidInvocation(error.to_string()))
    }

    pub(crate) fn into_arguments(self) -> exomonad_tool::ToolArguments {
        exomonad_tool::ToolArguments::Structured(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedInputStage {
    /// Provider projection is performed by the host's normal adapter.
    HostArguments,
    /// The exact endpoint owns strict wire validation and projection together.
    ProviderWire,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report_schema() -> harness_schema::FunctionToolSchema {
        harness_schema::FunctionToolSchema::new(json!({
            "type":"object", "properties":{"result":{
                "type":"object", "properties":{
                    "count":{"type":"integer"},
                    "note":{"type":"string"},
                    "choice":{"anyOf":[{"type":"string"},{"type":"null"}]}
                }, "required":["count","choice"], "additionalProperties":false
            }}, "required":["result"], "additionalProperties":false
        })).unwrap()
    }

    #[test]
    fn wire_validation_precedes_projection_and_refuses_unknown_missing_and_null() {
        let schema = report_schema();
        let valid = json!({"result":{"count":3,"note":null,"choice":null}});
        let admitted = ReplyHostArguments::admit(&schema, valid.clone()).unwrap();
        assert_eq!(admitted.0, json!({"result":{"count":3,"choice":null}}));
        for invalid in [
            json!({}),
            json!({"result":{"count":3,"note":null,"choice":null},"extra":true}),
            json!({"result":{"count":3,"note":null,"choice":null,"extra":true}}),
            json!({"result":{"note":null,"choice":null}}),
            json!({"result":{"count":null,"note":null,"choice":null}}),
            json!({"result":{"count":3,"choice":null}}),
            json!({"result":{"count":3,"note":null,"choice":9}}),
        ] {
            assert!(ReplyHostArguments::admit(&schema, invalid.clone()).is_err(), "{invalid}");
        }
        // The projected host representation is deliberately not provider wire.
        // Revalidation here catches an adapter accidentally projecting twice.
        assert!(ReplyHostArguments::admit(&schema, admitted.0).is_err());
        assert!(ReplyHostArguments::admit(&schema, valid).is_ok());
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(48))]
        #[test]
        fn generated_wire_and_host_representations_keep_optional_and_union_contracts(
            count in -1000i64..1000,
            note in proptest::option::of("[a-zλ]{0,16}"),
            choice in proptest::option::of("[a-z]{0,16}"),
            unknown in "x[a-z]{1,12}",
        ) {
            let schema = report_schema();
            let wire = json!({"result":{"count":count,"note":note,"choice":choice}});
            let admitted = ReplyHostArguments::admit(&schema, wire.clone()).unwrap();
            let projected = &admitted.0["result"];
            proptest::prop_assert_eq!(projected["count"].as_i64(), Some(count));
            proptest::prop_assert_eq!(projected.get("note"), note.as_ref().map(|_| &wire["result"]["note"]));
            proptest::prop_assert_eq!(&projected["choice"], &wire["result"]["choice"]);
            let mut extra = wire.clone();
            extra["result"].as_object_mut().unwrap().insert(unknown, json!(true));
            proptest::prop_assert!(ReplyHostArguments::admit(&schema, extra).is_err());
            let mut missing = wire;
            missing["result"].as_object_mut().unwrap().remove("count");
            proptest::prop_assert!(ReplyHostArguments::admit(&schema, missing).is_err());
        }
    }
}
