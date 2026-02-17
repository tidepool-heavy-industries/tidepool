use core_repr::*;
use core_repr::frame::CoreFrame;
use core_repr::tree::RecursiveTree;

#[test]
fn core_frame_has_11_variants() {
    // Construct one of each variant to verify they all exist
    let _: CoreFrame<NodeId> = CoreFrame::Var(VarId(0));
    let _: CoreFrame<NodeId> = CoreFrame::Lit(Literal::LitInt(42));
    let _: CoreFrame<NodeId> = CoreFrame::App { fun: NodeId(0), arg: NodeId(1) };
    let _: CoreFrame<NodeId> = CoreFrame::Lam { binder: VarId(0), body: NodeId(0) };
    let _: CoreFrame<NodeId> = CoreFrame::LetNonRec { binder: VarId(0), rhs: NodeId(0), body: NodeId(1) };
    let _: CoreFrame<NodeId> = CoreFrame::LetRec { bindings: vec![], body: NodeId(0) };
    let _: CoreFrame<NodeId> = CoreFrame::Case { scrutinee: NodeId(0), binder: VarId(0), alts: vec![] };
    let _: CoreFrame<NodeId> = CoreFrame::Con { tag: DataConId(0), fields: vec![] };
    let _: CoreFrame<NodeId> = CoreFrame::Join { label: JoinId(0), params: vec![], rhs: NodeId(0), body: NodeId(1) };
    let _: CoreFrame<NodeId> = CoreFrame::Jump { label: JoinId(0), args: vec![] };
    let _: CoreFrame<NodeId> = CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![] };
}

#[test]
fn map_layer_preserves_structure() {
    let frame: CoreFrame<u32> = CoreFrame::App { fun: 1, arg: 2 };
    let mapped: CoreFrame<u64> = frame.map_layer(|x| x as u64 * 10);
    assert_eq!(mapped, CoreFrame::App { fun: 10, arg: 20 });
}

#[test]
fn map_layer_identity() {
    let frame: CoreFrame<u32> = CoreFrame::Case {
        scrutinee: 0,
        binder: VarId(1),
        alts: vec![Alt { con: AltCon::Default, binders: vec![], body: 2 }],
    };
    let mapped = frame.clone().map_layer(|x| x);
    assert_eq!(frame, mapped);
}

#[test]
fn map_layer_leaves_have_no_children() {
    let var: CoreFrame<u32> = CoreFrame::Var(VarId(0));
    let mut called = false;
    let _mapped: CoreFrame<u64> = var.map_layer(|_| { called = true; 0u64 });
    assert!(!called, "Var has no children, f should not be called");
}

#[test]
fn recursive_tree_build_and_access() {
    // Build: (\x -> x)
    let mut tree = RecursiveTree::singleton(CoreFrame::Var(VarId(0)));
    let var_id = tree.root();
    let lam_node = tree.add_node(CoreFrame::Lam { binder: VarId(0), body: var_id });
    tree.set_root(lam_node);
    assert_eq!(tree.len(), 2);
    assert_eq!(tree.root(), lam_node);
}

#[test]
fn cata_counts_nodes() {
    // Build: App(Lit(1), Lit(2))
    let mut tree: CoreExpr = RecursiveTree::singleton(CoreFrame::Lit(Literal::LitInt(1)));
    let lit1 = tree.root();
    let lit2 = tree.add_node(CoreFrame::Lit(Literal::LitInt(2)));
    let app = tree.add_node(CoreFrame::App { fun: lit1, arg: lit2 });
    tree.set_root(app);

    let count = tree.cata(|frame: CoreFrame<usize>| -> usize {
        match frame {
            CoreFrame::App { fun, arg } => 1 + fun + arg,
            _ => 1,
        }
    });
    assert_eq!(count, 3);
}

#[test]
fn ana_builds_tree() {
    // Build a chain: Lam(x0, Lam(x1, Lam(x2, Var(x2))))
    let tree = RecursiveTree::ana(0u32, |depth| {
        if depth >= 3 {
            CoreFrame::Var(VarId(depth))
        } else {
            CoreFrame::Lam { binder: VarId(depth), body: depth + 1 }
        }
    });
    assert_eq!(tree.len(), 4); // 3 Lams + 1 Var
}

#[test]
fn hylo_roundtrip() {
    // hylo that counts depth of a linear chain
    let depth = core_repr::hylo(
        0u32,
        |n| {
            if n >= 5 {
                CoreFrame::Lit(Literal::LitInt(n as i64))
            } else {
                CoreFrame::Lam { binder: VarId(n), body: n + 1 }
            }
        },
        |frame: CoreFrame<usize>| -> usize {
            match frame {
                CoreFrame::Lam { body, .. } => 1 + body,
                _ => 1,
            }
        },
    );
    assert_eq!(depth, 6); // 5 Lams + 1 Lit
}

#[test]
fn core_expr_is_type_alias() {
    // Verify CoreExpr is usable as RecursiveTree<CoreFrame<NodeId>>
    let tree: CoreExpr = RecursiveTree::singleton(CoreFrame::Lit(Literal::LitInt(0)));
    let _: &CoreFrame<NodeId> = tree.node(tree.root());
}
