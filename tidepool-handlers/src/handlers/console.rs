use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};

// ============================================================================
// Tag 0: Console
// ============================================================================

#[derive(FromCore)]
pub enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

#[derive(Clone)]
pub struct ConsoleHandler;

impl DescribeEffect for ConsoleHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::console_decl()
    }
}

impl EffectHandler<CapturedOutput> for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(
        &mut self,
        req: ConsoleReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                cx.user().push(s);
                cx.respond(())
            }
        }
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
