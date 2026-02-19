use crate::error::EffectError;
use core_bridge::{FromCore, ToCore};
use core_eval::value::Value;
use core_repr::DataConTable;
use frunk::{HCons, HNil};

pub struct EffectContext<'a, U = ()> {
    table: &'a DataConTable,
    user: &'a U,
}

impl<'a, U> EffectContext<'a, U> {
    pub fn with_user(table: &'a DataConTable, user: &'a U) -> Self {
        Self { table, user }
    }

    pub fn respond<T: ToCore>(&self, val: T) -> Result<Value, EffectError> {
        val.to_value(self.table).map_err(EffectError::Bridge)
    }

    pub fn table(&self) -> &DataConTable {
        self.table
    }

    pub fn user(&self) -> &U {
        self.user
    }
}

pub trait EffectHandler<U = ()> {
    type Request: FromCore;
    fn handle(
        &mut self,
        req: Self::Request,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError>;
}

pub trait DispatchEffect<U = ()> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError>;
}

impl<U> DispatchEffect<U> for HNil {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError> {
        Err(EffectError::UnhandledEffect { tag })
    }
}

impl<U, H: EffectHandler<U>, T: DispatchEffect<U>> DispatchEffect<U> for HCons<H, T> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError> {
        if tag == 0 {
            let req = H::Request::from_value(request, cx.table())?;
            self.head.handle(req, cx)
        } else {
            self.tail.dispatch(tag - 1, request, cx)
        }
    }
}

// --- Async effect handling ---

#[cfg(feature = "async")]
pub trait AsyncEffectHandler<U = ()>: Send {
    type Request: FromCore + Send;
    fn handle<'a>(
        &'a mut self,
        req: Self::Request,
        cx: &'a EffectContext<'a, U>,
    ) -> impl std::future::Future<Output = Result<Value, EffectError>> + Send + 'a;
}

#[cfg(feature = "async")]
pub trait AsyncDispatchEffect<U = ()>: Send {
    fn dispatch<'a>(
        &'a mut self,
        tag: u64,
        request: &'a Value,
        cx: &'a EffectContext<'a, U>,
    ) -> impl std::future::Future<Output = Result<Value, EffectError>> + Send + 'a;
}

#[cfg(feature = "async")]
impl<U: Sync> AsyncDispatchEffect<U> for HNil {
    fn dispatch<'a>(
        &'a mut self,
        tag: u64,
        _request: &'a Value,
        _cx: &'a EffectContext<'a, U>,
    ) -> impl std::future::Future<Output = Result<Value, EffectError>> + Send + 'a {
        async move { Err(EffectError::UnhandledEffect { tag }) }
    }
}

#[cfg(feature = "async")]
impl<U, H, T> AsyncDispatchEffect<U> for HCons<H, T>
where
    U: Sync,
    H: AsyncEffectHandler<U> + Send,
    T: AsyncDispatchEffect<U> + Send,
{
    fn dispatch<'a>(
        &'a mut self,
        tag: u64,
        request: &'a Value,
        cx: &'a EffectContext<'a, U>,
    ) -> impl std::future::Future<Output = Result<Value, EffectError>> + Send + 'a {
        async move {
            if tag == 0 {
                let req = H::Request::from_value(request, cx.table())?;
                self.head.handle(req, cx).await
            } else {
                self.tail.dispatch(tag - 1, request, cx).await
            }
        }
    }
}
