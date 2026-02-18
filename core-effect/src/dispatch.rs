use crate::error::EffectError;
use core_bridge::FromCore;
use core_eval::value::Value;
use core_repr::DataConTable;
use frunk::{HCons, HNil};

pub trait EffectHandler {
    type Request: FromCore;
    fn handle(
        &mut self,
        req: Self::Request,
        table: &DataConTable,
    ) -> Result<Value, EffectError>;
}

pub trait DispatchEffect {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Value, EffectError>;
}

impl DispatchEffect for HNil {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _table: &DataConTable,
    ) -> Result<Value, EffectError> {
        Err(EffectError::UnhandledEffect { tag })
    }
}

impl<H: EffectHandler, T: DispatchEffect> DispatchEffect for HCons<H, T> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Value, EffectError> {
        if tag == 0 {
            let req = H::Request::from_value(request, table)?;
            self.head.handle(req, table)
        } else {
            self.tail.dispatch(tag - 1, request, table)
        }
    }
}
