use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Meta handler (debug path only — --debug flag in the eval server)
// ============================================================================

// MetaReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::meta_effect_def!(crate::effect_glue::effect_rust_projection);

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

impl MetaHandler {
    fn meta_constructors(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let mut pairs: Vec<(String, i64)> = cx
            .table()
            .iter()
            .map(|dc| (dc.name.clone(), dc.rep_arity as i64))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        cx.respond(pairs)
    }

    fn meta_lookup_con(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        name: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let result: Option<(i64, i64)> = cx.table().get_by_name(&name).and_then(|id| {
            cx.table()
                .get(id)
                .map(|dc| (dc.tag as i64, dc.rep_arity as i64))
        });
        cx.respond(result)
    }

    fn meta_primops(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
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

    fn meta_effects(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(self.effect_names.clone())
    }

    fn meta_diagnostics(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let diags = tidepool_runtime::drain_diagnostics();
        cx.respond(diags)
    }

    fn meta_version(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(env!("CARGO_PKG_VERSION").to_string())
    }

    fn meta_help(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(self.helper_sigs.clone())
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
        assert!(matches!(req, MetaReq::MetaVersion()));
    }

    #[test]
    fn test_meta_from_core_constructors() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("MetaConstructors").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = MetaReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, MetaReq::MetaConstructors()));
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
