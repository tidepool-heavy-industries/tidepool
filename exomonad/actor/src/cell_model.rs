//! Model work belongs to one admitted workbench execution and its principal.

use std::sync::Arc;

use tidepool_effect::{DeferredEffect, Response};
use tidepool_repr::{DataConTable, PrincipalId};
use tidepool_runtime::session::WorkbenchExecutionId;

pub use crate::generated::model_call::{ModelBoundaryError, ModelReq};
use crate::ActorDescriptor;

/// Host composition for one admitted execution. Binding captures policy and
/// allocates its cell allowance; provider work starts only in `prepare`'s work.
pub trait CellModelFactory: Send + Sync {
    fn bind(
        &self,
        execution: &WorkbenchExecutionId,
        principal: PrincipalId,
        descriptor: &ActorDescriptor,
    ) -> Arc<dyn CellModelBinding>;
}

/// Shared by every item, callback and after-tool slot in the same execution.
/// `cancel` must be idempotent and nonblocking: it signals the provider owner,
/// while the actor still joins work that has already started.
pub trait CellModelBinding: Send + Sync {
    fn prepare(
        &self,
        request: ModelReq,
        principal: PrincipalId,
        table: DataConTable,
    ) -> DeferredEffect;

    fn cancel(&self);

    /// Close admission, abandon callbacks the cell can no longer answer, and
    /// join the original invocations through their terminal receipts. An
    /// already completed callback keeps its result. Returning successfully
    /// proves settlement even when other owners retain this binding.
    fn settle(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<(), tidepool_effect::error::EffectError>>;
}

pub(crate) fn unavailable_model_work(request: ModelReq) -> DeferredEffect {
    DeferredEffect::blocking(move || {
        let error = ModelBoundaryError::ModelUnavailable(
            "this execution has no admitted model service".into(),
        );
        Ok(match request {
            ModelReq::ModelCloseWith(_) => Response::new(Err::<(), _>(error)),
            _ => Response::new(Err::<serde_json::Value, _>(error)),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_bridge::{FromHaskell, HaskellValue};
    use tidepool_repr::{DataCon, DataConId};

    #[test]
    fn unconfigured_model_returns_typed_unavailable_for_every_verb() {
        let mut table = DataConTable::new();
        for (index, (qualified, arity)) in [
            ("Data.Either.Left", 1),
            ("Tidepool.Effects.ModelUnavailable", 1),
            ("Data.Text.Text", 3),
        ]
        .into_iter()
        .enumerate()
        {
            table.insert_checked(DataCon {
            identity: tidepool_repr::execution_schema::SymbolIdentity { unit: "fixture".into(), module: "Fixture".into(), namespace: "constructor".into(), occurrence: (qualified.rsplit('.').next().unwrap().into()).clone(), record_parent: None },
                id: DataConId(index as u64),
                name: qualified.rsplit('.').next().unwrap().into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: Vec::new(),
                qualified_name: Some(qualified.into()),
                type_name: "test response".into(),
            }).expect("valid fixture metadata");
        }
        let value = || HaskellValue::Con(DataConId(50), Vec::new());
        for request in [
            ModelReq::ModelStartWith(value()),
            ModelReq::ModelResumeWith("invocation".into(), "call".into(), value()),
            ModelReq::ModelAnnotateWith("invocation".into(), "operation".into(), value()),
            ModelReq::ModelCloseWith("invocation".into()),
        ] {
            let DeferredEffect::Blocking(work) = unavailable_model_work(request) else {
                panic!("unconfigured response is ordinary owned work")
            };
            let response = work.into_inner().unwrap()().unwrap();
            let value = response.to_value(&table).unwrap();
            let HaskellValue::Con(DataConId(0), fields) = &value else {
                panic!("the unavailable response must be Left")
            };
            assert!(matches!(
                ModelBoundaryError::from_value(&fields[0], &table).unwrap(),
                ModelBoundaryError::ModelUnavailable(_)
            ));
        }
    }
}
