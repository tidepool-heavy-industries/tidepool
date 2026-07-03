use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};

// ============================================================================
// Meta handler (debug path only — --debug flag in the eval server)
// ============================================================================

#[derive(FromCore)]
pub enum MetaReq {
    #[core(name = "MetaConstructors")]
    Constructors,
    #[core(name = "MetaLookupCon")]
    LookupCon(String),
    #[core(name = "MetaPrimOps")]
    PrimOps,
    #[core(name = "MetaEffects")]
    Effects,
    #[core(name = "MetaDiagnostics")]
    Diagnostics,
    #[core(name = "MetaVersion")]
    Version,
    #[core(name = "MetaHelp")]
    Help,
}

#[derive(Clone)]
pub struct MetaHandler {
    effect_names: Vec<String>,
    helper_sigs: Vec<String>,
}

impl MetaHandler {
    pub fn new(effect_names: Vec<String>, helper_sigs: Vec<String>) -> Self {
        Self {
            effect_names,
            helper_sigs,
        }
    }
}

impl DescribeEffect for MetaHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::meta_decl()
    }
}

impl EffectHandler<CapturedOutput> for MetaHandler {
    type Request = MetaReq;
    fn handle(
        &mut self,
        req: MetaReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            MetaReq::Constructors => {
                let mut pairs: Vec<(String, i64)> = cx
                    .table()
                    .iter()
                    .map(|dc| (dc.name.clone(), dc.rep_arity as i64))
                    .collect();
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                cx.respond(pairs)
            }
            MetaReq::LookupCon(name) => {
                let result: Option<(i64, i64)> = cx.table().get_by_name(&name).and_then(|id| {
                    cx.table()
                        .get(id)
                        .map(|dc| (dc.tag as i64, dc.rep_arity as i64))
                });
                cx.respond(result)
            }
            MetaReq::PrimOps => {
                let primops: Vec<String> = vec![
                    "+#",
                    "-#",
                    "*#",
                    "negateInt#",
                    "==#",
                    "/=#",
                    "<#",
                    "<=#",
                    ">#",
                    ">=#",
                    "quotInt#",
                    "remInt#",
                    "andI#",
                    "orI#",
                    "xorI#",
                    "notI#",
                    "uncheckedIShiftL#",
                    "uncheckedIShiftRA#",
                    "uncheckedIShiftRL#",
                    "int2Double#",
                    "double2Int#",
                    "+##",
                    "-##",
                    "*##",
                    "/##",
                    "negateDouble#",
                    "==##",
                    "/=##",
                    "<##",
                    "<=##",
                    ">##",
                    ">=##",
                    "sqrtDouble#",
                    "sinDouble#",
                    "cosDouble#",
                    "expDouble#",
                    "logDouble#",
                    "**##",
                    "fabsDouble#",
                    "chr#",
                    "ord#",
                    "newMutVar#",
                    "readMutVar#",
                    "writeMutVar#",
                    "seq#",
                    "tagToEnum#",
                ]
                .into_iter()
                .map(String::from)
                .collect();
                cx.respond(primops)
            }
            MetaReq::Effects => cx.respond(self.effect_names.clone()),
            MetaReq::Diagnostics => {
                let diags = tidepool_runtime::drain_diagnostics();
                cx.respond(diags)
            }
            MetaReq::Version => cx.respond(env!("CARGO_PKG_VERSION").to_string()),
            MetaReq::Help => cx.respond(self.helper_sigs.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::FromCore;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;

    #[test]
    fn test_meta_from_core_version() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("MetaVersion").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = MetaReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, MetaReq::Version));
    }

    #[test]
    fn test_meta_from_core_constructors() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("MetaConstructors").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = MetaReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, MetaReq::Constructors));
    }

    #[test]
    fn test_meta_dispatch_roundtrip_version() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![MetaHandler::new(vec![], vec![])];
        let con_id = table.get_by_name("MetaVersion").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
        match &result {
            Value::Con(id, _) => {
                let name = table.name_of(*id).unwrap();
                assert_eq!(name, "Text", "MetaVersion should return a Text");
            }
            _ => panic!("Expected Con (Text), got {:?}", result),
        }
    }

    #[test]
    fn test_meta_dispatch_roundtrip_primops() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![MetaHandler::new(vec![], vec![])];
        let con_id = table.get_by_name("MetaPrimOps").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
        assert_is_cons_list(&result, &table);
    }
}
