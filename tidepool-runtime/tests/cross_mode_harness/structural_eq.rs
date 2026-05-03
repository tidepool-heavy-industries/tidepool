use std::collections::HashMap;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{AltCon, DataConId, JoinId, VarId};
use tidepool_repr::DataConTable;
use tidepool_eval::value::Value;
use super::CrossModeArtifacts;

/// Asserts that two Core expression trees are structurally equivalent.
pub fn assert_equivalent(art: &CrossModeArtifacts) {
    let single = &art.single_expr.nodes;
    let split = &art.split_expr.nodes;

    if single.len() != split.len() {
        panic!(
            "cross-mode divergence: tree size mismatch (single={}, split={})",
            single.len(),
            split.len()
        );
    }

    assert_table_compatible(&art.single_table, &art.split_table);

    // Alpha-equivalence tracking
    let mut var_map: HashMap<VarId, VarId> = HashMap::new();
    let mut join_map: HashMap<JoinId, JoinId> = HashMap::new();

    // Start walk from the root (last node)
    if !single.is_empty() {
        check_node(
            single.len() - 1,
            single,
            &art.single_table,
            split,
            &art.split_table,
            &mut var_map,
            &mut join_map,
        );
    }
}

fn check_node(
    idx: usize,
    single: &[CoreFrame<usize>],
    s_table: &DataConTable,
    split: &[CoreFrame<usize>],
    p_table: &DataConTable,
    var_map: &mut HashMap<VarId, VarId>,
    join_map: &mut HashMap<JoinId, JoinId>,
) {
    let s_node = &single[idx];
    let p_node = &split[idx];

    match (s_node, p_node) {
        (CoreFrame::Var(s_v), CoreFrame::Var(p_v)) => {
            // Alpha-equivalence at Var reference sites. If the single-side
            // VarId already has an established mapping, the split-side must
            // match it. Otherwise this is a free reference (typically a
            // top-level binder visible in both compilations) and we record
            // the pairing so subsequent occurrences of `s_v` are checked
            // for consistency. Without this insert, the original code
            // accepted *any* `p_v` for a previously-unseen `s_v`, which
            // would mask real divergences on free references.
            match var_map.get(s_v) {
                Some(&expected_p) => {
                    if expected_p != *p_v {
                        panic!(
                            "node {}: Var mismatch (alpha-equivalence). Expected {:?}, got {:?}",
                            idx, expected_p, p_v
                        );
                    }
                }
                None => {
                    var_map.insert(*s_v, *p_v);
                }
            }
        }
        (CoreFrame::Lit(sl), CoreFrame::Lit(pl)) => {
            if sl != pl {
                panic!("node {}: literals differ ({:?} vs {:?})", idx, sl, pl);
            }
        }
        (CoreFrame::App { fun: sf, arg: sa }, CoreFrame::App { fun: pf, arg: pa }) => {
            if sf != pf { panic!("node {}: App fun index mismatch ({} vs {})", idx, sf, pf); }
            if sa != pa { panic!("node {}: App arg index mismatch ({} vs {})", idx, sa, pa); }
            check_node(*sf, single, s_table, split, p_table, var_map, join_map);
            check_node(*sa, single, s_table, split, p_table, var_map, join_map);
        }
        (CoreFrame::Lam { binder: s_v, body: s_b }, CoreFrame::Lam { binder: p_v, body: p_b }) => {
            if s_b != p_b { panic!("node {}: Lam body index mismatch ({} vs {})", idx, s_b, p_b); }
            let old = var_map.insert(*s_v, *p_v);
            check_node(*s_b, single, s_table, split, p_table, var_map, join_map);
            if let Some(o) = old {
                var_map.insert(*s_v, o);
            } else {
                var_map.remove(s_v);
            }
        }
        (CoreFrame::LetNonRec { binder: s_v, rhs: s_r, body: s_b }, CoreFrame::LetNonRec { binder: p_v, rhs: p_r, body: p_b }) => {
            if s_r != p_r { panic!("node {}: LetNonRec rhs index mismatch ({} vs {})", idx, s_r, p_r); }
            if s_b != p_b { panic!("node {}: LetNonRec body index mismatch ({} vs {})", idx, s_b, p_b); }
            check_node(*s_r, single, s_table, split, p_table, var_map, join_map);
            let old = var_map.insert(*s_v, *p_v);
            check_node(*s_b, single, s_table, split, p_table, var_map, join_map);
            if let Some(o) = old {
                var_map.insert(*s_v, o);
            } else {
                var_map.remove(s_v);
            }
        }
        (CoreFrame::LetRec { bindings: s_binds, body: s_body }, CoreFrame::LetRec { bindings: p_binds, body: p_body }) => {
            if s_binds.len() != p_binds.len() {
                panic!("node {}: LetRec binding count mismatch", idx);
            }
            if s_body != p_body {
                panic!("node {}: LetRec body index mismatch ({} vs {})", idx, s_body, p_body);
            }
            let mut olds = Vec::new();
            for ((s_v, s_r), (p_v, p_r)) in s_binds.iter().zip(p_binds.iter()) {
                if s_r != p_r { panic!("node {}: LetRec binding index mismatch ({} vs {})", idx, s_r, p_r); }
                olds.push((*s_v, var_map.insert(*s_v, *p_v)));
            }
            for (_, s_r) in s_binds.iter() {
                check_node(*s_r, single, s_table, split, p_table, var_map, join_map);
            }
            check_node(*s_body, single, s_table, split, p_table, var_map, join_map);
            for (s_v, old) in olds {
                if let Some(o) = old {
                    var_map.insert(s_v, o);
                } else {
                    var_map.remove(&s_v);
                }
            }
        }
        (CoreFrame::Case { scrutinee: ss, binder: s_v, alts: s_alts }, CoreFrame::Case { scrutinee: ps, binder: p_v, alts: p_alts }) => {
            if ss != ps { panic!("node {}: Case scrutinee index mismatch ({} vs {})", idx, ss, ps); }
            check_node(*ss, single, s_table, split, p_table, var_map, join_map);
            if s_alts.len() != p_alts.len() {
                panic!("node {}: Case alt count mismatch", idx);
            }
            let old = var_map.insert(*s_v, *p_v);
            for (j, (s_alt, p_alt)) in s_alts.iter().zip(p_alts.iter()).enumerate() {
                if s_alt.body != p_alt.body { panic!("node {} alt {}: body index mismatch ({} vs {})", idx, j, s_alt.body, p_alt.body); }
                compare_alt_con(idx, j, &s_alt.con, s_table, &p_alt.con, p_table);
                if s_alt.binders.len() != p_alt.binders.len() {
                    panic!("node {} alt {}: binder count mismatch", idx, j);
                }
                let mut alt_olds = Vec::new();
                for (&sv, &pv) in s_alt.binders.iter().zip(p_alt.binders.iter()) {
                    alt_olds.push((sv, var_map.insert(sv, pv)));
                }
                check_node(s_alt.body, single, s_table, split, p_table, var_map, join_map);
                for (sv, a_old) in alt_olds {
                    if let Some(o) = a_old {
                        var_map.insert(sv, o);
                    } else {
                        var_map.remove(&sv);
                    }
                }
            }
            if let Some(o) = old {
                var_map.insert(*s_v, o);
            } else {
                var_map.remove(s_v);
            }
        }
        (CoreFrame::Con { tag: st, fields: sf }, CoreFrame::Con { tag: pt, fields: pf }) => {
            compare_datacon_ids(idx, *st, s_table, *pt, p_table);
            if sf.len() != pf.len() {
                panic!("node {}: Con field count mismatch", idx);
            }
            for (&si, &pi) in sf.iter().zip(pf.iter()) {
                if si != pi { panic!("node {}: Con field index mismatch ({} vs {})", idx, si, pi); }
                check_node(si, single, s_table, split, p_table, var_map, join_map);
            }
        }
        (CoreFrame::Join { label: s_l, params: s_params, rhs: s_r, body: s_b }, CoreFrame::Join { label: p_l, params: p_params, rhs: p_r, body: p_b }) => {
            if s_r != p_r { panic!("node {}: Join rhs index mismatch ({} vs {})", idx, s_r, p_r); }
            if s_b != p_b { panic!("node {}: Join body index mismatch ({} vs {})", idx, s_b, p_b); }
            if s_params.len() != p_params.len() {
                panic!("node {}: Join param count mismatch", idx);
            }
            let l_old = join_map.insert(*s_l, *p_l);
            
            // Join RHS scope
            let mut p_olds = Vec::new();
            for (&sv, &pv) in s_params.iter().zip(p_params.iter()) {
                p_olds.push((sv, var_map.insert(sv, pv)));
            }
            check_node(*s_r, single, s_table, split, p_table, var_map, join_map);
            for (sv, p_old) in p_olds {
                if let Some(o) = p_old {
                    var_map.insert(sv, o);
                } else {
                    var_map.remove(&sv);
                }
            }

            // Join body scope
            check_node(*s_b, single, s_table, split, p_table, var_map, join_map);

            if let Some(o) = l_old {
                join_map.insert(*s_l, o);
            } else {
                join_map.remove(s_l);
            }
        }
        (CoreFrame::Jump { label: s_l, args: s_args }, CoreFrame::Jump { label: p_l, args: p_args }) => {
            if let Some(&expected_p) = join_map.get(s_l) {
                if expected_p != *p_l {
                    panic!("node {}: Jump label mismatch. Expected {:?}, got {:?}", idx, expected_p, p_l);
                }
            }
            if s_args.len() != p_args.len() {
                panic!("node {}: Jump arg count mismatch", idx);
            }
            for (&sa, &pa) in s_args.iter().zip(p_args.iter()) {
                if sa != pa { panic!("node {}: Jump arg index mismatch ({} vs {})", idx, sa, pa); }
                check_node(sa, single, s_table, split, p_table, var_map, join_map);
            }
        }
        (CoreFrame::PrimOp { op: so, args: sa }, CoreFrame::PrimOp { op: po, args: pa }) => {
            if so != po {
                panic!("node {}: PrimOp kind mismatch ({:?} vs {:?})", idx, so, po);
            }
            if sa.len() != pa.len() {
                panic!("node {}: PrimOp arg count mismatch", idx);
            }
            for (&sai, &pai) in sa.iter().zip(pa.iter()) {
                if sai != pai { panic!("node {}: PrimOp arg index mismatch ({} vs {})", idx, sai, pai); }
                check_node(sai, single, s_table, split, p_table, var_map, join_map);
            }
        }
        (s, p) => {
            panic!(
                "node {}: variant mismatch (single={:?}, split={:?})",
                idx, core_kind(s), core_kind(p)
            );
        }
    }
}

fn core_kind<A>(frame: &CoreFrame<A>) -> &'static str {
    match frame {
        CoreFrame::Var(_) => "Var",
        CoreFrame::Lit(_) => "Lit",
        CoreFrame::App { .. } => "App",
        CoreFrame::Lam { .. } => "Lam",
        CoreFrame::LetNonRec { .. } => "LetNonRec",
        CoreFrame::LetRec { .. } => "LetRec",
        CoreFrame::Case { .. } => "Case",
        CoreFrame::Con { .. } => "Con",
        CoreFrame::Join { .. } => "Join",
        CoreFrame::Jump { .. } => "Jump",
        CoreFrame::PrimOp { .. } => "PrimOp",
    }
}

fn compare_alt_con(node_idx: usize, alt_idx: usize, s_con: &AltCon, s_table: &DataConTable, p_con: &AltCon, p_table: &DataConTable) {
    match (s_con, p_con) {
        (AltCon::Default, AltCon::Default) => {}
        (AltCon::LitAlt(sl), AltCon::LitAlt(pl)) => {
            if sl != pl {
                panic!("node {} alt {}: literals differ ({:?} vs {:?})", node_idx, alt_idx, sl, pl);
            }
        }
        (AltCon::DataAlt(si), AltCon::DataAlt(pi)) => {
            compare_datacon_ids_detailed(format!("node {} alt {}", node_idx, alt_idx), *si, s_table, *pi, p_table);
        }
        (s, p) => {
            panic!("node {} alt {}: AltCon variant mismatch (single={:?}, split={:?})", node_idx, alt_idx, s, p);
        }
    }
}

fn compare_datacon_ids(node_idx: usize, s_id: DataConId, s_table: &DataConTable, p_id: DataConId, p_table: &DataConTable) {
    compare_datacon_ids_detailed(format!("node {}", node_idx), s_id, s_table, p_id, p_table);
}

fn compare_datacon_ids_detailed(loc: String, s_id: DataConId, s_table: &DataConTable, p_id: DataConId, p_table: &DataConTable) {
    let s_dc = s_table.get(s_id);
    let p_dc = p_table.get(p_id);

    // Cross-mode DataCon equivalence is tiered:
    //
    // 1. If both sides expose a qualified_name AND they match exactly,
    //    that's a strong match.
    // 2. Otherwise compare unqualified (name, rep_arity). Cross-mode
    //    fixtures legitimately define the same logical constructor in
    //    different modules (e.g. `data Echo` in `Test` vs in `Def`), so
    //    qualified-name *differences* are expected and not by themselves
    //    a divergence — the stable cross-mode key is name+arity.
    //
    // This admits a known false-negative: two genuinely-different
    // constructors that share name+arity across modules will compare
    // equal at this site. That collision case is exercised end-to-end
    // by the dimension_b1/b2 targeted-regression tests via runtime
    // value comparison, not at the structural layer.
    let s_qual = s_dc.and_then(|dc| dc.qualified_name.as_deref());
    let p_qual = p_dc.and_then(|dc| dc.qualified_name.as_deref());
    if let (Some(sq), Some(pq)) = (s_qual, p_qual) {
        if sq == pq {
            return;
        }
        // Fall through to name+arity comparison.
    }

    let s_info = s_dc.map(|dc| (dc.name.as_str(), dc.rep_arity));
    let p_info = p_dc.map(|dc| (dc.name.as_str(), dc.rep_arity));
    if s_info != p_info {
        panic!(
            "cross-mode divergence at {}: DataCon mismatch. \
             single={:?} (id={:?}, qual={:?}), split={:?} (id={:?}, qual={:?})",
            loc, s_info, s_id, s_qual, p_info, p_id, p_qual
        );
    }
}

/// Asserts that every constructor in the single-mode table has a compatible
/// counterpart in the split-mode table. Lookup prefers the module-qualified
/// name when present (so name+arity collisions across modules don't
/// silently match the wrong constructor); falls back to name+arity for
/// legacy entries that lack a qualified name.
pub fn assert_table_compatible(single_table: &DataConTable, split_table: &DataConTable) {
    for s_dc in single_table.iter() {
        let p_dc_id = if let Some(qn) = s_dc.qualified_name.as_deref() {
            split_table
                .get_by_qualified_name(qn)
                .or_else(|| split_table.get_by_name_arity(&s_dc.name, s_dc.rep_arity))
                .unwrap_or_else(|| {
                    panic!(
                        "split table missing DataCon '{}' (qualified='{}', arity {})",
                        s_dc.name, qn, s_dc.rep_arity
                    )
                })
        } else {
            split_table
                .get_by_name_arity(&s_dc.name, s_dc.rep_arity)
                .unwrap_or_else(|| {
                    panic!(
                        "split table missing DataCon '{}' with arity {}",
                        s_dc.name, s_dc.rep_arity
                    )
                })
        };
        let p_dc = split_table.get(p_dc_id).unwrap();
        if s_dc.field_bangs != p_dc.field_bangs {
            panic!(
                "DataCon '{}' has different field_bangs (single={:?}, split={:?})",
                s_dc.name, s_dc.field_bangs, p_dc.field_bangs
            );
        }
    }
}

/// Asserts that two runtime values are equivalent.
pub fn assert_value_equivalent(s_val: &Value, s_table: &DataConTable, p_val: &Value, p_table: &DataConTable) {
    compare_values(Vec::new(), s_val, s_table, p_val, p_table);
}

fn compare_values(path: Vec<String>, s_val: &Value, s_table: &DataConTable, p_val: &Value, p_table: &DataConTable) {
    match (s_val, p_val) {
        (Value::Lit(sl), Value::Lit(pl)) => {
            if sl != pl {
                panic!("value divergence at {}: literals differ ({:?} vs {:?})", format_path(&path), sl, pl);
            }
        }
        (Value::Con(si, sf), Value::Con(pi, pf)) => {
            let s_dc = s_table.get(*si).expect("single_table missing Con ID");
            let p_dc = p_table.get(*pi).expect("split_table missing Con ID");
            if s_dc.name != p_dc.name || s_dc.rep_arity != p_dc.rep_arity {
                panic!("value divergence at {}: constructor mismatch (single={} arity {}, split={} arity {})",
                    format_path(&path), s_dc.name, s_dc.rep_arity, p_dc.name, p_dc.rep_arity);
            }
            if sf.len() != pf.len() {
                panic!("value divergence at {}: field count mismatch (single={}, split={})",
                    format_path(&path), sf.len(), pf.len());
            }
            for (i, (sv, pv)) in sf.iter().zip(pf.iter()).enumerate() {
                let mut new_path = path.clone();
                new_path.push(format!("{}.fields[{}]", s_dc.name, i));
                compare_values(new_path, sv, s_table, pv, p_table);
            }
        }
        (Value::Closure(..), Value::Closure(..)) => {} // Closures are opaque
        (Value::ThunkRef(_), Value::ThunkRef(_)) => {} // Thunks are opaque
        (Value::JoinCont(..), Value::JoinCont(..)) => {} // Join points are opaque
        (Value::ConFun(si, sa, sf), Value::ConFun(pi, pa, pf)) => {
            let s_dc = s_table.get(*si).expect("single_table missing ConFun ID");
            let p_dc = p_table.get(*pi).expect("split_table missing ConFun ID");
            if s_dc.name != p_dc.name || *sa != *pa {
                panic!("value divergence at {}: ConFun mismatch (single={} arity {}, split={} arity {})",
                    format_path(&path), s_dc.name, sa, p_dc.name, pa);
            }
            if sf.len() != pf.len() {
                panic!("value divergence at {}: ConFun arg count mismatch (single={}, split={})",
                    format_path(&path), sf.len(), pf.len());
            }
            for (i, (sv, pv)) in sf.iter().zip(pf.iter()).enumerate() {
                let mut new_path = path.clone();
                new_path.push(format!("{}.partial_args[{}]", s_dc.name, i));
                compare_values(new_path, sv, s_table, pv, p_table);
            }
        }
        (Value::ByteArray(sb), Value::ByteArray(pb)) => {
            let s_bytes = sb.lock().expect("single ByteArray poisoned");
            let p_bytes = pb.lock().expect("split ByteArray poisoned");
            if *s_bytes != *p_bytes {
                panic!("value divergence at {}: ByteArray contents differ", format_path(&path));
            }
        }
        (s, p) => {
            panic!("value divergence at {}: variant mismatch (single={}, split={})",
                format_path(&path), val_kind(s), val_kind(p));
        }
    }
}

fn format_path(path: &[String]) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.join(".")
    }
}

fn val_kind(v: &Value) -> &'static str {
    match v {
        Value::Lit(_) => "Lit",
        Value::Con(..) => "Con",
        Value::Closure(..) => "Closure",
        Value::ThunkRef(_) => "ThunkRef",
        Value::JoinCont(..) => "JoinCont",
        Value::ConFun(..) => "ConFun",
        Value::ByteArray(_) => "ByteArray",
    }
}
