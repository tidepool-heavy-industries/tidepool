use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{AltCon, DataConId};
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

    for (i, (s_node, p_node)) in single.iter().zip(split.iter()).enumerate() {
        match (s_node, p_node) {
            (CoreFrame::Var(_), CoreFrame::Var(_)) => {} // Tolerate any VarId
            (CoreFrame::Lit(s_lit), CoreFrame::Lit(p_lit)) => {
                if s_lit != p_lit {
                    panic!("cross-mode divergence at node {}: literals differ (single={:?}, split={:?})", i, s_lit, p_lit);
                }
            }
            (CoreFrame::App { fun: sf, arg: sa }, CoreFrame::App { fun: pf, arg: pa }) => {
                if sf != pf || sa != pa {
                    panic!("cross-mode divergence at node {}: App indices differ", i);
                }
            }
            (CoreFrame::Lam { binder: _, body: sb }, CoreFrame::Lam { binder: _, body: pb }) => {
                if sb != pb {
                    panic!("cross-mode divergence at node {}: Lam body indices differ", i);
                }
            }
            (CoreFrame::LetNonRec { binder: _, rhs: sr, body: sb }, CoreFrame::LetNonRec { binder: _, rhs: pr, body: pb }) => {
                if sr != pr || sb != pb {
                    panic!("cross-mode divergence at node {}: LetNonRec indices differ", i);
                }
            }
            (CoreFrame::LetRec { bindings: sb, body: s_body }, CoreFrame::LetRec { bindings: pb, body: p_body }) => {
                if sb.len() != pb.len() {
                    panic!("cross-mode divergence at node {}: LetRec binding count mismatch", i);
                }
                for (j, ((_, sr), (_, pr))) in sb.iter().zip(pb.iter()).enumerate() {
                    if sr != pr {
                        panic!("cross-mode divergence at node {}: LetRec binding {} rhs index mismatch", i, j);
                    }
                }
                if s_body != p_body {
                    panic!("cross-mode divergence at node {}: LetRec body index mismatch", i);
                }
            }
            (CoreFrame::Case { scrutinee: ss, binder: _, alts: sa }, CoreFrame::Case { scrutinee: ps, binder: _, alts: pa }) => {
                if ss != ps {
                    panic!("cross-mode divergence at node {}: Case scrutinee index mismatch", i);
                }
                if sa.len() != pa.len() {
                    panic!("cross-mode divergence at node {}: Case alt count mismatch", i);
                }
                for (j, (s_alt, p_alt)) in sa.iter().zip(pa.iter()).enumerate() {
                    compare_alt_con(i, j, &s_alt.con, &art.single_table, &p_alt.con, &art.split_table);
                    if s_alt.binders.len() != p_alt.binders.len() {
                        panic!("cross-mode divergence at node {} alt {}: binder count mismatch", i, j);
                    }
                    if s_alt.body != p_alt.body {
                        panic!("cross-mode divergence at node {} alt {}: body index mismatch", i, j);
                    }
                }
            }
            (CoreFrame::Con { tag: st, fields: sf }, CoreFrame::Con { tag: pt, fields: pf }) => {
                compare_datacon_ids(i, *st, &art.single_table, *pt, &art.split_table);
                if sf.len() != pf.len() {
                    panic!("cross-mode divergence at node {}: Con field count mismatch", i);
                }
                if sf != pf {
                    panic!("cross-mode divergence at node {}: Con field indices differ", i);
                }
            }
            (CoreFrame::Join { label: _, params: sp, rhs: sr, body: sb }, CoreFrame::Join { label: _, params: pp, rhs: pr, body: pb }) => {
                if sp.len() != pp.len() {
                    panic!("cross-mode divergence at node {}: Join param count mismatch", i);
                }
                if sr != pr || sb != pb {
                    panic!("cross-mode divergence at node {}: Join indices differ", i);
                }
            }
            (CoreFrame::Jump { label: _, args: sa }, CoreFrame::Jump { label: _, args: pa }) => {
                if sa.len() != pa.len() {
                    panic!("cross-mode divergence at node {}: Jump arg count mismatch", i);
                }
                if sa != pa {
                    panic!("cross-mode divergence at node {}: Jump arg indices differ", i);
                }
            }
            (CoreFrame::PrimOp { op: so, args: sa }, CoreFrame::PrimOp { op: po, args: pa }) => {
                if so != po {
                    panic!("cross-mode divergence at node {}: PrimOp kind mismatch (single={:?}, split={:?})", i, so, po);
                }
                if sa.len() != pa.len() {
                    panic!("cross-mode divergence at node {}: PrimOp arg count mismatch", i);
                }
                if sa != pa {
                    panic!("cross-mode divergence at node {}: PrimOp arg indices differ", i);
                }
            }
            (s, p) => {
                panic!(
                    "cross-mode divergence at node {}: variant mismatch (single={:?}, split={:?})",
                    i, core_kind(s), core_kind(p)
                );
            }
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
                panic!("cross-mode divergence at node {} alt {}: literals differ ({:?} vs {:?})", node_idx, alt_idx, sl, pl);
            }
        }
        (AltCon::DataAlt(si), AltCon::DataAlt(pi)) => {
            compare_datacon_ids_detailed(format!("node {} alt {}", node_idx, alt_idx), *si, s_table, *pi, p_table);
        }
        (s, p) => {
            panic!("cross-mode divergence at node {} alt {}: AltCon variant mismatch (single={:?}, split={:?})", node_idx, alt_idx, s, p);
        }
    }
}

fn compare_datacon_ids(node_idx: usize, s_id: DataConId, s_table: &DataConTable, p_id: DataConId, p_table: &DataConTable) {
    compare_datacon_ids_detailed(format!("node {}", node_idx), s_id, s_table, p_id, p_table);
}

fn compare_datacon_ids_detailed(loc: String, s_id: DataConId, s_table: &DataConTable, p_id: DataConId, p_table: &DataConTable) {
    let s_info = s_table.get(s_id).map(|dc| (dc.name.as_str(), dc.rep_arity));
    let p_info = p_table.get(p_id).map(|dc| (dc.name.as_str(), dc.rep_arity));

    if s_info != p_info {
        panic!(
            "cross-mode divergence at {}: DataCon mismatch. \
             single={:?} (id={:?}), split={:?} (id={:?})",
            loc, s_info, s_id, p_info, p_id
        );
    }
}

/// Asserts that all constructors in the single-mode table are present and compatible in the split-mode table.
pub fn assert_table_compatible(single_table: &DataConTable, split_table: &DataConTable) {
    for s_dc in single_table.iter() {
        let p_dc_id = split_table.get_by_name_arity(&s_dc.name, s_dc.rep_arity)
            .unwrap_or_else(|| panic!("split table missing DataCon '{}' with arity {}", s_dc.name, s_dc.rep_arity));
        let p_dc = split_table.get(p_dc_id).unwrap();
        if s_dc.field_bangs != p_dc.field_bangs {
            panic!("DataCon '{}' has different field_bangs (single={:?}, split={:?})", s_dc.name, s_dc.field_bangs, p_dc.field_bangs);
        }
    }
    // And vice versa
    for p_dc in split_table.iter() {
        if single_table.get_by_name_arity(&p_dc.name, p_dc.rep_arity).is_none() {
             // It's technically okay if split has MORE datacons (e.g. from extra modules), 
             // as long as the ones used in the expression tree match.
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
