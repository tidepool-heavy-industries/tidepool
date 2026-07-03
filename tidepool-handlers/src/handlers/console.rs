use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Tag 0: Console
// ============================================================================

// `ConsoleReq` + `DescribeEffect` + the `EffectHandler` dispatch match are
// generated from the single-source definition in
// `tidepool-mcp/src/effect_defs.rs` — the same table that generates
// `console_decl()`. Only the handler struct and the per-verb method bodies
// below are hand-written.
tidepool_mcp::console_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct ConsoleHandler;

impl ConsoleHandler {
    fn print(
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
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;

    #[test]
    fn test_console_from_core_print() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("Print").unwrap();
        let msg = "hello".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![msg]);
        let req = ConsoleReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, ConsoleReq::Print(ref s) if s == "hello"));
    }

    #[test]
    fn test_console_dispatch_roundtrip() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![ConsoleHandler];
        let con_id = table.get_by_name("Print").unwrap();
        let msg = "test output".to_string().to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![msg]);
        let _result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_eq!(captured.drain(), vec!["test output".to_string()]);
    }
}
