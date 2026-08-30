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
    use tidepool_bridge::ToCore;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;

    #[test]
    fn test_console_dispatch_roundtrip() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![ConsoleHandler];
        let con_id = table.get_by_name("Print").unwrap();
        let msg = "test output".to_string().to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![msg]);
        handlers
            .dispatch(&request, &cx)
            .unwrap()
            .expect("Print should be handled");
        assert_eq!(captured.drain(), vec!["test output".to_string()]);
    }
}
