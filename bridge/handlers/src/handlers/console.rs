use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Tag 0: Console
// ============================================================================

// Codecs and dispatch are generated from the protocol schema.
pub use crate::generated::console::ConsoleReq;

#[derive(Clone)]
pub struct ConsoleHandler;

impl ConsoleHandler {
    pub(crate) fn display_view_with(
        &mut self,
        _cx: &EffectContext<'_, CapturedOutput>,
        _view: crate::effect_glue::JsonArg,
    ) -> Result<tidepool_effect::Response, EffectError> {
        Err(EffectError::Handler(
            "rich display requires an actor resource owner".into(),
        ))
    }
    pub(crate) fn display_with(
        &mut self,
        _cx: &EffectContext<'_, CapturedOutput>,
        _view: ((i64, i64, i64), String, Vec<(i64, String)>, bool),
        _continuation: tidepool_bridge::HaskellValue,
    ) -> Result<tidepool_effect::Response, EffectError> {
        Err(EffectError::Handler(
            "display requires an actor resource owner".into(),
        ))
    }

    pub(crate) fn display_expand_with(
        &mut self,
        _cx: &EffectContext<'_, CapturedOutput>,
        _selection: ((i64, i64, i64), i64),
    ) -> Result<tidepool_effect::Response, EffectError> {
        Err(EffectError::Handler(
            "display expansion requires an actor resource owner".into(),
        ))
    }

    pub(crate) fn display_allowance_with(
        &mut self,
        _cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        Err(EffectError::Handler(
            "display allowance requires an actor resource owner".into(),
        ))
    }

    pub(crate) fn display_expansion_input_with(
        &mut self,
        _cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        Err(EffectError::Handler(
            "display input requires an active expansion".into(),
        ))
    }

    pub(crate) fn print(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        msg: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.user().push(msg);
        cx.respond(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::HaskellValue;
    use tidepool_bridge::ToHaskell;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};

    #[test]
    pub(crate) fn test_console_dispatch_roundtrip() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![ConsoleHandler];
        let con_id = table.get_by_name("Print").unwrap();
        let msg = "test output".to_string().to_value(&table).unwrap();
        let request = HaskellValue::Con(con_id, vec![msg]);
        handlers
            .dispatch(&request, &cx)
            .unwrap()
            .expect("Print should be handled");
        assert_eq!(captured.drain(), vec!["test output".to_string()]);
    }
}
